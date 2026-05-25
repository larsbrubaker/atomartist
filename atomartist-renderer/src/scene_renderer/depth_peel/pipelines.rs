//! Dual depth peeling pipelines + chain orchestration.
//!
//! This module owns the three render pipelines that execute the
//! MatterCAD-style dual peeling chain:
//!
//! * `init_pipeline` — writes `(-z, z)` to the dual-depth target for
//!   every transparent fragment whose depth passes the opaque-depth
//!   cutoff. Uses MatterCAD's [`CreateDualDepthPeelBlendState`]
//!   colour-target-0 entry: `Max` blend on RG only.
//! * `peel_pipeline` — MRT3 pass run once per peel iteration. Output
//!   target 0 mirrors the init pass's `Max` blend; target 1 uses
//!   premultiplied **under** blending for the front accumulator;
//!   target 2 uses straight **over** blending for the back accumulator.
//! * `resolve_pipeline` — full-screen triangle composing the opaque
//!   scene colour with the front + back accumulators via
//!   [`DualDepthPeelingMath::ResolveForComposition`].
//!
//! The chain's iteration count is fixed at
//! [`super::iteration_count`]`(DEFAULT_LAYERS)` per frame; the public
//! entry point is [`DualPeelPipelines::execute_chain`].

use bytemuck::{Pod, Zeroable};
use glam::Mat4;

use super::pipeline_build::{build_init_pipeline, build_peel_pipeline, build_resolve_pipeline};
use super::DualPeelTargets;
pub use crate::scene_renderer::body_uniform::BodyDrawHandle;
use crate::scene_renderer::body_uniform::{BodyUniform, DYN_OFFSET_ALIGN};
use crate::scene_renderer::opaque_pass::build_body_bgl;

/// Push-constants-style uniform consumed by the dual-depth init shader.
/// The `resolution` field is the framebuffer pixel size — the fragment
/// shader divides `gl_FragCoord.xy` by it to sample the opaque depth at
/// the matching pixel.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct InitUniforms {
    pub mvp: [f32; 16],
    pub resolution: [f32; 4],
}

/// Per-iteration uniform for the peel colour shader. Layout reuses
/// the opaque scene shader's uniform set so a fragment peeled out
/// of the dual-peel chain shades identically to how it would have
/// shaded through the opaque pipeline. See
/// [`crate::scene_renderer::opaque_pass::Uniforms`] for the field
/// breakdown.
pub use crate::scene_renderer::opaque_pass::Uniforms as PeelUniforms;

// Match `super::DUAL_DEPTH_FORMAT`. Half-float is the largest format
// `wgpu` guarantees `Max`-blend support on without a backend-specific
// feature flag.
const DUAL_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Clear colours per MatterCAD: dual-depth slots reset to
/// `(-1, -1, 0, 0)` so the first `Max` blend picks any incoming depth;
/// front accumulator starts at `(0, 0, 0, 1)` so the premultiplied
/// under-blend formula begins with full residual transmittance.
const DUAL_DEPTH_CLEAR: wgpu::Color = wgpu::Color {
    r: -1.0,
    g: -1.0,
    b: 0.0,
    a: 0.0,
};
const FRONT_ACCUM_CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const BACK_ACCUM_CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};

pub struct DualPeelPipelines {
    /// Output format the resolve pipeline writes to. Equals the
    /// accumulation chain's [`super::super::accumulation::SAMPLE_FORMAT`]
    /// (`Rgba16Float`) rather than the surface format — the resolve
    /// feeds the per-sample HDR accumulator, NOT the surface
    /// directly. Stored so callers can sanity-check the wiring.
    resolve_output_format: wgpu::TextureFormat,

    init_pipeline: wgpu::RenderPipeline,
    init_bgl: wgpu::BindGroupLayout,
    init_ub: wgpu::Buffer,

    peel_pipeline: wgpu::RenderPipeline,
    peel_bgl: wgpu::BindGroupLayout,
    peel_ub: wgpu::Buffer,

    resolve_pipeline: wgpu::RenderPipeline,
    resolve_bgl: wgpu::BindGroupLayout,

    /// Per-body bind-group layout — shared with the opaque pass so a
    /// single dynamic uniform buffer drives every per-body pipeline
    /// in the chain. Built via
    /// [`crate::scene_renderer::opaque_pass::build_body_bgl`].
    body_bgl: wgpu::BindGroupLayout,

    /// Per-body bind group resolved against the renderer's dynamic
    /// uniform buffer. Rebuilt by [`Self::rebuild_body_bg`] whenever
    /// that buffer reallocates. `None` until the first body lands.
    body_bg: Option<wgpu::BindGroup>,
}

impl DualPeelPipelines {
    /// Build all three pipelines.
    ///
    /// `resolve_output_format` is the format of the texture the
    /// resolve pass writes to — must match the texture passed as
    /// `output_view` in [`Self::execute_chain`]. For the AtomArtist
    /// viewport this is the accumulation chain's HDR sample target
    /// (`Rgba16Float`), not the surface format.
    pub fn new(device: &wgpu::Device, resolve_output_format: wgpu::TextureFormat) -> Self {
        let body_bgl = build_body_bgl(device);
        let (init_pipeline, init_bgl) = build_init_pipeline(device, &body_bgl);
        let init_ub = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atomartist dual-peel init ub"),
            size: std::mem::size_of::<InitUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (peel_pipeline, peel_bgl) = build_peel_pipeline(device, &body_bgl);
        let peel_ub = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atomartist dual-peel ub"),
            size: std::mem::size_of::<PeelUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (resolve_pipeline, resolve_bgl) = build_resolve_pipeline(device, resolve_output_format);

        Self {
            resolve_output_format,
            init_pipeline,
            init_bgl,
            init_ub,
            peel_pipeline,
            peel_bgl,
            peel_ub,
            resolve_pipeline,
            resolve_bgl,
            body_bgl,
            body_bg: None,
        }
    }

    /// Resolve the per-body bind group. Called by the renderer once
    /// per frame after the dynamic uniform buffer is allocated /
    /// reallocated, identical signature to
    /// [`crate::scene_renderer::opaque_pass::OpaquePipelines::rebuild_body_bg`].
    pub fn rebuild_body_bg(&mut self, device: &wgpu::Device, body_buffer: &wgpu::Buffer) {
        self.body_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist dual-peel body bg"),
            layout: &self.body_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: body_buffer,
                    offset: 0,
                    size: std::num::NonZeroU64::new(std::mem::size_of::<BodyUniform>() as u64),
                }),
            }],
        }));
    }

    pub fn resolve_output_format(&self) -> wgpu::TextureFormat {
        self.resolve_output_format
    }

    /// Drive the full peel chain into `encoder`:
    /// init → `iterations` peel passes → resolve into `output_view`.
    ///
    /// Multi-body iteration: each peel pass draws EVERY body in
    /// `bodies` before the next iteration starts. Per-body uniforms
    /// (model matrix, colour, flags) come from the renderer's
    /// dynamic uniform buffer via the bind group resolved by
    /// [`Self::rebuild_body_bg`] — the caller is responsible for
    /// invoking that on a buffer realloc.
    ///
    /// Skips body draws silently when `bodies` is empty — the
    /// resolve still runs against the (cleared) accumulators so
    /// `output_view` receives a correct passthrough of the opaque
    /// scene colour.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_chain(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        targets: &DualPeelTargets,
        opaque_depth_color_view: &wgpu::TextureView,
        opaque_color_view: &wgpu::TextureView,
        output_view: &wgpu::TextureView,
        bodies: &[BodyDrawHandle<'_>],
        peel_uniforms: &PeelUniforms,
        iterations: u32,
    ) {
        // Init uniforms reuse the MVP + resolution slice of the peel
        // uniforms — keeps the shader bindings cheap and avoids a
        // second per-frame write. The peel uniforms hold `proj` and
        // `view` separately so the colour shader can pass view-space
        // position through; the init shader still wants a combined
        // MVP, so we fold them here.
        let proj = Mat4::from_cols_array(&peel_uniforms.proj);
        let view = Mat4::from_cols_array(&peel_uniforms.view);
        let mvp = (proj * view).to_cols_array();
        let init_uniforms = InitUniforms {
            mvp,
            resolution: peel_uniforms.resolution,
        };
        queue.write_buffer(&self.init_ub, 0, bytemuck::bytes_of(&init_uniforms));
        queue.write_buffer(&self.peel_ub, 0, bytemuck::bytes_of(peel_uniforms));

        // ---- Pass A: init ------------------------------------------------
        let init_bg = self.build_init_bind_group(device, opaque_depth_color_view);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist dual-peel init"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.dual_depth_views[0],
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(DUAL_DEPTH_CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let (w, h) = targets.size();
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, w, h);
            if let Some(body_bg) = &self.body_bg {
                pass.set_pipeline(&self.init_pipeline);
                pass.set_bind_group(0, &init_bg, &[]);
                for h in bodies {
                    let off = h.body_index * DYN_OFFSET_ALIGN;
                    pass.set_bind_group(1, body_bg, &[off]);
                    pass.set_vertex_buffer(0, h.vbuf.slice(..));
                    pass.set_vertex_buffer(1, h.cbuf.slice(..));
                    pass.set_index_buffer(h.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..h.index_count, 0, 0..1);
                }
            }
        }

        // ---- Pass B: peel iterations -------------------------------------
        // Both accumulators start cleared on the first iteration; subsequent
        // iterations LOAD so the under / over blends accrete across all
        // iterations (the WHOLE point of the chain).
        let mut accum_load = (
            wgpu::LoadOp::Clear(FRONT_ACCUM_CLEAR),
            wgpu::LoadOp::Clear(BACK_ACCUM_CLEAR),
        );
        for i in 0..iterations {
            let (src_dual_depth, dst_dual_depth) = targets.dual_depth_for_iteration(i);
            let peel_bg =
                self.build_peel_bind_group(device, opaque_depth_color_view, src_dual_depth);
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("atomartist dual-peel iter"),
                    color_attachments: &[
                        Some(wgpu::RenderPassColorAttachment {
                            view: dst_dual_depth,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(DUAL_DEPTH_CLEAR),
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: &targets.front_accum_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: accum_load.0,
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                        Some(wgpu::RenderPassColorAttachment {
                            view: &targets.back_accum_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: accum_load.1,
                                store: wgpu::StoreOp::Store,
                            },
                        }),
                    ],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                let (w, h) = targets.size();
                pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
                pass.set_scissor_rect(0, 0, w, h);
                if let Some(body_bg) = &self.body_bg {
                    pass.set_pipeline(&self.peel_pipeline);
                    pass.set_bind_group(0, &peel_bg, &[]);
                    for h in bodies {
                        let off = h.body_index * DYN_OFFSET_ALIGN;
                        pass.set_bind_group(1, body_bg, &[off]);
                        pass.set_vertex_buffer(0, h.vbuf.slice(..));
                        pass.set_index_buffer(h.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..h.index_count, 0, 0..1);
                    }
                }
            }
            accum_load = (wgpu::LoadOp::Load, wgpu::LoadOp::Load);
        }

        // If we never ran an iteration, the accumulators still hold their
        // construction-time clear values rather than `FRONT_ACCUM_CLEAR` /
        // `BACK_ACCUM_CLEAR`. Open an empty pass to apply the per-frame
        // clears so the resolve sees the right starting state.
        if iterations == 0 {
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist dual-peel zero-iter clear"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &targets.front_accum_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(FRONT_ACCUM_CLEAR),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &targets.back_accum_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(BACK_ACCUM_CLEAR),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        // ---- Pass C: resolve --------------------------------------------
        let resolve_bg = self.build_resolve_bind_group(device, opaque_color_view, targets);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist dual-peel resolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let (w, h) = targets.size();
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, w, h);
            pass.set_pipeline(&self.resolve_pipeline);
            pass.set_bind_group(0, &resolve_bg, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn build_init_bind_group(
        &self,
        device: &wgpu::Device,
        opaque_depth_color: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist dual-peel init bg"),
            layout: &self.init_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.init_ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(opaque_depth_color),
                },
            ],
        })
    }

    fn build_peel_bind_group(
        &self,
        device: &wgpu::Device,
        opaque_depth_color: &wgpu::TextureView,
        src_dual_depth: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist dual-peel bg"),
            layout: &self.peel_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.peel_ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(opaque_depth_color),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(src_dual_depth),
                },
            ],
        })
    }

    fn build_resolve_bind_group(
        &self,
        device: &wgpu::Device,
        opaque_color: &wgpu::TextureView,
        targets: &DualPeelTargets,
    ) -> wgpu::BindGroup {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atomartist dual-peel resolve sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist dual-peel resolve bg"),
            layout: &self.resolve_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(opaque_color),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.front_accum_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&targets.back_accum_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        })
    }
}

/// Borrowed handles to the user mesh's GPU buffers — passed through
/// to [`DualPeelPipelines::execute_chain`] when there's transparent
/// geometry to peel. The same shape used by the rest of the renderer
/// so callers can hand-roll a value without depending on this module's
/// internals.
#[derive(Clone, Copy)]
pub struct MeshHandles<'a> {
    pub vbuf: &'a wgpu::Buffer,
    pub ibuf: &'a wgpu::Buffer,
    pub index_count: u32,
}

// `BodyDrawHandle` re-exported above — defined in
// `scene_renderer::body_uniform` so it's shared with the bed shadow
// caster + future outline multi-body iteration.
