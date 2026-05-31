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
use super::depth_peel::pipelines::{BodyDrawHandle, PeelUniforms};
use super::depth_peel::{iteration_count, DEFAULT_LAYERS};
use super::opaque_pass::Uniforms;
use super::post_outline::{self, OutlineUniforms};
use super::timings::{elapsed_ms, log_scene_timings, SceneTimings};
use super::util::normalize3;
use super::{RenderStyle, WgpuSceneRenderer};

/// Cache key for the bed-shadow chain. Hashes mesh pointer + matrix
/// per body so a drag (mesh ptr unchanged, matrix shifts) rolls the
/// key and forces the shadow to re-render at the body's new
/// position. Without the matrix term the shadow cache would hit and
/// the silhouette would stick under the body's pre-drag position.
///
/// Quantised to integers (`1e4` factor) to silence trivial fp
/// noise from matrix recomposition.
pub(crate) fn shadow_cache_key(bodies: &[atomartist_lib::geometry::Body]) -> u64 {
    bodies.iter().fold(0u64, |acc, b| {
        let mesh_ptr = b.mesh.vert_properties.as_ptr() as usize as u64;
        let mut k = acc.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ mesh_ptr;
        for &f in &b.matrix {
            let q = (f * 1e4).round() as i64 as u64;
            k = k.wrapping_mul(0x100000001B3) ^ q;
        }
        k
    })
}

#[cfg(test)]
mod shadow_key_tests {
    use super::shadow_cache_key;
    use atomartist_lib::geometry::Body;
    use manifold_rust::types::MeshGL;
    use std::sync::Arc;

    fn body() -> Body {
        let mesh = Arc::new(MeshGL {
            num_prop: 6,
            vert_properties: vec![0.0; 6],
            tri_verts: vec![0, 0, 0],
            ..Default::default()
        });
        Body::from_mesh(mesh)
    }

    #[test]
    fn key_changes_when_a_body_moves() {
        // Mesh data identical, only matrix shifts → shadow cache
        // MUST treat this as a different scene so the silhouette
        // re-rasterises at the new position.
        let a = body();
        let mut b = a.clone();
        b.matrix[12] = 5.0; // translate X
        let k_a = shadow_cache_key(&[a]);
        let k_b = shadow_cache_key(&[b]);
        assert_ne!(k_a, k_b, "matrix shift must roll the shadow key");
    }

    #[test]
    fn key_stable_for_identical_bodies() {
        let a = body();
        let b = a.clone();
        assert_eq!(shadow_cache_key(&[a]), shadow_cache_key(&[b]));
    }
}

/// Model matrix for the body the selection-outline ID prepass
/// rasterises. `index` addresses `bodies` (built in lockstep with the
/// GPU body list by `ensure_body_buffers`). Returns identity when the
/// index is out of range — a defensive fallback that draws the
/// silhouette at the mesh origin rather than panicking.
///
/// This is the seam that keeps the outline glued to its body: the ID
/// prepass shader multiplies each LOCAL mesh vertex by
/// `proj · view · model`, so handing it the body's own matrix is what
/// makes the silhouette translate and rotate with the body. Returning
/// identity here (the prior behaviour) froze the outline at the
/// untransformed mesh position.
fn outline_model_matrix(bodies: &[atomartist_lib::geometry::Body], index: usize) -> [f32; 16] {
    bodies.get(index).map(|b| b.matrix).unwrap_or_else(|| {
        let mut m = [0.0_f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        m
    })
}

#[cfg(test)]
mod outline_matrix_tests {
    use super::outline_model_matrix;
    use atomartist_lib::geometry::Body;
    use manifold_rust::types::MeshGL;
    use std::sync::Arc;

    fn body_at(tx: f32) -> Body {
        let mesh = Arc::new(MeshGL {
            num_prop: 6,
            vert_properties: vec![0.0; 6],
            tri_verts: vec![0, 0, 0],
            ..Default::default()
        });
        let mut m = [0.0_f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        m[12] = tx;
        Body::from_mesh(mesh).with_matrix(m)
    }

    #[test]
    fn outline_uses_the_selected_bodys_matrix_not_identity() {
        // Two bodies translated to different X. The outline MVP must
        // pick up the *selected* body's translation — the regression
        // that detached the outline used identity (tx = 0) for every
        // body regardless of where it had been moved.
        let bodies = [body_at(3.0), body_at(-7.0)];
        assert_eq!(outline_model_matrix(&bodies, 0)[12], 3.0);
        assert_eq!(outline_model_matrix(&bodies, 1)[12], -7.0);
    }

    #[test]
    fn out_of_range_index_falls_back_to_identity() {
        let bodies = [body_at(9.0)];
        let m = outline_model_matrix(&bodies, 5);
        assert_eq!(m[12], 0.0, "fallback must not inherit a stale translation");
        assert_eq!(m[0], 1.0);
        assert_eq!(m[15], 1.0);
    }
}

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

        // Keep `viewport_size` in lockstep with the framebuffer dimensions
        // pulled from `ctx.screen_rect`. The cache fingerprint feeds
        // `viewport_size` into `fb_size` + the projection's aspect ratio,
        // so a resize (window resize, splitter drag, etc.) only invalidates
        // the cache when this field reflects the new dimensions. Without
        // this assignment the field stays at its `(0, 0)` initial value
        // forever, the fingerprint never sees a resize, and after the
        // accumulator converges the renderer short-circuits to a blit of
        // a freshly-reallocated (empty) output framebuffer.
        self.viewport_size = (fb_w, fb_h);

        let t_fb = web_time::Instant::now();
        self.ensure_framebuffer(ctx.device, fb_w, fb_h);
        let fb_ms = elapsed_ms(t_fb);
        let t_mesh = web_time::Instant::now();
        let body_buffer_realloc = self.ensure_body_buffers(ctx.device, ctx.queue);
        // Rebuild the per-pipeline body bind groups whenever the
        // underlying uniform buffer reallocates — the bind group
        // resource pointer would otherwise dangle. Cheap on stable
        // body counts (no rebuild) and at worst once per body-count
        // growth (powers of two).
        if body_buffer_realloc {
            if let Some(s) = &mut self.state {
                if let Some(buf) = s.body_uniforms.buffer.as_ref() {
                    let buf_clone = buf.clone();
                    s.opaque.rebuild_body_bg(ctx.device, &buf_clone);
                    s.dual_peel.rebuild_body_bg(ctx.device, &buf_clone);
                    s.bed.rebuild_body_bg(ctx.device, &buf_clone);
                }
            }
        }
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
        //
        // Multi-body bed shadow: every body casts. Same dynamic
        // uniform buffer feeds the shadow caster shader's per-body
        // model matrix, so each body's silhouette appears at its
        // world-space position. NodeDesigner's contact-shadows.js
        // does the equivalent — every transparentGroup mesh is
        // material-swapped to the shadow caster and rendered into a
        // single shadow texture before the blur + composite.
        let t_bed_composite = web_time::Instant::now();
        let mut bed_ran_chain = false;
        if self.draw_grid {
            let shadow_bodies: Vec<super::body_uniform::BodyDrawHandle> = s
                .bodies_gpu
                .iter()
                .enumerate()
                .filter(|(_, b)| b.index_count > 0)
                .map(|(i, b)| super::body_uniform::BodyDrawHandle {
                    vbuf: &b.vbuf,
                    ibuf: &b.ibuf,
                    cbuf: &b.cbuf,
                    index_count: b.index_count,
                    body_index: i as u32,
                })
                .collect();
            let shadow_caster_key = shadow_cache_key(&self.bodies);
            bed_ran_chain = s.bed.render_to_composite(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &shadow_bodies,
                shadow_caster_key,
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
            if draw_surface {
                // Per-body depth-only — populates the opaque depth
                // attachment + R32Float depth-colour mirror for the
                // dual-peel discard. Each body's model matrix lives
                // in its slot of the dynamic uniform buffer
                // (`body_index` indexes the slot).
                for (body_index, body) in s.bodies_gpu.iter().enumerate() {
                    if body.index_count == 0 {
                        continue;
                    }
                    s.opaque.draw_body_depth_only(
                        &mut pass,
                        &body.vbuf,
                        &body.ibuf,
                        &body.cbuf,
                        body.index_count,
                        body_index as u32,
                    );
                }
            }
        }

        // ── Pass 2: dual depth-peeling chain ──────────────────────────────
        let t_peel = web_time::Instant::now();
        let body_handles: Vec<BodyDrawHandle> = if draw_surface {
            s.bodies_gpu
                .iter()
                .enumerate()
                .filter(|(_, b)| b.index_count > 0)
                .map(|(i, b)| BodyDrawHandle {
                    vbuf: &b.vbuf,
                    ibuf: &b.ibuf,
                    cbuf: &b.cbuf,
                    index_count: b.index_count,
                    body_index: i as u32,
                })
                .collect()
        } else {
            Vec::new()
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
            &body_handles,
            &peel_uniforms,
            iterations,
        );
        let peel_ms = elapsed_ms(t_peel);

        // ── Pass 2.5: post-process selection outline ─────────────────────
        //
        // Multi-body outline: phase-1 outlines just the first body
        // (the original single-mesh behaviour). Full multi-body
        // outline — every selected body contributes its silhouette
        // to the same ID texture — lands in a follow-up. The
        // outline pipeline needs the same body-uniform binding as
        // opaque + peel before it can iterate bodies.
        let want_outline = self.outline_enabled
            && self.render_style == RenderStyle::Shaded
            && !s.bodies_gpu.is_empty();
        if want_outline {
            // Pick the body to outline: host-supplied index (matches
            // the body whose origin == active selection), or the
            // first body when no index was set / out of range. The
            // selection state lives on the host side (viewport
            // `ViewportInputs::selection`), so the renderer never
            // looks up origin → body itself. `self.bodies` and
            // `s.bodies_gpu` are built in lockstep by
            // `ensure_body_buffers`, so this index addresses the same
            // body in both.
            let outline_idx = self
                .outline_body_index
                .filter(|&i| i < s.bodies_gpu.len())
                .unwrap_or(0);
            let outline_body = s.bodies_gpu.get(outline_idx);
            if let (Some(body), Some(outline_targets)) = (outline_body, &s.outline_targets)
            {
                // The ID prepass rasterises the selected mesh's LOCAL
                // vertices, so its MVP must fold in that body's model
                // matrix — exactly like the opaque + peel passes, which
                // apply the matrix per-body in the shader. Without it
                // the silhouette renders at the mesh's untransformed
                // origin and the outline detaches from the body the
                // moment it's translated or rotated.
                let model =
                    Mat4::from_cols_array(&outline_model_matrix(&self.bodies, outline_idx));
                let outline_mvp = (jittered_proj * view * model).to_cols_array();
                let outline_u = OutlineUniforms {
                    mvp: outline_mvp,
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
                        vbuf: &body.vbuf,
                        ibuf: &body.ibuf,
                        index_count: body.index_count,
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
        // Filled-triangle gizmos — handle meshes for the control
        // gizmos. Share the same pipeline pair as the line gizmos but
        // route through the TriangleList variant with back-face culling.
        for gizmo in &self.gizmo_triangles {
            let model = gizmo
                .matrix
                .as_ref()
                .map(Mat4::from_cols_array)
                .unwrap_or(Mat4::IDENTITY);
            let gmvp = (jittered_proj * view * model).to_cols_array();
            s.gizmo_pipelines.execute_tri(
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
