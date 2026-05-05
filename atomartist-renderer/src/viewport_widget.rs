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
//! Camera controls:
//!   - Left-drag      → orbit
//!   - Right-drag     → pan
//!   - Middle-drag    → pan (alias)
//!   - Scroll wheel   → zoom
//!   - F key          → fit camera to current mesh bounds

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use agg_gui::{
    Color, DrawCtx, Event, EventResult, HAnchor, Key, Modifiers, MouseButton, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};
use manifold_rust::types::MeshGL;

use crate::camera::{mul4, transform_point4, OrbitCamera};
use crate::scene_renderer::WgpuSceneRenderer;

/// External hooks the widget needs from the app: where to read the latest
/// mesh from, and how to mark the viewport as repainted (so `needs_draw`
/// in a future render-loop integration can be derived).
pub struct ViewportInputs {
    pub last_mesh_output: Arc<Mutex<Option<Arc<MeshGL>>>>,
}

#[derive(Clone, Debug)]
enum CameraDrag {
    None,
    Orbit { start_local: Point, start_az: f32, start_el: f32 },
    Pan { start_local: Point, start_center: [f32; 3] },
}

pub struct Viewport3dWidget {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    inputs: ViewportInputs,
    pub camera: OrbitCamera,
    drag: CameraDrag,
    /// Track the most recent mesh seen; if a new one comes in, auto-fit
    /// the camera once.
    last_mesh_ptr: usize,
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
            camera: OrbitCamera::default(),
            drag: CameraDrag::None,
            last_mesh_ptr: 0,
            bg_color: Color::rgb(0.10, 0.11, 0.13),
            scene: Rc::new(RefCell::new(WgpuSceneRenderer::new())),
        }
    }

    fn current_mesh(&self) -> Option<Arc<MeshGL>> {
        self.inputs.last_mesh_output.lock().ok().and_then(|g| g.clone())
    }

    fn maybe_auto_fit(&mut self, mesh: &MeshGL) {
        let ptr = Arc::as_ptr(&Arc::new(())) as usize; // dummy; replaced below
        // We want pointer-identity tracking on the Arc; fetch a fresh ptr.
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
            self.camera.fit_to_bounds(mn, mx);
            // Sit the floor grid at the model's lowest point.
            self.scene.borrow_mut().grid_y = mn[1];
        }
        let _ = ptr;
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
        let view = self.camera.view_matrix();
        let proj = self.camera.projection_matrix(aspect);
        let mvp = mul4(&proj, &view);
        let (right, up, fwd) = self.camera.basis();
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
            let to_cam = sub3(self.camera.eye(), centroid);
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
        {
            let mut s = self.scene.borrow_mut();
            s.mesh = mesh_opt.clone();
            s.camera = self.camera.clone();
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
    }

    fn claims_pointer_exclusively(&self, _local_pos: Point) -> bool {
        !matches!(self.drag, CameraDrag::None)
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        match event {
            Event::MouseDown { pos, button, .. } => self.on_mouse_down(*pos, *button),
            Event::MouseUp { pos, button, .. } => self.on_mouse_up(*pos, *button),
            Event::MouseMove { pos } => self.on_mouse_move(*pos),
            Event::MouseWheel { delta_y, .. } => self.on_wheel(*delta_y),
            Event::KeyDown { key, modifiers } => self.on_key_down(key, *modifiers),
            _ => EventResult::Ignored,
        }
    }
}

impl Viewport3dWidget {
    fn on_mouse_down(&mut self, pos: Point, button: MouseButton) -> EventResult {
        match button {
            MouseButton::Left => {
                self.drag = CameraDrag::Orbit {
                    start_local: pos,
                    start_az: self.camera.azimuth,
                    start_el: self.camera.elevation,
                };
                EventResult::Consumed
            }
            MouseButton::Right | MouseButton::Middle => {
                self.drag = CameraDrag::Pan {
                    start_local: pos,
                    start_center: self.camera.center,
                };
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        match &self.drag {
            CameraDrag::None => EventResult::Ignored,
            CameraDrag::Orbit { start_local, start_az, start_el } => {
                let dx = (pos.x - start_local.x) as f32;
                let dy = (pos.y - start_local.y) as f32;
                let scale = 0.005;
                self.camera.azimuth = start_az + dx * scale;
                self.camera.elevation = start_el - dy * scale;
                let limit = std::f32::consts::PI * 0.49;
                self.camera.elevation = self.camera.elevation.clamp(-limit, limit);
                EventResult::Consumed
            }
            CameraDrag::Pan { start_local, start_center } => {
                let dx = (pos.x - start_local.x) as f32;
                let dy = (pos.y - start_local.y) as f32;
                // Pan scales with distance so the world point under the
                // cursor stays roughly under the cursor.
                let pan_scale = self.camera.radius * 0.0025;
                let (right, up, _fwd) = self.camera.basis();
                self.camera.center = [
                    start_center[0] - right[0] * dx * pan_scale + up[0] * dy * pan_scale,
                    start_center[1] - right[1] * dx * pan_scale + up[1] * dy * pan_scale,
                    start_center[2] - right[2] * dx * pan_scale + up[2] * dy * pan_scale,
                ];
                EventResult::Consumed
            }
        }
    }

    fn on_mouse_up(&mut self, _pos: Point, _button: MouseButton) -> EventResult {
        if matches!(self.drag, CameraDrag::None) {
            EventResult::Ignored
        } else {
            self.drag = CameraDrag::None;
            EventResult::Consumed
        }
    }

    fn on_wheel(&mut self, delta_y: f64) -> EventResult {
        if delta_y == 0.0 {
            return EventResult::Ignored;
        }
        let factor = if delta_y > 0.0 { 0.9 } else { 1.0 / 0.9 };
        self.camera.zoom(factor as f32);
        EventResult::Consumed
    }

    fn on_key_down(&mut self, key: &Key, _mods: Modifiers) -> EventResult {
        if let Key::Char(c) = key {
            if c.eq_ignore_ascii_case(&'f') {
                if let Some(mesh) = self.current_mesh() {
                    self.last_mesh_ptr = 0;
                    self.maybe_auto_fit(&mesh);
                }
                return EventResult::Consumed;
            }
        }
        EventResult::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn empty_inputs() -> ViewportInputs {
        ViewportInputs {
            last_mesh_output: Arc::new(Mutex::new(None)),
        }
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
