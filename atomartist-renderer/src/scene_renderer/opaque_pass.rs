//! Opaque scene pipelines for [`super::WgpuSceneRenderer`].
//!
//! Owns the wgpu resources that draw the solid mesh surface (Blinn-Phong
//! shaded) and the depth-only twin used to populate the opaque-pass
//! depth attachment without writing colour. Knows nothing about depth
//! peeling or accumulation — those run after the opaque pass against
//! the populated scene-depth attachment.
//!
//! Public surface:
//!
//! * [`Uniforms`] / [`Vertex`] — packed layouts the orchestrator writes
//!   per frame and the shaders consume.
//! * [`OpaquePipelines`] — built once during
//!   `WgpuSceneRenderer::ensure_state`, holds the scene + depth-only
//!   pipelines, the persistent uniform buffer, and the bind group
//!   (single-allocation pattern).
//! * [`OpaquePipelines::draw_scene`] / [`OpaquePipelines::draw_depth_only`]
//!   — record draw calls against an already-bound render pass. The
//!   caller handles uniform uploads via
//!   [`OpaquePipelines::write_scene_uniforms`] before opening the
//!   pass (uniform writes happen on the queue, not inside the pass).

use bytemuck::{Pod, Zeroable};

use super::opaque_shaders::SCENE_SHADER;
use super::util::SCENE_DEPTH_COLOR_FORMAT;

/// Shading uniforms shared by the opaque scene shader and the
/// dual-peel colour shader. Layout matches the WGSL `U` struct in
/// `SCENE_SHADER` and `DUAL_PEEL_COLOR_SHADER` field-for-field.
///
/// Port of NodeDesigner's `createDepthPeelMaterial` uniform set:
/// projection + view as separate matrices (so the shader can pass
/// view-space position through and shade in view space), two
/// camera-fixed directional lights with independent diffuse +
/// specular + per-light ambient, global ambient, configurable
/// shininess, sRGB-encoded base colour. The `resolution` slot is
/// only meaningful for the peel pipeline (which samples the
/// opaque-depth mirror at integer pixel coords) but kept in the
/// shared layout so both pipelines bind an identical struct.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Uniforms {
    pub proj: [f32; 16],
    pub view: [f32; 16],
    pub light_dir0: [f32; 4],
    pub light_dir1: [f32; 4],
    pub light_diffuse0: [f32; 4],
    pub light_specular0: [f32; 4],
    pub light_ambient0: [f32; 4],
    pub light_diffuse1: [f32; 4],
    pub light_specular1: [f32; 4],
    pub global_ambient: [f32; 4],
    pub material_specular: [f32; 4],
    pub base_color: [f32; 4],
    /// `x` = Blinn-Phong shininess exponent. `y..w` reserved.
    pub params: [f32; 4],
    /// `xy` = framebuffer pixel size. `zw` reserved.
    pub resolution: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

/// Both opaque-scene pipelines plus their persistent uniform storage.
/// Allocated once during `ensure_state` and reused frame after frame —
/// uniforms are rewritten via [`wgpu::Queue::write_buffer`] rather than
/// reallocated, matching the cost-saving pattern documented on the
/// original `GpuState::scene_ub` field.
pub struct OpaquePipelines {
    pub scene_pipeline: wgpu::RenderPipeline,
    pub scene_ub: wgpu::Buffer,
    pub scene_bg: wgpu::BindGroup,

    /// Depth-only twin of `scene_pipeline`: runs the same vertex
    /// shader, writes depth + `scene_depth_color` (the R32Float mirror
    /// the dual-peel chain samples), but the colour attachment's
    /// write mask is empty so the surface colour stays untouched —
    /// the mesh's colour is produced by the dual-peel chain rather
    /// than the opaque pass. Mirrors MatterCAD's `RenderSceneDepth`
    /// use of `sceneEffectDepthPS` with `colorWritesEnabled: false`.
    pub depth_only_pipeline: wgpu::RenderPipeline,
}

impl OpaquePipelines {
    /// Build both pipelines for the given offscreen target format and
    /// sample count. Depth attachment format is always
    /// `Depth32Float` — depth peeling samples the per-pixel depth value
    /// from this texture via shader, which would be incoherent against
    /// an MSAA per-sample depth, so the caller is expected to keep
    /// `sample_count == 1`.
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let (scene_pipeline, scene_bgl) =
            build_scene_pipeline(device, surface_format, sample_count);
        let (scene_ub, scene_bg) = build_scene_uniforms(device, &scene_bgl);

        let depth_only_pipeline =
            build_depth_only_pipeline(device, &scene_bgl, surface_format, sample_count);

        Self {
            scene_pipeline,
            scene_ub,
            scene_bg,
            depth_only_pipeline,
        }
    }

    pub fn write_scene_uniforms(&self, queue: &wgpu::Queue, u: &Uniforms) {
        queue.write_buffer(&self.scene_ub, 0, bytemuck::bytes_of(u));
    }

    /// Draw the indexed mesh as a shaded surface. Caller must have already
    /// uploaded scene uniforms via [`Self::write_scene_uniforms`] and have
    /// bound viewport / scissor on `pass`.
    pub fn draw_scene<'rpass>(
        &'rpass self,
        pass: &mut wgpu::RenderPass<'rpass>,
        vbuf: &'rpass wgpu::Buffer,
        ibuf: &'rpass wgpu::Buffer,
        index_count: u32,
    ) {
        pass.set_pipeline(&self.scene_pipeline);
        pass.set_bind_group(0, &self.scene_bg, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..index_count, 0, 0..1);
    }

    /// Draw the indexed mesh depth-only (no color, depth-write on). Used
    /// to seed the opaque depth attachment + R32Float mirror so the
    /// dual-peel chain can discard fragments behind the mesh; the
    /// mesh's *color* is produced by the dual-peel chain.
    pub fn draw_depth_only<'rpass>(
        &'rpass self,
        pass: &mut wgpu::RenderPass<'rpass>,
        vbuf: &'rpass wgpu::Buffer,
        ibuf: &'rpass wgpu::Buffer,
        index_count: u32,
    ) {
        pass.set_pipeline(&self.depth_only_pipeline);
        pass.set_bind_group(0, &self.scene_bg, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..index_count, 0, 0..1);
    }
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    // Defined as a function (not a const) because `VertexBufferLayout`
    // borrows its attribute slice — the slice lives in static storage
    // through this function's literal so the borrow is `'static`.
    static ATTRS: [wgpu::VertexAttribute; 2] = [
        wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x3,
        },
        wgpu::VertexAttribute {
            offset: 12,
            shader_location: 1,
            format: wgpu::VertexFormat::Float32x3,
        },
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

fn shared_bgl(device: &wgpu::Device, label: &'static str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn build_scene_pipeline(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    sample_count: u32,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist scene shader"),
        source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
    });
    let bgl = shared_bgl(device, "atomartist scene bgl");
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("atomartist scene pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist scene pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[vertex_layout()],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[
                Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                }),
                Some(scene_depth_color_target()),
            ],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
}

/// Colour target for the auxiliary `scene_depth_color` attachment
/// every opaque-pass pipeline writes alongside the regular shaded
/// colour. R32Float so we can store raw `clip.z` without precision
/// loss. No blending and write-mask `RED` only — the green / blue /
/// alpha channels of the attachment are unused.
fn scene_depth_color_target() -> wgpu::ColorTargetState {
    wgpu::ColorTargetState {
        format: SCENE_DEPTH_COLOR_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::RED,
    }
}

fn build_scene_uniforms(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let ub = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("atomartist scene ub"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("atomartist scene bg"),
        layout: bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: ub.as_entire_binding(),
        }],
    });
    (ub, bg)
}

fn build_depth_only_pipeline(
    device: &wgpu::Device,
    scene_bgl: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    sample_count: u32,
) -> wgpu::RenderPipeline {
    // Reuse the scene shader's vertex + fragment stages; the fragment
    // shader still runs (cheap, no varying / texture work) but the
    // colour target's write_mask is `empty`, so no colour writes
    // reach the attachment. We *could* drop the fragment stage
    // entirely (`fragment: None`), but combining a no-fragment
    // pipeline with a colour-attached render pass is a wgpu
    // validation grey area on some backends; a real fragment with an
    // empty write mask is the well-established depth-only idiom.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist scene shader (depth-only reuse)"),
        source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("atomartist depth-only pl"),
        bind_group_layouts: &[Some(scene_bgl)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist depth-only pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[vertex_layout()],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[
                Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    // Suppresses the mesh's *colour* (the dual-peel
                    // chain writes that), while we still let the
                    // `depth_color` attachment receive the mesh's z.
                    write_mask: wgpu::ColorWrites::empty(),
                }),
                Some(scene_depth_color_target()),
            ],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}
