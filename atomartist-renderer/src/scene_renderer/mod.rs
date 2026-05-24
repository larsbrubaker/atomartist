//! wgpu scene renderer — implements `WgpuCustomRender` to draw the latest
//! mesh as a shaded 3D scene through agg-gui's custom-render hook.
//!
//! ## Offscreen-buffered viewport
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
//! ## Why single-sample
//!
//! Dual depth peeling samples the per-pixel scene-depth via shader
//! ([`crate::scene_renderer::depth_peel`]). MSAA stores a per-sample
//! depth value, which makes that lookup incoherent — a fragment shader
//! that asks "what is the opaque-pass depth at this pixel?" cannot
//! reliably answer when each sample slot in the MSAA target has a
//! different depth. Both reference implementations (MatterCAD's dual
//! depth peeling and NodeDesigner's single-direction peeling) keep their
//! offscreen 3-D targets at `sample_count = 1` for the same reason.
//! Anti-aliasing for the viewport instead comes from the 16-tap Halton
//! jitter accumulator in [`crate::scene_renderer::accumulation`] — only
//! the main viewport gets jittered; the tumble cube + bed render
//! single-shot.
//!
//! The shader stack is single Blinn-Phong-ish: vertex carries position +
//! normal; fragment shades against a fixed key + fill light plus ambient.

use std::sync::Arc;

use bytemuck::cast_slice;
use demo_wgpu::{MsaaFramebuffer, WgpuCustomRender, WgpuCustomRenderCtx};
use manifold_rust::types::MeshGL;
use wgpu::util::DeviceExt;

use glam::Mat4;

use crate::bed::BedRenderer;
use crate::camera::OrbitCamera;

pub mod accumulation;
pub mod cache;
pub mod depth_peel;
pub mod gizmo_pass;
pub mod opaque_pass;
mod opaque_shaders;
pub mod post_outline;
mod timings;
mod util;

use timings::{elapsed_ms, log_scene_timings, SceneTimings};
use util::{ensure_scene_depth, ensure_scene_depth_color, normalize3};

use accumulation::{
    apply_jitter_to_proj, jitter_offset, AccumulationPipelines, AccumulationTargets, MAX_SAMPLES,
    SAMPLE_FORMAT,
};
use cache::{handle_cache_hit, CacheOutcome, SceneFingerprint};
use depth_peel::pipelines::{DualPeelPipelines, MeshHandles, PeelUniforms};
use depth_peel::{iteration_count, DualPeelTargets, DEFAULT_LAYERS};
use gizmo_pass::{GizmoLinePipelines, GizmoLineUniforms};
pub use gizmo_pass::GizmoLineSet;
use opaque_pass::{OpaquePipelines, Uniforms, Vertex};
use post_outline::{OutlinePipelines, OutlineTargets, OutlineUniforms};

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

/// Sample count for the offscreen 3-D framebuffer.
///
/// Must stay at 1 — see the "Why single-sample" note at the top of this
/// module. Dual depth peeling cannot tolerate MSAA's per-sample depth
/// values.
pub const SAMPLE_COUNT: u32 = 1;

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

    /// Progressive-accumulation pipelines — blend (sample into accum)
    /// and copy (accum → surface). Built once during `ensure_state`.
    accum_pipes: AccumulationPipelines,

    /// Bed renderer — owns the baked grid texture and the contact-shadow
    /// chain. See [`crate::bed`] for the off-screen silhouette → blur →
    /// composite pipeline that runs each frame before the main pass.
    bed: BedRenderer,

    /// Cached vertex/index buffers and the source mesh pointer they were
    /// built from. The pointer doubles as the cache key.
    mesh_ptr: usize,
    vbuf: Option<wgpu::Buffer>,
    ibuf: Option<wgpu::Buffer>,
    index_count: u32,

    /// Offscreen framebuffer (color only) for the opaque pass — bed,
    /// mesh depth-only, and outline render into this. The resolve pass
    /// samples this texture as `scene_color`. We allocate the depth
    /// attachment separately so it can be made `TEXTURE_BINDING`
    /// sample-able by the dual-peel shaders.
    framebuffer: Option<MsaaFramebuffer>,

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

    /// Progressive-accumulation textures: per-sample resolve target +
    /// 2-slot ping-pong HDR accumulator. The dual-peel resolve writes
    /// to `accum_targets.sample_view`, the blend pass folds it into
    /// the accumulator, and the copy pass downsamples the accumulator
    /// into `output_fb` for the final surface blit.
    accum_targets: Option<AccumulationTargets>,

    /// Final composited output — the accumulation copy pass writes
    /// here. Held as an `MsaaFramebuffer` (with `sample_count = 1`,
    /// no depth) so the existing `MsaaFramebuffer::blit_to` path
    /// keeps working for the final surface composite.
    output_fb: Option<MsaaFramebuffer>,

    /// Pipelines + uniforms for the Blender-style post-process
    /// selection outline. Built once during `ensure_state`; runs
    /// against `output_fb` after the accumulation copy. See
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
    pub mesh: Option<Arc<MeshGL>>,
    pub viewport_size: (u32, u32),
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

    /// Progressive-AA sample index. Bumped each frame that the chain
    /// runs and clamped at [`MAX_SAMPLES`]; reset to 0 on a scene
    /// fingerprint mismatch (see [`crate::scene_renderer::cache`]).
    sample_count: u32,

    /// Which accumulator slot holds the latest blended result. The
    /// blend pass writes into `1 - accum_read`, then we swap.
    accum_read: u8,

    /// Last accepted scene fingerprint. `None` on the very first
    /// frame, then `Some(prev)` while the cache is being maintained.
    /// See [`cache::SceneFingerprint`] for the field-by-field
    /// composition and why each input is included.
    last_fingerprint: Option<SceneFingerprint>,
}

impl WgpuSceneRenderer {
    pub fn new() -> Self {
        Self {
            state: None,
            camera: OrbitCamera::default(),
            mesh: None,
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
            render_style: RenderStyle::Shaded,
            gizmo_lines: Vec::new(),
            sample_count: 0,
            accum_read: 0,
            last_fingerprint: None,
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

        let opaque = OpaquePipelines::new(device, surface_format, SAMPLE_COUNT);
        // The dual-peel resolve writes into the HDR per-sample target
        // owned by the accumulation chain — NOT the surface — so its
        // colour attachment must use `SAMPLE_FORMAT` (`Rgba16Float`).
        // Mismatching the pipeline format vs the bound attachment
        // panics at draw time inside wgpu's validation layer.
        let dual_peel = DualPeelPipelines::new(device, SAMPLE_FORMAT);
        let accum_pipes = AccumulationPipelines::new(device, surface_format);

        let mut bed = BedRenderer::new(
            device,
            queue,
            surface_format,
            SAMPLE_COUNT,
            self.grid_line_color,
        );
        bed.set_dark_mode(self.grid_dark_mode);

        // Post-process outline writes into the per-sample HDR target
        // (`accum_targets.sample_view`) so it lives inside the
        // jittered sample stream and gets averaged across 16 Halton
        // offsets. That target's format is `SAMPLE_FORMAT`
        // (Rgba16Float), not the surface format.
        let post_outline = OutlinePipelines::new(device, SAMPLE_FORMAT);

        // Gizmo line pipelines target the same per-sample HDR view
        // (so gizmos AA-smooth with the rest of the scene) and depth-
        // test the solid variant against `scene_depth` (the opaque
        // pass's depth attachment).
        let gizmo_pipelines = GizmoLinePipelines::new(
            device,
            SAMPLE_FORMAT,
            wgpu::TextureFormat::Depth32Float,
        );

        self.state = Some(GpuState {
            surface_format,
            opaque,
            dual_peel,
            accum_pipes,
            bed,
            mesh_ptr: 0,
            vbuf: None,
            ibuf: None,
            index_count: 0,
            framebuffer: None,
            scene_depth: None,
            scene_depth_color: None,
            peel_targets: None,
            accum_targets: None,
            output_fb: None,
            post_outline,
            outline_targets: None,
            gizmo_pipelines,
        });
    }

    /// Lazily allocate (or resize) the offscreen framebuffer, the
    /// sample-able scene-depth texture, the dual-peel targets, and the
    /// final output framebuffer. Cheap when the size is stable.
    fn ensure_framebuffer(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let s = match &mut self.state {
            Some(s) => s,
            None => return,
        };
        let format = s.surface_format;
        let w = w.max(1);
        let h = h.max(1);
        match &mut s.framebuffer {
            Some(fb) => fb.ensure_size(device, w, h),
            None => {
                s.framebuffer = Some(MsaaFramebuffer::new(
                    device,
                    w,
                    h,
                    SAMPLE_COUNT,
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
        match &mut s.accum_targets {
            Some(t) => t.ensure_size(device, w, h),
            None => s.accum_targets = Some(AccumulationTargets::new(device, w, h)),
        }
        match &mut s.output_fb {
            Some(fb) => fb.ensure_size(device, w, h),
            None => {
                s.output_fb = Some(MsaaFramebuffer::new(
                    device,
                    w,
                    h,
                    SAMPLE_COUNT,
                    format,
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

    /// Re-upload mesh buffers if the mesh changed since the last frame.
    fn ensure_mesh_buffers(&mut self, device: &wgpu::Device) {
        let mesh = match &self.mesh {
            Some(m) => m.clone(),
            None => return,
        };
        let s = match &mut self.state {
            Some(s) => s,
            None => return,
        };
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
        s.vbuf = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist scene vb"),
            contents: cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        }));
        s.ibuf = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("atomartist scene ib"),
            contents: cast_slice(&mesh.tri_verts),
            usage: wgpu::BufferUsages::INDEX,
        }));
        s.index_count = mesh.tri_verts.len() as u32;
        s.mesh_ptr = ptr;
    }
}

impl Default for WgpuSceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl WgpuCustomRender for WgpuSceneRenderer {
    fn render(&mut self, ctx: WgpuCustomRenderCtx<'_>) {
        let t_total = web_time::Instant::now();
        let t_ensure = web_time::Instant::now();
        self.ensure_state(ctx.device, ctx.queue, ctx.surface_format);
        let ensure_ms = elapsed_ms(t_ensure);

        // Pixel size of the viewport widget rect.  The framebuffer matches
        // this exactly (1:1 mapping), so blit_to runs an effectively no-op
        // bilinear sampler.
        let fb_w = ctx.screen_rect.width.max(1.0) as u32;
        let fb_h = ctx.screen_rect.height.max(1.0) as u32;
        if fb_w == 0 || fb_h == 0 {
            return;
        }

        let t_fb = web_time::Instant::now();
        self.ensure_framebuffer(ctx.device, fb_w, fb_h);
        let fb_ms = elapsed_ms(t_fb);
        let t_mesh = web_time::Instant::now();
        self.ensure_mesh_buffers(ctx.device);
        let mesh_ms = elapsed_ms(t_mesh);

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

        // ── Fingerprint / cache hit check ────────────────────────────────
        // Compute the per-frame scene fingerprint and update
        // `sample_count`. On a miss we restart accumulation; on a hit
        // we either run one more refinement pass or skip the entire
        // chain when already converged.
        //
        // The fingerprint reflects `viewport_size` (set by
        // `Viewport3dWidget` before calling paint) rather than the
        // per-frame `ctx.screen_rect`, but those should agree because
        // the widget mirrors its rect into the renderer pre-paint.
        let current_fp = SceneFingerprint::from_renderer(self);
        let outcome = handle_cache_hit(
            &mut self.last_fingerprint,
            current_fp,
            &mut self.sample_count,
        );
        if matches!(outcome, CacheOutcome::Miss) {
            // Restart the accumulator from sample 0 — the blend pass
            // will pick `weight = 1` so the read slot's stale value
            // is ignored. `accum_read` doesn't need to change; we
            // just swap slots on each sample.
        }

        // Bind GPU state by reference. The accumulation chain needs
        // `self.sample_count` (mutable) but only borrows `&self.state`
        // immutably; we read `sample_count` out *before* taking the
        // state borrow so the update at the end of the frame doesn't
        // alias.
        let sample_count_before = self.sample_count;
        let accum_read_before = self.accum_read;

        let s = match &self.state {
            Some(s) => s,
            None => return,
        };
        let fb = match &s.framebuffer {
            Some(fb) => fb,
            None => return,
        };
        let scene_depth_view = match &s.scene_depth {
            Some((_, v)) => v,
            None => return,
        };
        let scene_depth_color_view = match &s.scene_depth_color {
            Some((_, v)) => v,
            None => return,
        };
        let peel_targets = match &s.peel_targets {
            Some(t) => t,
            None => return,
        };
        let accum_targets = match &s.accum_targets {
            Some(t) => t,
            None => return,
        };
        let output_fb = match &s.output_fb {
            Some(fb) => fb,
            None => return,
        };

        // Build uniforms — projection uses the widget's aspect ratio (the
        // framebuffer matches that aspect 1:1).
        let aspect = fb_w as f32 / fb_h.max(1) as f32;
        let view = Mat4::from_cols_array(&self.camera.view_matrix());
        let proj = Mat4::from_cols_array(&self.camera.projection_matrix(aspect));

        // ── Cache short-circuit ──────────────────────────────────────────
        // When the accumulator already holds the converged image we skip
        // every per-frame pass and just re-blit. The fingerprint check
        // above ensures `sample_count` is reset on any scene change, so
        // a converged frame can only be reached after `MAX_SAMPLES`
        // identical-fingerprint frames.
        let already_converged = sample_count_before >= MAX_SAMPLES;
        if already_converged {
            output_fb.blit_to(
                ctx.device,
                ctx.encoder,
                ctx.target_view,
                ctx.target_size,
                ctx.screen_rect,
                ctx.parent_clip,
                ctx.pipelines,
            );
            return;
        }

        // Jitter the projection by a sub-pixel Halton(2,3) offset so
        // 16 successive frames produce a 16x supersampled average when
        // the scene is static. Sample 0 returns `(0, 0)` so the first
        // frame after a scene change shows the un-jittered image
        // immediately.
        let (jx, jy) = jitter_offset(sample_count_before);
        let mut proj_arr = proj.to_cols_array();
        apply_jitter_to_proj(&mut proj_arr, jx, jy, fb_w as f32, fb_h as f32);
        let jittered_proj = Mat4::from_cols_array(&proj_arr);
        let mvp = (jittered_proj * view).to_cols_array();
        let jittered_proj_arr = jittered_proj.to_cols_array();
        let view_arr = view.to_cols_array();
        let l0 = normalize3(self.light_dir);
        let l1 = normalize3(self.light_dir1);
        let to_vec4 = |v: [f32; 3]| [v[0], v[1], v[2], 0.0];
        // Shading uniforms — shared between the opaque scene pipeline
        // and the dual-peel colour pipeline so a peeled fragment shades
        // identically to how it would have shaded through the opaque
        // pass. Layout mirrors NodeDesigner's `createDepthPeelMaterial`
        // uniform set (two view-space directional lights, configurable
        // shininess, sRGB-encoded base colour).
        let uniforms = Uniforms {
            proj: jittered_proj_arr,
            view: view_arr,
            light_dir0: to_vec4(l0),
            light_dir1: to_vec4(l1),
            light_diffuse0: to_vec4(self.light_diffuse0),
            light_specular0: to_vec4(self.light_specular0),
            light_ambient0: to_vec4(self.light_ambient0),
            light_diffuse1: to_vec4(self.light_diffuse1),
            light_specular1: to_vec4(self.light_specular1),
            global_ambient: to_vec4(self.global_ambient),
            material_specular: to_vec4(self.material_specular),
            base_color: self.base_color,
            params: [self.shininess, 0.0, 0.0, 0.0],
            resolution: [fb_w as f32, fb_h as f32, 0.0, 0.0],
        };

        s.opaque.write_scene_uniforms(ctx.queue, &uniforms);

        // ── Pass 0: refresh the bed composite (grid + contact shadow) ──────
        // Runs in its own set of off-screen passes against `ctx.encoder`
        // BEFORE we open the main framebuffer pass, so the bed quad in
        // the main pass can sample the freshly-blitted composite
        // texture. Skipped when the bed is hidden — no shadow update
        // needed if the bed isn't being drawn.
        let t_bed_composite = web_time::Instant::now();
        let mut bed_ran_chain = false;
        if self.draw_grid {
            let mesh_ref = match (&s.vbuf, &s.ibuf) {
                (Some(vbuf), Some(ibuf)) if s.index_count > 0 => Some(crate::bed::MeshRef {
                    vbuf,
                    ibuf,
                    index_count: s.index_count,
                }),
                _ => None,
            };
            bed_ran_chain = s.bed.render_to_composite(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                mesh_ref,
                s.mesh_ptr as u64,
                // Locked to 0 alongside `bed_render_z` while the
                // bed-Z offset is reworked. Keeps the shadow caster's
                // ortho aimed at world z=0 so the silhouette stays put.
                0.0,
            );
        }
        let bed_composite_ms = elapsed_ms(t_bed_composite);

        // ── Pass 1: opaque scene — bed + mesh-depth-only + outline ─────────
        // Bed draws colour + depth, the mesh writes *only* depth (so
        // the dual-peel chain can rim-test against it without the
        // mesh's colour landing in scene_color). Mesh colour comes
        // from the dual-peel chain below. The scene-depth attachment
        // is `STORE`d for the peel chain to sample.
        let draw_surface = self.render_style == RenderStyle::Shaded;
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("atomartist scene opaque"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: fb.render_view(),
                        resolve_target: fb.resolve_target(),
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    // R32Float mirror of the opaque depth — the
                    // dual-peel chain samples this. Clear to 1.0 (far
                    // plane) so any pixel the opaque pass leaves
                    // untouched reads as "no opaque geometry here"
                    // and lets every transparent fragment through.
                    Some(wgpu::RenderPassColorAttachment {
                        view: scene_depth_color_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 1.0,
                                g: 0.0,
                                b: 0.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: scene_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_viewport(0.0, 0.0, fb_w as f32, fb_h as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, fb_w, fb_h);

            // Bed first — depth-write on so subsequent passes (including
            // the peeled mesh) compete against the bed's depth.
            if self.draw_grid {
                let bed_z = self.bed_render_z();
                s.bed.draw_bed(ctx.queue, &mut pass, mvp, bed_z);
            }

            // `RenderStyle::Shaded` populates the mesh's depth so the
            // dual-peel chain has something to discard against.
            // `Wireframe` skips this — the host widget draws the
            // wireframe at the 2-D layer instead.
            if let (Some(vbuf), Some(ibuf)) = (&s.vbuf, &s.ibuf) {
                if s.index_count > 0 && draw_surface {
                    s.opaque
                        .draw_depth_only(&mut pass, vbuf, ibuf, s.index_count);
                }
            }
        }

        // ── Pass 2: dual depth-peeling chain ──────────────────────────────
        // Routes the user mesh's *colour* through the peel chain so any
        // future translucent material renders order-independent. Opaque
        // meshes peel to a single front layer (visually identical to a
        // standard depth-tested render) so this is safe to run
        // unconditionally. `Wireframe` mode skips peeling because the
        // surface is drawn by the host widget at the 2-D layer instead.
        let t_peel = web_time::Instant::now();
        let mesh_handles = if draw_surface {
            match (&s.vbuf, &s.ibuf) {
                (Some(vbuf), Some(ibuf)) if s.index_count > 0 => Some(MeshHandles {
                    vbuf,
                    ibuf,
                    index_count: s.index_count,
                }),
                _ => None,
            }
        } else {
            None
        };
        // PeelUniforms is a type alias for the shared shading uniform
        // struct — reuse the value computed above so the peel chain
        // sees the exact same light setup as the opaque pass.
        let peel_uniforms: PeelUniforms = uniforms;
        let iterations = iteration_count(DEFAULT_LAYERS as i32);
        s.dual_peel.execute_chain(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            peel_targets,
            scene_depth_color_view,
            // For SAMPLE_COUNT = 1 the render_view *is* the resolve
            // view, so the resolve shader's `scene_color` sample reads
            // the same texture the opaque pass wrote to.
            fb.render_view(),
            // Resolve writes to the HDR per-sample target so the
            // accumulation chain can fold each jittered sample into
            // the running average at full precision.
            &accum_targets.sample_view,
            mesh_handles,
            &peel_uniforms,
            iterations,
        );
        let peel_ms = elapsed_ms(t_peel);

        // ── Pass 2.5: post-process selection outline ─────────────────────
        // Blender-style edge-detect outline rendered into the per-sample
        // HDR target (`accum_targets.sample_view`) AFTER the dual-peel
        // resolve but BEFORE the accumulation blend folds the sample
        // into the running average. Running pre-jitter means the
        // outline gets averaged across 16 jittered Halton offsets, so
        // the rim anti-aliases the same way the rest of the scene
        // does instead of staying a crunchy 1-pixel boundary.
        //
        // The ID prepass uses the same jittered MVP as the opaque
        // pass, so each sample writes the outline at the matching
        // sub-pixel offset.
        let want_outline = self.outline_enabled
            && self.render_style == RenderStyle::Shaded
            && s.index_count > 0;
        if want_outline {
            if let (Some(vbuf), Some(ibuf), Some(outline_targets)) =
                (&s.vbuf, &s.ibuf, &s.outline_targets)
            {
                let outline_u = OutlineUniforms {
                    mvp,
                    outline_color: self.outline_color,
                    resolution: [fb_w as f32, fb_h as f32, 0.0, 0.0],
                    // x = outline width in texels (NodeDesigner default
                    // 2.0). y = occluded-alpha (NodeDesigner default
                    // 0.35).
                    params: [self.outline_width.max(1.0), 0.35, 0.0, 0.0],
                };
                s.post_outline.write_uniforms(ctx.queue, &outline_u);

                // ID prepass — write selected mesh into id_mask +
                // selected_depth. Clear id_mask to 0 (=unselected) and
                // selected_depth to 1.0 (= far plane, so the edge
                // shader's `min` over neighbours picks real depth
                // only where the prepass actually wrote).
                {
                    let mut pass =
                        ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("atomartist outline id prepass"),
                            color_attachments: &[
                                Some(wgpu::RenderPassColorAttachment {
                                    view: &outline_targets.id_mask_view,
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(
                                            wgpu::Color::TRANSPARENT,
                                        ),
                                        store: wgpu::StoreOp::Store,
                                    },
                                }),
                                Some(wgpu::RenderPassColorAttachment {
                                    view: &outline_targets.selected_depth_view,
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 1.0,
                                            g: 0.0,
                                            b: 0.0,
                                            a: 1.0,
                                        }),
                                        store: wgpu::StoreOp::Store,
                                    },
                                }),
                            ],
                            depth_stencil_attachment: Some(
                                wgpu::RenderPassDepthStencilAttachment {
                                    view: &outline_targets.id_depth_view,
                                    depth_ops: Some(wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(1.0),
                                        store: wgpu::StoreOp::Store,
                                    }),
                                    stencil_ops: None,
                                },
                            ),
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                    pass.set_viewport(0.0, 0.0, fb_w as f32, fb_h as f32, 0.0, 1.0);
                    pass.set_scissor_rect(0, 0, fb_w, fb_h);
                    pass.set_pipeline(&s.post_outline.id_pipeline);
                    pass.set_bind_group(0, &s.post_outline.id_bg, &[]);
                    pass.set_vertex_buffer(0, vbuf.slice(..));
                    pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..s.index_count, 0, 0..1);
                }

                // Edge-detect pass — composite outline into the HDR
                // sample target so the next blend pass folds the
                // outline + scene together. `LoadOp::Load` preserves
                // the dual-peel resolve's content; the outline
                // alpha-blends on top through the pipeline's OVER
                // blend state.
                let edge_bg = s.post_outline.build_edge_bind_group(
                    ctx.device,
                    &outline_targets.id_mask_view,
                    &outline_targets.selected_depth_view,
                    scene_depth_color_view,
                );
                {
                    let mut pass =
                        ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("atomartist outline edge detect"),
                            color_attachments: &[Some(
                                wgpu::RenderPassColorAttachment {
                                    view: &accum_targets.sample_view,
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Load,
                                        store: wgpu::StoreOp::Store,
                                    },
                                },
                            )],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                    pass.set_viewport(0.0, 0.0, fb_w as f32, fb_h as f32, 0.0, 1.0);
                    pass.set_scissor_rect(0, 0, fb_w, fb_h);
                    pass.set_pipeline(&s.post_outline.edge_pipeline);
                    pass.set_bind_group(0, &edge_bg, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }

        // ── Pass 2.6: gizmos ─────────────────────────────────────────────
        // Each entry in `self.gizmo_lines` becomes up to two draws —
        // a depth-tested solid variant against `scene_depth` and an
        // optional no-depth overlay variant for the occluded portion.
        // Per-frame buffer allocations are cheap because gizmos are
        // small (12 segments for the bounds box; control gizmos are
        // similar). All draws target `accum_targets.sample_view` so
        // they get folded into the Halton-jittered AA average.
        for gizmo in &self.gizmo_lines {
            if gizmo.vertices.is_empty()
                || (!gizmo.draw_solid && !gizmo.draw_overlay)
            {
                continue;
            }
            let model = gizmo
                .matrix
                .as_ref()
                .map(Mat4::from_cols_array)
                .unwrap_or(Mat4::IDENTITY);
            let gmvp = (jittered_proj * view * model).to_cols_array();
            let vbuf = ctx
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("atomartist gizmo line vb"),
                    contents: cast_slice(&gizmo.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let vertex_count = gizmo.vertices.len() as u32;

            if gizmo.draw_solid {
                let u = GizmoLineUniforms {
                    mvp: gmvp,
                    color: gizmo.color,
                };
                let ub = ctx.device.create_buffer_init(
                    &wgpu::util::BufferInitDescriptor {
                        label: Some("atomartist gizmo line solid ub"),
                        contents: bytemuck::bytes_of(&u),
                        usage: wgpu::BufferUsages::UNIFORM,
                    },
                );
                let bg = s.gizmo_pipelines.build_bind_group(ctx.device, &ub);
                let mut pass =
                    ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("atomartist gizmo solid"),
                        color_attachments: &[Some(
                            wgpu::RenderPassColorAttachment {
                                view: &accum_targets.sample_view,
                                resolve_target: None,
                                depth_slice: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                },
                            },
                        )],
                        depth_stencil_attachment: Some(
                            wgpu::RenderPassDepthStencilAttachment {
                                view: scene_depth_view,
                                depth_ops: Some(wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                }),
                                stencil_ops: None,
                            },
                        ),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                pass.set_viewport(0.0, 0.0, fb_w as f32, fb_h as f32, 0.0, 1.0);
                pass.set_scissor_rect(0, 0, fb_w, fb_h);
                pass.set_pipeline(&s.gizmo_pipelines.solid_pipeline);
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
                let u = GizmoLineUniforms {
                    mvp: gmvp,
                    color: overlay_color,
                };
                let ub = ctx.device.create_buffer_init(
                    &wgpu::util::BufferInitDescriptor {
                        label: Some("atomartist gizmo line overlay ub"),
                        contents: bytemuck::bytes_of(&u),
                        usage: wgpu::BufferUsages::UNIFORM,
                    },
                );
                let bg = s.gizmo_pipelines.build_bind_group(ctx.device, &ub);
                let mut pass =
                    ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("atomartist gizmo overlay"),
                        color_attachments: &[Some(
                            wgpu::RenderPassColorAttachment {
                                view: &accum_targets.sample_view,
                                resolve_target: None,
                                depth_slice: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                },
                            },
                        )],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                pass.set_viewport(0.0, 0.0, fb_w as f32, fb_h as f32, 0.0, 1.0);
                pass.set_scissor_rect(0, 0, fb_w, fb_h);
                pass.set_pipeline(&s.gizmo_pipelines.overlay_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.draw(0..vertex_count, 0..1);
            }
        }

        // ── Pass 3: progressive accumulation ──────────────────────────────
        let t_accum = web_time::Instant::now();
        let new_read = s.accum_pipes.execute_blend(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            accum_targets,
            sample_count_before,
            accum_read_before,
        );
        s.accum_pipes.execute_copy_to_surface(
            ctx.device,
            ctx.encoder,
            accum_targets,
            new_read,
            output_fb.render_view(),
            (fb_w, fb_h),
        );
        let accum_ms = elapsed_ms(t_accum);

        let t_blit = web_time::Instant::now();
        // ── Pass 4: composite resolved scene onto the active 2-D target ────
        // Same alpha-blended blit used pre-peel; the only change is the
        // source framebuffer (`output_fb` now holds the accumulated
        // average instead of the raw opaque pass).
        output_fb.blit_to(
            ctx.device,
            ctx.encoder,
            ctx.target_view,
            ctx.target_size,
            ctx.screen_rect,
            ctx.parent_clip,
            ctx.pipelines,
        );
        let blit_ms = elapsed_ms(t_blit);

        // Advance the AA state. Once we cross MAX_SAMPLES the cache
        // short-circuit at the top of `render` takes over and skips
        // GPU work until the scene fingerprint invalidates (next
        // step). Request another draw while we still have samples to
        // collect — the agg-gui animation loop will redraw on the
        // next vsync, picking up the next Halton offset.
        self.sample_count = sample_count_before + 1;
        self.accum_read = new_read;
        if self.sample_count < MAX_SAMPLES {
            // IMPORTANT: must NOT call `request_draw()` here — that
            // version advances the global invalidation epoch, which
            // forces EVERY retained widget cache (the entire 2-D UI
            // — node editor, panels, menus, etc.) to rebuild every
            // single frame for the duration of the accumulation. The
            // node-editor specifically loses drag visibility and
            // numeric-field edits during that storm, because each
            // bump dirties parent backbuffers mid-event-dispatch and
            // overwrites pending visual state.
            //
            // Our visual change is confined to this widget's own
            // direct-to-surface composite (`output_fb.blit_to`) —
            // there's no retained bitmap of the 3-D output anywhere
            // upstream that needs invalidating. So
            // `request_draw_without_invalidation` is the precise
            // tool: it schedules a frame without touching the epoch.
            // The 3-D render runs again, the next Halton sample
            // folds in, the surface gets re-blit, and 2-D widgets
            // composite their (still-valid) cached bitmaps.
            agg_gui::animation::request_draw_without_invalidation();
        }
        let total_ms = elapsed_ms(t_total);
        log_scene_timings(SceneTimings {
            total_ms,
            ensure_ms,
            fb_ms,
            mesh_ms,
            bed_composite_ms,
            bed_ran_chain,
            peel_ms,
            accum_ms,
            blit_ms,
        });
    }
}

#[cfg(test)]
mod tests;
