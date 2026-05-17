//! `Viewport3dWidget` — renders the latest `MeshGL` from `AppState` as a
//! shaded wireframe inside a custom `Widget`. Software-only path: projects
//! triangles to screen space and strokes their edges with normal-modulated
//! colors via the existing 2D `DrawCtx`.
//!
//! A future Phase will replace this with a wgpu render pass that fills
//! triangles, once agg-gui exposes a generic custom-render hook. The
//! wireframe approach is sufficient for the first MVP and works on every
//! platform agg-gui already runs on.
//!
//! Camera controls (matches MatterCAD's documented viewport navigation —
//! `MatterCAD/MatterCAD_Docs/docs/Help/getting-started/viewport-navigation.md`):
//!
//! | Action       | Primary           | Modifier alternative              |
//! |--------------|-------------------|-----------------------------------|
//! | Selection    | Left-click / drag | —                                 |
//! | Orbit        | Right-drag        | Ctrl + Left-drag                  |
//! | Pan          | Middle-drag       | Ctrl + Shift + Left-drag          |
//! | Zoom         | Scroll wheel      | Ctrl + Alt + Left-drag (vertical) |
//!
//! Keyboard:
//!   - `W` — fit-all (canonical MatterCAD shortcut). `F` is kept as a legacy alias.
//!   - `Z` — zoom-to-selected (falls back to fit-all when nothing is selected).
//!   - Arrow keys — small-step orbit; **Shift + Arrow keys** small-step pan.
//!   - Ctrl + `+` / Ctrl + `-` — zoom in / out.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use agg_gui::{
    Color, DrawCtx, Event, EventResult, HAnchor, Key, Modifiers, MouseButton, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};
use atomartist_lib::graph::node::NodeId;
use manifold_rust::types::MeshGL;

use crate::camera::{mul4, transform_point4, CameraPoseAnimation, OrbitCamera};
use crate::picking::{project_to_view_plane, raycast_mesh};
use crate::scene_renderer::{RenderStyle, WgpuSceneRenderer};

/// Default left-mouse-drag behaviour, picked by the radio cluster of
/// buttons around the tumble cube.  Mirrors MatterCAD's
/// `ViewControls3DButtons` enum minus the printer-specific entries.
///
/// `Select` is the historical AtomArtist behaviour: plain left-drag
/// becomes a click-or-drag selection.  The other variants change what
/// plain left-drag does — useful on trackpads without a right or middle
/// mouse button, exactly the case MatterCAD targets these buttons at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewportTool {
    Select,
    Rotate,
    Pan,
    Zoom,
}

impl Default for ViewportTool {
    fn default() -> Self {
        Self::Select
    }
}

/// External hooks the widget needs from the app: where to read the latest
/// mesh from, the live `display_node` so a left-click selection mirrors
/// into the canvas, and a writable selection slot.
pub struct ViewportInputs {
    pub last_mesh_output: Arc<Mutex<Option<Arc<MeshGL>>>>,
    /// The display node whose mesh is currently rendered. Read-only from
    /// the viewport's perspective — used to know which node id to write
    /// into `selection` when the user left-clicks the displayed mesh.
    pub display_node: Arc<Mutex<Option<NodeId>>>,
    /// The currently-selected node id (mirrored to / from the canvas).
    /// The viewport writes here when the user left-clicks a hit; the
    /// canvas writes here when the user clicks a node. Both paint sides
    /// read it to render highlights / outlines.
    pub selection: Arc<Mutex<Option<NodeId>>>,
    /// Shared orbit camera — held in an `Arc<Mutex<>>` so the tumble
    /// cube widget can read the current orientation each paint and
    /// write back animated orientations on click-to-orient.
    pub camera: Arc<Mutex<OrbitCamera>>,
    /// Active mouse-button-1 tool (Select / Rotate / Pan / Zoom).
    pub tool: Arc<Mutex<ViewportTool>>,
    /// Render style picker beneath the tumble cube.
    pub render_style: Arc<Mutex<RenderStyle>>,
    /// Bed-toggle state.  Mirrored into
    /// `WgpuSceneRenderer::draw_grid` each paint so flipping the
    /// button hides / shows the floor grid on the next frame.
    pub show_bed: Arc<Mutex<bool>>,
    /// Optional camera pose tween started by external HUD controls
    /// (Home / Fit).  The viewport owns ticking it during paint.
    pub camera_animation: Arc<Mutex<Option<CameraPoseAnimation>>>,
}

impl ViewportInputs {
    /// Build a default-populated input bundle with empty `Arc<Mutex<>>`s
    /// for every slot — used by tests and the unit-of-work paint code
    /// to avoid replicating every default in every call site.
    pub fn empty() -> Self {
        Self {
            last_mesh_output: Arc::new(Mutex::new(None)),
            display_node: Arc::new(Mutex::new(None)),
            selection: Arc::new(Mutex::new(None)),
            camera: Arc::new(Mutex::new(OrbitCamera::default())),
            tool: Arc::new(Mutex::new(ViewportTool::default())),
            render_style: Arc::new(Mutex::new(RenderStyle::default())),
            show_bed: Arc::new(Mutex::new(true)),
            camera_animation: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Clone, Debug)]
enum CameraDrag {
    None,
    Orbit { start_local: Point, start_az: f32, start_el: f32 },
    Pan { start_local: Point, start_center: [f32; 3] },
    /// Left-button down — pending selection.  Becomes a click-or-drag
    /// selection on mouse-up (Phase A4 wires the selection write).
    Selecting { start_local: Point, moved: bool },
    /// Ctrl + Alt + Left-drag — zoom by vertical drag distance (matches
    /// MatterCAD's modifier-only zoom path).
    Zooming { start_local: Point, start_radius: f32 },
}

pub struct Viewport3dWidget {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    inputs: ViewportInputs,
    drag: CameraDrag,
    /// Track the most recent mesh seen; if a new one comes in, auto-fit
    /// the camera once.
    last_mesh_ptr: usize,
    /// AABB of the last auto-fit mesh — cached so `zoom_to_selection_bounds`
    /// can re-fit on demand without retraversing the mesh.
    last_aabb: Option<([f32; 3], [f32; 3])>,
    bg_color: Color,
    /// wgpu-backed scene renderer (lazy-initialized GPU state). When the
    /// active `DrawCtx` is a `WgpuGfxCtx`, the widget pushes a custom
    /// render command holding a clone of this `Rc`. Otherwise (software
    /// backend, or pre-wgpu agg-gui) the widget falls through to the
    /// wireframe path that uses only the 2-D `DrawCtx`.
    scene: Rc<RefCell<WgpuSceneRenderer>>,
}

impl Viewport3dWidget {
    pub fn new(inputs: ViewportInputs) -> Self {
        Self {
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            children: Vec::new(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::STRETCH)
                .with_v_anchor(VAnchor::STRETCH),
            inputs,
            drag: CameraDrag::None,
            last_mesh_ptr: 0,
            last_aabb: None,
            bg_color: Color::rgb(0.10, 0.11, 0.13),
            scene: Rc::new(RefCell::new(WgpuSceneRenderer::new())),
        }
    }

    /// Snapshot the shared orbit camera.  Cheap: an `OrbitCamera::clone`
    /// is just a few f32 copies.
    fn cam(&self) -> OrbitCamera {
        self.inputs.camera.lock().unwrap().clone()
    }

    /// Mutate the shared camera under a short-lived lock.  All internal
    /// callsites use this so the tumble cube widget's writes are picked
    /// up the next time the viewport reads its camera, and vice-versa.
    fn cam_mut<F: FnOnce(&mut OrbitCamera)>(&self, f: F) {
        f(&mut *self.inputs.camera.lock().unwrap())
    }

    fn current_mesh(&self) -> Option<Arc<MeshGL>> {
        self.inputs.last_mesh_output.lock().ok().and_then(|g| g.clone())
    }

    /// Re-fit the camera to the last seen mesh's AABB. Used by the Home
    /// / Fit-All button.  No-op if no mesh has been displayed yet.
    pub fn fit_all(&mut self) {
        if let Some((mn, mx)) = self.last_aabb {
            self.cam_mut(|c| c.fit_to_bounds(mn, mx));
            self.scene.borrow_mut().grid_y = mn[1];
        } else {
            // Reset to the default orientation when nothing has been
            // displayed yet — at least gives the user feedback.
            self.cam_mut(|c| c.reset_view());
        }
    }

    /// Fit the camera to an explicit AABB. Used by the Zoom-to-Selection
    /// button. With per-node mesh tracking still pending, callers can
    /// pass the displayed mesh's bounds.
    pub fn zoom_to_bounds(&mut self, min: [f32; 3], max: [f32; 3]) {
        self.cam_mut(|c| c.fit_to_bounds(min, max));
        self.scene.borrow_mut().grid_y = min[1];
    }

    fn maybe_auto_fit(&mut self, mesh: &MeshGL) {
        let real_ptr = mesh.vert_properties.as_ptr() as usize;
        if real_ptr == self.last_mesh_ptr {
            return;
        }
        self.last_mesh_ptr = real_ptr;
        // Compute AABB.
        if mesh.num_prop == 0 || mesh.vert_properties.is_empty() {
            return;
        }
        let stride = mesh.num_prop as usize;
        let n = mesh.vert_properties.len() / stride;
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for i in 0..n {
            for k in 0..3 {
                let v = mesh.vert_properties[i * stride + k];
                if v < mn[k] { mn[k] = v; }
                if v > mx[k] { mx[k] = v; }
            }
        }
        if mn[0].is_finite() && mx[0].is_finite() {
            self.cam_mut(|c| c.fit_to_bounds(mn, mx));
            // Sit the floor grid at the model's lowest point.
            self.scene.borrow_mut().grid_y = mn[1];
            self.last_aabb = Some((mn, mx));
        }
    }

    /// If the active backend is wgpu, push a custom render command via
    /// `WgpuGfxCtx::push_custom_render` and return `true`. Returns `false`
    /// when the backend is something else (e.g. software `GfxCtx`) — the
    /// caller then falls back to the wireframe path.
    fn try_push_wgpu_render(&mut self, ctx: &mut dyn DrawCtx, w: f64, h: f64) -> bool {
        // The widget's local origin is (0,0); transform (w,h) into agg-gui
        // screen-space pixels via the active DrawCtx affine. The transform
        // maps widget-local → screen.
        let any = match ctx.as_any_mut() { Some(a) => a, None => return false };
        let wgpu_ctx = match any.downcast_mut::<demo_wgpu::WgpuGfxCtx>() {
            Some(c) => c,
            None => return false,
        };
        let t = wgpu_ctx.transform();
        let mut x0 = 0.0; let mut y0 = 0.0;
        t.transform(&mut x0, &mut y0);
        let mut x1 = w; let mut y1 = h;
        t.transform(&mut x1, &mut y1);
        let screen_rect = agg_gui::Rect::new(
            x0.min(x1),
            y0.min(y1),
            (x1 - x0).abs(),
            (y1 - y0).abs(),
        );
        wgpu_ctx.push_custom_render(self.scene.clone(), screen_rect);
        true
    }

    fn draw_mesh(&self, ctx: &mut dyn DrawCtx, mesh: &MeshGL, w: f64, h: f64) {
        if mesh.num_prop < 6 || mesh.vert_properties.is_empty() {
            return;
        }
        let stride = mesh.num_prop as usize;
        let aspect = (w / h.max(1.0)) as f32;
        let cam = self.cam();
        let view = cam.view_matrix();
        let proj = cam.projection_matrix(aspect);
        let mvp = mul4(&proj, &view);
        let (right, up, fwd) = cam.basis();
        // Light direction in world space (normalized).
        let light = normalize3([0.4, 0.7, 0.6]);
        let _ = (right, up); // basis used by future ops

        let n_tri = mesh.tri_verts.len() / 3;
        ctx.set_line_width(1.0);

        for tri in 0..n_tri {
            let i0 = mesh.tri_verts[tri * 3] as usize;
            let i1 = mesh.tri_verts[tri * 3 + 1] as usize;
            let i2 = mesh.tri_verts[tri * 3 + 2] as usize;
            let p0 = vert_pos(mesh, i0, stride);
            let p1 = vert_pos(mesh, i1, stride);
            let p2 = vert_pos(mesh, i2, stride);

            // Backface culling: skip triangles whose face normal points
            // away from the camera (dot with eye-to-centroid > 0).
            let e1 = sub3(p1, p0);
            let e2 = sub3(p2, p0);
            let face_n = normalize3(cross3(e1, e2));
            let centroid = [
                (p0[0] + p1[0] + p2[0]) / 3.0,
                (p0[1] + p1[1] + p2[1]) / 3.0,
                (p0[2] + p1[2] + p2[2]) / 3.0,
            ];
            let to_cam = sub3(cam.eye(), centroid);
            if dot3(face_n, to_cam) <= 0.0 {
                continue;
            }

            // Project — NDC then map to widget pixel space.
            let s0 = match project(&mvp, p0, w, h) { Some(x) => x, None => continue };
            let s1 = match project(&mvp, p1, w, h) { Some(x) => x, None => continue };
            let s2 = match project(&mvp, p2, w, h) { Some(x) => x, None => continue };

            // Lighting: dot face normal with light dir (clamped) → tone.
            let diffuse = dot3(face_n, light).max(0.0);
            let ambient = 0.18;
            let v = (ambient + diffuse * 0.75).clamp(0.05, 1.0);
            // Reduce intensity for back-of-camera angle so far-side faces
            // appear cooler.
            let back_factor = (dot3(face_n, fwd).abs()).clamp(0.3, 1.0);
            let v = v * back_factor;
            let color = Color::rgba(
                0.55 + 0.35 * v,
                0.62 + 0.32 * v,
                0.78 + 0.22 * v,
                0.85,
            );
            ctx.set_stroke_color(color);

            ctx.begin_path();
            ctx.move_to(s0.0, s0.1);
            ctx.line_to(s1.0, s1.1);
            ctx.line_to(s2.0, s2.1);
            ctx.line_to(s0.0, s0.1);
            ctx.stroke();
        }
    }

    fn tick_camera_animation(&self, dt_seconds: f32) {
        let mut slot = self.inputs.camera_animation.lock().unwrap();
        let Some(anim) = slot.as_mut() else { return };
        let mut cam = self.inputs.camera.lock().unwrap();
        let done = anim.step(&mut cam, dt_seconds);
        if done {
            *slot = None;
        } else {
            agg_gui::animation::request_draw();
        }
    }
}

/// Pick an outline thickness scaled to the model's bounding-box extent so
/// the silhouette reads at any model size without micro-tuning per scene.
/// 0.6% of the largest dimension is enough to be visible from typical
/// orbit distances, small enough not to obscure surface detail.
fn estimate_outline_width(mesh: &MeshGL) -> f32 {
    if mesh.num_prop == 0 || mesh.vert_properties.is_empty() {
        return 0.05;
    }
    let stride = mesh.num_prop as usize;
    let n = mesh.vert_properties.len() / stride;
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    for i in 0..n {
        for k in 0..3 {
            let v = mesh.vert_properties[i * stride + k];
            if v < mn[k] { mn[k] = v; }
            if v > mx[k] { mx[k] = v; }
        }
    }
    if !mn[0].is_finite() || !mx[0].is_finite() {
        return 0.05;
    }
    let dx = mx[0] - mn[0];
    let dy = mx[1] - mn[1];
    let dz = mx[2] - mn[2];
    let extent = dx.max(dy).max(dz).max(1e-3);
    (extent * 0.006).max(0.005)
}

fn vert_pos(mesh: &MeshGL, i: usize, stride: usize) -> [f32; 3] {
    [
        mesh.vert_properties[i * stride],
        mesh.vert_properties[i * stride + 1],
        mesh.vert_properties[i * stride + 2],
    ]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    [v[0] / l, v[1] / l, v[2] / l]
}

/// Project a world-space point through the MVP matrix and map to
/// widget-local pixel coords. Returns `None` if the point is behind the
/// near plane (w ≤ 0).
fn project(mvp: &[f32; 16], p: [f32; 3], w: f64, h: f64) -> Option<(f64, f64)> {
    let h4 = transform_point4(mvp, p);
    if h4[3].abs() < 1e-6 {
        return None;
    }
    let inv_w = 1.0 / h4[3];
    if h4[3] <= 0.0 {
        return None;
    }
    let ndc_x = h4[0] * inv_w;
    let ndc_y = h4[1] * inv_w;
    // Map NDC [-1,1] to widget local pixel space, Y-up.
    let sx = (ndc_x as f64 * 0.5 + 0.5) * w;
    let sy = (ndc_y as f64 * 0.5 + 0.5) * h;
    Some((sx, sy))
}

impl Widget for Viewport3dWidget {
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, bounds: Rect) { self.bounds = bounds; }
    fn children(&self) -> &[Box<dyn Widget>] { &self.children }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> { &mut self.children }
    fn type_name(&self) -> &'static str { "Viewport3dWidget" }

    /// Stable instance id for the test harness — see [`NodeCanvas::id`].
    fn id(&self) -> Option<&str> { Some("viewport-3d") }
    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        self.bounds = Rect::new(0.0, 0.0, available.width, available.height);
        available
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        let w = self.bounds.width;
        let h = self.bounds.height;
        if w <= 0.0 || h <= 0.0 { return; }

        self.tick_camera_animation(0.016);

        // Theme-aware background: slightly lighter / darker than the
        // canvas backdrop so the viewport reads as a distinct pane.
        let visuals = ctx.visuals();
        let dark = 0.299 * visuals.bg_color.r
            + 0.587 * visuals.bg_color.g
            + 0.114 * visuals.bg_color.b
            < 0.5;
        self.bg_color = if dark {
            Color::rgb(0.10, 0.11, 0.13)
        } else {
            Color::rgb(0.985, 0.985, 0.99)
        };
        // Update the wgpu scene colors per theme — model surface color
        // and floor-grid line color.
        {
            let mut s = self.scene.borrow_mut();
            s.base_color = if dark {
                [0.62, 0.66, 0.78, 1.0]
            } else {
                [0.74, 0.78, 0.86, 1.0]
            };
            s.grid_line_color = if dark {
                [0.55, 0.58, 0.66, 0.55]
            } else {
                [0.55, 0.58, 0.66, 0.55]
            };
            // Background: same as the viewport bg so grid lines composite
            // cleanly against whatever 2-D content sits behind.
            s.grid_bg_color = [
                self.bg_color.r,
                self.bg_color.g,
                self.bg_color.b,
                0.0,
            ];
        }

        // Install system font so any text we paint actually renders.
        if let Some(f) = agg_gui::font_settings::current_system_font() {
            ctx.set_font(f);
        }

        // Clip 2-D paint calls to widget bounds so the empty-state hint
        // text and border can't bleed into siblings when the splitter
        // shrinks our pane. The wgpu pass below uses set_scissor_rect
        // for the same purpose on the 3-D side.
        ctx.save();
        ctx.clip_rect(0.0, 0.0, w, h);

        // Background fill always painted via the 2-D ctx so the underlying
        // surface gets a solid backdrop before the 3-D pass overdraws on top.
        ctx.set_fill_color(self.bg_color);
        ctx.begin_path();
        ctx.rect(0.0, 0.0, w, h);
        ctx.fill();

        // Push the latest mesh + camera into the scene renderer (cheap;
        // the renderer detects ptr equality and skips re-uploading
        // identical meshes).
        let mesh_opt = self.current_mesh();
        if let Some(mesh) = &mesh_opt {
            self.maybe_auto_fit(mesh);
        }
        // Read the selection slot once per paint to drive the outline
        // pass: an outline shows whenever something is selected and we
        // have a mesh to draw it around. With one displayed mesh today
        // the selected node *is* the displayed node by construction; A4+
        // generalise that.
        let selection_active = self.inputs.selection.lock().unwrap().is_some();
        // Scale the outline thickness off the model's current AABB so it
        // stays visible across model sizes — the auto-fit path captured
        // bounds in `last_mesh_ptr`/grid_y; recompute briefly here from
        // the live mesh.
        let outline_width = mesh_opt.as_deref().map(estimate_outline_width).unwrap_or(0.05);
        {
            let mut s = self.scene.borrow_mut();
            s.mesh = mesh_opt.clone();
            s.camera = self.cam();
            s.outline_enabled = selection_active;
            s.outline_width = outline_width;
            // Theme-driven outline colour: warm orange against either
            // dark or light backgrounds reads as "selected" without
            // clashing with the model's surface tint.
            s.outline_color = if dark {
                [1.00, 0.65, 0.20, 1.0]
            } else {
                [0.95, 0.50, 0.10, 1.0]
            };
            // Sync render style from app state so the picker beneath the
            // tumble cube takes effect on the next frame without any
            // extra plumbing.
            s.render_style = *self.inputs.render_style.lock().unwrap();
            // Sync bed toggle → floor-grid pass.
            s.draw_grid = *self.inputs.show_bed.lock().unwrap();
        }

        // Try the wgpu path. The widget's `bounds` are widget-local — the
        // `DrawCtx` already has a transform that maps (0,0) to the widget's
        // bottom-left. We need the screen-space rect (in agg-gui Y-up
        // pixel coords). We get that from `ctx.transform()` applied to
        // origin + size.
        let used_wgpu = self.try_push_wgpu_render(ctx, w, h);

        if !used_wgpu {
            // Software fallback wireframe — kept for the GfxCtx (CPU AGG)
            // backend or any non-wgpu DrawCtx.
            if let Some(mesh) = mesh_opt.as_ref() {
                self.draw_mesh(ctx, mesh, w, h);
            }
        }

        if mesh_opt.is_none() {
            // Empty-state hint.
            ctx.set_fill_color(Color::rgba(1.0, 1.0, 1.0, 0.4));
            ctx.set_font_size(12.0);
            ctx.fill_text("No geometry — select a node with a 3D output", 16.0, h - 24.0);
        }

        // Border.
        ctx.set_stroke_color(Color::rgba(1.0, 1.0, 1.0, 0.10));
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.rect(0.5, 0.5, (w - 1.0).max(0.0), (h - 1.0).max(0.0));
        ctx.stroke();

        ctx.restore(); // pop clip
    }

    fn claims_pointer_exclusively(&self, _local_pos: Point) -> bool {
        !matches!(self.drag, CameraDrag::None)
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        match event {
            Event::MouseDown { pos, button, modifiers, .. } => {
                self.on_mouse_down(*pos, *button, *modifiers)
            }
            Event::MouseUp { pos, button, .. } => self.on_mouse_up(*pos, *button),
            Event::MouseMove { pos } => self.on_mouse_move(*pos),
            Event::MouseWheel { delta_y, .. } => self.on_wheel(*delta_y),
            Event::KeyDown { key, modifiers } => self.on_key_down(key, *modifiers),
            _ => EventResult::Ignored,
        }
    }
}

impl Viewport3dWidget {
    /// Compute the world-space pivot for an orbit drag started at the
    /// given widget-local cursor position.
    ///
    /// 1. If the cursor's ray hits the live mesh, the hit point becomes
    ///    the new orbit center and the eye-to-hit distance becomes the
    ///    new orbit radius.
    /// 2. If the cursor misses, project the ray onto the plane through
    ///    the current `center` perpendicular to forward (matches
    ///    MatterCAD / NodeDesigner).
    ///
    /// `pos` is the agg-gui Y-up local coord; we flip Y to top-down for
    /// `screen_to_ray` since that's the convention the unprojection
    /// expects.
    fn orbit_pivot_from_cursor(&self, pos: Point) -> ([f32; 3], f32) {
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        // agg-gui events are in widget-local Y-up coords. screen_to_ray
        // expects top-down (origin top-left), so flip Y.
        let cursor_top_down = (pos.x, h - pos.y);
        let cam = self.cam();
        let (origin, dir) = cam.screen_to_ray(cursor_top_down, (w, h));
        let pivot = match self.current_mesh().as_ref() {
            Some(mesh) => raycast_mesh(mesh, origin, dir)
                .unwrap_or_else(|| project_to_view_plane(&cam, origin, dir)),
            None => project_to_view_plane(&cam, origin, dir),
        };
        let eye = cam.eye();
        let dx = pivot[0] - eye[0];
        let dy = pivot[1] - eye[1];
        let dz = pivot[2] - eye[2];
        let radius = (dx * dx + dy * dy + dz * dz).sqrt().max(0.05);
        (pivot, radius)
    }

    fn on_mouse_down(&mut self, pos: Point, button: MouseButton, mods: Modifiers) -> EventResult {
        match button {
            MouseButton::Right => {
                // Right-drag → orbit, pivoting at the cursor hit point.
                let (pivot, radius) = self.orbit_pivot_from_cursor(pos);
                let (start_az, start_el);
                {
                    let mut c = self.inputs.camera.lock().unwrap();
                    c.center = pivot;
                    c.radius = radius;
                    start_az = c.azimuth;
                    start_el = c.elevation;
                }
                self.drag = CameraDrag::Orbit { start_local: pos, start_az, start_el };
                EventResult::Consumed
            }
            MouseButton::Middle => {
                self.drag = CameraDrag::Pan {
                    start_local: pos,
                    start_center: self.cam().center,
                };
                EventResult::Consumed
            }
            MouseButton::Left => {
                // Modifier-aware fallbacks for users without dedicated
                // middle/right buttons (trackpads). Match MatterCAD's docs.
                let cam_snapshot = self.cam();
                if mods.ctrl && mods.alt {
                    self.drag = CameraDrag::Zooming {
                        start_local: pos,
                        start_radius: cam_snapshot.radius,
                    };
                    EventResult::Consumed
                } else if mods.ctrl && mods.shift {
                    self.drag = CameraDrag::Pan {
                        start_local: pos,
                        start_center: cam_snapshot.center,
                    };
                    EventResult::Consumed
                } else if mods.ctrl {
                    let (pivot, radius) = self.orbit_pivot_from_cursor(pos);
                    let (start_az, start_el);
                    {
                        let mut c = self.inputs.camera.lock().unwrap();
                        c.center = pivot;
                        c.radius = radius;
                        start_az = c.azimuth;
                        start_el = c.elevation;
                    }
                    self.drag = CameraDrag::Orbit { start_local: pos, start_az, start_el };
                    EventResult::Consumed
                } else {
                    // No modifier → fall back to the active tool from the
                    // viewport toolbar (Select / Rotate / Pan / Zoom).
                    // `Select` keeps AtomArtist's original click-to-pick
                    // behaviour; the others trade selection for camera
                    // manipulation on plain left-drag.
                    let tool = *self.inputs.tool.lock().unwrap();
                    match tool {
                        ViewportTool::Select => {
                            self.drag = CameraDrag::Selecting {
                                start_local: pos,
                                moved: false,
                            };
                        }
                        ViewportTool::Rotate => {
                            let (pivot, radius) = self.orbit_pivot_from_cursor(pos);
                            let (start_az, start_el);
                            {
                                let mut c = self.inputs.camera.lock().unwrap();
                                c.center = pivot;
                                c.radius = radius;
                                start_az = c.azimuth;
                                start_el = c.elevation;
                            }
                            self.drag = CameraDrag::Orbit { start_local: pos, start_az, start_el };
                        }
                        ViewportTool::Pan => {
                            self.drag = CameraDrag::Pan {
                                start_local: pos,
                                start_center: cam_snapshot.center,
                            };
                        }
                        ViewportTool::Zoom => {
                            self.drag = CameraDrag::Zooming {
                                start_local: pos,
                                start_radius: cam_snapshot.radius,
                            };
                        }
                    }
                    EventResult::Consumed
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        match &mut self.drag {
            CameraDrag::None => EventResult::Ignored,
            CameraDrag::Orbit { start_local, start_az, start_el } => {
                let dx = (pos.x - start_local.x) as f32;
                let dy = (pos.y - start_local.y) as f32;
                let scale = 0.005;
                let mut c = self.inputs.camera.lock().unwrap();
                // Drag right (dx > 0) should turn the world right
                // (object follows the cursor) — that's the camera
                // orbiting counter-clockwise around world-up, i.e.
                // azimuth DECREASING under our `eye = [r*ce*sin(az),
                // r*se, r*ce*cos(az)]` formula.
                c.azimuth = *start_az - dx * scale;
                c.elevation = *start_el - dy * scale;
                let limit = std::f32::consts::PI * 0.49;
                c.elevation = c.elevation.clamp(-limit, limit);
                EventResult::Consumed
            }
            CameraDrag::Pan { start_local, start_center } => {
                let dx = (pos.x - start_local.x) as f32;
                let dy = (pos.y - start_local.y) as f32;
                let mut c = self.inputs.camera.lock().unwrap();
                // Pan scales with distance so the world point under the
                // cursor stays roughly under the cursor. Drag-down (negative
                // dy in agg-gui Y-up coords) lowers the look-at point — see
                // `OrbitCamera::pan` and the regression test for the bug.
                let pan_scale = c.radius * 0.0025;
                let (right, up, _fwd) = c.basis();
                c.center = [
                    start_center[0] - right[0] * dx * pan_scale - up[0] * dy * pan_scale,
                    start_center[1] - right[1] * dx * pan_scale - up[1] * dy * pan_scale,
                    start_center[2] - right[2] * dx * pan_scale - up[2] * dy * pan_scale,
                ];
                EventResult::Consumed
            }
            CameraDrag::Zooming { start_local, start_radius } => {
                // Vertical drag distance maps to a multiplicative zoom in
                // the same direction as MatterCAD's documented modifier
                // path (drag up = zoom out, drag down = zoom in).
                let dy = (pos.y - start_local.y) as f32;
                // 200-pixel drag ≈ 2.7× zoom in either direction.
                let factor = (dy * 0.005).exp();
                let r = (*start_radius * factor).clamp(0.05, 10_000.0);
                if r.is_finite() {
                    self.inputs.camera.lock().unwrap().radius = r;
                }
                EventResult::Consumed
            }
            CameraDrag::Selecting { start_local, moved } => {
                let dx = (pos.x - start_local.x).abs();
                let dy = (pos.y - start_local.y).abs();
                if dx > 2.0 || dy > 2.0 {
                    *moved = true;
                }
                EventResult::Consumed
            }
        }
    }

    fn on_mouse_up(&mut self, pos: Point, _button: MouseButton) -> EventResult {
        let prev = std::mem::replace(&mut self.drag, CameraDrag::None);
        match prev {
            CameraDrag::None => EventResult::Ignored,
            CameraDrag::Selecting { moved, .. } if !moved => {
                // Treat as a click: raycast against the displayed mesh
                // and, if hit, mark its source node as selected. With
                // only one displayed mesh today, that's whatever node
                // the host is rendering.
                let mesh_opt = self.current_mesh();
                let display_id = *self.inputs.display_node.lock().unwrap();
                if let (Some(mesh), Some(id)) = (mesh_opt, display_id) {
                    let w = self.bounds.width.max(1.0);
                    let h = self.bounds.height.max(1.0);
                    let cursor_top_down = (pos.x, h - pos.y);
                    let (origin, dir) = self.cam().screen_to_ray(cursor_top_down, (w, h));
                    if raycast_mesh(&mesh, origin, dir).is_some() {
                        *self.inputs.selection.lock().unwrap() = Some(id);
                    } else {
                        // Click on empty space clears selection.
                        *self.inputs.selection.lock().unwrap() = None;
                    }
                } else {
                    *self.inputs.selection.lock().unwrap() = None;
                }
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }

    fn on_wheel(&mut self, delta_y: f64) -> EventResult {
        if delta_y == 0.0 {
            return EventResult::Ignored;
        }
        let factor = if delta_y > 0.0 { 0.9 } else { 1.0 / 0.9 };
        self.cam_mut(|c| c.zoom(factor as f32));
        EventResult::Consumed
    }

    /// Keyboard navigation. Mirrors MatterCAD's documented shortcuts —
    /// see the file-header table.
    fn on_key_down(&mut self, key: &Key, mods: Modifiers) -> EventResult {
        // Pan / orbit step constants in physical-pixel deltas — sized so
        // a single arrow press feels like a deliberate small adjustment.
        const ARROW_PAN_PX: f32 = 24.0;
        const ARROW_ORBIT_PX: f32 = 24.0;
        const KEYBOARD_ZOOM_FACTOR: f32 = 1.1;

        match key {
            Key::Char(c) => {
                if c.eq_ignore_ascii_case(&'w') || c.eq_ignore_ascii_case(&'f') {
                    // W = canonical fit-all (MatterCAD); F kept as legacy alias.
                    if let Some(mesh) = self.current_mesh() {
                        self.last_mesh_ptr = 0;
                        self.maybe_auto_fit(&mesh);
                    }
                    return EventResult::Consumed;
                }
                if c.eq_ignore_ascii_case(&'z') {
                    // Z = zoom-to-selected. With no per-node mesh tracking
                    // yet, fall through to fit-all (Phase A4 will tighten
                    // this to use the selected node's bounds when one is
                    // selected).
                    if let Some(mesh) = self.current_mesh() {
                        self.last_mesh_ptr = 0;
                        self.maybe_auto_fit(&mesh);
                    }
                    return EventResult::Consumed;
                }
                // Ctrl + +/- → zoom in/out.
                if mods.ctrl {
                    if *c == '+' || *c == '=' {
                        self.cam_mut(|c| c.zoom(1.0 / KEYBOARD_ZOOM_FACTOR));
                        return EventResult::Consumed;
                    }
                    if *c == '-' || *c == '_' {
                        self.cam_mut(|c| c.zoom(KEYBOARD_ZOOM_FACTOR));
                        return EventResult::Consumed;
                    }
                }
            }
            Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown => {
                let (dx, dy) = match key {
                    Key::ArrowLeft => (-1.0f32, 0.0),
                    Key::ArrowRight => (1.0, 0.0),
                    Key::ArrowUp => (0.0, 1.0),
                    Key::ArrowDown => (0.0, -1.0),
                    _ => unreachable!(),
                };
                if mods.shift {
                    let mut c = self.inputs.camera.lock().unwrap();
                    let scale = c.radius * 0.0025;
                    c.pan(dx * ARROW_PAN_PX * scale, dy * ARROW_PAN_PX * scale);
                } else {
                    let scale = 0.005;
                    self.cam_mut(|c| {
                        c.orbit(dx * ARROW_ORBIT_PX * scale, -dy * ARROW_ORBIT_PX * scale)
                    });
                }
                return EventResult::Consumed;
            }
            _ => {}
        }
        EventResult::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_inputs() -> ViewportInputs {
        ViewportInputs::empty()
    }

    #[test]
    fn project_returns_none_for_point_behind_camera() {
        let cam = OrbitCamera::default();
        let view = cam.view_matrix();
        let proj = cam.projection_matrix(1.0);
        let mvp = mul4(&proj, &view);
        // Point behind the camera (w ends up <= 0).
        let p = [
            cam.eye()[0] * 2.0 - cam.center[0],
            cam.eye()[1] * 2.0 - cam.center[1],
            cam.eye()[2] * 2.0 - cam.center[2],
        ];
        let result = project(&mvp, p, 100.0, 100.0);
        assert!(result.is_none());
    }

    #[test]
    fn project_origin_lands_near_center_of_widget() {
        let cam = OrbitCamera::default();
        let mvp = mul4(&cam.projection_matrix(1.0), &cam.view_matrix());
        let s = project(&mvp, [0.0, 0.0, 0.0], 200.0, 200.0).unwrap();
        // Center is somewhere in the middle of the widget within tolerance.
        assert!(s.0 > 60.0 && s.0 < 140.0);
        assert!(s.1 > 60.0 && s.1 < 140.0);
    }

    #[test]
    fn widget_constructs_and_lays_out() {
        let inputs = empty_inputs();
        let mut w = Viewport3dWidget::new(inputs);
        let s = w.layout(Size::new(400.0, 300.0));
        assert_eq!(s.width, 400.0);
        assert_eq!(s.height, 300.0);
    }
}
