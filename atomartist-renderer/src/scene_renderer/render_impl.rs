//! [`super::WgpuSceneRenderer`]'s `WgpuCustomRender::render` implementation.
//!
//! Lives in its own file so `scene_renderer/mod.rs` stays under the
//! 800-line guardrail — the render flow itself is ~460 lines of
//! per-pass orchestration and shrinking it without losing clarity
//! would require structural changes we'd rather make alongside a
//! real perf pass, not as part of the line-count fix.

use bytemuck::cast_slice;
use demo_wgpu::{WgpuCustomRender, WgpuCustomRenderCtx};
use glam::Mat4;

use super::accumulation::{
    apply_jitter_to_proj, jitter_offset, MAX_SAMPLES,
};
use super::cache::{handle_cache_hit, CacheOutcome, SceneFingerprint};
use super::depth_peel::pipelines::{MeshHandles, PeelUniforms};
use super::depth_peel::{iteration_count, DEFAULT_LAYERS};
use super::opaque_pass::Uniforms;
use super::post_outline::{self, OutlineUniforms};
use super::timings::{elapsed_ms, log_scene_timings, SceneTimings};
use super::util::normalize3;
use super::{RenderStyle, WgpuSceneRenderer};

impl WgpuCustomRender for WgpuSceneRenderer {
    fn render(&mut self, ctx: WgpuCustomRenderCtx<'_>) {
        let t_total = web_time::Instant::now();
        let t_ensure = web_time::Instant::now();
        self.ensure_state(ctx.device, ctx.queue, ctx.surface_format);
        let ensure_ms = elapsed_ms(t_ensure);

        // Pixel size of the viewport widget rect.  The framebuffer matches
        // this exactly (1:1 mapping), so blit_to runs an effectively no-op
        // bilinear sampler.
        let fb_w = ctx.screen_rect.width.max(1.0) as u32;
        let fb_h = ctx.screen_rect.height.max(1.0) as u32;
        if fb_w == 0 || fb_h == 0 {
            return;
        }

        let t_fb = web_time::Instant::now();
        self.ensure_framebuffer(ctx.device, fb_w, fb_h);
        let fb_ms = elapsed_ms(t_fb);
        let t_mesh = web_time::Instant::now();
        self.ensure_mesh_buffers(ctx.device);
        let mesh_ms = elapsed_ms(t_mesh);

        // Forward theme inputs to the bed before any pass runs — these
        // are cheap (no-ops when unchanged) and let the bed grid /
        // composite-shadow chain track the active theme without extra
        // plumbing in the host widget.
        let grid_line = self.grid_line_color;
        let grid_dark = self.grid_dark_mode;
        if let Some(s) = &mut self.state {
            s.bed.set_line_color(ctx.device, ctx.queue, grid_line);
            s.bed.set_dark_mode(grid_dark);
        }

        // ── Fingerprint / cache hit check ────────────────────────────────
        let current_fp = SceneFingerprint::from_renderer(self);
        let outcome = handle_cache_hit(
            &mut self.last_fingerprint,
            current_fp,
            &mut self.sample_count,
        );
        if matches!(outcome, CacheOutcome::Miss) {
            // Restart the accumulator from sample 0 — the blend pass
            // will pick `weight = 1` so the read slot's stale value
            // is ignored. `accum_read` doesn't need to change; we
            // just swap slots on each sample.
        }

        let sample_count_before = self.sample_count;
        let accum_read_before = self.accum_read;

        let s = match &self.state {
            Some(s) => s,
            None => return,
        };
        let fb = match &s.framebuffer {
            Some(fb) => fb,
            None => return,
        };
        let scene_depth_view = match &s.scene_depth {
            Some((_, v)) => v,
            None => return,
        };
        let scene_depth_color_view = match &s.scene_depth_color {
            Some((_, v)) => v,
            None => return,
        };
        let peel_targets = match &s.peel_targets {
            Some(t) => t,
            None => return,
        };
        let accum_targets = match &s.accum_targets {
            Some(t) => t,
            None => return,
        };
        let output_fb = match &s.output_fb {
            Some(fb) => fb,
            None => return,
        };

        let aspect = fb_w as f32 / fb_h.max(1) as f32;
        let view = Mat4::from_cols_array(&self.camera.view_matrix());
        let proj = Mat4::from_cols_array(&self.camera.projection_matrix(aspect));

        let already_converged = sample_count_before >= MAX_SAMPLES;
        if already_converged {
            output_fb.blit_to(
                ctx.device,
                ctx.encoder,
                ctx.target_view,
                ctx.target_size,
                ctx.screen_rect,
                ctx.parent_clip,
                ctx.pipelines,
            );
            return;
        }

        let (jx, jy) = jitter_offset(sample_count_before);
        let mut proj_arr = proj.to_cols_array();
        apply_jitter_to_proj(&mut proj_arr, jx, jy, fb_w as f32, fb_h as f32);
        let jittered_proj = Mat4::from_cols_array(&proj_arr);
        let mvp = (jittered_proj * view).to_cols_array();
        let jittered_proj_arr = jittered_proj.to_cols_array();
        let view_arr = view.to_cols_array();
        let l0 = normalize3(self.light_dir);
        let l1 = normalize3(self.light_dir1);
        let to_vec4 = |v: [f32; 3]| [v[0], v[1], v[2], 0.0];
        let uniforms = Uniforms {
            proj: jittered_proj_arr,
            view: view_arr,
            light_dir0: to_vec4(l0),
            light_dir1: to_vec4(l1),
            light_diffuse0: to_vec4(self.light_diffuse0),
            light_specular0: to_vec4(self.light_specular0),
            light_ambient0: to_vec4(self.light_ambient0),
            light_diffuse1: to_vec4(self.light_diffuse1),
            light_specular1: to_vec4(self.light_specular1),
            global_ambient: to_vec4(self.global_ambient),
            material_specular: to_vec4(self.material_specular),
            base_color: self.base_color,
            params: [self.shininess, 0.0, 0.0, 0.0],
            resolution: [fb_w as f32, fb_h as f32, 0.0, 0.0],
        };
        s.opaque.write_scene_uniforms(ctx.queue, &uniforms);
        // Suppress an unused warning until the cast_slice import is
        // referenced again (kept here so future per-frame uploads have
        // the helper in scope).
        let _ = cast_slice::<f32, u8>;

        // ── Pass 0: refresh the bed composite (grid + contact shadow) ──────
        let t_bed_composite = web_time::Instant::now();
        let mut bed_ran_chain = false;
        if self.draw_grid {
            let mesh_ref = match (&s.vbuf, &s.ibuf) {
                (Some(vbuf), Some(ibuf)) if s.index_count > 0 => Some(crate::bed::MeshRef {
                    vbuf,
                    ibuf,
                    index_count: s.index_count,
                }),
                _ => None,
            };
            bed_ran_chain = s.bed.render_to_composite(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                mesh_ref,
                s.mesh_ptr as u64,
                // Locked to 0 alongside `bed_render_z` while the
                // bed-Z offset is reworked.
                0.0,
            );
        }
        let bed_composite_ms = elapsed_ms(t_bed_composite);

        // ── Pass 1: opaque scene — bed + mesh-depth-only ─────────
        let draw_surface = self.render_style == RenderStyle::Shaded;
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist scene opaque"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: fb.render_view(),
                        resolve_target: None, // SSAA: no MSAA resolve; downsample is a later blit
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: scene_depth_color_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 1.0,
                                g: 0.0,
                                b: 0.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: scene_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(0.0, 0.0, fb_w as f32, fb_h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, fb_w, fb_h);
            if self.draw_grid {
                let bed_z = self.bed_render_z();
                s.bed.draw_bed(ctx.queue, &mut pass, mvp, bed_z);
            }
            if let (Some(vbuf), Some(ibuf)) = (&s.vbuf, &s.ibuf) {
                if s.index_count > 0 && draw_surface {
                    s.opaque
                        .draw_depth_only(&mut pass, vbuf, ibuf, s.index_count);
                }
            }
        }

        // ── Pass 2: dual depth-peeling chain ──────────────────────────────
        let t_peel = web_time::Instant::now();
        let mesh_handles = if draw_surface {
            match (&s.vbuf, &s.ibuf) {
                (Some(vbuf), Some(ibuf)) if s.index_count > 0 => Some(MeshHandles {
                    vbuf,
                    ibuf,
                    index_count: s.index_count,
                }),
                _ => None,
            }
        } else {
            None
        };
        let peel_uniforms: PeelUniforms = uniforms;
        let iterations = iteration_count(DEFAULT_LAYERS as i32);
        s.dual_peel.execute_chain(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            peel_targets,
            scene_depth_color_view,
            fb.render_view(),
            &accum_targets.sample_view,
            mesh_handles,
            &peel_uniforms,
            iterations,
        );
        let peel_ms = elapsed_ms(t_peel);

        // ── Pass 2.5: post-process selection outline ─────────────────────
        let want_outline = self.outline_enabled
            && self.render_style == RenderStyle::Shaded
            && s.index_count > 0;
        if want_outline {
            if let (Some(vbuf), Some(ibuf), Some(outline_targets)) =
                (&s.vbuf, &s.ibuf, &s.outline_targets)
            {
                let outline_u = OutlineUniforms {
                    mvp,
                    outline_color: self.outline_color,
                    resolution: [fb_w as f32, fb_h as f32, 0.0, 0.0],
                    params: [self.outline_width.max(1.0), 0.35, 0.0, 0.0],
                };
                s.post_outline.execute(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    &outline_u,
                    outline_targets,
                    scene_depth_color_view,
                    &accum_targets.sample_view,
                    post_outline::pipelines_mesh::Mesh {
                        vbuf,
                        ibuf,
                        index_count: s.index_count,
                    },
                    (fb_w, fb_h),
                );
            }
        }

        // ── Pass 2.6: gizmos ─────────────────────────────────────────────
        for gizmo in &self.gizmo_lines {
            let model = gizmo
                .matrix
                .as_ref()
                .map(Mat4::from_cols_array)
                .unwrap_or(Mat4::IDENTITY);
            let gmvp = (jittered_proj * view * model).to_cols_array();
            s.gizmo_pipelines.execute(
                ctx.device,
                ctx.encoder,
                gizmo,
                gmvp,
                &accum_targets.sample_view,
                scene_depth_view,
                (fb_w, fb_h),
            );
        }

        // ── Pass 3: progressive accumulation ──────────────────────────────
        let t_accum = web_time::Instant::now();
        let new_read = s.accum_pipes.execute_blend(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            accum_targets,
            sample_count_before,
            accum_read_before,
        );
        s.accum_pipes.execute_copy_to_surface(
            ctx.device,
            ctx.encoder,
            accum_targets,
            new_read,
            output_fb.render_view(),
            (fb_w, fb_h),
        );
        let accum_ms = elapsed_ms(t_accum);

        // ── Pass 4: composite resolved scene onto the active 2-D target ────
        let t_blit = web_time::Instant::now();
        output_fb.blit_to(
            ctx.device,
            ctx.encoder,
            ctx.target_view,
            ctx.target_size,
            ctx.screen_rect,
            ctx.parent_clip,
            ctx.pipelines,
        );
        let blit_ms = elapsed_ms(t_blit);

        self.sample_count = sample_count_before + 1;
        self.accum_read = new_read;
        if self.sample_count < MAX_SAMPLES {
            // IMPORTANT: must NOT call `request_draw()` — that
            // advances the global invalidation epoch and forces every
            // retained 2-D widget cache to rebuild for the duration
            // of accumulation. Our visual change is confined to this
            // widget's own composite, so the no-invalidation variant
            // is the precise tool.
            agg_gui::animation::request_draw_without_invalidation();
        }
        let total_ms = elapsed_ms(t_total);
        log_scene_timings(SceneTimings {
            total_ms,
            ensure_ms,
            fb_ms,
            mesh_ms,
            bed_composite_ms,
            bed_ran_chain,
            peel_ms,
            accum_ms,
            blit_ms,
        });
    }
}
