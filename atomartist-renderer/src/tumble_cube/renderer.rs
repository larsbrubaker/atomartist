//! wgpu custom-render impl for the tumble cube.
//!
//! Allocates and re-uses one render pipeline + six texture/sampler bind
//! groups for the cube faces, painting into a per-widget MSAA
//! framebuffer (matching [`super::super::scene_renderer::WgpuSceneRenderer`]'s
//! approach), then blitting the resolved buffer onto the active 2-D
//! target.
//!
//! The cube's own camera is independent of the main viewport: it sits
//! on a small orbit at the origin with `(azimuth, elevation)` mirrored
//! from the main camera so the cube's orientation faithfully reflects
//! the user's current view.  This way clicking a face / dragging the
//! cube produces an immediately legible result.

use bytemuck::{Pod, Zeroable};
use demo_wgpu::{MsaaFramebuffer, WgpuCustomRender, WgpuCustomRenderCtx};
use glam::{Mat4, Quat, Vec3};
use wgpu::util::DeviceExt;

use super::cube_geometry::{build_cube, CubeVertex};
use super::face_textures::{FaceTexture, TEX_SIZE};

/// 4× MSAA matches the main scene renderer; lets the cube edges read
/// crisply at the small 100 px widget size.
const SAMPLE_COUNT: u32 = 4;
const FACE_MIP_COUNT: u32 = 9; // 256 -> 1

/// Radius of the miniature cube camera.  A radius of 3 made the cube
/// nearly fill the whole 100×100 widget; MatterCAD leaves visible
/// padding inside the tumble-cube control.  4.1 brings the face size
/// into the same visual range while preserving the existing FOV.
pub const TUMBLE_CUBE_CAMERA_RADIUS: f32 = 4.1;
/// Match MatterCAD's `world.RotationMatrix = LookAt(...) * Scale(.8)`
/// for the miniature orientation cube.
pub const TUMBLE_CUBE_MODEL_SCALE: f32 = 0.8;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CubeUniforms {
    /// Combined view-projection-model. Model is identity (cube already
    /// at the origin), so this is `proj * view`.
    mvp: [f32; 16],
    /// World-space directional light. xyz = direction, w = unused.
    light_dir: [f32; 4],
}

const CUBE_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    light_dir: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var face_tex: texture_2d<f32>;
@group(0) @binding(2) var face_smp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VOut {
    var o: VOut;
    o.clip = u.mvp * vec4<f32>(pos, 1.0);
    o.world_normal = normal;
    o.uv = uv;
    return o;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let tex = textureSample(face_tex, face_smp, in.uv);
    // Mild Lambert tint so adjacent faces remain visually distinct even
    // when they share the same label palette.
    let n = normalize(in.world_normal);
    let l = normalize(u.light_dir.xyz);
    let diff = max(dot(n, l), 0.0);
    let ambient = 0.55;
    let lit = ambient + diff * 0.45;
    return vec4<f32>(tex.rgb * lit, tex.a);
}
"#;

/// Per-face GPU resources.  The per-frame bind group is built fresh
/// each `render` against the current frame UBO, so we only store the
/// texture + view + upload bookkeeping here.
struct GpuFace {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// `as_ptr()` of the CPU pixel buffer the last upload was made from.
    /// Used together with the per-face `dirty` flag to skip pointless
    /// re-uploads when nothing has changed.
    last_upload_ptr: usize,
}

struct GpuState {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    faces: Vec<GpuFace>,
    framebuffer: Option<MsaaFramebuffer>,
}

/// Tumble-cube custom render renderer.
///
/// Owned by the [`super::widget::TumbleCubeWidget`] via an
/// `Rc<RefCell<>>` and registered with agg-gui's
/// [`WgpuGfxCtx::push_custom_render`] each paint.
pub struct TumbleCubeRenderer {
    state: Option<GpuState>,
    /// Latest CPU pixel state — the widget updates this every paint
    /// before the renderer runs.  Six entries, one per face index.
    pub faces_cpu: Vec<FaceTexture>,
    /// Camera-to-world orientation mirrored from the main viewport's
    /// `OrbitCamera`. The cube renders from the same orientation so
    /// it always shows the user's current view direction.
    pub orientation: Quat,
}

impl TumbleCubeRenderer {
    pub fn new(faces_cpu: Vec<FaceTexture>) -> Self {
        Self {
            state: None,
            faces_cpu,
            orientation: Quat::IDENTITY,
        }
    }

    /// Update the mirrored orientation. Called each paint by the widget.
    pub fn set_orientation(&mut self, orientation: Quat) {
        self.orientation = orientation;
    }

    fn ensure_state(&mut self, device: &wgpu::Device, surface_format: wgpu::TextureFormat) {
        if let Some(s) = &self.state {
            if s.surface_format == surface_format {
                return;
            }
        }
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tumble cube shader"),
            source: wgpu::ShaderSource::Wgsl(CUBE_SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tumble cube bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tumble cube pl"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let vlayout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<CubeVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { offset: 0,  shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
                wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Float32x3 },
                wgpu::VertexAttribute { offset: 24, shader_location: 2, format: wgpu::VertexFormat::Float32x2 },
            ],
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tumble cube pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[vlayout],
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

        // Vertex / index buffers — cube geometry doesn't change.
        let (verts, indices) = build_cube();
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tumble cube vb"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tumble cube ib"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tumble cube sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        // Six 256×256 textures — initial dirty=true on every face so the
        // first frame's `render` uploads the painted labels.
        let mut faces = Vec::with_capacity(6);
        for _ in 0..6 {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tumble cube face tex"),
                size: wgpu::Extent3d {
                    width: TEX_SIZE,
                    height: TEX_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: FACE_MIP_COUNT,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            faces.push(GpuFace {
                texture,
                view,
                last_upload_ptr: 0,
            });
        }

        self.state = Some(GpuState {
            pipeline,
            bind_group_layout: bgl,
            surface_format,
            vbuf,
            ibuf,
            sampler,
            faces,
            framebuffer: None,
        });
    }

    fn ensure_framebuffer(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let s = match &mut self.state { Some(s) => s, None => return };
        let format = s.surface_format;
        let w = w.max(1);
        let h = h.max(1);
        match &mut s.framebuffer {
            Some(fb) => fb.ensure_size(device, w, h),
            None => {
                s.framebuffer = Some(MsaaFramebuffer::new(
                    device, w, h, SAMPLE_COUNT, format, true,
                ));
            }
        }
    }

    fn upload_dirty_faces(&mut self, queue: &wgpu::Queue) {
        let s = match &mut self.state { Some(s) => s, None => return };
        for (i, face_tex) in self.faces_cpu.iter_mut().enumerate() {
            let ptr = face_tex.active.as_ptr() as usize;
            if !face_tex.dirty && ptr == s.faces[i].last_upload_ptr {
                continue;
            }
            let mips = build_mip_chain(&face_tex.active, TEX_SIZE, TEX_SIZE);
            for (mip_level, (w, h, rgba)) in mips.iter().enumerate() {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &s.faces[i].texture,
                        mip_level: mip_level as u32,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: Some(*h),
                    },
                    wgpu::Extent3d {
                        width: *w,
                        height: *h,
                        depth_or_array_layers: 1,
                    },
                );
            }
            face_tex.dirty = false;
            s.faces[i].last_upload_ptr = ptr;
        }
    }

    fn build_mvp(&self, aspect: f32) -> [f32; 16] {
        // Cube-local orbit camera: sit back far enough that the cube
        // leaves visible padding inside its 100×100 widget, like the
        // MatterCAD control. The orientation is mirrored from the
        // main viewport, so the cube always shows the user's current
        // view direction. Using a slightly tighter FOV than the main
        // viewport keeps the cube visually compact.
        let eye = self.orientation * Vec3::Z * TUMBLE_CUBE_CAMERA_RADIUS;
        // view = inverse(camera world transform = T(eye) * R(orient))
        let camera_world = Mat4::from_rotation_translation(self.orientation, eye);
        let view = camera_world.inverse();
        let proj = Mat4::perspective_rh_gl(
            std::f32::consts::PI * 0.22,
            aspect.max(1e-6),
            0.1,
            100.0,
        );
        let model = Mat4::from_scale(Vec3::splat(TUMBLE_CUBE_MODEL_SCALE));
        (proj * view * model).to_cols_array()
    }
}

impl WgpuCustomRender for TumbleCubeRenderer {
    fn render(&mut self, ctx: WgpuCustomRenderCtx<'_>) {
        self.ensure_state(ctx.device, ctx.surface_format);
        let fb_w = ctx.screen_rect.width.max(1.0) as u32;
        let fb_h = ctx.screen_rect.height.max(1.0) as u32;
        if fb_w == 0 || fb_h == 0 {
            return;
        }
        self.ensure_framebuffer(ctx.device, fb_w, fb_h);
        self.upload_dirty_faces(ctx.queue);

        let aspect = fb_w as f32 / fb_h.max(1) as f32;
        let mvp = self.build_mvp(aspect);
        let uniforms = CubeUniforms {
            mvp,
            light_dir: [0.4, 0.8, 0.5, 0.0],
        };

        // The bind groups hold individual UBOs, but they were created
        // with COPY_DST so we can rewrite the uniforms each frame.
        // wgpu doesn't expose the underlying buffer through the bind
        // group, so we allocate a fresh UBO + bind group per frame —
        // cheap (six small allocations) and avoids holding stale state
        // when the surface format changes.
        let s_state = self.state.as_ref().unwrap();
        let bgl = &s_state.bind_group_layout;

        let frame_ubo = ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tumble cube frame ubo"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        // Build six per-face bind groups against the same UBO + per-face
        // texture so we can pick the correct label texture in the draw.
        let mut per_face_bg: Vec<wgpu::BindGroup> = Vec::with_capacity(6);
        for i in 0..6 {
            per_face_bg.push(ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tumble cube per-face bg"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: frame_ubo.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&s_state.faces[i].view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&s_state.sampler),
                    },
                ],
            }));
        }

        let s = self.state.as_ref().unwrap();
        let fb = s.framebuffer.as_ref().unwrap();
        let depth = match fb.depth_view() { Some(d) => d, None => return };

        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tumble cube offscreen"),
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
            pass.set_pipeline(&s.pipeline);
            pass.set_vertex_buffer(0, s.vbuf.slice(..));
            pass.set_index_buffer(s.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            // Draw face by face — 6 faces × 6 indices = 36 indices total.
            for i in 0..6 {
                pass.set_bind_group(0, &per_face_bg[i], &[]);
                let base = (i * 6) as u32;
                pass.draw_indexed(base..(base + 6), 0, 0..1);
            }
        }

        // Composite the resolved colour onto the active 2-D target,
        // alpha-blending so the cube reads on whatever sits behind it
        // (the viewport scene + 2-D HUD ring).
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

fn build_mip_chain(base: &[u8], w: u32, h: u32) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out = Vec::new();
    out.push((w, h, base.to_vec()));
    let mut cur_w = w;
    let mut cur_h = h;
    while cur_w > 1 || cur_h > 1 {
        let prev = &out.last().unwrap().2;
        let next_w = (cur_w / 2).max(1);
        let next_h = (cur_h / 2).max(1);
        let next = downsample_rgba_box(prev, cur_w, cur_h, next_w, next_h);
        out.push((next_w, next_h, next));
        cur_w = next_w;
        cur_h = next_h;
    }
    out
}

fn downsample_rgba_box(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];
    for y in 0..dst_h {
        for x in 0..dst_w {
            let sx0 = (x * 2).min(src_w - 1);
            let sy0 = (y * 2).min(src_h - 1);
            let sx1 = (sx0 + 1).min(src_w - 1);
            let sy1 = (sy0 + 1).min(src_h - 1);
            let mut acc = [0u32; 4];
            for (sx, sy) in [(sx0, sy0), (sx1, sy0), (sx0, sy1), (sx1, sy1)] {
                let i = ((sy * src_w + sx) * 4) as usize;
                for c in 0..4 {
                    acc[c] += src[i + c] as u32;
                }
            }
            let di = ((y * dst_w + x) * 4) as usize;
            for c in 0..4 {
                dst[di + c] = ((acc[c] + 2) / 4) as u8;
            }
        }
    }
    dst
}

// `look_at` / `perspective` / `scale4` / `cross3` / `dot3` /
// `normalize3` previously lived here as hand-rolled matrix helpers.
// `build_mvp` now uses `glam::Mat4` directly, so those helpers are
// gone — the few external callers route through `crate::camera`'s
// public wrappers (`mul4`, `inverse4`, `transform_point4`).
