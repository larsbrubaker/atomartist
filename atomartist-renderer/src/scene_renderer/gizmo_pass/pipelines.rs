//! Pipelines for the gizmo line pass.
//!
//! Two variants share one shader: a solid depth-tested variant and an
//! overlay no-depth-test variant. NodeDesigner's gizmos
//! (`bounds-gizmo.js`, `z-control-gizmo.js`, etc.) all run both
//! variants and stack them — the solid variant draws the visible
//! parts at full opacity, the overlay variant fills in the occluded
//! parts with reduced alpha. The host picks which variants to draw
//! per gizmo via [`GizmoLineSet::draw_solid`] / `draw_overlay`.

use bytemuck::{Pod, Zeroable};

use super::shaders::GIZMO_LINE_SHADER;

/// Uniform layout — must match the WGSL `U` struct in
/// `GIZMO_LINE_SHADER` field-for-field.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct GizmoLineUniforms {
    pub mvp: [f32; 16],
    pub color: [f32; 4],
}

/// CPU-side vertex layout — a single `vec3<f32>` per vertex. No
/// normal / colour / barycentric — gizmo colour comes from the
/// uniform and the line topology supplies the visible shape.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GizmoLineVertex {
    pub pos: [f32; 3],
}

pub struct GizmoLinePipelines {
    pub solid_pipeline: wgpu::RenderPipeline,
    pub overlay_pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
}

impl GizmoLinePipelines {
    /// Build both pipelines for the given output target format and
    /// scene-depth format. The solid pipeline needs the depth-attachment
    /// format because it depth-tests against the opaque-pass depth;
    /// the overlay pipeline has no depth attachment.
    pub fn new(
        device: &wgpu::Device,
        output_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atomartist gizmo line bgl"),
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
            label: Some("atomartist gizmo line pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atomartist gizmo line shader"),
            source: wgpu::ShaderSource::Wgsl(GIZMO_LINE_SHADER.into()),
        });

        let solid_pipeline = build_variant(
            device,
            &pl,
            &shader,
            output_format,
            Some(depth_format),
            "atomartist gizmo line solid pipeline",
        );
        let overlay_pipeline = build_variant(
            device,
            &pl,
            &shader,
            output_format,
            None,
            "atomartist gizmo line overlay pipeline",
        );

        Self {
            solid_pipeline,
            overlay_pipeline,
            bgl,
        }
    }

    pub fn build_bind_group(&self, device: &wgpu::Device, ub: &wgpu::Buffer) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist gizmo line bg"),
            layout: &self.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ub.as_entire_binding(),
            }],
        })
    }
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    static ATTRS: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
        offset: 0,
        shader_location: 0,
        format: wgpu::VertexFormat::Float32x3,
    }];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GizmoLineVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

fn build_variant(
    device: &wgpu::Device,
    pl: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    output_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    label: &'static str,
) -> wgpu::RenderPipeline {
    // Both variants alpha-blend over their target — the solid variant
    // typically draws fully opaque (alpha = 1) but blending is cheap
    // and keeps the pipeline uniform so a gizmo can pass a per-line
    // alpha in colour.a if it wants to.
    let blend = Some(wgpu::BlendState {
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
    });

    let depth_stencil = depth_format.map(|format| wgpu::DepthStencilState {
        format,
        // Don't write depth — the gizmo doesn't claim depth, it only
        // tests against the existing scene depth so it can be
        // occluded by closer geometry.
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(pl),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            buffers: &[vertex_layout()],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: output_format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}
