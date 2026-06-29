//! wgpu scene renderer — implements `WgpuCustomRender` to draw the latest
//! mesh as a shaded 3D scene through agg-gui's custom-render hook.
//!
//! ## Offscreen-buffered viewport
//!
//! Rather than injecting render commands into the same wgpu encoder + target
//! view that the 2-D UI pipeline uses (which couples 3-D anti-aliasing
//! settings to the 2-D pipeline and forces every viewport-overlay control
//! to live inside the 3-D pass), the renderer owns a dedicated
//! [`SsaaFramebuffer`] sized to the viewport widget's pixel rect:
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
//! ## Anti-aliasing — spatial 3×3 supersampling
//!
//! Every offscreen scene target is single-sample and allocated at
//! [`SSAA_SCALE`]× the on-screen pixel size; the whole scene renders
//! once into that oversized buffer, then the final composite uses
//! [`SsaaFramebuffer::blit_downsample_3x_to`] (a 9-tap box filter) to
//! resolve it down to the widget rect — one pass, fully AA'd.
//!
//! The targets must stay single-sample: dual depth peeling
//! ([`crate::scene_renderer::depth_peel`]) samples the per-pixel
//! scene-depth in-shader, and a per-sample depth attachment would make
//! that "what is the opaque-pass depth here?" lookup ambiguous.
//!
//! The shader stack is single Blinn-Phong-ish: vertex carries position +
//! normal; fragment shades against a fixed key + fill light plus ambient.

use bytemuck::cast_slice;
use demo_wgpu::SsaaFramebuffer;
use wgpu::util::DeviceExt;

use atomartist_lib::geometry::{is_inherit_color, Body, DEFAULT_GEOMETRY_COLOR};

use crate::bed::BedRenderer;
use crate::camera::OrbitCamera;

pub mod body_uniform;
pub mod depth_peel;
pub mod gizmo_pass;
pub mod opaque_pass;
mod opaque_shaders;
pub mod post_outline;
mod render_impl;
mod timings;
mod util;

use util::{ensure_scene_depth, ensure_scene_depth_color};

use depth_peel::pipelines::DualPeelPipelines;
use depth_peel::DualPeelTargets;
use gizmo_pass::GizmoLinePipelines;
pub use gizmo_pass::{GizmoLineSet, GizmoTriangleSet};
use opaque_pass::{OpaquePipelines, Vertex};
use post_outline::{OutlinePipelines, OutlineTargets};

/// Render-style picker beneath the tumble cube.  Drives the surface
/// pipeline used by [`WgpuSceneRenderer`] so the user can compare a
/// shaded model with a wireframe-only or outline-only view, matching
/// MatterCAD's `ViewStyleButton` choices without the printer-specific
/// modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderStyle {
    /// Default Blinn-Phong shaded surface.
    Shaded,
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

/// One body's worth of cached GPU buffers + the source-Body
/// fingerprint we use to detect changes.
///
/// The cache key is `(mesh_ptr, vertex_colors_ptr, body_color_q)` —
/// swapping any of those rebuilds this entry. The body's transform
/// rides on the uniform write path and does NOT invalidate the
/// vertex/index/colour buffers.
///
/// ## Colour buffer is always allocated
///
/// Every body carries a `cbuf` at vertex-buffer slot 1, regardless
/// of whether the source [`atomartist_lib::geometry::Body`] has a
/// `vertex_colors` overlay:
///
/// * Source body has `vertex_colors = Some(v)` — `cbuf` mirrors `v`
///   (per-vertex RGBA carried directly).
/// * Source body has `vertex_colors = None` — `cbuf` is filled with
///   the body's uniform `color` repeated per vertex.
///
/// Either way, the shader's `@location(2)` color attribute reads a
/// valid value per vertex and the fragment shader's
/// `v_color * b.color` math produces the right result without a
/// branch. Keeps the pipeline cache to a single variant — the
/// alternative (two pipelines selecting on `has_vertex_colors`) was
/// considered and rejected because the colour-fill cost is small
/// compared with the pipeline-switching overhead and binding-group
/// rebuild on a real multi-body scene.
pub struct BodyGpu {
    /// Pointer to the source `MeshGL::vert_properties` buffer.
    /// Doubles as the primary cache key.
    pub mesh_ptr: usize,
    /// Pointer to the source `Body::vertex_colors` buffer (0 when
    /// the body has no per-vertex colour overlay). Secondary cache
    /// key so a colour-only swap rebuilds the colour VBO.
    pub vertex_colors_ptr: usize,
    /// Quantised body colour — tertiary cache key so the cbuf
    /// rebuilds when a Color-node-tinted body has no per-vertex
    /// data but its uniform tint changes.
    pub body_color_q: u32,
    /// Position + normal vertex buffer (slot 0).
    pub vbuf: wgpu::Buffer,
    /// Triangle index buffer.
    pub ibuf: wgpu::Buffer,
    /// Per-vertex RGBA colour buffer (slot 1). Always populated —
    /// see the type-level doc for the per-vertex vs uniform-fill
    /// branch at build time.
    pub cbuf: wgpu::Buffer,
    /// Triangle index count for `draw_indexed`.
    pub index_count: u32,
    /// Vertex count — used to size the colour-fill when the source
    /// body lacks per-vertex data.
    pub vert_count: u32,
}

/// Linear SSAA scale: every offscreen scene target is allocated at
/// `SSAA_SCALE × {on-screen w, h}` and box-downsampled on the final
/// composite. `3` → a 3×3 (9×) supersample, matching agg-gui's
/// [`SsaaFramebuffer::blit_downsample_3x_to`] kernel — all 9 source
/// texels under each output pixel contribute equally.
pub const SSAA_SCALE: u32 = 3;

/// Linear HDR format for the offscreen scene composite target.
/// `Rgba16Float` keeps the dual-peel resolve, outline, and gizmo
/// passes shading in linear space so the final 3×3 box downsample
/// averages linear colour (correct) and the hardware encodes
/// linear→sRGB once on the write to the surface. The peel / outline /
/// gizmo pipelines are all built for this format.
pub const SAMPLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// GPU resources that survive across frames once `ensure_state` runs.
/// Held inside an `Option` on the renderer so it can be lazily built on
/// the first frame (when the device + surface format become available).
struct GpuState {
    surface_format: wgpu::TextureFormat,

    opaque: OpaquePipelines,

    /// Dual depth-peeling pipelines — init / peel / resolve. Built once
    /// during `ensure_state`; the per-frame chain orchestration walks
    /// these by reference.
    dual_peel: DualPeelPipelines,

    /// Bed renderer — owns the baked grid texture and the contact-shadow
    /// chain. See [`crate::bed`] for the off-screen silhouette → blur →
    /// composite pipeline that runs each frame before the main pass.
    bed: BedRenderer,

    /// Per-body GPU cache. One entry per `WgpuSceneRenderer::bodies`
    /// element, rebuilt lazily when the source mesh pointer changes.
    /// See [`BodyGpu`] for the per-body field breakdown.
    bodies_gpu: Vec<BodyGpu>,

    /// Dynamic-offset uniform buffer holding one [`BodyUniform`] slot
    /// per body. Sized via [`BodyUniformBuffer::ensure_capacity`].
    body_uniforms: body_uniform::BodyUniformBuffer,

    /// Offscreen background framebuffer for the opaque pass — bed +
    /// mesh depth-only render into this. The dual-peel resolve samples
    /// this texture as `scene_color`. Sized at `SSAA_SCALE ×` the
    /// on-screen rect (the whole scene supersamples). We allocate the
    /// depth attachment separately so it can be made `TEXTURE_BINDING`
    /// sample-able by the dual-peel shaders.
    framebuffer: Option<SsaaFramebuffer>,

    /// Hardware depth attachment for the opaque pass — used for
    /// regular depth testing during scene / bed / outline draws.
    /// Not sample-able from shaders because Naga's WebGL2 backend
    /// can't `textureLoad` from depth textures (it binds them as
    /// `sampler2DShadow` in GLSL).
    scene_depth: Option<(wgpu::Texture, wgpu::TextureView)>,

    /// R32Float mirror of `scene_depth` populated by the opaque
    /// pipelines from their fragment shader at `@location(1)`. The
    /// dual-peel init / colour shaders sample this view as a
    /// regular `texture_2d<f32>` (see `depth_peel::shaders` for the
    /// matching `textureLoad`).
    scene_depth_color: Option<(wgpu::Texture, wgpu::TextureView)>,

    /// Dual-peel ping-pong + accumulator textures. Sized to match
    /// `framebuffer`; reallocated on resize via
    /// [`DualPeelTargets::ensure_size`].
    peel_targets: Option<DualPeelTargets>,

    /// Offscreen scene composite target, held as an [`SsaaFramebuffer`]
    /// in [`SAMPLE_FORMAT`] (HDR, no depth) sized at `SSAA_SCALE ×` the
    /// on-screen rect. The dual-peel resolve, the selection outline,
    /// and the gizmo passes all render into `scene_fb.render_view()`;
    /// the final composite calls
    /// [`SsaaFramebuffer::blit_downsample_3x_to`] to box-filter it down
    /// onto the active 2-D target.
    scene_fb: Option<SsaaFramebuffer>,

    /// Pipelines + uniforms for the Blender-style post-process
    /// selection outline. Built once during `ensure_state`; renders
    /// into `scene_fb` after the dual-peel resolve. See
    /// [`crate::scene_renderer::post_outline`] for the per-pass
    /// rationale.
    post_outline: OutlinePipelines,

    /// Textures the outline chain renders into: ID mask, hardware
    /// depth for the ID prepass, and an `R32Float` mirror of the
    /// selected-mesh depth. Reallocated on resize via
    /// [`OutlineTargets::ensure_size`].
    outline_targets: Option<OutlineTargets>,

    /// Solid + overlay line pipelines used by the gizmo pass. See
    /// [`crate::scene_renderer::gizmo_pass`] for the rationale
    /// behind the two-variant pattern (depth-tested solid + no-depth
    /// alpha-blended overlay) shared across all gizmos.
    gizmo_pipelines: GizmoLinePipelines,
}

pub struct WgpuSceneRenderer {
    state: Option<GpuState>,
    pub camera: OrbitCamera,
    /// Bodies to render this frame. The viewport widget pushes a
    /// `Geometry3d`'s `bodies` here verbatim; the renderer iterates
    /// them per peel pass (matching NodeDesigner /
    /// MatterCAD: each peel iteration draws every body).
    ///
    /// Empty = "nothing to draw" — the chain still runs (the bed
    /// composite + the SSAA downsample), but every per-body pipeline
    /// is skipped.
    pub bodies: Vec<Body>,
    pub viewport_size: (u32, u32),
    /// Fallback tint used when `bodies` is empty (so the bed pass
    /// still has a sane background colour). Per-body tint lives on
    /// each `Body::color`.
    pub base_color: [f32; 4],
    /// Light 0 (key light) direction — used as a *view-space* (camera-
    /// fixed) directional light, matching NodeDesigner's
    /// `lightDir0` uniform default of `(-1, -1, 1).normalize()`.
    pub light_dir: [f32; 3],
    /// Light 1 (fill light) direction. Camera-fixed; NodeDesigner
    /// default `(1, 1, 1).normalize()`.
    pub light_dir1: [f32; 3],
    /// Per-channel diffuse intensity of light 0 (NodeDesigner default
    /// `(0.7, 0.7, 0.7)`).
    pub light_diffuse0: [f32; 3],
    /// Per-channel specular intensity of light 0 (NodeDesigner default
    /// `(0.05, 0.05, 0.05)`).
    pub light_specular0: [f32; 3],
    /// Per-channel ambient intensity attached to light 0 (NodeDesigner
    /// keeps this at zero and uses `global_ambient` for the scene-wide
    /// floor).
    pub light_ambient0: [f32; 3],
    /// Per-channel diffuse intensity of light 1 (NodeDesigner default
    /// `(0.5, 0.5, 0.5)`).
    pub light_diffuse1: [f32; 3],
    /// Per-channel specular intensity of light 1 (NodeDesigner default
    /// `(0.05, 0.05, 0.05)`).
    pub light_specular1: [f32; 3],
    /// Per-channel scene-wide ambient (NodeDesigner default
    /// `(0.2, 0.2, 0.2)`).
    pub global_ambient: [f32; 3],
    /// Per-channel material specular tint (NodeDesigner default
    /// `(1.0, 1.0, 1.0)` — lets per-light specular control intensity).
    pub material_specular: [f32; 3],
    /// Blinn-Phong shininess exponent (NodeDesigner default `30.0`).
    pub shininess: f32,
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
    /// Which body in `bodies` the outline silhouette should rim.
    /// `None` (or out-of-range) → first body, so a single-body scene
    /// keeps working without the host pre-computing the index. Host
    /// (viewport) sets this to the body whose `origin` matches the
    /// active selection so clicking body 2 of a multi-body group
    /// outlines body 2, not body 0.
    pub outline_body_index: Option<usize>,
    /// Surface render style — picked by the render-style picker beneath
    /// the tumble cube.  Drives the shaded vs outline-only vs wireframe
    /// branch in the main pass.
    pub render_style: RenderStyle,

    /// Gizmo line sets — the host populates this each frame with one
    /// entry per visible gizmo (bounds box, Z control, XY control,
    /// rotate corner, measurement overlay). Each entry carries its
    /// own vertices + colour + transform; see [`GizmoLineSet`] for
    /// the field-by-field breakdown. Empty by default — gizmos are
    /// pushed by viewport code in response to selection changes.
    pub gizmo_lines: Vec<GizmoLineSet>,

    /// Per-frame list of filled-triangle gizmo sets — the handle
    /// meshes (small spheres / cubes) that the control gizmos drag.
    /// Same lifecycle as [`gizmo_lines`]: the host populates this
    /// each frame in response to selection / drag state, the renderer
    /// re-uploads the vertex buffer on every draw.
    pub gizmo_triangles: Vec<GizmoTriangleSet>,
}

impl WgpuSceneRenderer {
    pub fn new() -> Self {
        Self {
            state: None,
            camera: OrbitCamera::default(),
            bodies: Vec::new(),
            viewport_size: (0, 0),
            base_color: [0.62, 0.66, 0.78, 1.0],
            // NodeDesigner `lightDir0 = (-1, -1, 1).normalize()`.
            light_dir: [-0.577_350_3, -0.577_350_3, 0.577_350_3],
            // NodeDesigner `lightDir1 = (1, 1, 1).normalize()`.
            light_dir1: [0.577_350_3, 0.577_350_3, 0.577_350_3],
            light_diffuse0: [0.7, 0.7, 0.7],
            light_specular0: [0.05, 0.05, 0.05],
            light_ambient0: [0.0, 0.0, 0.0],
            light_diffuse1: [0.5, 0.5, 0.5],
            light_specular1: [0.05, 0.05, 0.05],
            global_ambient: [0.2, 0.2, 0.2],
            material_specular: [1.0, 1.0, 1.0],
            shininess: 30.0,
            grid_line_color: [0.55, 0.58, 0.66, 0.7],
            grid_dark_mode: false,
            draw_grid: true,
            grid_z: 0.0,
            outline_enabled: false,
            outline_color: [1.0, 0.55, 0.10, 1.0],
            outline_width: 0.05,
            outline_body_index: None,
            render_style: RenderStyle::Shaded,
            gizmo_lines: Vec::new(),
            gizmo_triangles: Vec::new(),
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

        let opaque = OpaquePipelines::new(device, surface_format);
        // The dual-peel resolve writes into the HDR scene composite
        // target (`scene_fb`) — NOT the surface — so its colour
        // attachment must use `SAMPLE_FORMAT` (`Rgba16Float`).
        // Mismatching the pipeline format vs the bound attachment
        // panics at draw time inside wgpu's validation layer.
        let dual_peel = DualPeelPipelines::new(device, SAMPLE_FORMAT);

        let mut bed = BedRenderer::new(
            device,
            queue,
            surface_format,
            self.grid_line_color,
        );
        bed.set_dark_mode(self.grid_dark_mode);

        // Post-process outline writes into the HDR scene composite
        // (`scene_fb`) so it supersamples with the rest of the scene
        // and resolves through the same 3×3 box downsample. That
        // target's format is `SAMPLE_FORMAT` (Rgba16Float), not the
        // surface format.
        let post_outline = OutlinePipelines::new(device, SAMPLE_FORMAT);

        // Gizmo line pipelines target the same HDR scene view (so
        // gizmos AA-smooth with the rest of the scene) and depth-test
        // the solid variant against `scene_depth` (the opaque pass's
        // depth attachment).
        let gizmo_pipelines = GizmoLinePipelines::new(
            device,
            SAMPLE_FORMAT,
            wgpu::TextureFormat::Depth32Float,
        );

        self.state = Some(GpuState {
            surface_format,
            opaque,
            dual_peel,
            bed,
            bodies_gpu: Vec::new(),
            body_uniforms: body_uniform::BodyUniformBuffer::new(),
            framebuffer: None,
            scene_depth: None,
            scene_depth_color: None,
            peel_targets: None,
            scene_fb: None,
            post_outline,
            outline_targets: None,
            gizmo_pipelines,
        });
    }

    /// Lazily allocate (or resize) every offscreen scene target at
    /// `SSAA_SCALE × {w, h}` — the background framebuffer, the
    /// sample-able scene-depth texture, the dual-peel targets, the HDR
    /// scene composite (`scene_fb`), and the outline targets. `(w, h)`
    /// is the **on-screen** widget size; this multiplies by
    /// [`SSAA_SCALE`] so the whole scene supersamples. Cheap when the
    /// size is stable.
    fn ensure_framebuffer(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let s = match &mut self.state {
            Some(s) => s,
            None => return,
        };
        let format = s.surface_format;
        // Supersample dimensions — every scene target renders at this
        // size; the final composite box-downsamples it to `(w, h)`.
        let w = (w.max(1)) * SSAA_SCALE;
        let h = (h.max(1)) * SSAA_SCALE;
        match &mut s.framebuffer {
            Some(fb) => fb.ensure_size(device, w, h),
            None => {
                s.framebuffer = Some(SsaaFramebuffer::new(
                    device,
                    w,
                    h,
                    format,
                    // Depth lives in `scene_depth` so it can be marked
                    // TEXTURE_BINDING for the dual-peel discard sampler.
                    /* with_depth */ false,
                ));
            }
        }
        ensure_scene_depth(device, &mut s.scene_depth, w, h);
        ensure_scene_depth_color(device, &mut s.scene_depth_color, w, h);
        match &mut s.peel_targets {
            Some(t) => t.ensure_size(device, w, h),
            None => s.peel_targets = Some(DualPeelTargets::new(device, w, h, format)),
        }
        match &mut s.scene_fb {
            Some(fb) => fb.ensure_size(device, w, h),
            None => {
                // HDR (SAMPLE_FORMAT) so the dual-peel / outline / gizmo
                // passes — all built for SAMPLE_FORMAT — render into it
                // and the 3×3 box downsample averages linear colour.
                s.scene_fb = Some(SsaaFramebuffer::new(
                    device,
                    w,
                    h,
                    SAMPLE_FORMAT,
                    /* with_depth */ false,
                ));
            }
        }
        match &mut s.outline_targets {
            Some(t) => t.ensure_size(device, w, h),
            None => s.outline_targets = Some(OutlineTargets::new(device, w, h)),
        }
    }

    /// Bed-quad render-time Z. Temporarily locked to literal `0.0`
    /// while the camera-distance-based offset is reworked — the
    /// previous formula moved the bed in the wrong direction and
    /// with too large a magnitude under some camera orientations.
    /// `grid_z` is intentionally ignored too, so any stale writes
    /// can't reintroduce motion until the new formula lands.
    fn bed_render_z(&self) -> f32 {
        0.0
    }

    /// Refresh the per-body GPU cache + the dynamic body-uniform
    /// buffer so they reflect `self.bodies`.
    ///
    /// Strategy: for each body in declaration order, reuse the
    /// existing `bodies_gpu` entry when its `(mesh_ptr, vertex_colors_ptr)`
    /// matches; rebuild otherwise. Surplus entries are dropped.
    ///
    /// Per-body uniforms (model + colour + flags) are repacked into
    /// the dynamic uniform buffer every frame — the body Vec is small
    /// (typically ≤ 16) and the slot write is one `queue.write_buffer`
    /// call, so amortising further isn't worth the bookkeeping.
    ///
    /// Returns `true` when the underlying uniform buffer reallocated
    /// (capacity grew). Callers rebuild any bind group that resolves
    /// against the buffer's identity on a `true` return.
    pub(crate) fn ensure_body_buffers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> bool {
        let bodies = self.bodies.clone();
        let s = match &mut self.state {
            Some(s) => s,
            None => return false,
        };

        let mut new_cache: Vec<BodyGpu> = Vec::with_capacity(bodies.len());
        let mut taken = vec![false; s.bodies_gpu.len()];

        for body in bodies.iter() {
            let mesh = &body.mesh;
            if mesh.num_prop < 6 || mesh.vert_properties.is_empty() {
                // Skip — degenerate body. Slot still consumes a
                // `BodyUniform` entry below for index parity.
                continue;
            }
            let mesh_ptr = mesh.vert_properties.as_ptr() as usize;
            let vc_ptr = body
                .vertex_colors
                .as_ref()
                .map(|v| v.as_ptr() as usize)
                .unwrap_or(0);
            let color_q = pack_color_q(body.color);

            // Reuse an existing cache entry with matching pointers
            // AND matching tint (the tint participates in the
            // cbuf fill when there's no per-vertex overlay).
            let mut reused = false;
            for (i, prev) in s.bodies_gpu.iter().enumerate() {
                if !taken[i]
                    && prev.mesh_ptr == mesh_ptr
                    && prev.vertex_colors_ptr == vc_ptr
                    && prev.body_color_q == color_q
                {
                    taken[i] = true;
                    let clone = BodyGpu {
                        mesh_ptr: prev.mesh_ptr,
                        vertex_colors_ptr: prev.vertex_colors_ptr,
                        body_color_q: prev.body_color_q,
                        vbuf: prev.vbuf.clone(),
                        ibuf: prev.ibuf.clone(),
                        cbuf: prev.cbuf.clone(),
                        index_count: prev.index_count,
                        vert_count: prev.vert_count,
                    };
                    new_cache.push(clone);
                    reused = true;
                    break;
                }
            }
            if reused {
                continue;
            }

            // Build fresh — pos+normal VBO, index, then the colour
            // VBO at slot 1.
            let stride = mesh.num_prop as usize;
            let n_verts = mesh.vert_properties.len() / stride;
            let mut verts: Vec<Vertex> = Vec::with_capacity(n_verts);
            for i in 0..n_verts {
                verts.push(Vertex {
                    pos: [
                        mesh.vert_properties[i * stride],
                        mesh.vert_properties[i * stride + 1],
                        mesh.vert_properties[i * stride + 2],
                    ],
                    normal: [
                        mesh.vert_properties[i * stride + 3],
                        mesh.vert_properties[i * stride + 4],
                        mesh.vert_properties[i * stride + 5],
                    ],
                });
            }
            let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist body vb"),
                contents: cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist body ib"),
                contents: cast_slice(&mesh.tri_verts),
                usage: wgpu::BufferUsages::INDEX,
            });

            // Colour VBO — per-vertex when the source body has one,
            // otherwise fill with the body's uniform colour. See the
            // `BodyGpu` doc for why we always allocate this rather
            // than gating on `has_vertex_colors`.
            let cbuf_data: Vec<f32> = match body.vertex_colors.as_ref() {
                Some(colors) if colors.len() == n_verts * 4 => (**colors).clone(),
                _ => {
                    // Either no per-vertex overlay OR length mismatch
                    // (defensive — a mis-sized overlay falls back to
                    // the uniform tint rather than risking a buffer
                    // overrun in the shader).
                    let mut v = Vec::with_capacity(n_verts * 4);
                    for _ in 0..n_verts {
                        v.extend_from_slice(&body.color);
                    }
                    v
                }
            };
            let cbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("atomartist body cb"),
                contents: cast_slice(&cbuf_data),
                usage: wgpu::BufferUsages::VERTEX,
            });

            new_cache.push(BodyGpu {
                mesh_ptr,
                vertex_colors_ptr: vc_ptr,
                body_color_q: color_q,
                vbuf,
                ibuf,
                cbuf,
                index_count: mesh.tri_verts.len() as u32,
                vert_count: n_verts as u32,
            });
        }

        s.bodies_gpu = new_cache;

        // Resize + repopulate the dynamic uniform buffer. One slot
        // per body — the slot order matches `bodies_gpu` so a draw
        // call's body index doubles as the uniform-slot index.
        let needed = bodies.len() as u32;
        let realloc = s.body_uniforms.ensure_capacity(device, needed);
        let mut slots: Vec<body_uniform::BodyUniform> = Vec::with_capacity(bodies.len());
        for body in bodies.iter() {
            // Renderer-side fallback for the `INHERIT_COLOR` sentinel:
            // if a body reaches the renderer with alpha = 0, no node
            // along its chain set an explicit colour, so substitute
            // `DEFAULT_GEOMETRY_COLOR` to keep the body visible.
            let color = if is_inherit_color(&body.color) {
                DEFAULT_GEOMETRY_COLOR
            } else {
                body.color
            };
            slots.push(body_uniform::BodyUniform {
                model: body.matrix,
                color,
                flags: [body.has_vertex_colors() as u32, 0, 0, 0],
            });
        }
        if !slots.is_empty() {
            s.body_uniforms.write_slots(queue, &slots);
        }
        realloc
    }
}

/// Quantise an RGBA colour to a 32-bit packed key — 8 bits per
/// channel. Used as the tertiary body-cache key so a Color-node tint
/// change (with no per-vertex overlay) rebuilds the colour VBO.
fn pack_color_q(c: [f32; 4]) -> u32 {
    let to_u8 = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
    (to_u8(c[0]) << 24) | (to_u8(c[1]) << 16) | (to_u8(c[2]) << 8) | to_u8(c[3])
}

impl Default for WgpuSceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}


#[cfg(test)]
mod tests;
