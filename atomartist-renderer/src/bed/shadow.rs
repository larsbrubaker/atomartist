//! Off-screen contact-shadow + composite chain for the bed.
//!
//! Port of NodeDesigner's
//! [`contact-shadows.js`](../../../../../FDS/NodeDesigner/static/js/node-editor/rendering/contact-shadows.js):
//!
//! 1. Render the displayed mesh as a flat black silhouette into
//!    `shadow_tex` from a top-down orthographic camera covering the
//!    bed footprint.
//! 2. Ping-pong a separable 9-tap Gaussian blur through `blur_a` /
//!    `blur_b` to soften the silhouette.
//! 3. Composite the baked bed grid texture with the blurred shadow
//!    into `composite_tex`. The bed quad pipeline samples this single
//!    composite texture.
//! 4. Re-build `composite_tex`'s mip chain in-place so the bed quad
//!    can sample with a linear-mipmap sampler and avoid moiré at
//!    glancing angles.
//!
//! All offscreen attachments are `sample_count = 1`. The bed-quad pass
//! that consumes `composite_tex` runs inside the main 3-D MSAA
//! framebuffer (`SAMPLE_COUNT = 4` in `scene_renderer`), so the
//! resolve-and-blit-to-screen path is unchanged.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use super::shadow_pipelines::{
    alloc_chain_tex, build_blur_pipeline, build_composite_pipeline, build_mip_pipeline,
    build_shadow_caster_pipeline, BlitVertex,
};
use super::texture::mip_level_count;

/// Size of the offscreen shadow / blur / composite textures. NodeDesigner
/// uses 1024 — a good trade-off between blur quality and bandwidth.
pub const SHADOW_TEX_SIZE: u32 = 1024;

/// Half-extent of the orthographic shadow camera. The bed quad lives in
/// world coords `[-BED_HALF_EXTENT, BED_HALF_EXTENT]` on XY, so a
/// silhouette ortho with the same bounds maps 1:1 between the bed UVs
/// and the shadow texture UVs.
pub const BED_HALF_EXTENT: f32 = 100.0;

/// Gaussian blur radius in pixels — verbatim from NodeDesigner.
const BLUR_RADIUS: f32 = 4.0;

/// Shadow opacity multiplier — same default as NodeDesigner. Stored on
/// the renderer so callers can tune it without rebuilding the
/// pipeline.
pub const DEFAULT_SHADOW_OPACITY: f32 = 0.35;

/// Color attachment format for the silhouette / blur / composite chain.
/// Linear (non-sRGB) keeps the Gaussian arithmetic and the composite's
/// `grid * a + shadow * (1 - a)` math in a perceptually-uniform space —
/// the final sRGB encoding happens at the bed-quad → MSAA write.
pub const CHAIN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowCasterUniforms {
    mvp: [f32; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlurUniforms {
    inv_resolution: [f32; 2],
    direction: [f32; 2],
    radius: f32,
    _pad: f32,
    _pad2: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniforms {
    /// xy = (shadow_opacity, invert_flag), zw = pad.
    params: [f32; 4],
}

/// Mesh buffers a caller hands to [`ShadowChain::render`] so the
/// silhouette pass can rasterize the shadow caster. Pass `None` to
/// produce a grid-only composite (empty shadow).
#[derive(Clone, Copy)]
pub struct MeshRef<'a> {
    pub vbuf: &'a wgpu::Buffer,
    pub ibuf: &'a wgpu::Buffer,
    pub index_count: u32,
}

/// Pipelines + offscreen attachments. Owned by [`super::BedRenderer`];
/// the [`ShadowChain::render`] method records all four passes into the
/// caller's encoder, leaving `composite_tex` populated for the bed quad.
///
/// `wgpu` keeps the underlying texture alive via the view's internal
/// `Arc`, so the storage textures themselves don't need separate
/// fields — only the views we sample / render with.
pub(super) struct ShadowChain {
    shadow_view: wgpu::TextureView,
    blur_a_view: wgpu::TextureView,
    blur_b_view: wgpu::TextureView,
    composite_view: wgpu::TextureView,
    /// Per-mip views of `composite_tex` (level 0 first). The render pass
    /// targets level i and samples from level i-1, so we hold one view
    /// per level.
    composite_mip_views: Vec<wgpu::TextureView>,
    composite_mip_count: u32,

    quad_vbuf: wgpu::Buffer,

    shadow_caster_pipeline: wgpu::RenderPipeline,
    shadow_caster_bgl: wgpu::BindGroupLayout,

    blur_pipeline: wgpu::RenderPipeline,
    blur_bgl: wgpu::BindGroupLayout,

    composite_pipeline: wgpu::RenderPipeline,
    composite_bgl: wgpu::BindGroupLayout,

    /// Pass-through downsample pipeline used to populate
    /// `composite_tex` mips 1..N from mip 0. Same texture, separate
    /// view per level.
    mip_pipeline: wgpu::RenderPipeline,
    mip_bgl: wgpu::BindGroupLayout,

    /// Linear-clamp sampler reused across blur / composite / mip gen.
    linear_sampler: wgpu::Sampler,

    composite_opacity: f32,
    composite_invert: f32,
}

impl ShadowChain {
    pub(super) fn new(
        device: &wgpu::Device,
        composite_format: wgpu::TextureFormat,
    ) -> Self {
        // ── Offscreen attachments ─────────────────────────────────────
        let shadow_view = alloc_chain_tex(device, "atomartist bed shadow", CHAIN_FORMAT, 1)
            .create_view(&wgpu::TextureViewDescriptor::default());
        let blur_a_view = alloc_chain_tex(device, "atomartist bed blur a", CHAIN_FORMAT, 1)
            .create_view(&wgpu::TextureViewDescriptor::default());
        let blur_b_view = alloc_chain_tex(device, "atomartist bed blur b", CHAIN_FORMAT, 1)
            .create_view(&wgpu::TextureViewDescriptor::default());

        let composite_mip_count = mip_level_count(SHADOW_TEX_SIZE, SHADOW_TEX_SIZE);
        let composite_tex = alloc_chain_tex(
            device,
            "atomartist bed composite",
            composite_format,
            composite_mip_count,
        );
        let composite_view = composite_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("atomartist bed composite all mips"),
            ..Default::default()
        });
        let composite_mip_views: Vec<wgpu::TextureView> = (0..composite_mip_count)
            .map(|level| {
                composite_tex.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("atomartist bed composite mip"),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();

        // ── Shared quad vbuf (2-tri full-screen, NDC) ────────────────
        let quad: [BlitVertex; 6] = [
            BlitVertex { pos: [-1.0, -1.0], uv: [0.0, 1.0] },
            BlitVertex { pos: [ 1.0, -1.0], uv: [1.0, 1.0] },
            BlitVertex { pos: [ 1.0,  1.0], uv: [1.0, 0.0] },
            BlitVertex { pos: [-1.0, -1.0], uv: [0.0, 1.0] },
            BlitVertex { pos: [ 1.0,  1.0], uv: [1.0, 0.0] },
            BlitVertex { pos: [-1.0,  1.0], uv: [0.0, 0.0] },
        ];
        let quad_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist bed blit vb"),
            contents: bytemuck::cast_slice(&quad),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // ── Linear-clamp sampler — used everywhere except the bed-quad
        //    (which has its own mipmap sampler in `super::mod`).
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atomartist bed chain sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // ── Shadow caster pipeline ───────────────────────────────────
        let (shadow_caster_pipeline, shadow_caster_bgl) =
            build_shadow_caster_pipeline(device, CHAIN_FORMAT);

        // ── Blur pipeline ────────────────────────────────────────────
        let (blur_pipeline, blur_bgl) = build_blur_pipeline(device, CHAIN_FORMAT);

        // ── Composite pipeline ───────────────────────────────────────
        let (composite_pipeline, composite_bgl) =
            build_composite_pipeline(device, composite_format);

        // ── Mip-gen pipeline ─────────────────────────────────────────
        let (mip_pipeline, mip_bgl) = build_mip_pipeline(device, composite_format);

        Self {
            shadow_view,
            blur_a_view,
            blur_b_view,
            composite_view,
            composite_mip_views,
            composite_mip_count,
            quad_vbuf,
            shadow_caster_pipeline,
            shadow_caster_bgl,
            blur_pipeline,
            blur_bgl,
            composite_pipeline,
            composite_bgl,
            mip_pipeline,
            mip_bgl,
            linear_sampler,
            composite_opacity: DEFAULT_SHADOW_OPACITY,
            composite_invert: 0.0,
        }
    }

    pub(super) fn composite_view(&self) -> &wgpu::TextureView {
        &self.composite_view
    }

    /// `is_dark` mirrors NodeDesigner's `setShadowTheme(isLightMode)` —
    /// when set, the composite shader inverts black shadows into white
    /// so they remain visible against a dark backdrop.
    pub(super) fn set_dark_mode(&mut self, is_dark: bool) {
        self.composite_invert = if is_dark { 1.0 } else { 0.0 };
    }

    pub(super) fn shadow_opacity(&self) -> f32 {
        self.composite_opacity
    }

    /// Record the silhouette → blur → composite → mip chain into
    /// `encoder`. When `mesh` is `None`, the silhouette pass just
    /// clears its target so the composite is grid-only.
    ///
    /// `camera_center_xy` is the world-space pivot the main camera
    /// orbits around — the shadow ortho is anchored to its XY so the
    /// silhouette stays centred under the model as the user pans.
    pub(super) fn render(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        mesh: Option<MeshRef<'_>>,
        grid_view: &wgpu::TextureView,
        bed_z: f32,
        camera_center_xy: [f32; 2],
    ) {
        self.run_shadow_pass(device, encoder, mesh, bed_z, camera_center_xy);
        self.run_blur_pass(device, encoder, &self.shadow_view, &self.blur_a_view, [1.0, 0.0]);
        self.run_blur_pass(device, encoder, &self.blur_a_view, &self.blur_b_view, [0.0, 1.0]);
        self.run_composite_pass(device, encoder, grid_view, &self.blur_b_view);
        self.regenerate_composite_mips(device, encoder);
    }

    fn run_shadow_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        mesh: Option<MeshRef<'_>>,
        bed_z: f32,
        camera_center_xy: [f32; 2],
    ) {
        // Top-down ortho looking from +Z toward -Z in our Z-up world,
        // centered on the camera target. wgpu/glam ortho convention
        // (NDC z in [0, 1]) — matches the rest of the renderer.
        let half = BED_HALF_EXTENT;
        let proj = Mat4::orthographic_rh(-half, half, -half, half, 0.1, 400.0);
        let eye = Vec3::new(camera_center_xy[0], camera_center_xy[1], bed_z + 200.0);
        let target = Vec3::new(camera_center_xy[0], camera_center_xy[1], bed_z);
        let view = Mat4::look_at_rh(eye, target, Vec3::Y);
        let mvp = (proj * view).to_cols_array();

        let uniforms = ShadowCasterUniforms { mvp };
        let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist bed shadow ub"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist bed shadow bg"),
            layout: &self.shadow_caster_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ub.as_entire_binding(),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atomartist bed shadow pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.shadow_view,
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
        pass.set_viewport(
            0.0,
            0.0,
            SHADOW_TEX_SIZE as f32,
            SHADOW_TEX_SIZE as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(0, 0, SHADOW_TEX_SIZE, SHADOW_TEX_SIZE);
        if let Some(m) = mesh {
            if m.index_count > 0 {
                pass.set_pipeline(&self.shadow_caster_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.set_vertex_buffer(0, m.vbuf.slice(..));
                pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.index_count, 0, 0..1);
            }
        }
        drop(pass);
        let _ = (ub, bg);
    }

    fn run_blur_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        direction: [f32; 2],
    ) {
        let uniforms = BlurUniforms {
            inv_resolution: [
                1.0 / SHADOW_TEX_SIZE as f32,
                1.0 / SHADOW_TEX_SIZE as f32,
            ],
            direction,
            radius: BLUR_RADIUS,
            _pad: 0.0,
            _pad2: [0.0; 2],
        };
        let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist bed blur ub"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist bed blur bg"),
            layout: &self.blur_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atomartist bed blur pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
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
        pass.set_viewport(
            0.0,
            0.0,
            SHADOW_TEX_SIZE as f32,
            SHADOW_TEX_SIZE as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(0, 0, SHADOW_TEX_SIZE, SHADOW_TEX_SIZE);
        pass.set_pipeline(&self.blur_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_vertex_buffer(0, self.quad_vbuf.slice(..));
        pass.draw(0..6, 0..1);
        drop(pass);
        let _ = (ub, bg);
    }

    fn run_composite_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        grid_view: &wgpu::TextureView,
        shadow_view: &wgpu::TextureView,
    ) {
        let uniforms = CompositeUniforms {
            params: [self.composite_opacity, self.composite_invert, 0.0, 0.0],
        };
        let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist bed composite ub"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist bed composite bg"),
            layout: &self.composite_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(grid_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atomartist bed composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.composite_mip_views[0],
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
        pass.set_viewport(
            0.0,
            0.0,
            SHADOW_TEX_SIZE as f32,
            SHADOW_TEX_SIZE as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(0, 0, SHADOW_TEX_SIZE, SHADOW_TEX_SIZE);
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_vertex_buffer(0, self.quad_vbuf.slice(..));
        pass.draw(0..6, 0..1);
        drop(pass);
        let _ = (ub, bg);
    }

    fn regenerate_composite_mips(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        for level in 1..self.composite_mip_count {
            let src_view = &self.composite_mip_views[(level - 1) as usize];
            let dst_view = &self.composite_mip_views[level as usize];
            let dst_w = (SHADOW_TEX_SIZE >> level).max(1);
            let dst_h = (SHADOW_TEX_SIZE >> level).max(1);
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("atomartist bed mip bg"),
                layout: &self.mip_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist bed mip pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
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
            pass.set_viewport(0.0, 0.0, dst_w as f32, dst_h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, dst_w, dst_h);
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(0, self.quad_vbuf.slice(..));
            pass.draw(0..6, 0..1);
            drop(pass);
            let _ = bg;
        }
    }
}
