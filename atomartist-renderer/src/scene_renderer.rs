//! wgpu scene renderer — implements `WgpuCustomRender` to draw the latest
//! mesh as a shaded 3D scene through agg-gui's custom-render hook.
//!
//! The renderer keeps its GPU state lazily: pipeline + depth buffer are
//! created on the first frame (when device + surface format are known),
//! and the vertex buffer is re-uploaded only when a new mesh arrives
//! (detected via `Arc::ptr_eq` on the cached mesh handle).
//!
//! The shader is a single Blinn-Phong-ish lighting model: vertex carries
//! position + normal; fragment shades against a fixed key + fill light
//! plus ambient. Output writes RGBA8 with `LoadOp::Load` against the
//! active 2-D surface — content beneath the viewport rect remains and 2-D
//! UI on top composites cleanly afterward.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use demo_wgpu::{WgpuCustomRender, WgpuCustomRenderCtx};
use manifold_rust::types::MeshGL;
use wgpu::util::DeviceExt;

use crate::camera::{mul4, OrbitCamera};

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

const SHADER: &str = r#"
struct Uniforms {
    mvp: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    light_dir: vec4<f32>,
    base_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>) -> VOut {
    var o: VOut;
    o.clip = u.mvp * vec4<f32>(pos, 1.0);
    o.world_normal = (u.normal_mat * vec4<f32>(normal, 0.0)).xyz;
    return o;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let l = normalize(u.light_dir.xyz);
    let diff = max(dot(n, l), 0.0);
    let fill = max(dot(n, vec3<f32>(-l.x, l.y * 0.4, -l.z)), 0.0) * 0.35;
    let ambient = 0.18;
    let lit = ambient + diff * 0.85 + fill;
    let rgb = u.base_color.rgb * lit;
    return vec4<f32>(rgb, u.base_color.a);
}
"#;

/// Procedural floor grid. Draws a single large quad at a configurable Y
/// and computes line coverage in the fragment shader so the grid stays
/// sharp at any zoom and fades to transparent at distance.
const GRID_SHADER: &str = r#"
struct GridU {
    mvp: mat4x4<f32>,
    cell: vec4<f32>,        // x = minor cell size, y = major cell stride, z = grid_y
    line_color: vec4<f32>,
    bg_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: GridU;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_xz: vec2<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>) -> VOut {
    var o: VOut;
    let p = vec3<f32>(pos.x, u.cell.z, pos.z);
    o.clip = u.mvp * vec4<f32>(p, 1.0);
    o.world_xz = p.xz;
    return o;
}

// Coverage of a 1-pixel-wide line at integer grid coordinates, derived
// from screen-space derivatives so it stays sharp at any zoom.
fn line_coverage(coord: vec2<f32>) -> f32 {
    let d = fwidth(coord);
    let g = abs(fract(coord - 0.5) - 0.5) / d;
    let line = min(g.x, g.y);
    return 1.0 - clamp(line, 0.0, 1.0);
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let cell  = u.cell.x;
    let major = u.cell.y;

    // Minor + major grid coverages (in world-space cell units).
    let minor_c = coord_to_cell(in.world_xz, cell);
    let major_c = coord_to_cell(in.world_xz, cell * major);
    let minor_a = line_coverage(minor_c) * 0.35;
    let major_a = line_coverage(major_c);

    let alpha = max(minor_a, major_a);
    if alpha < 0.01 {
        discard;
    }
    let col = mix(u.bg_color.rgb, u.line_color.rgb, alpha);
    return vec4<f32>(col, alpha * u.line_color.a);
}

fn coord_to_cell(p: vec2<f32>, cell: f32) -> vec2<f32> {
    return p / cell;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GridUniforms {
    mvp: [f32; 16],
    /// x = minor cell size, y = major cell stride (in cells), z/w = pad
    cell: [f32; 4],
    line_color: [f32; 4],
    bg_color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GridVertex {
    pos: [f32; 3],
}

struct GpuState {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    /// Lazy depth buffer; recreated on size change.
    depth_view: Option<wgpu::TextureView>,
    depth_size: (u32, u32),
    /// Cached vertex/index buffers and the source mesh pointer they were built from.
    mesh_ptr: usize,
    vbuf: Option<wgpu::Buffer>,
    ibuf: Option<wgpu::Buffer>,
    index_count: u32,
    /// Floor grid pipeline + buffers — built lazily alongside the mesh
    /// pipeline. `grid_vbuf` covers a large XZ plane at Y=0; the shader
    /// fades grid lines via screen-space derivatives.
    grid_pipeline: wgpu::RenderPipeline,
    grid_bind_group_layout: wgpu::BindGroupLayout,
    grid_vbuf: wgpu::Buffer,
}

pub struct WgpuSceneRenderer {
    state: Option<GpuState>,
    pub camera: OrbitCamera,
    pub mesh: Option<Arc<MeshGL>>,
    pub viewport_size: (u32, u32),
    pub base_color: [f32; 4],
    pub light_dir: [f32; 3],
    /// Floor-grid line color — caller adapts to the active theme.
    pub grid_line_color: [f32; 4],
    /// Floor-grid background — typically the viewport bg, used to blend
    /// grid lines against the existing 2-D backdrop already painted there.
    pub grid_bg_color: [f32; 4],
    /// True to draw the floor grid before the mesh.
    pub draw_grid: bool,
    /// World Y where the floor grid sits — `Viewport3dWidget` updates this
    /// to the model's bounds-min Y so the grid always feels like a floor.
    pub grid_y: f32,
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
            grid_bg_color: [1.0, 1.0, 1.0, 0.0],
            draw_grid: true,
            grid_y: 0.0,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, surface_format: wgpu::TextureFormat) {
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
            multisample: wgpu::MultisampleState::default(),
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

        // ── Grid pipeline + vbuf ────────────────────────────────────────────
        let grid_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atomartist grid shader"),
            source: wgpu::ShaderSource::Wgsl(GRID_SHADER.into()),
        });
        let grid_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atomartist grid bgl"),
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
        let grid_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("atomartist grid pl"),
            bind_group_layouts: &[Some(&grid_bgl)],
            immediate_size: 0,
        });
        let grid_vert_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GridVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            }],
        };
        let grid_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("atomartist grid pipeline"),
            layout: Some(&grid_pl),
            vertex: wgpu::VertexState {
                module: &grid_shader,
                entry_point: Some("vs"),
                buffers: &[grid_vert_layout],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                // Grid writes depth so the model occludes lines behind it.
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &grid_shader,
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
        // Two triangles forming a large XZ-plane quad at Y=0.
        let plane: [GridVertex; 6] = [
            GridVertex { pos: [-2000.0, 0.0, -2000.0] },
            GridVertex { pos: [ 2000.0, 0.0, -2000.0] },
            GridVertex { pos: [ 2000.0, 0.0,  2000.0] },
            GridVertex { pos: [-2000.0, 0.0, -2000.0] },
            GridVertex { pos: [ 2000.0, 0.0,  2000.0] },
            GridVertex { pos: [-2000.0, 0.0,  2000.0] },
        ];
        let grid_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist grid vb"),
            contents: bytemuck::cast_slice(&plane),
            usage: wgpu::BufferUsages::VERTEX,
        });

        self.state = Some(GpuState {
            pipeline,
            bind_group_layout: bgl,
            surface_format,
            depth_view: None,
            depth_size: (0, 0),
            mesh_ptr: 0,
            vbuf: None,
            ibuf: None,
            index_count: 0,
            grid_pipeline,
            grid_bind_group_layout: grid_bgl,
            grid_vbuf,
        });
    }

    fn ensure_depth(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        let s = match &mut self.state { Some(s) => s, None => return };
        if s.depth_view.is_some() && s.depth_size == size {
            return;
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atomartist scene depth"),
            size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        s.depth_view = Some(tex.create_view(&Default::default()));
        s.depth_size = size;
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
        self.ensure_state(ctx.device, ctx.surface_format);

        // Compute viewport rect in wgpu's top-down screen space.
        // ctx.screen_rect is Y-up (origin bottom-left); convert to top-down.
        let target_h = ctx.target_size.1 as f32;
        let vp_x = ctx.screen_rect.x.max(0.0) as f32;
        let vp_w = ctx.screen_rect.width.max(1.0) as f32;
        let vp_h = ctx.screen_rect.height.max(1.0) as f32;
        let vp_y_topdown = target_h - (ctx.screen_rect.y as f32) - vp_h;

        // Depth attachment MUST match the color attachment size (the full
        // target_view), not the viewport rect — wgpu validates this. We
        // limit drawing to the widget's rect via set_viewport / scissor.
        self.ensure_depth(ctx.device, ctx.target_size);
        self.ensure_mesh_buffers(ctx.device);

        let s = match &self.state { Some(s) => s, None => return };
        let depth = match &s.depth_view { Some(d) => d, None => return };
        let vbuf = match &s.vbuf { Some(v) => v, None => return };
        let ibuf = match &s.ibuf { Some(i) => i, None => return };
        if s.index_count == 0 {
            return;
        }

        // Build uniforms.
        let aspect = vp_w / vp_h.max(1.0);
        let view = self.camera.view_matrix();
        let proj = self.camera.projection_matrix(aspect);
        let mvp = mul4(&proj, &view);
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

        // Open a render pass against the active target (color = Load,
        // depth = Clear). Scissor + viewport limit drawing to our rect.
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("atomartist scene pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: ctx.target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
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

        pass.set_viewport(vp_x, vp_y_topdown.max(0.0), vp_w, vp_h, 0.0, 1.0);
        let scissor_x = vp_x as u32;
        let scissor_y = vp_y_topdown.max(0.0) as u32;
        pass.set_scissor_rect(scissor_x, scissor_y, vp_w as u32, vp_h as u32);

        // Draw the floor grid first (depth-write on so the mesh occludes
        // lines hidden behind it).
        if self.draw_grid {
            let grid_uniforms = GridUniforms {
                mvp,
                cell: [1.0, 10.0, self.grid_y, 0.0],
                line_color: self.grid_line_color,
                bg_color: self.grid_bg_color,
            };
            let grid_ub = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist grid ub"),
                contents: bytemuck::bytes_of(&grid_uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let grid_bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("atomartist grid bg"),
                layout: &s.grid_bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: grid_ub.as_entire_binding(),
                }],
            });
            pass.set_pipeline(&s.grid_pipeline);
            pass.set_bind_group(0, &grid_bg, &[]);
            pass.set_vertex_buffer(0, s.grid_vbuf.slice(..));
            pass.draw(0..6, 0..1);
        }

        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..s.index_count, 0, 0..1);

        // Pass drops here — encoder is freed for caller to continue.
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
}
