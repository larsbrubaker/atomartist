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
use atomartist_lib::geometry::Geometry3d;
use atomartist_lib::graph::node::NodeId;
use manifold_rust::types::MeshGL;

use glam::Mat4;

use crate::camera::OrbitCamera;
use crate::camera_animations::{CameraPoseAnimation, ProjectionAnimation};
use crate::picking::{resolve_pivot_or_fallback, HitPlane, PivotResolution};

#[path = "viewport_widget_helpers.rs"]
mod viewport_widget_helpers;
use viewport_widget_helpers::{
    cross3, dot3, estimate_outline_width, mesh_aabb, mouse_button_bit, normalize3, project,
    selected_body_world_aabb, stroke_circle, sub3, vert_pos,
};
use crate::scene_renderer::{GizmoLineSet, RenderStyle, WgpuSceneRenderer};

#[path = "viewport_widget/inputs.rs"]
mod inputs;
pub use inputs::{ViewportInputs, ViewportTool};

// `ViewportInputs` + `ViewportTool` live in `viewport_widget/inputs.rs`
// — see the module above. Re-exported here so callers continue to
// import via `crate::viewport_widget::{ViewportInputs, ViewportTool}`.

// `CameraDrag` (the left/right/middle-button drag state machine) lives
// in `viewport_widget/camera_drag.rs` — see the module declaration near
// the bottom of this file. Re-imported below so the widget + its
// interaction submodules keep referring to it as `CameraDrag`.

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
    /// World point of the most recent mouse-down (or wheel-zoom) —
    /// MatterCAD's `mouseDownWorldPosition`. Anchors pan/rotate/
    /// wheel-zoom to a fixed world location across each drag, and
    /// drives the on-screen rotate cursor.
    mouse_down_world_pos: [f32; 3],
    /// Plane stored alongside `mouse_down_world_pos`. For mesh hits
    /// it is perpendicular to the screen-centre view direction; for
    /// the empty-scene case it is the bed (Z=0). Pan and wheel-zoom
    /// re-intersect this plane every frame.
    hit_plane: HitPlane,
    /// Whether the last mouse-down landed on a real scene mesh.
    /// `true` → the circle cursor renders the "pivot on part"
    /// treatment; `false` → bed / view-plane fallback. The
    /// distinction is reserved for richer cursor styling later;
    /// the current circle uses the accent colour either way.
    #[allow(dead_code)]
    pivot_on_scene: bool,
    /// Bitmask of currently-held mouse buttons (bit 0 = Left,
    /// bit 1 = Right, bit 2 = Middle). Used as a safety net so a
    /// stale `drag` state can never apply camera updates on plain
    /// hover. Rationale: in normal operation `drag` is cleared by
    /// `on_mouse_up`, but if a release happens outside the window
    /// (or any other edge case where MouseUp is dropped) the drag
    /// would otherwise linger and every subsequent MouseMove on
    /// re-entry would orbit / pan / zoom. The check in
    /// `on_mouse_move` resets the drag the next time a hover event
    /// comes in with no buttons held.
    pressed_buttons: u8,
    /// Which rotate-gizmo axis handle (0=X, 1=Y, 2=Z) the cursor is
    /// currently hovering, if any. Updated each `MouseMove` while idle
    /// (no drag) so the hovered handle paints accent-colored and its
    /// rotation compass appears — MatterCAD's `MouseIsOver` behaviour,
    /// but tracked per-axis since we have three handles. `None` when
    /// the cursor is over no rotate handle.
    hovered_rotate_axis: Option<u8>,
    /// Whether the cursor is currently over the Z-translate control
    /// (the cone + shaft). Updated each `MouseMove` while idle so the
    /// control paints accent-coloured on hover, exactly like the rotate
    /// handles and MatterCAD's `MoveInZControl` (`MouseIsOver`).
    hovered_z_control: bool,
    /// Whether the cursor is over the height / scale-Z box (top-face
    /// centre). Drives that control's accent highlight on hover, like
    /// MatterCAD's `ScaleHeightControl`.
    hovered_height_control: bool,
    /// Latest keyboard modifier state, refreshed on every mouse-down and
    /// key up/down. `MouseMove` carries no modifiers, so the rotate drag
    /// reads this to honour the Shift-to-45° snap lock live mid-drag
    /// (MatterCAD's `ModifierKeys == Shift` check).
    current_mods: Modifiers,
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
            mouse_down_world_pos: [0.0, 0.0, 0.0],
            // Default plane: XY at Z=0 (the bed). Updated on every
            // mouse-down / wheel-zoom via `resolve_pivot_or_fallback`.
            hit_plane: HitPlane {
                point: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
            },
            pivot_on_scene: false,
            pressed_buttons: 0,
            hovered_rotate_axis: None,
            hovered_z_control: false,
            hovered_height_control: false,
            current_mods: Modifiers::default(),
        }
    }

    /// Set the bit corresponding to a MouseButton in
    /// `pressed_buttons`.
    pub(crate) fn note_mouse_down(&mut self, button: MouseButton) {
        self.pressed_buttons |= mouse_button_bit(button);
    }

    /// Clear the bit corresponding to a MouseButton in
    /// `pressed_buttons`.
    pub(crate) fn note_mouse_up(&mut self, button: MouseButton) {
        self.pressed_buttons &= !mouse_button_bit(button);
    }

    /// `true` while at least one of (Left, Middle, Right) is held.
    pub(crate) fn any_mouse_button_held(&self) -> bool {
        self.pressed_buttons != 0
    }

    /// Compute the world pivot and interaction plane for a cursor
    /// at widget-local `pos`. Mirrors MatterCAD's
    /// `CalculateMouseDownPostionAndPlane` — picks against the live
    /// mesh, falls back to the previous pivot's plane or the bed.
    pub(crate) fn resolve_pivot(&self, pos: Point) -> PivotResolution {
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let cursor_top_down = (pos.x, h - pos.y);
        let cam = self.cam();
        let (ray_origin, ray_dir) = cam.screen_to_ray(cursor_top_down, (w, h));
        // Screen-centre ray: the plane normal MatterCAD uses for
        // the hit plane. Use the camera-forward vector rather than
        // unprojecting (0.5, 0.5); they're the same for our
        // standard view matrices and a vector is cheaper.
        let (_right, _up, fwd) = cam.basis();
        let mesh_slot = self.inputs.last_mesh_output.lock().unwrap();
        // Phase-1 multi-body integration: raycast uses the first
        // body's mesh. Multi-body raycast (returning which body was
        // hit) is the next renderer pass.
        let mesh_ref = mesh_slot.as_deref().and_then(|g| g.first()).map(|b| &*b.mesh);
        resolve_pivot_or_fallback(
            mesh_ref,
            ray_origin,
            ray_dir,
            fwd,
            self.mouse_down_world_pos,
        )
    }

    /// Update the stored pivot + plane from the given cursor
    /// position. Returns the resolution so callers can also use
    /// `world_pos` for camera state (e.g. setting `center` for
    /// rotate).
    pub(crate) fn refresh_pivot(&mut self, pos: Point) -> PivotResolution {
        let res = self.resolve_pivot(pos);
        self.mouse_down_world_pos = res.world_pos;
        self.hit_plane = res.plane;
        self.pivot_on_scene = res.hit_scene;
        res
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

    /// Snapshot the currently-displayed geometry bundle. Returns the
    /// full [`Geometry3d`] so callers that only need the mesh can
    /// pull `.mesh`, while paint code can also read `.color` (for
    /// the renderer's `base_color`) and `.matrix` (future
    /// control-gizmo wiring).
    fn current_geometry(&self) -> Option<Arc<Geometry3d>> {
        self.inputs.last_mesh_output.lock().ok().and_then(|g| g.clone())
    }

    /// Mesh-only convenience used by raycast / fit-to-bounds /
    /// software fallback callers that don't care about the matrix or
    /// colour.
    fn current_mesh(&self) -> Option<Arc<MeshGL>> {
        // Single-mesh callers read the first body. Multi-body
        // viewport rendering (one draw call per body, each with its
        // own base_color) is a follow-up pass; this fallback keeps
        // raycast / fit-to-bounds / software-render paths working
        // through phase 1.
        self.current_geometry()
            .and_then(|g| g.first().map(|b| b.mesh.clone()))
    }

    /// Re-fit the camera to the last seen mesh's AABB. Used by the Home
    /// / Fit-All button.  No-op if no mesh has been displayed yet.
    ///
    /// Does NOT move the floor grid — `grid_z` stays at the bed
    /// plane (Z=0) regardless of where the model is. The grid is
    /// a fixed world reference, like MatterCAD's `BedSurfaceZ`.
    pub fn fit_all(&mut self) {
        if let Some((mn, mx)) = self.last_aabb {
            self.cam_mut(|c| c.fit_to_bounds(mn, mx));
        } else {
            // Reset to the default orientation when nothing has been
            // displayed yet — at least gives the user feedback.
            self.cam_mut(|c| c.reset_view());
        }
    }

    /// Fit the camera to an explicit AABB. Used by the Zoom-to-Selection
    /// button. With per-node mesh tracking still pending, callers can
    /// pass the displayed mesh's bounds. Like `fit_all`, this only
    /// moves the camera — the floor grid stays at Z=0.
    pub fn zoom_to_bounds(&mut self, min: [f32; 3], max: [f32; 3]) {
        self.cam_mut(|c| c.fit_to_bounds(min, max));
    }

    /// Update the cached mesh AABB and — only on the very first
    /// mesh — fit the camera to it.
    ///
    /// Critical UX: editing a node value re-evaluates the graph,
    /// which produces a new mesh every keystroke / slider tick.
    /// We must NOT call `fit_to_bounds` for those updates, or the
    /// camera would jump every time the user adjusts a parameter.
    /// Instead we just refresh `last_aabb` so the explicit
    /// fit-all / zoom-to-selection buttons (and the `W` / `F`
    /// keyboard shortcuts) have current bounds to work with the
    /// next time the user actually asks for a fit.
    ///
    /// `grid_z` similarly stays at 0 (the bed in our Z-up world,
    /// mirroring MatterCAD's `BedSurfaceZ`) rather than tracking
    /// the model's lowest Z — the floor is a fixed reference, not
    /// a model-derived value.
    fn maybe_auto_fit(&mut self, mesh: &MeshGL) {
        let real_ptr = mesh.vert_properties.as_ptr() as usize;
        if real_ptr == self.last_mesh_ptr {
            return;
        }
        self.last_mesh_ptr = real_ptr;
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
        if !mn[0].is_finite() || !mx[0].is_finite() {
            return;
        }
        let is_first_mesh = self.last_aabb.is_none();
        self.last_aabb = Some((mn, mx));
        if is_first_mesh {
            // First mesh ever — fit the view so the user has
            // something visible to start from.
            self.cam_mut(|c| c.fit_to_bounds(mn, mx));
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
        let view = Mat4::from_cols_array(&cam.view_matrix());
        let proj = Mat4::from_cols_array(&cam.projection_matrix(aspect));
        let mvp = (proj * view).to_cols_array();
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

    /// Whether the rotate cursor should appear this frame. True
    /// when the active tool is Rotate or Pan, OR an Orbit / Pan
    /// drag is in progress. MatterCAD only draws the circle on
    /// hover (`CurrentTrackingType == None`), but it's far more
    /// useful to keep it visible during the drag so the user can
    /// see what world point they're spinning around — so we extend
    /// the condition.
    fn should_show_pivot_cursor(&self) -> bool {
        let tool = *self.inputs.tool.lock().unwrap();
        if matches!(tool, ViewportTool::Rotate | ViewportTool::Pan) {
            return true;
        }
        matches!(self.drag, CameraDrag::Orbit { .. } | CameraDrag::Pan { .. })
    }

    /// Paint the screen-space rotate cursor at the projection of
    /// `mouse_down_world_pos`. Layered to mimic MatterCAD's
    /// `drawCircle`: an inner accent ring and a wider translucent
    /// halo so the cursor reads against any background.
    fn paint_pivot_cursor(&self, ctx: &mut dyn DrawCtx, w: f64, h: f64) {
        let cam = self.cam();
        let view = Mat4::from_cols_array(&cam.view_matrix());
        let proj = Mat4::from_cols_array(&cam.projection_matrix((w / h.max(1.0)) as f32));
        let mvp = (proj * view).to_cols_array();
        let p = self.mouse_down_world_pos;
        let Some((sx, sy)) = project(&mvp, p, w, h) else {
            return;
        };
        // 8-pixel ring + 4-pixel halo, matching the
        // `Stroke(circle, 2*DeviceScale)` /
        // `Stroke(Stroke(circle, 4*DeviceScale), DeviceScale)`
        // call pair in MatterCAD.
        let r = 8.0;
        let v = ctx.visuals();
        let accent = v.accent;
        let halo = v.text_color.with_alpha(0.45);
        // Outer translucent halo so the ring reads on any backdrop.
        ctx.set_stroke_color(halo);
        ctx.set_line_width(4.0);
        stroke_circle(ctx, sx, sy, r);
        // Inner accent stroke — the "actual" rotate cursor.
        ctx.set_stroke_color(accent);
        ctx.set_line_width(2.0);
        stroke_circle(ctx, sx, sy, r);
    }

    // Drag-readout overlays (`paint_rotation_readout`,
    // `paint_z_measure_readout`, `paint_height_readout`) live in
    // `viewport_widget/overlays.rs` so this file stays under the
    // 800-line guardrail.

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

    /// Step the in-flight perspective <-> orthographic tween, if any.
    /// Runs every paint alongside `tick_camera_animation`, drops the
    /// handle when the tween reaches `progress = 1`, and keeps the
    /// frame loop spinning while it's active. Mirrors MatterCAD's
    /// `Animation.Run(this, 0.25, 10, …)` callback.
    fn tick_projection_animation(&self, dt_seconds: f32) {
        let mut slot = self.inputs.projection_animation.lock().unwrap();
        let Some(anim) = slot.as_mut() else { return };
        let mut cam = self.inputs.camera.lock().unwrap();
        let done = anim.step(&mut cam, dt_seconds);
        if done {
            *slot = None;
        } else {
            agg_gui::animation::request_draw();
        }
    }

    // `populate_scene_state` lives in `viewport_widget/scene_state.rs`
    // so this file stays under the 800-line guardrail.
}

// `mesh_aabb` + `estimate_outline_width` moved to viewport_widget_helpers.rs
// so this file stays under the 800-line guardrail.

// Free helpers (vector ops, projection, circle stroke, mouse-bit
// map) live in the sibling `viewport_widget_helpers.rs` so this
// file stays under the line-count guardrail. They're pulled in
// via the `use` declaration near the top of this module.

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
        self.tick_projection_animation(0.016);

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
            // Light theme → muted greyish-blue lines that read against a
            // near-white bed. Dark theme → near-white lines so the grid
            // still pops against the dark backdrop (matches
            // NodeDesigner's `setShadowTheme` which flips both the
            // shadow and the grid colour together).
            s.grid_line_color = if dark {
                [0.85, 0.88, 0.94, 0.40]
            } else {
                [0.55, 0.58, 0.66, 0.55]
            };
            // Theme flag drives the contact-shadow composite — black
            // shadows for light bg, bright shadows for dark.
            s.grid_dark_mode = dark;
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

        // Push the latest mesh + camera + per-node colour into the
        // scene renderer (cheap; the renderer detects ptr equality
        // and skips re-uploading identical meshes). We pull the full
        // `Geometry3d` bundle so the shader's `base_color` can be
        // driven by the upstream node's `color` property — without
        // this the renderer would always paint its `new()` default
        // tint regardless of what the user set on the node.
        let geom_opt = self.current_geometry();
        // Multi-body: push the full body list to the scene renderer.
        // The renderer's per-pass loop iterates every body with its
        // own model matrix + colour out of the dynamic uniform buffer.
        // First body still drives auto-fit / outline-width / bounds
        // gizmo heuristics (those are cosmetic and don't need
        // multi-body summation for now).
        let bodies = geom_opt.as_ref().map(|g| g.bodies.clone()).unwrap_or_default();
        let first_body = bodies.first().cloned();
        let mesh_opt: Option<Arc<MeshGL>> = first_body.as_ref().map(|b| b.mesh.clone());
        if let Some(mesh) = &mesh_opt {
            self.maybe_auto_fit(mesh);
        }
        let selection_active = self.inputs.selection.lock().unwrap().is_some();
        let outline_width = mesh_opt.as_deref().map(estimate_outline_width).unwrap_or(0.05);
        self.populate_scene_state(
            bodies,
            first_body.as_ref(),
            selection_active,
            outline_width,
            [visuals.accent.r, visuals.accent.g, visuals.accent.b, 1.0],
        );

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

        // MatterCAD-style rotation cursor: a small 2-D circle at the
        // screen projection of `mouse_down_world_pos`. Direct port
        // of `TrackballTumbleWidgetExtended.OnDraw`'s `drawCircle`
        // (`graphics2D.Render(new Ellipse(...))`).
        if self.should_show_pivot_cursor() {
            self.paint_pivot_cursor(ctx, w, h);
        }

        // Live rotation angle readout (MatterCAD `angleTextControl`),
        // drawn only while a rotate-handle drag is in flight.
        self.paint_rotation_readout(ctx, w, h);
        // Z-drag distance readout (MatterCAD measure control), drawn
        // only while moving the body in Z.
        self.paint_z_measure_readout(ctx, w, h);
        // Height readout (MatterCAD ScaleHeightControl), drawn only
        // while scaling the body in Z.
        self.paint_height_readout(ctx, w, h);

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
                self.current_mods = *modifiers;
                self.note_mouse_down(*button);
                self.on_mouse_down(*pos, *button, *modifiers)
            }
            Event::MouseUp { pos, button, .. } => {
                self.note_mouse_up(*button);
                self.on_mouse_up(*pos, *button)
            }
            Event::MouseMove { pos } => self.on_mouse_move(*pos),
            Event::MouseWheel { pos, delta_y, .. } => self.on_wheel_at_pos(*pos, *delta_y),
            Event::KeyDown { key, modifiers } => {
                // Track Shift etc. live so a mid-drag rotate can read the
                // snap-lock state (MouseMove carries no modifiers).
                self.current_mods = *modifiers;
                self.on_key_down(key, *modifiers)
            }
            Event::KeyUp { modifiers, .. } => {
                self.current_mods = *modifiers;
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    }

    /// The viewport isn't focusable, so keyboard shortcuts arrive here
    /// (agg-gui offers unconsumed keys to every visible widget) rather
    /// than through the focused-`on_event` path. Route them to the same
    /// `on_key_down` handler — this is what lets `Esc` cancel an
    /// in-flight drag, and the `W` / arrow camera shortcuts fire,
    /// without the pane needing focus. Only consumes keys it actually
    /// handles, so `Esc` still bubbles to close menus when no drag is
    /// active.
    fn on_unconsumed_key(&mut self, key: &Key, modifiers: Modifiers) -> EventResult {
        self.current_mods = modifiers;
        self.on_key_down(key, modifiers)
    }
}

#[path = "viewport_widget/viewport_widget_interactions.rs"]
mod viewport_widget_interactions;

#[path = "viewport_widget/rotate_interactions.rs"]
mod rotate_interactions;

#[path = "viewport_widget/scale_interactions.rs"]
mod scale_interactions;

#[path = "viewport_widget/overlays.rs"]
mod overlays;

#[path = "viewport_widget/scene_state.rs"]
mod scene_state;

#[path = "viewport_widget/camera_drag.rs"]
mod camera_drag;
use camera_drag::CameraDrag;

#[path = "viewport_widget/z_control_gizmo.rs"]
mod z_control_gizmo;

#[path = "viewport_widget/rotate_gizmo/mod.rs"]
mod rotate_gizmo;

#[path = "viewport_widget/body_drag.rs"]
mod body_drag;

#[cfg(test)]
#[path = "viewport_widget_tests.rs"]
mod viewport_widget_tests;
