//! wgpu scene renderer — implements `WgpuCustomRender` to draw the latest
//! mesh as a shaded 3D scene through agg-gui's custom-render hook.
//!
//! ## Offscreen-buffered viewport (Phase A0)
//!
//! Rather than injecting render commands into the same wgpu encoder + target
//! view that the 2-D UI pipeline uses (which couples 3-D anti-aliasing
//! settings to the 2-D pipeline and forces every viewport-overlay control
//! to live inside the 3-D pass), the renderer owns a dedicated
//! [`MsaaFramebuffer`] sized to the viewport widget's pixel rect:
//!
//! 1. Allocate an offscreen colour texture + matching depth at the widget's
//!    pixel size.
//! 2. Render the 3-D scene (floor grid + selected mesh + outline pass +
//!    future gizmos) into that color attachment with depth on.
//! 3. Composite the offscreen colour onto the active 2-D target through the
//!    shared `tex_pipeline` (alpha-blended) so 2-D content beneath the
//!    widget rect shows through transparent pixels and 2-D content drawn
//!    on top of the widget composites cleanly.
//!
//! This keeps the 3-D content isolated from the 2-D UI layer while avoiding
//! full-screen MSAA; the depth-peeled path depends on stencil and should run
//! single-sample.
//!
//! The shader stack is single Blinn-Phong-ish: vertex carries position +
//! normal; fragment shades against a fixed key + fill light plus ambient.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use demo_wgpu::{MsaaFramebuffer, WgpuCustomRender, WgpuCustomRenderCtx};
use manifold_rust::types::MeshGL;
use wgpu::util::DeviceExt;

use glam::Mat4;

use crate::bed::BedRenderer;
use crate::camera::OrbitCamera;
use crate::scene_shaders::{OUTLINE_SHADER, SHADER};

/// Render-style picker beneath the tumble cube.  Drives the surface
/// pipeline used by [`WgpuSceneRenderer`] so the user can compare a
/// shaded model with a wireframe-only or outline-only view, matching
/// MatterCAD's `ViewStyleButton` choices without the printer-specific
/// modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderStyle {
    /// Default Blinn-Phong shaded surface.
    Shaded,
    /// Hide the filled surface; only the inverted-hull silhouette draws.
    /// Useful for inspecting outline silhouettes / boundary fairing.
    OutlineOnly,
    /// Software wireframe — falls back to the existing CPU edge path.
    /// Disables the wgpu fill pass so the 2-D viewport draws the
    /// per-triangle edges from `Viewport3dWidget::draw_mesh`.
    Wireframe,
}

impl Default for RenderStyle {
    fn default() -> Self {
        Self::Shaded
    }
}

/// Sample count for the offscreen 3-D framebuffer.
///
/// Keep this single-sample: the depth-peeled renderer uses stencil, so
/// full-screen MSAA adds attachment complexity without helping that path.
const SAMPLE_COUNT: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    mvp: [f32; 16],
    /// Inverse-transpose of upper-3x3 of the model matrix, padded to mat4.
    /// AtomArtist applies the model transform on the CPU before submission,
    /// so this is identity for the time being — kept in the layout for
    /// when per-instance MVPs land.
    normal_mat: [f32; 16],
    light_dir: [f32; 4],
    base_color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
}


struct GpuState {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    /// Cached vertex/index buffers and the source mesh pointer they were built from.
    mesh_ptr: usize,
    vbuf: Option<wgpu::Buffer>,
    ibuf: Option<wgpu::Buffer>,
    index_count: u32,
    /// Bed renderer — owns the baked grid texture and the contact-shadow
    /// chain. Replaces the old procedural floor-grid pipeline. See
    /// [`crate::bed`] for the off-screen silhouette → blur → composite
    /// pipeline that runs each frame before the main pass.
    bed: BedRenderer,
    /// Inverted-hull outline pipeline — inflates each vertex along its
    /// normal in the vertex shader, draws *only* the back-faces (so the
    /// inflated rim peeks out from behind the regular front-face render).
    /// Pairs with the same vbuf/ibuf as the main mesh.
    outline_pipeline: wgpu::RenderPipeline,
    outline_bind_group_layout: wgpu::BindGroupLayout,
    /// Offscreen framebuffer + matching depth attachment, sized to the
    /// viewport widget's pixel rect. The 3-D pass renders here first, then
    /// composites onto the surface via the shared 2-D `tex_pipeline`.
    framebuffer: Option<MsaaFramebuffer>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OutlineUniforms {
    mvp: [f32; 16],
    color: [f32; 4],
    /// World-space outline thickness, applied along each vertex's normal.
    /// `[0]` is the actual width; `[1..3]` are pad bytes for std140
    /// alignment.
    width: [f32; 4],
}


pub struct WgpuSceneRenderer {
    state: Option<GpuState>,
    pub camera: OrbitCamera,
    pub mesh: Option<Arc<MeshGL>>,
    pub viewport_size: (u32, u32),
    pub base_color: [f32; 4],
    pub light_dir: [f32; 3],
    /// Floor-grid line color — caller adapts to the active theme.
    /// Forwarded to [`crate::bed::BedRenderer::set_line_color`] each
    /// frame; cheap when unchanged.
    pub grid_line_color: [f32; 4],
    /// True when the bed should render dark-mode contact shadows
    /// (bright instead of black). Mirrored from the viewport theme by
    /// [`crate::viewport_widget::Viewport3dWidget::paint`].
    pub grid_dark_mode: bool,
    /// True to draw the bed before the mesh.
    pub draw_grid: bool,
    /// World Z (height) where the bed sits — `Viewport3dWidget`
    /// updates this to the model's bounds-min Z so the bed always
    /// feels like a floor in the Z-up world.
    pub grid_z: f32,
    /// Render the inverted-hull outline pass. The host sets this when a
    /// node is selected — the outline is drawn around `mesh` (the
    /// currently-displayed mesh; per-node mesh tracking lands later).
    pub outline_enabled: bool,
    /// RGBA colour of the outline silhouette. Theme-driven — viewport sets
    /// it to a high-contrast colour against the current bg.
    pub outline_color: [f32; 4],
    /// World-space outline thickness — set by the host based on the mesh's
    /// bounding-box extent so it scales sensibly across model sizes.
    pub outline_width: f32,
    /// Surface render style — picked by the render-style picker beneath
    /// the tumble cube.  Drives the shaded vs outline-only vs wireframe
    /// branch in the main pass.
    pub render_style: RenderStyle,
}

impl WgpuSceneRenderer {
    pub fn new() -> Self {
        Self {
            state: None,
            camera: OrbitCamera::default(),
            mesh: None,
            viewport_size: (0, 0),
            base_color: [0.62, 0.66, 0.78, 1.0],
            light_dir: [0.4, 0.7, 0.6],
            grid_line_color: [0.55, 0.58, 0.66, 0.7],
            grid_dark_mode: false,
            draw_grid: true,
            grid_z: 0.0,
            outline_enabled: false,
            outline_color: [1.0, 0.55, 0.10, 1.0],
            outline_width: 0.05,
            render_style: RenderStyle::Shaded,
        }
    }

    fn ensure_state(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) {
        if let Some(s) = &self.state {
            if s.surface_format == surface_format {
                return;
            }
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atomartist scene shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atomartist scene bgl"),
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
            label: Some("atomartist scene pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let vert_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { offset: 0,  shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
                wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Float32x3 },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("atomartist scene pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[vert_layout],
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
                count: SAMPLE_COUNT,
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

        // ── Bed renderer (replaces old grid pipeline) ──────────────────────
        let mut bed = BedRenderer::new(
            device,
            queue,
            surface_format,
            SAMPLE_COUNT,
            self.grid_line_color,
        );
        bed.set_dark_mode(self.grid_dark_mode);

        // ── Outline (inverted-hull) pipeline ───────────────────────────────
        let outline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atomartist outline shader"),
            source: wgpu::ShaderSource::Wgsl(OUTLINE_SHADER.into()),
        });
        let outline_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atomartist outline bgl"),
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
        let outline_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("atomartist outline pl"),
            bind_group_layouts: &[Some(&outline_bgl)],
            immediate_size: 0,
        });
        let outline_vert_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { offset: 0,  shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
                wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Float32x3 },
            ],
        };
        let outline_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("atomartist outline pipeline"),
            layout: Some(&outline_pl),
            vertex: wgpu::VertexState {
                module: &outline_shader,
                entry_point: Some("vs"),
                buffers: &[outline_vert_layout],
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
                count: SAMPLE_COUNT,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: &outline_shader,
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

        self.state = Some(GpuState {
            pipeline,
            bind_group_layout: bgl,
            surface_format,
            mesh_ptr: 0,
            vbuf: None,
            ibuf: None,
            index_count: 0,
            bed,
            outline_pipeline,
            outline_bind_group_layout: outline_bgl,
            framebuffer: None,
        });
    }

    /// Lazily allocate (or resize) the offscreen framebuffer to match
    /// the widget's pixel rect.  Cheap when the size is stable.
    fn ensure_framebuffer(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let s = match &mut self.state { Some(s) => s, None => return };
        let format = s.surface_format;
        let w = w.max(1);
        let h = h.max(1);
        match &mut s.framebuffer {
            Some(fb) => fb.ensure_size(device, w, h),
            None => {
                s.framebuffer = Some(MsaaFramebuffer::new(
                    device, w, h, SAMPLE_COUNT, format, /* with_depth */ true,
                ));
            }
        }
    }

    /// Compute the bed-quad's render-time Z, slightly nudged away
    /// from the camera so the bed never Z-fights with model geometry
    /// that rests at `grid_z`. Port of NodeDesigner's `three-viewer`
    /// camera-distance offset: at typical zoom the offset is a few
    /// thousandths of a world unit — invisible to the eye but well
    /// above depth-buffer precision noise.
    fn bed_render_z(&self) -> f32 {
        let eye_z = self.camera.eye()[2];
        let dist = (eye_z - self.grid_z).abs();
        let sign = if eye_z >= self.grid_z { -1.0 } else { 1.0 };
        // Nudge toward the camera so the bed sits *in front of*
        // geometry resting at grid_z; the bed's depth-write still
        // lets the model occlude grid lines behind it.
        self.grid_z + sign * dist * 0.004
    }

    /// Re-upload mesh buffers if the mesh changed since the last frame.
    fn ensure_mesh_buffers(&mut self, device: &wgpu::Device) {
        let mesh = match &self.mesh { Some(m) => m.clone(), None => return };
        let s = match &mut self.state { Some(s) => s, None => return };
        let ptr = mesh.vert_properties.as_ptr() as usize;
        if s.mesh_ptr == ptr && s.vbuf.is_some() {
            return;
        }
        if mesh.num_prop < 6 || mesh.vert_properties.is_empty() {
            return;
        }
        let stride = mesh.num_prop as usize;
        let n_verts = mesh.vert_properties.len() / stride;
        let mut verts: Vec<Vertex> = Vec::with_capacity(n_verts);
        for i in 0..n_verts {
            verts.push(Vertex {
                pos:    [mesh.vert_properties[i * stride],
                         mesh.vert_properties[i * stride + 1],
                         mesh.vert_properties[i * stride + 2]],
                normal: [mesh.vert_properties[i * stride + 3],
                         mesh.vert_properties[i * stride + 4],
                         mesh.vert_properties[i * stride + 5]],
            });
        }
        s.vbuf = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist scene vb"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        }));
        s.ibuf = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist scene ib"),
            contents: bytemuck::cast_slice(&mesh.tri_verts),
            usage: wgpu::BufferUsages::INDEX,
        }));
        s.index_count = mesh.tri_verts.len() as u32;
        s.mesh_ptr = ptr;
    }
}

impl Default for WgpuSceneRenderer {
    fn default() -> Self { Self::new() }
}

impl WgpuCustomRender for WgpuSceneRenderer {
    fn render(&mut self, ctx: WgpuCustomRenderCtx<'_>) {
        // Lazy GPU init — runs once.
        self.ensure_state(ctx.device, ctx.queue, ctx.surface_format);

        // Pixel size of the viewport widget rect.  The framebuffer matches
        // this exactly (1:1 mapping), so blit_to runs an effectively no-op
        // bilinear sampler.
        let fb_w = ctx.screen_rect.width.max(1.0) as u32;
        let fb_h = ctx.screen_rect.height.max(1.0) as u32;
        if fb_w == 0 || fb_h == 0 {
            return;
        }

        self.ensure_framebuffer(ctx.device, fb_w, fb_h);
        self.ensure_mesh_buffers(ctx.device);

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

        let s = match &self.state { Some(s) => s, None => return };
        let fb = match &s.framebuffer { Some(fb) => fb, None => return };
        let depth = match fb.depth_view() { Some(d) => d, None => return };

        // Build uniforms — projection uses the widget's aspect ratio (the
        // framebuffer matches that aspect 1:1).
        let aspect = fb_w as f32 / fb_h.max(1) as f32;
        let view = Mat4::from_cols_array(&self.camera.view_matrix());
        let proj = Mat4::from_cols_array(&self.camera.projection_matrix(aspect));
        let mvp = (proj * view).to_cols_array();
        let mut normal_mat = [0.0f32; 16];
        normal_mat[0] = 1.0; normal_mat[5] = 1.0; normal_mat[10] = 1.0; normal_mat[15] = 1.0;
        let l = normalize3(self.light_dir);
        let uniforms = Uniforms {
            mvp,
            normal_mat,
            light_dir: [l[0], l[1], l[2], 0.0],
            base_color: self.base_color,
        };

        let ub = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist scene ub"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist scene bg"),
            layout: &s.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ub.as_entire_binding(),
            }],
        });

        // ── Pass 0: refresh the bed composite (grid + contact shadow) ──────
        // Runs in its own set of off-screen passes against `ctx.encoder`
        // BEFORE we open the main framebuffer pass, so the bed quad in
        // the main pass can sample the freshly-blitted composite
        // texture. Skipped when the bed is hidden — no shadow update
        // needed if the bed isn't being drawn.
        if self.draw_grid {
            let mesh_ref = match (&s.vbuf, &s.ibuf) {
                (Some(vbuf), Some(ibuf)) if s.index_count > 0 => Some(crate::bed::MeshRef {
                    vbuf,
                    ibuf,
                    index_count: s.index_count,
                }),
                _ => None,
            };
            s.bed.render_to_composite(
                ctx.device,
                ctx.encoder,
                mesh_ref,
                self.grid_z,
                [self.camera.center[0], self.camera.center[1]],
            );
        }

        // ── Pass 1: render 3-D into the offscreen framebuffer ──────────────
        // With SAMPLE_COUNT = 1 the framebuffer writes directly into the
        // single-sample texture that the composite pass samples from.
        // No 2-D content beneath the widget bleeds through because we
        // clear to fully transparent and the composite pass below
        // alpha-blends only where the 3-D content covered pixels.
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist scene offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: fb.render_view(),
                    resolve_target: fb.resolve_target(),
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_viewport(0.0, 0.0, fb_w as f32, fb_h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, fb_w, fb_h);

            // Bed first — depth-write on so the mesh occludes the lines
            // hidden behind it. The bed pipeline samples the composite
            // texture we refreshed in Pass 0. A small camera-distance Z
            // nudge keeps the plane from Z-fighting with model geometry
            // resting on Z = grid_z (port of NodeDesigner three-viewer's
            // distance-scaled offset).
            if self.draw_grid {
                let bed_z = self.bed_render_z();
                s.bed.draw_bed(ctx.device, &mut pass, mvp, bed_z);
            }

            // Mesh — only when both a vertex buffer and index buffer are
            // present.  Skipping leaves the grid + transparent pixels
            // composited as the empty viewport hint.
            //
            // `RenderStyle::Shaded` draws the surface; `OutlineOnly` and
            // `Wireframe` skip the surface fill so only the outline /
            // 2-D wireframe path contributes.  Outlines are forced on
            // for `OutlineOnly` regardless of selection so the user
            // always has something to look at when they pick that mode.
            let draw_surface = self.render_style == RenderStyle::Shaded;
            let outline_force_on = self.render_style == RenderStyle::OutlineOnly;
            if let (Some(vbuf), Some(ibuf)) = (&s.vbuf, &s.ibuf) {
                if s.index_count > 0 {
                    if draw_surface {
                        pass.set_pipeline(&s.pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.set_vertex_buffer(0, vbuf.slice(..));
                        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..s.index_count, 0, 0..1);
                    }

                    // Outline silhouette — runs *after* the mesh so the
                    // inflated back-faces can be depth-tested against the
                    // already-written front-face depth and only show
                    // beyond the original silhouette. Skipped when no
                    // selection is active.
                    if (self.outline_enabled || outline_force_on) && self.outline_width > 0.0 {
                        let outline_uniforms = OutlineUniforms {
                            mvp,
                            color: self.outline_color,
                            width: [self.outline_width, 0.0, 0.0, 0.0],
                        };
                        let outline_ub = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("atomartist outline ub"),
                            contents: bytemuck::bytes_of(&outline_uniforms),
                            usage: wgpu::BufferUsages::UNIFORM,
                        });
                        let outline_bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("atomartist outline bg"),
                            layout: &s.outline_bind_group_layout,
                            entries: &[wgpu::BindGroupEntry {
                                binding: 0,
                                resource: outline_ub.as_entire_binding(),
                            }],
                        });
                        pass.set_pipeline(&s.outline_pipeline);
                        pass.set_bind_group(0, &outline_bg, &[]);
                        pass.set_vertex_buffer(0, vbuf.slice(..));
                        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..s.index_count, 0, 0..1);
                        drop(outline_bg);
                        drop(outline_ub);
                    }
                }
            }
        } // pass dropped — encoder freed for the composite pass.

        // ── Pass 2: composite offscreen colour onto the active 2-D target ──
        // 1:1 size mapping (framebuffer matches widget pixel rect), so
        // the bilinear sampler in the shared `tex_pipeline` is identity-
        // equivalent.  Alpha-blends through `BLEND_STANDARD` so transparent
        // pixels (where the 3-D scene didn't draw) preserve the 2-D
        // backdrop underneath.
        fb.blit_to(
            ctx.device,
            ctx.encoder,
            ctx.target_view,
            ctx.target_size,
            ctx.screen_rect,
            ctx.parent_clip,
            ctx.pipelines,
        );
    }
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    [v[0] / l, v[1] / l, v[2] / l]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_is_constructible() {
        let r = WgpuSceneRenderer::new();
        assert!(r.mesh.is_none());
    }

    /// Bed Z-fight offset nudges the plane toward the camera — when
    /// the camera is above `grid_z` the result is below `grid_z`, and
    /// vice versa. Magnitude scales with camera distance so the
    /// adjustment is invisible at typical zooms but always exceeds
    /// depth-buffer precision.
    #[test]
    fn bed_render_z_nudges_toward_camera() {
        let mut r = WgpuSceneRenderer::new();
        r.grid_z = 0.0;
        // Default camera looks at the origin from radius=300-ish on +Z;
        // exact value isn't important — only the sign of the nudge.
        let eye_z = r.camera.eye()[2];
        let bed_z = r.bed_render_z();
        assert!(bed_z != r.grid_z);
        if eye_z > 0.0 {
            assert!(bed_z < 0.0, "camera above bed -> bed nudged below");
        } else {
            assert!(bed_z > 0.0, "camera below bed -> bed nudged above");
        }
        // 0.004 × distance scaling — the offset for a 100-unit eye
        // should be at least 0.3 (much larger than f32 depth noise).
        assert!(bed_z.abs() > 0.001 * eye_z.abs().max(1.0));
    }

    #[test]
    fn bed_toggle_default_is_on() {
        let r = WgpuSceneRenderer::new();
        assert!(r.draw_grid);
    }
}
