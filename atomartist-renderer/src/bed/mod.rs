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

    bed_pipeline: wgpu::RenderPipeline,
    bed_bgl: wgpu::BindGroupLayout,
    bed_vbuf: wgpu::Buffer,
    bed_sampler: wgpu::Sampler,
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

        Self {
            surface_format,
            chain,
            grid_view,
            grid_line_color: initial_line_color,
            bed_pipeline,
            bed_bgl,
            bed_vbuf,
            bed_sampler,
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
    /// change.
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
    }

    /// Inverts the composite shader's shadow colour for dark themes
    /// (white shadows against a dark bed); pass `false` for light
    /// themes (black shadows). Cheap — single uniform value flip.
    pub fn set_dark_mode(&mut self, is_dark: bool) {
        self.chain.set_dark_mode(is_dark);
    }

    /// Run the silhouette → blur → composite → mip chain. Pass `None`
    /// for `mesh` when no model is loaded — the shadow pass just
    /// clears its target and the composite is grid-only.
    pub fn render_to_composite(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        mesh: Option<MeshRef<'_>>,
        bed_z: f32,
        camera_center_xy: [f32; 2],
    ) {
        self.chain.render(
            device,
            encoder,
            mesh,
            &self.grid_view,
            bed_z,
            camera_center_xy,
        );
    }

    /// Draw the bed quad into the currently-bound render pass. Caller
    /// is responsible for having set the correct viewport / scissor;
    /// this just configures pipeline + bind group + draw.
    pub fn draw_bed<'rpass>(
        &'rpass self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass<'rpass>,
        mvp: [f32; 16],
        bed_z: f32,
    ) {
        let uniforms = BedUniforms {
            mvp,
            plane: [BED_HALF_EXTENT, BED_HALF_EXTENT, bed_z, 0.0],
        };
        let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist bed quad ub"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atomartist bed quad bg"),
            layout: &self.bed_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(self.chain.composite_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.bed_sampler),
                },
            ],
        });
        pass.set_pipeline(&self.bed_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_vertex_buffer(0, self.bed_vbuf.slice(..));
        pass.draw(0..6, 0..1);
        // Drop the temporaries after the draw — wgpu keeps them alive
        // for the encoded commands via internal Arc bumps.
        drop(bg);
        drop(ub);
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
