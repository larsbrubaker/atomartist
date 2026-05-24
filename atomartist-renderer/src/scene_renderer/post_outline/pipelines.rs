//! Pipelines for the Blender-style post-process selection outline.
//!
//! Two pipelines:
//!
//! * **ID prepass** — rasterizes the currently-selected mesh into an
//!   `R8Unorm` mask + an `R32Float` depth mirror. Uses the same vertex
//!   layout as the opaque pass so the renderer can hand the existing
//!   mesh vbuf / ibuf straight through.
//! * **Edge-detect quad** — full-screen triangle that samples the ID
//!   mask + the selected-mesh depth mirror + the full-scene depth
//!   mirror to draw an alpha-blended outline over the composited
//!   scene.
//!
//! See [`super::shaders`] for the shader text and the rationale behind
//! collapsing NodeDesigner's two edge-detect passes into one.

use bytemuck::{Pod, Zeroable};

use super::shaders::{EDGE_DETECT_SHADER, ID_PREPASS_SHADER};
use crate::scene_renderer::opaque_pass::Vertex;
use crate::scene_renderer::util::SCENE_DEPTH_COLOR_FORMAT;

/// Texture format for the ID mask. `R8Unorm` is the smallest format
/// every wgpu backend (including WebGL2) accepts as both a render
/// attachment AND a `Float { filterable: false }` sampled texture.
/// `1.0` means selected, `0.0` means not selected.
pub const ID_MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// `R32Float` mirror of the selected-mesh clip-space depth, sampled
/// by the edge-detect shader via `textureLoad`. Same format used by
/// the opaque pass's `scene_depth_color` so we can reuse the
/// existing sample-type wiring.
pub const SELECTED_DEPTH_FORMAT: wgpu::TextureFormat = SCENE_DEPTH_COLOR_FORMAT;

/// Hardware depth attachment for the ID prepass. Not sampled —
/// drives the `LessEqual` depth test so the prepass produces the
/// nearest selected-mesh fragment at every pixel.
pub const ID_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Shared uniform layout for both pipelines. The ID prepass only
/// touches `mvp`; the edge-detect quad only touches
/// `outline_color` / `resolution` / `params`. Keeping a single
/// struct keeps the per-frame upload cheap.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct OutlineUniforms {
    pub mvp: [f32; 16],
    pub outline_color: [f32; 4],
    /// `xy` = framebuffer pixel size; `zw` reserved.
    pub resolution: [f32; 4],
    /// `x` = outline width in texels (NodeDesigner default `2.0`).
    /// `y` = occluded-alpha multiplier (NodeDesigner default `0.35`).
    pub params: [f32; 4],
}

pub struct OutlinePipelines {
    pub id_pipeline: wgpu::RenderPipeline,
    pub id_bgl: wgpu::BindGroupLayout,
    pub id_ub: wgpu::Buffer,
    pub id_bg: wgpu::BindGroup,

    pub edge_pipeline: wgpu::RenderPipeline,
    pub edge_bgl: wgpu::BindGroupLayout,
    pub edge_ub: wgpu::Buffer,
}

impl OutlinePipelines {
    /// `output_format` is the format of the texture the edge-detect
    /// quad writes into — must match the texture passed as the colour
    /// attachment in `execute_edge_pass`. For the main viewport this
    /// is the renderer's surface format (the same `output_fb` the
    /// accumulation chain writes to).
    pub fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> Self {
        let (id_pipeline, id_bgl) = build_id_pipeline(device);
        let id_ub = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atomartist outline id ub"),
            size: std::mem::size_of::<OutlineUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let id_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist outline id bg"),
            layout: &id_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: id_ub.as_entire_binding(),
            }],
        });

        let (edge_pipeline, edge_bgl) = build_edge_pipeline(device, output_format);
        let edge_ub = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atomartist outline edge ub"),
            size: std::mem::size_of::<OutlineUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            id_pipeline,
            id_bgl,
            id_ub,
            id_bg,
            edge_pipeline,
            edge_bgl,
            edge_ub,
        }
    }

    pub fn write_uniforms(&self, queue: &wgpu::Queue, u: &OutlineUniforms) {
        queue.write_buffer(&self.id_ub, 0, bytemuck::bytes_of(u));
        queue.write_buffer(&self.edge_ub, 0, bytemuck::bytes_of(u));
    }

    pub fn build_edge_bind_group(
        &self,
        device: &wgpu::Device,
        id_mask: &wgpu::TextureView,
        selected_depth: &wgpu::TextureView,
        scene_depth: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist outline edge bg"),
            layout: &self.edge_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.edge_ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(id_mask),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(selected_depth),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(scene_depth),
                },
            ],
        })
    }
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
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

fn build_id_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("atomartist outline id bgl"),
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
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("atomartist outline id pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist outline id shader"),
        source: wgpu::ShaderSource::Wgsl(ID_PREPASS_SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist outline id pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[vertex_layout()],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // No culling — selection outline should include back-faces
            // visible through transparent geometry. NodeDesigner uses
            // `THREE.DoubleSide` on the ID prepass material for the
            // same reason.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: ID_DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[
                Some(wgpu::ColorTargetState {
                    format: ID_MASK_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::RED,
                }),
                Some(wgpu::ColorTargetState {
                    format: SELECTED_DEPTH_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::RED,
                }),
            ],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
}

fn build_edge_pipeline(
    device: &wgpu::Device,
    output_format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("atomartist outline edge bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // id_mask, selected_depth, scene_depth — all sampled via
            // `textureLoad`, so `Float { filterable: false }` and no
            // separate sampler binding. `R8Unorm` survives this with
            // the same wiring as the R32Float depth mirror.
            float_texture_entry(1),
            float_texture_entry(2),
            float_texture_entry(3),
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("atomartist outline edge pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist outline edge shader"),
        source: wgpu::ShaderSource::Wgsl(EDGE_DETECT_SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist outline edge pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
                // Standard OVER alpha blend so the outline composites
                // on top of the accumulation chain's output without
                // touching pixels the shader discards.
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::SrcAlpha,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
}

fn float_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
