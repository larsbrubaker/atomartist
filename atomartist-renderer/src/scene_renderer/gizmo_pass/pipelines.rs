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
    /// Filled-triangle counterparts for gizmo handle meshes. Same
    /// shader + uniform layout as the line variants; only the
    /// primitive topology differs (TriangleList) and we cull
    /// back-faces so handles read as solid 3-D shapes.
    pub solid_tri_pipeline: wgpu::RenderPipeline,
    pub overlay_tri_pipeline: wgpu::RenderPipeline,
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
            wgpu::PrimitiveTopology::LineList,
            None,
            "atomartist gizmo line solid pipeline",
        );
        let overlay_pipeline = build_variant(
            device,
            &pl,
            &shader,
            output_format,
            None,
            wgpu::PrimitiveTopology::LineList,
            None,
            "atomartist gizmo line overlay pipeline",
        );
        // Filled-triangle variants: cull back-faces so the handle
        // meshes (small spheres + cubes) read as solid 3-D shapes
        // instead of showing through to the back wall.
        let solid_tri_pipeline = build_variant(
            device,
            &pl,
            &shader,
            output_format,
            Some(depth_format),
            wgpu::PrimitiveTopology::TriangleList,
            Some(wgpu::Face::Back),
            "atomartist gizmo tri solid pipeline",
        );
        let overlay_tri_pipeline = build_variant(
            device,
            &pl,
            &shader,
            output_format,
            None,
            wgpu::PrimitiveTopology::TriangleList,
            Some(wgpu::Face::Back),
            "atomartist gizmo tri overlay pipeline",
        );

        Self {
            solid_pipeline,
            overlay_pipeline,
            solid_tri_pipeline,
            overlay_tri_pipeline,
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

    /// Draw one [`super::GizmoLineSet`] into the HDR scene composite.
    /// Solid variant depth-tests against `scene_depth_view`; overlay
    /// variant runs without a depth attachment. Per-gizmo scratch
    /// buffers are created here and dropped at end-of-call — cheap
    /// because gizmo geometry is tiny (≤ a few hundred verts).
    ///
    /// `mvp` is the renderer's projection × view, already composed with
    /// the gizmo's optional model matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        gizmo: &super::GizmoLineSet,
        mvp: [f32; 16],
        sample_view: &wgpu::TextureView,
        scene_depth_view: &wgpu::TextureView,
        viewport: (u32, u32),
    ) {
        if gizmo.vertices.is_empty() || (!gizmo.draw_solid && !gizmo.draw_overlay) {
            return;
        }
        let (w, h) = viewport;
        use wgpu::util::DeviceExt;
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist gizmo line vb"),
            contents: bytemuck::cast_slice(&gizmo.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let vertex_count = gizmo.vertices.len() as u32;

        if gizmo.draw_solid {
            let u = GizmoLineUniforms { mvp, color: gizmo.color };
            let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist gizmo line solid ub"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = self.build_bind_group(device, &ub);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist gizmo solid"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: sample_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: scene_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, w, h);
            pass.set_pipeline(&self.solid_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..vertex_count, 0..1);
        }

        if gizmo.draw_overlay {
            let overlay_color = [
                gizmo.color[0],
                gizmo.color[1],
                gizmo.color[2],
                gizmo.color[3] * gizmo.occluded_alpha,
            ];
            let u = GizmoLineUniforms { mvp, color: overlay_color };
            let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist gizmo line overlay ub"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = self.build_bind_group(device, &ub);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist gizmo overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: sample_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, w, h);
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..vertex_count, 0..1);
        }
    }

    /// Filled-triangle counterpart of [`execute`]. Same uniform layout
    /// and per-set scratch buffers; routes the draw through the
    /// TriangleList pipelines with back-face culling.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_tri(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        gizmo: &super::GizmoTriangleSet,
        mvp: [f32; 16],
        sample_view: &wgpu::TextureView,
        scene_depth_view: &wgpu::TextureView,
        viewport: (u32, u32),
    ) {
        if gizmo.vertices.is_empty() || (!gizmo.draw_solid && !gizmo.draw_overlay) {
            return;
        }
        let (w, h) = viewport;
        use wgpu::util::DeviceExt;
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist gizmo tri vb"),
            contents: bytemuck::cast_slice(&gizmo.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let vertex_count = gizmo.vertices.len() as u32;

        if gizmo.draw_solid {
            let u = GizmoLineUniforms { mvp, color: gizmo.color };
            let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist gizmo tri solid ub"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = self.build_bind_group(device, &ub);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist gizmo tri solid"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: sample_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: scene_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, w, h);
            pass.set_pipeline(&self.solid_tri_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..vertex_count, 0..1);
        }

        if gizmo.draw_overlay {
            let overlay_color = [
                gizmo.color[0],
                gizmo.color[1],
                gizmo.color[2],
                gizmo.color[3] * gizmo.occluded_alpha,
            ];
            let u = GizmoLineUniforms { mvp, color: overlay_color };
            let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist gizmo tri overlay ub"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = self.build_bind_group(device, &ub);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist gizmo tri overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: sample_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, w, h);
            pass.set_pipeline(&self.overlay_tri_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..vertex_count, 0..1);
        }
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

#[allow(clippy::too_many_arguments)]
fn build_variant(
    device: &wgpu::Device,
    pl: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    output_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    topology: wgpu::PrimitiveTopology,
    cull_mode: Option<wgpu::Face>,
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
            topology,
            cull_mode,
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
