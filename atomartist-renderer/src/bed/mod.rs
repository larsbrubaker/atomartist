//! Bed rendering for the 3-D viewport.
//!
//! Replaces the procedural-fwidth floor grid that used to live inline
//! in [`crate::scene_renderer`] with NodeDesigner's contact-shadow
//! pipeline:
//!
//! * [`texture::bake_grid_texture`] bakes a mipmapped grid texture
//!   into a [`wgpu::Texture`] once per theme.
//! * [`shadow::ShadowChain`] renders the mesh as a silhouette every
//!   frame, blurs it, and composites the bed grid + blurred shadow
//!   into a mipmapped offscreen texture.
//! * `BedRenderer` owns the bed quad pipeline that samples the
//!   composite texture and draws into the main MSAA framebuffer.
//!
//! The exported entry points (`new`, `render_to_composite`,
//! `draw_bed`, `set_line_color`, `set_dark_mode`) are the only API
//! the scene renderer needs.

use std::cell::Cell;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

mod shaders;
pub mod shadow;
mod shadow_pipelines;
pub mod texture;

use shaders::BED_QUAD_SHADER;
use shadow::{ShadowChain, BED_HALF_EXTENT};
use texture::bake_grid_texture;

pub use shadow::MeshRef;

/// All inputs that affect the contents of `composite_tex`. When this
/// matches the value used to populate the texture, the entire
/// silhouette → blur → composite → mip chain can be skipped — saves
/// ~14 render passes per idle frame. See `BedRenderer::render_to_composite`.
///
/// Note: the bed is fixed at the world origin (matching MatterCAD and
/// NodeDesigner), so the camera position is *not* an input to the
/// composite — panning never invalidates the cached texture.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CompositeKey {
    /// Stable identity for the mesh's vertex/index buffers. `0` ⇒ no
    /// mesh (composite reduces to the grid alone). The scene renderer
    /// already tracks mesh upload identity via its `mesh_ptr` field,
    /// so we forward that value here unchanged.
    mesh_id: u64,
    /// `1` ⇒ dark-mode shadow inversion enabled.
    invert_flag: u8,
    /// Grid-line colour packed as RGBA8. Re-baking the grid texture
    /// is what actually invalidates the composite — but tracking the
    /// colour here makes the dependency explicit and survives the
    /// `colors_equal` quantisation step in `set_line_color`.
    color_key: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BedUniforms {
    mvp: [f32; 16],
    /// xy = half-extents of the bed quad in world units,
    /// z = world-Z height of the plane, w = pad.
    plane: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BedQuadVertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

/// Bed-rendering glue. Holds the per-frame contact-shadow chain plus
/// the per-frame bed-quad pipeline that draws the composite texture
/// into the scene renderer's MSAA framebuffer.
pub struct BedRenderer {
    /// Surface format threaded through from
    /// [`demo_wgpu::WgpuCustomRenderCtx`] — drives the bed quad's
    /// pipeline target and the composite texture's format.
    surface_format: wgpu::TextureFormat,
    chain: ShadowChain,

    /// `wgpu::TextureView` holds an internal `Arc` to the texture, so
    /// we keep only the view (re-bake replaces it).
    grid_view: wgpu::TextureView,
    grid_line_color: [f32; 4],
    /// Tracks the current dark-mode flag for [`CompositeKey`] so the
    /// invalidation logic in `render_to_composite` can detect a
    /// theme flip without rereading the chain's private state.
    is_dark: bool,

    bed_pipeline: wgpu::RenderPipeline,
    bed_vbuf: wgpu::Buffer,
    /// Kept alive only to back the persistent `bed_bg` below — the
    /// sampler binding holds a borrow into this field internally.
    _bed_sampler: wgpu::Sampler,
    /// Persistent uniform buffer + bind group for the bed quad —
    /// rewritten via `queue.write_buffer` each frame instead of being
    /// reallocated. Allocating fresh wgpu resources per frame is the
    /// single biggest source of CPU-side render overhead in the
    /// frame-time logger, and a static-size UB lets us avoid it.
    bed_ub: wgpu::Buffer,
    bed_bg: wgpu::BindGroup,

    /// Last `CompositeKey` the offscreen chain was rendered with.
    /// `None` means the composite texture has never been populated
    /// (first frame) and must be rendered unconditionally. Interior
    /// mutability so `render_to_composite` can stay `&self`.
    last_composite_key: Cell<Option<CompositeKey>>,
    /// Frames since startup. Used purely by the diagnostic logging in
    /// `render_to_composite` so we can rate-limit the cache-miss
    /// chatter on stderr (set `ATOMARTIST_BED_LOG=1` to enable).
    frame_counter: Cell<u64>,
}

impl BedRenderer {
    /// Construct the bed renderer for the given surface format and
    /// MSAA sample count. The grid texture is baked immediately with
    /// `initial_line_color`; callers can re-bake by calling
    /// [`Self::set_line_color`] when the theme flips.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        msaa_sample_count: u32,
        initial_line_color: [f32; 4],
    ) -> Self {
        let chain = ShadowChain::new(device, surface_format);

        let grid_view = bake_grid_texture(device, queue, surface_format, initial_line_color)
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bed_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atomartist bed quad sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        // Unit quad on XY in [-1, 1] with UVs flipped on Y so that
        // (u, v) = (0, 0) maps to the top-left of the texture — matches
        // the shadow ortho's UV convention, so the silhouette stays
        // aligned with the bed grid.
        let quad: [BedQuadVertex; 6] = [
            BedQuadVertex { pos: [-1.0, -1.0], uv: [0.0, 1.0] },
            BedQuadVertex { pos: [ 1.0, -1.0], uv: [1.0, 1.0] },
            BedQuadVertex { pos: [ 1.0,  1.0], uv: [1.0, 0.0] },
            BedQuadVertex { pos: [-1.0, -1.0], uv: [0.0, 1.0] },
            BedQuadVertex { pos: [ 1.0,  1.0], uv: [1.0, 0.0] },
            BedQuadVertex { pos: [-1.0,  1.0], uv: [0.0, 0.0] },
        ];
        let bed_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist bed quad vb"),
            contents: bytemuck::cast_slice(&quad),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let (bed_pipeline, bed_bgl) =
            build_bed_pipeline(device, surface_format, msaa_sample_count);

        let bed_ub = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atomartist bed quad ub"),
            size: std::mem::size_of::<BedUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // The composite view, sampler, and uniform buffer are all
        // stable for the lifetime of the renderer — re-baking the
        // grid does NOT invalidate the bed quad's bind group because
        // the quad samples `composite_view`, not the grid view. We
        // can therefore allocate the bind group once and reuse it
        // every frame.
        let bed_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist bed quad bg"),
            layout: &bed_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: bed_ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(chain.composite_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&bed_sampler),
                },
            ],
        });

        Self {
            surface_format,
            chain,
            grid_view,
            grid_line_color: initial_line_color,
            is_dark: false,
            bed_pipeline,
            bed_vbuf,
            _bed_sampler: bed_sampler,
            bed_ub,
            bed_bg,
            last_composite_key: Cell::new(None),
            frame_counter: Cell::new(0),
        }
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    pub fn shadow_opacity(&self) -> f32 {
        self.chain.shadow_opacity()
    }

    /// Re-bake the grid texture with a new line colour. Cheap — single
    /// CPU paint + mip box-downsample + upload, runs once per theme
    /// change. The shadow chain's composite bind group references the
    /// old grid view, so it must be invalidated here; the chain will
    /// rebuild it lazily on its next `render` call.
    pub fn set_line_color(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        new_color: [f32; 4],
    ) {
        if colors_equal(self.grid_line_color, new_color) {
            return;
        }
        self.grid_line_color = new_color;
        self.grid_view = bake_grid_texture(device, queue, self.surface_format, new_color)
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.chain.invalidate_grid_binding();
    }

    /// Inverts the composite shader's shadow colour for dark themes
    /// (white shadows against a dark bed); pass `false` for light
    /// themes (black shadows). Cheap — single uniform value flip.
    pub fn set_dark_mode(&mut self, is_dark: bool) {
        self.is_dark = is_dark;
        self.chain.set_dark_mode(is_dark);
    }

    /// Run the silhouette → blur → composite → mip chain. Pass `None`
    /// for `mesh` when no model is loaded — the shadow pass just
    /// clears its target and the composite is grid-only.
    ///
    /// `mesh_id` is a stable identity for the mesh buffers (the scene
    /// renderer's `mesh_ptr`, or `0` for "no mesh"). When the inputs
    /// are unchanged from the last successful render, the entire
    /// chain is skipped — the composite texture is already correct
    /// and re-running 14 GPU passes would just retrace the same
    /// pixels. This is what keeps the bed cheap on idle frames.
    /// Returns `true` when the shadow chain actually ran this frame —
    /// useful for the scene-renderer timing log to attribute frame
    /// cost between "cache hit" and "real GPU work".
    pub fn render_to_composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        mesh: Option<MeshRef<'_>>,
        mesh_id: u64,
        bed_z: f32,
    ) -> bool {
        let key = CompositeKey {
            mesh_id: if mesh.is_some() { mesh_id } else { 0 },
            invert_flag: u8::from(self.is_dark),
            color_key: pack_rgba8(self.grid_line_color),
        };
        let prev = self.last_composite_key.get();
        if prev == Some(key) {
            self.frame_counter.set(self.frame_counter.get().wrapping_add(1));
            return false;
        }
        log_cache_miss(prev, key);
        self.chain
            .render(device, queue, encoder, mesh, &self.grid_view, bed_z);
        self.last_composite_key.set(Some(key));
        self.frame_counter.set(self.frame_counter.get().wrapping_add(1));
        true
    }

    /// Draw the bed quad into the currently-bound render pass. Caller
    /// is responsible for having set the correct viewport / scissor;
    /// this just configures pipeline + bind group + draw.
    ///
    /// The bed is fixed at the world origin (matching MatterCAD and
    /// NodeDesigner), so the quad's XY span is `[-BED_HALF_EXTENT,
    /// BED_HALF_EXTENT]` regardless of where the camera is pointing.
    pub fn draw_bed<'rpass>(
        &'rpass self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'rpass>,
        mvp: [f32; 16],
        bed_z: f32,
    ) {
        let uniforms = BedUniforms {
            mvp,
            plane: [BED_HALF_EXTENT, BED_HALF_EXTENT, bed_z, 0.0],
        };
        queue.write_buffer(&self.bed_ub, 0, bytemuck::bytes_of(&uniforms));
        pass.set_pipeline(&self.bed_pipeline);
        pass.set_bind_group(0, &self.bed_bg, &[]);
        pass.set_vertex_buffer(0, self.bed_vbuf.slice(..));
        pass.draw(0..6, 0..1);
    }
}

fn pack_rgba8(c: [f32; 4]) -> u32 {
    let q = |v: f32| -> u32 { (v.clamp(0.0, 1.0) * 255.0).round() as u32 };
    (q(c[0]) << 24) | (q(c[1]) << 16) | (q(c[2]) << 8) | q(c[3])
}

/// Print a single human-readable line on every shadow-chain cache miss
/// when `ATOMARTIST_BED_LOG=1` is set. Helps identify which input is
/// churning when frame time spikes (mesh upload, theme flip, or a real
/// first-frame populate).
fn log_cache_miss(prev: Option<CompositeKey>, now: CompositeKey) {
    if !bed_log_enabled() {
        return;
    }
    match prev {
        None => eprintln!("[bed] first-frame chain run; key={now:?}"),
        Some(p) => {
            let mut diffs: Vec<&'static str> = Vec::new();
            if p.mesh_id != now.mesh_id { diffs.push("mesh_id"); }
            if p.invert_flag != now.invert_flag { diffs.push("invert"); }
            if p.color_key != now.color_key { diffs.push("color"); }
            eprintln!("[bed] cache miss — changed: {diffs:?}; prev={p:?} now={now:?}");
        }
    }
}

fn bed_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ATOMARTIST_BED_LOG")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "yes"))
            .unwrap_or(false)
    })
}

impl std::fmt::Debug for CompositeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{{mesh=0x{:x} inv={} col=0x{:08x}}}",
            self.mesh_id, self.invert_flag, self.color_key
        )
    }
}

fn build_bed_pipeline(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    msaa_sample_count: u32,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atomartist bed quad shader"),
        source: wgpu::ShaderSource::Wgsl(BED_QUAD_SHADER.into()),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("atomartist bed quad bgl"),
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
        label: Some("atomartist bed quad pl"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let vert_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<BedQuadVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 8,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x2,
            },
        ],
    };
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("atomartist bed quad pipeline"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[vert_layout],
            compilation_options: Default::default(),
        },
        // Render both sides of the quad — when the camera dips below
        // the bed plane the user still sees grid lines underneath.
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
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
            count: msaa_sample_count,
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

fn colors_equal(a: [f32; 4], b: [f32; 4]) -> bool {
    let eps = 1.0 / 512.0;
    (a[0] - b[0]).abs() < eps
        && (a[1] - b[1]).abs() < eps
        && (a[2] - b[2]).abs() < eps
        && (a[3] - b[3]).abs() < eps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_equal_within_quantisation_step() {
        assert!(colors_equal([0.5, 0.5, 0.5, 1.0], [0.5, 0.5, 0.5, 1.0]));
        // Same 8-bit byte after quantisation.
        assert!(colors_equal([0.50, 0.50, 0.50, 1.0], [0.501, 0.501, 0.501, 1.0]));
        // Different 8-bit byte after quantisation.
        assert!(!colors_equal([0.50, 0.50, 0.50, 1.0], [0.60, 0.60, 0.60, 1.0]));
    }

    /// Headless device construction — instantiates a wgpu adapter +
    /// device without a surface, then asserts [`BedRenderer::new`] can
    /// build its pipelines. Soft-skips when no suitable adapter is
    /// available (CI / sandbox), since failing then would be a false
    /// negative.
    #[test]
    fn bed_renderer_constructs_pipelines() {
        let Some((device, queue)) = headless_device() else {
            eprintln!("bed_renderer_constructs_pipelines: no wgpu adapter, skipping");
            return;
        };
        let bed = BedRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            1,
            [0.55, 0.58, 0.66, 0.7],
        );
        // Surface format round-trips through the renderer.
        assert_eq!(bed.surface_format(), wgpu::TextureFormat::Rgba8UnormSrgb);
        // Default shadow opacity matches NodeDesigner's constant.
        assert!((bed.shadow_opacity() - 0.35).abs() < 1e-6);
    }

    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("atomartist-bed-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some((device, queue))
    }
}
