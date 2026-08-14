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

mod body_buffers;
pub mod body_uniform;
pub mod depth_peel;
pub mod gizmo_pass;
pub mod opaque_pass;
mod opaque_shaders;
pub mod post_outline;
mod render_impl;
pub mod render_modes;
mod timings;
mod util;

use util::{ensure_scene_depth, ensure_scene_depth_color};

use depth_peel::pipelines::DualPeelPipelines;
use depth_peel::DualPeelTargets;
use gizmo_pass::GizmoLinePipelines;
pub use gizmo_pass::{GizmoLineSet, GizmoTriangleSet};
use opaque_pass::{OpaquePipelines, Vertex};
use post_outline::{OutlinePipelines, OutlineTargets};

/// Render-style picker beneath the tumble cube. Mirrors MatterCAD's
/// `ViewStyleButton` / `RenderTypes` choices (minus the printer-only
/// `Hidden` / deprecated `Wireframe`): every mode draws the shaded
/// surface; the three edge modes overlay a wireframe on top, and
/// Overhang recolours the surface by face slope.
///
/// See [`render_modes`] for the pure CPU analysis behind the overlays
/// and [`RenderStyle::edge_kind`] / [`RenderStyle::is_overhang`] for
/// how the renderer dispatches each mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderStyle {
    /// Blinn-Phong shaded surface, no overlay.
    Shaded,
    /// Shaded surface + feature-edge wireframe (adjacent faces > 45°).
    Outlines,
    /// Shaded surface + non-manifold / boundary edges drawn red.
    NonManifold,
    /// Shaded surface + every geometric edge (full wireframe).
    Polygons,
    /// Surface recoloured cyan→red by world-space face slope (FDM
    /// overhang preview). No wireframe overlay.
    Overhang,
}

impl Default for RenderStyle {
    fn default() -> Self {
        // MatterCAD's verified default. The whole point of the render-
        // modes port was to land Outlines as the out-of-box view.
        Self::Outlines
    }
}

impl RenderStyle {
    /// The edge overlay this mode draws over the shaded surface, or
    /// `None` for Shaded / Overhang. Matches MatterCAD's
    /// `ShouldDrawWireframeOverlay`.
    pub fn edge_kind(self) -> Option<render_modes::EdgeKind> {
        match self {
            RenderStyle::Outlines => Some(render_modes::EdgeKind::Feature),
            RenderStyle::NonManifold => Some(render_modes::EdgeKind::NonManifold),
            RenderStyle::Polygons => Some(render_modes::EdgeKind::All),
            RenderStyle::Shaded | RenderStyle::Overhang => None,
        }
    }

    /// True when the surface's per-vertex colour is the overhang ramp
    /// rather than the body tint.
    pub fn is_overhang(self) -> bool {
        matches!(self, RenderStyle::Overhang)
    }

    /// RGBA colour for this mode's edge overlay. Feature / all edges
    /// use MatterCAD's `darkWireframe` (#3334 → dark grey); non-manifold
    /// edges are drawn red, matching the shader's edge-class-2 branch.
    pub fn edge_color(self) -> [f32; 4] {
        match self {
            RenderStyle::NonManifold => [1.0, 0.0, 0.0, 1.0],
            _ => [0.2, 0.2, 0.2, 1.0],
        }
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
    /// **De-indexed** position + normal vertex buffer (slot 0): three
    /// vertices per triangle, drawn non-indexed. De-indexing lets the
    /// surface shaders fold the wireframe into the polygon pass — each
    /// triangle owns its barycentric corners (derived from the vertex
    /// index) even where the source mesh shares vertices (e.g. tess2
    /// extrude caps). No separate index buffer.
    pub vbuf: wgpu::Buffer,
    /// De-indexed per-vertex RGBA colour buffer (slot 1), same corner
    /// order as `vbuf`. Always populated — per-vertex overlay or the
    /// body's uniform tint repeated.
    pub cbuf: wgpu::Buffer,
    /// De-indexed per-vertex edge-hint buffer (slot 2): `[hint.xyz]`,
    /// the triangle's three-edge classification replicated across its
    /// corners. Zero for Shaded / Overhang. The surface shaders gate
    /// the barycentric wireframe on it. Always allocated so the shared
    /// vertex layout binds a valid slot 2 on every per-body draw.
    pub hbuf: wgpu::Buffer,
    /// Vertex count for the non-indexed draws (`3 × tri_count`).
    pub vertex_count: u32,
    /// Render-mode fingerprint the mode-dependent `hbuf` was built for.
    /// Quaternary cache key: switching render style rolls this and
    /// rebuilds the hint buffer while leaving `vbuf` / `cbuf` reusable.
    /// See [`variant_key`].
    pub variant_key: u64,
    /// True when the body's resolved colour is fully opaque (alpha ≈ 1).
    /// Opaque bodies render through the shaded, depth-tested opaque pass
    /// ([`OpaquePipelines::draw_body`]); only translucent bodies go
    /// through the dual-peel chain. Forcing an opaque, self-overlapping
    /// mesh through the peel produced the black/white splotches this
    /// split fixes — MatterCAD likewise peels only transparent geometry.
    pub opaque: bool,
}

/// Best linear SSAA scale we will ever use: every offscreen scene
/// target is allocated at `scale × {on-screen w, h}` and box-downsampled
/// on the final composite. `3` → a 3×3 (9×) supersample, matching
/// agg-gui's [`SsaaFramebuffer::blit_downsample_3x_to`] kernel — all 9
/// source texels under each output pixel contribute equally.
///
/// The scale actually used is chosen per device by
/// [`choose_ssaa_scale`], because this used to be a hard constant and
/// that is what made the app unusable on phones — see that function.
pub const MAX_SSAA_SCALE: u32 = 3;

/// Ceiling on total offscreen VRAM for the scene targets. Past this the
/// supersample factor steps down.
///
/// Anti-aliasing is not worth an unbounded memory budget: at 3× a
/// 2560×1440 viewport wants ~2.4 GiB of offscreen targets, which is
/// absurd on any GPU and fatal on most. 768 MiB comfortably covers a
/// desktop viewport at 3× (a 1280×400 pane measures ~330 MiB) while
/// forcing high-DPI and large-viewport cases down a step.
const SSAA_MEMORY_BUDGET_BYTES: u64 = 768 * 1024 * 1024;

/// Per-supersampled-pixel cost of the scene's offscreen targets, in
/// bytes. Single source of truth for both the budget check in
/// [`choose_ssaa_scale`] and the diagnostic report in
/// [`report_offscreen_budget`], so the two can't drift.
///
/// `dual_depth_bpp` is 16 with 32-bit blendable float, 8 with the
/// half-float fallback (see [`depth_peel::dual_depth_format`]).
fn scene_bytes_per_pixel(dual_depth_bpp: u32) -> u32 {
    // framebuffer + scene_depth + scene_depth_color
    4 + 4 + 4
        // dual_depth ping-pong pair
        + dual_depth_bpp * 2
        // front + back peel accumulators
        + 8 * 2
        // HDR scene composite
        + 8
        // outline ID + blur targets
        + 4 * 2
}

/// Pick the supersample factor for this device.
///
/// The scene's offscreen targets are sized `scale × widget size`, and
/// the widget size is **already in device pixels**. A fixed 3× is fine
/// at device-pixel-ratio 1, but on a phone at ratio 3 it multiplies an
/// already-tripled resolution: a 9× area factor on top of a 9× pixel
/// count. That overruns `max_texture_dimension_2d` on tall screens and
/// exhausts GPU memory everywhere else, and because a failed texture
/// allocation just yields an empty frame, the visible symptom is a
/// black canvas rather than an error.
///
/// So we treat `MAX_SSAA_SCALE` as a *quality target expressed per CSS
/// pixel* and subtract what the display already provides, then step
/// down further until the result fits the device's texture limit and
/// the memory budget. A high-DPI screen needs little or no
/// supersampling — it already has the samples.
fn choose_ssaa_scale(
    device_scale: f64,
    screen_w: u32,
    screen_h: u32,
    max_texture_dim: u32,
    bytes_per_pixel: u32,
) -> u32 {
    let dpr = if device_scale.is_finite() && device_scale >= 1.0 {
        device_scale
    } else {
        1.0
    };
    let from_dpr = (MAX_SSAA_SCALE as f64 / dpr)
        .round()
        .clamp(1.0, MAX_SSAA_SCALE as f64) as u32;

    let fits = |scale: u32| -> bool {
        let w = screen_w.saturating_mul(scale);
        let h = screen_h.saturating_mul(scale);
        if w > max_texture_dim || h > max_texture_dim {
            return false;
        }
        let bytes = w as u64 * h as u64 * bytes_per_pixel as u64;
        bytes <= SSAA_MEMORY_BUDGET_BYTES
    };

    let mut scale = from_dpr;
    while scale > 1 && !fits(scale) {
        scale -= 1;
    }
    // Scale 1 may still not fit on an extreme display; nothing more we
    // can trade away here, and the diagnostic report will say so.
    scale
}

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
    /// Supersample factor in effect this frame, chosen per device by
    /// [`choose_ssaa_scale`] and refreshed in `ensure_framebuffer`.
    /// Read by the render impl to size the offscreen passes and to
    /// select the matching downsample kernel. `1` means "no
    /// supersampling" — the scene renders at native device resolution.
    ssaa_scale: u32,
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
    /// Forwarded to [`crate::bed::BedRenderer::set_colors`] each
    /// frame; cheap when unchanged.
    pub grid_line_color: [f32; 4],
    /// Translucent wash painted across the whole bed, under the grid
    /// lines — the alpha that makes the bed a surface rather than a
    /// set of floating lines. MatterCAD's `BedShadowTextureRenderer`
    /// uses `theme.BackgroundColor.WithAlpha(80)`; the viewport widget
    /// forwards the themed equivalent.
    pub grid_fill_color: [f32; 4],
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
            ssaa_scale: MAX_SSAA_SCALE,
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
            // 80/255 — MatterCAD's `BedFillAlpha`.
            grid_fill_color: [0.985, 0.985, 0.99, 80.0 / 255.0],
            grid_dark_mode: false,
            draw_grid: true,
            grid_z: 0.0,
            outline_enabled: false,
            outline_color: [1.0, 0.55, 0.10, 1.0],
            outline_width: 0.05,
            outline_body_index: None,
            render_style: RenderStyle::default(),
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
            self.grid_fill_color,
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
    /// `ssaa_scale × {w, h}` — the background framebuffer, the
    /// sample-able scene-depth texture, the dual-peel targets, the HDR
    /// scene composite (`scene_fb`), and the outline targets. `(w, h)`
    /// is the **on-screen** widget size in device pixels; this
    /// multiplies by the per-device supersample factor from
    /// [`choose_ssaa_scale`]. Cheap when the size is stable.
    ///
    /// The factor is (re)chosen here rather than once at startup so it
    /// tracks a window resize, a splitter drag, or a browser zoom that
    /// changes the device-pixel ratio.
    fn ensure_framebuffer(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        // Chosen before the `state` borrow — `choose_ssaa_scale` needs
        // the device limits, not renderer state.
        let dual_depth_bpp = match depth_peel::dual_depth_format(device) {
            wgpu::TextureFormat::Rgba32Float => 16,
            _ => 8,
        };
        let scale = choose_ssaa_scale(
            agg_gui::device_scale(),
            w.max(1),
            h.max(1),
            device.limits().max_texture_dimension_2d,
            scene_bytes_per_pixel(dual_depth_bpp),
        );
        self.ssaa_scale = scale;

        let s = match &mut self.state {
            Some(s) => s,
            None => return,
        };
        let format = s.surface_format;
        // Kept for the allocation report below — `w` / `h` are about to
        // be rewritten to their supersampled values.
        let (screen_w, screen_h) = (w, h);
        // Supersample dimensions — every scene target renders at this
        // size; the final composite box-downsamples it to `(w, h)`.
        let w = (w.max(1)) * scale;
        let h = (h.max(1)) * scale;
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

        report_offscreen_budget(device, w, h, screen_w, screen_h, scale);
    }

    /// Supersample factor in effect — see [`choose_ssaa_scale`].
    pub fn ssaa_scale(&self) -> u32 {
        self.ssaa_scale
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
}

/// Hand the offscreen target inventory to [`crate::diagnostics`] so a
/// resize prints (or, when over the device limit, warns about) the full
/// VRAM budget for the scene.
///
/// Every one of these is allocated at `SSAA_SCALE × widget size`, and
/// the widget size is already in **device** pixels. On a desktop at
/// DPR 1 that is a comfortable multiplier; on a phone at DPR 3 it is a
/// 9× area multiplier on top of a screen that already has 3× the
/// pixels, which is what makes this worth printing at all.
///
/// Byte counts are per-pixel for each target's format — see the
/// matching allocation sites in `ensure_framebuffer`.
fn report_offscreen_budget(
    device: &wgpu::Device,
    fb_w: u32,
    fb_h: u32,
    screen_w: u32,
    screen_h: u32,
    ssaa_scale: u32,
) {
    use crate::diagnostics::TargetDesc;

    // `dual_depth` is Rgba32Float when the device can blend 32-bit
    // float and Rgba16Float otherwise — the single biggest line item
    // either way, since it is a ping-pong pair.
    let dual_depth_bpp = match depth_peel::dual_depth_format(device) {
        wgpu::TextureFormat::Rgba32Float => 16,
        _ => 8,
    };
    let targets = [
        TargetDesc { label: "framebuffer", bytes_per_pixel: 4, count: 1 },
        TargetDesc { label: "scene_depth", bytes_per_pixel: 4, count: 1 },
        TargetDesc { label: "scene_depth_color", bytes_per_pixel: 4, count: 1 },
        TargetDesc { label: "dual_depth (ping-pong)", bytes_per_pixel: dual_depth_bpp, count: 2 },
        TargetDesc { label: "peel accum (f/b)", bytes_per_pixel: 8, count: 2 },
        TargetDesc { label: "scene_fb (HDR)", bytes_per_pixel: 8, count: 1 },
        TargetDesc { label: "outline targets", bytes_per_pixel: 4, count: 2 },
    ];
    crate::diagnostics::report_target_allocation(
        fb_w,
        fb_h,
        screen_w,
        screen_h,
        ssaa_scale,
        &device.limits(),
        &targets,
    );
}

// `ensure_body_buffers` — the per-body GPU buffer builder — lives in
// `body_buffers.rs` to keep this file under the 800-line guardrail.

/// A body counts as opaque when its resolved colour's alpha is ≈ 1.
/// The `INHERIT_COLOR` sentinel (alpha 0) resolves to the opaque
/// [`DEFAULT_GEOMETRY_COLOR`] — same substitution the uniform-slot path
/// makes — so an un-tinted body is opaque, not treated as fully
/// transparent. Opaque bodies bypass the dual-peel chain and render in
/// the shaded opaque pass instead.
fn is_opaque_color(color: [f32; 4]) -> bool {
    const OPAQUE_ALPHA_THRESHOLD: f32 = 0.999;
    let resolved = if is_inherit_color(&color) {
        DEFAULT_GEOMETRY_COLOR
    } else {
        color
    };
    resolved[3] >= OPAQUE_ALPHA_THRESHOLD
}

/// Fingerprint of the render mode for the body cache. Rolls whenever
/// the selected [`RenderStyle`] changes so the mode-dependent edge
/// overlay buffer rebuilds. Overhang is now computed live in the shader
/// from the world normal, so it no longer participates — the key is
/// purely the style discriminant.
fn variant_key(style: RenderStyle) -> u64 {
    // `+ 1` so the Shaded discriminant (0) can't collide with a
    // freshly-zeroed key.
    style as u64 + 1
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
