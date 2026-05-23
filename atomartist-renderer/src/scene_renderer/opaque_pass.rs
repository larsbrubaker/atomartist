//! Opaque scene + inverted-hull outline pipelines for [`super::WgpuSceneRenderer`].
//!
//! Owns the wgpu resources that draw the solid mesh surface (Blinn-Phong
//! shaded) and the inverted-hull silhouette pass. Knows nothing about
//! depth peeling or accumulation — those run after the opaque pass
//! against the populated scene-depth attachment.
//!
//! Public surface:
//!
//! * [`Uniforms`] / [`OutlineUniforms`] / [`Vertex`] — packed layouts the
//!   orchestrator writes per frame and the shaders consume.
//! * [`OpaquePipelines`] — built once during `WgpuSceneRenderer::ensure_state`,
//!   holds both pipelines, both persistent uniform buffers, and both bind
//!   groups (matches the prior single-allocation pattern in
//!   `scene_renderer.rs`).
//! * [`OpaquePipelines::draw_scene`] / [`OpaquePipelines::draw_outline`] —
//!   record draw calls against an already-bound render pass. The caller
//!   handles uniform uploads via [`OpaquePipelines::write_scene_uniforms`]
//!   and [`OpaquePipelines::write_outline_uniforms`] before opening the
//!   pass (uniform writes happen on the queue, not inside the pass).

use bytemuck::{Pod, Zeroable};

use super::opaque_shaders::{OUTLINE_SHADER, SCENE_SHADER};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Uniforms {
    pub mvp: [f32; 16],
    /// Inverse-transpose of upper-3x3 of the model matrix, padded to mat4.
    /// AtomArtist applies the model transform on the CPU before submission,
    /// so this is identity for the time being — kept in the layout for
    /// when per-instance MVPs land.
    pub normal_mat: [f32; 16],
    pub light_dir: [f32; 4],
    pub base_color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct OutlineUniforms {
    pub mvp: [f32; 16],
    pub color: [f32; 4],
    /// World-space outline thickness, applied along each vertex's normal.
    /// `[0]` is the actual width; `[1..3]` are pad bytes for std140
    /// alignment.
    pub width: [f32; 4],
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

    /// Depth-only twin of `scene_pipeline`: runs the same vertex shader,
    /// no fragment stage, depth-write on. Used to populate the opaque
    /// depth attachment with the user mesh's depth so the inverted-hull
    /// outline can depth-test against it — the mesh's *color* is
    /// rendered through the dual-peel chain rather than the opaque
    /// pass, so without this depth-only path the outline would draw
    /// as a solid silhouette instead of a rim around the mesh.
    ///
    /// Mirrors MatterCAD's `RenderSceneDepth` use of `sceneEffectDepthPS`
    /// with `colorWritesEnabled: false`.
    pub depth_only_pipeline: wgpu::RenderPipeline,

    /// Inverted-hull outline pipeline — inflates each vertex along its
    /// normal in the vertex shader, draws *only* the back-faces (so the
    /// inflated rim peeks out from behind the regular front-face render).
    /// Pairs with the same vbuf/ibuf as the main mesh.
    pub outline_pipeline: wgpu::RenderPipeline,
    pub outline_ub: wgpu::Buffer,
    pub outline_bg: wgpu::BindGroup,
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

        let (outline_pipeline, outline_bgl) =
            build_outline_pipeline(device, surface_format, sample_count);
        let (outline_ub, outline_bg) = build_outline_uniforms(device, &outline_bgl);

        Self {
            scene_pipeline,
            scene_ub,
            scene_bg,
            depth_only_pipeline,
            outline_pipeline,
            outline_ub,
            outline_bg,
        }
    }

    pub fn write_scene_uniforms(&self, queue: &wgpu::Queue, u: &Uniforms) {
        queue.write_buffer(&self.scene_ub, 0, bytemuck::bytes_of(u));
    }

    pub fn write_outline_uniforms(&self, queue: &wgpu::Queue, u: &OutlineUniforms) {
        queue.write_buffer(&self.outline_ub, 0, bytemuck::bytes_of(u));
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
    /// to seed the opaque depth attachment with the user mesh's depth so
    /// the outline pass can rim-test against it; the mesh's *color* is
    /// produced by the dual-peel chain.
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

    /// Draw the inverted-hull outline. Same constraints as `draw_scene`
    /// but uses the outline uniform/pipeline pair.
    pub fn draw_outline<'rpass>(
        &'rpass self,
        pass: &mut wgpu::RenderPass<'rpass>,
        vbuf: &'rpass wgpu::Buffer,
        ibuf: &'rpass wgpu::Buffer,
        index_count: u32,
    ) {
        pass.set_pipeline(&self.outline_pipeline);
        pass.set_bind_group(0, &self.outline_bg, &[]);
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
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
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

fn build_outline_pipeline(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    sample_count: u32,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist outline shader"),
        source: wgpu::ShaderSource::Wgsl(OUTLINE_SHADER.into()),
    });
    let bgl = shared_bgl(device, "atomartist outline bgl");
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("atomartist outline pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist outline pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[vertex_layout()],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Cull *front*-faces so only the inflated *back*-faces draw —
            // they peek out from behind the regular front-face render
            // wherever they extend beyond its silhouette, producing a
            // constant-thickness rim.
            cull_mode: Some(wgpu::Face::Front),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            // Don't write depth — the main mesh has already populated
            // depth, and we want subsequent passes to compete against
            // the original geometry rather than the inflated rim.
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
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
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
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
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn build_outline_uniforms(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let ub = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("atomartist outline ub"),
        size: std::mem::size_of::<OutlineUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("atomartist outline bg"),
        layout: bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: ub.as_entire_binding(),
        }],
    });
    (ub, bg)
}
