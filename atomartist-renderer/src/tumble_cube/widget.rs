//! Tumble-cube widget — port of `TumbleCubeControl.cs`.
//!
//! Anchored to the top-right corner of `Viewport3dWidget`, the cube:
//!   - mirrors the main viewport's `(azimuth, elevation)` each paint so
//!     its orientation reflects the user's view;
//!   - on hover, highlights the face / edge / corner under the cursor
//!     (paints overlay tiles into the affected `FaceTexture::active`
//!     buffers and re-uploads to the GPU);
//!   - on click, animates the main camera to look at the picked
//!     face / edge / corner (`OrientAnimation` slerps `(az, el)`);
//!   - on drag, orbits the main camera as if the user grabbed the
//!     world cube directly (port of MatterCAD's
//!     `TrackballTumbleWidgetExtended.StartRotateAroundOrigin /
//!     DoRotateAroundOrigin / EndRotateAroundOrigin`).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use agg_gui::{
    animation::request_draw,
    Color, DrawCtx, Event, EventResult, HAnchor, MouseButton, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};
use glam::Quat;

use super::cube_geometry::Face;
use super::face_textures::{
    apply_hover_overlay, build_face_textures, clear_hover_overlay, default_hover_overlay,
    FaceTexture,
};
use super::hit_test::{get_hit_data, raycast_unit_cube, HitData};
use super::orient::{target_for_hit, TargetPose};
use super::renderer::{TumbleCubeRenderer, TUMBLE_CUBE_CAMERA_RADIUS, TUMBLE_CUBE_MODEL_SCALE};
use crate::camera::OrbitCamera;
use crate::camera_animations::OrientAnimation;

/// External hooks the cube widget needs.  Identical-in-spirit to
/// [`crate::ViewportInputs`] but trimmed to the surfaces the cube
/// actually touches.
pub struct TumbleCubeInputs {
    /// Shared orbit camera — same `Arc<Mutex<>>` the viewport uses.  The
    /// cube reads `(az, el)` each paint and writes the animation step
    /// result on each subsequent paint.
    pub camera: Arc<Mutex<OrbitCamera>>,
    /// Optional completion hook fired exactly once after a click-to-orient
    /// animation reaches its final camera pose. Mirrors MatterCAD's
    /// `AnimateRotation(..., Action after = null)` completion callback.
    pub animation_completed: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone, Copy, Debug)]
enum CubeDrag {
    None,
    /// Mouse-down recorded but no significant movement yet — promotes to
    /// `Rotating` once threshold passed, or treated as a click on
    /// mouse-up otherwise.
    Pending { start_local: Point },
    /// Drag is rotating the main camera. We track the previous cursor
    /// sample so each `MouseMove` can apply an incremental rotation
    /// via `OrbitCamera::orbit_drag`, which honours the active
    /// `OrbitMode`. Mirrors MatterCAD's
    /// `DoRotateAroundOrigin` which updates
    /// `rotationStartPosition = mousePosition` per frame.
    Rotating { last_local: Point },
}

pub struct TumbleCubeWidget {
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    base: WidgetBase,
    inputs: TumbleCubeInputs,
    drag: CubeDrag,
    /// Last hit recorded by mouse-move so we only re-upload the texture
    /// when the tile under the cursor actually changes.
    last_hit: HitData,
    /// Per-face CPU pixel buffers.  Mirrored to the GPU by the renderer.
    faces_cpu: Rc<RefCell<Vec<FaceTexture>>>,
    /// wgpu-backed renderer; same `Rc<RefCell<>>` is pushed into the
    /// `WgpuGfxCtx` each paint via `push_custom_render`.
    scene: Rc<RefCell<TumbleCubeRenderer>>,
    /// In-flight click-to-orient animation.  `Some` while the camera is
    /// slerping toward a clicked face / edge / corner; cleared on done.
    animation: Option<OrientAnimation>,
    /// Wall-clock instant of the last paint, used to step `animation`
    /// at real elapsed time.
    last_paint_ms: Option<f64>,
}

impl TumbleCubeWidget {
    pub fn new(inputs: TumbleCubeInputs) -> Self {
        // Build CPU textures; without a font installed (tests) the
        // labels paint as background only — still valid.
        let font = agg_gui::font_settings::current_system_font();
        let faces = build_face_textures(font.as_ref());
        let faces_vec: Vec<FaceTexture> = faces.into_iter().collect();
        // Move into the Rc<RefCell<>> immediately so the renderer can
        // borrow each frame without re-allocating.
        let faces_cpu = Rc::new(RefCell::new(faces_vec));
        let scene = Rc::new(RefCell::new(TumbleCubeRenderer::new(
            faces_cpu.borrow().clone_for_renderer(),
        )));
        Self {
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            children: Vec::new(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::FIT)
                .with_v_anchor(VAnchor::FIT)
                .with_min_size(Size::new(100.0, 100.0))
                .with_max_size(Size::new(100.0, 100.0)),
            inputs,
            drag: CubeDrag::None,
            last_hit: HitData::empty(),
            faces_cpu,
            scene,
            animation: None,
            last_paint_ms: None,
        }
    }

    /// Convenience for tests: drive an orient animation to completion
    /// against the given camera handle.  Used by the orient test to
    /// verify the cube can converge a Face::Front click to identity.
    pub fn orient_to_face(&mut self, face: Face) {
        let cam = self.inputs.camera.lock().unwrap().clone();
        let hit = HitData::single(face as u8, 4);
        let Some(target) = target_for_hit(hit) else { return };
        self.animation = Some(OrientAnimation::to_orientation(
            &cam,
            target.orientation,
            0.25,
        ));
        request_draw();
    }

    /// Test / harness helper: whether a click-to-orient animation is
    /// currently in flight.
    pub fn animation_active(&self) -> bool {
        self.animation.is_some()
    }

    /// Test / harness helper: advance an in-flight animation by an
    /// explicit delta. Production uses [`Self::paint`] to tick at a
    /// nominal 60 Hz and request the next frame.
    pub fn step_animation_for_test(&mut self, dt_seconds: f32) -> bool {
        self.tick_animation(dt_seconds)
    }

    /// Step any in-flight animation toward completion. Returns `true` if
    /// any state changed (the caller should request a repaint).
    fn tick_animation(&mut self, dt_seconds: f32) -> bool {
        let Some(anim) = self.animation.as_mut() else { return false };
        let mut cam = self.inputs.camera.lock().unwrap();
        let finished = anim.step(&mut cam, dt_seconds);
        if finished {
            self.animation = None;
            drop(cam);
            if let Some(cb) = &self.inputs.animation_completed {
                cb();
            }
        } else {
            // Keep the host event loop drawing until the animation has
            // completed. Without this, the camera can move for one paint
            // and then stall until another unrelated event requests a
            // frame.
            request_draw();
        }
        true
    }

    /// Apply a hover overlay to the painted-active face textures so the
    /// renderer re-uploads them on next paint.  Clears any prior
    /// overlay first.
    fn apply_hit_overlay(&self, hit: HitData) {
        let mut faces = self.faces_cpu.borrow_mut();
        // Reset every face first — simpler than tracking which ones we
        // dirtied last frame.  Each `clear_hover_overlay` is a single
        // `Vec::clone_from`, cheap on 256×256 buffers.
        for f in faces.iter_mut() {
            clear_hover_overlay(f);
        }
        for slot in hit.face_tile.iter() {
            let Some((face_idx, tile)) = slot else { continue };
            if let Some(face) = faces.get_mut(*face_idx as usize) {
                apply_hover_overlay(face, *tile, default_hover_overlay());
            }
        }
    }

    /// Convert a widget-local cursor position to a world-space ray on
    /// the cube's mini-viewport, then resolve to `HitData`.
    fn hit_for_local_pos(&self, pos: Point) -> HitData {
        let (w, h) = (self.bounds.width.max(1.0), self.bounds.height.max(1.0));
        let cam_orientation = self.cube_camera_snapshot();
        // Build a tiny `OrbitCamera` configured the same way the cube
        // renderer is, then reuse `screen_to_ray` for the unprojection.
        let mut tmp = OrbitCamera::default();
        tmp.orientation = cam_orientation;
        tmp.radius = TUMBLE_CUBE_CAMERA_RADIUS;
        tmp.center = [0.0, 0.0, 0.0];
        // Flip Y because screen_to_ray expects top-down coords.
        let cursor_top_down = (pos.x, h - pos.y);
        let (origin, dir) = tmp.screen_to_ray(cursor_top_down, (w, h));
        // The renderer draws the cube with a uniform model scale of
        // TUMBLE_CUBE_MODEL_SCALE.  Transform the ray into the cube's
        // unscaled model space before intersecting the canonical
        // [-1, 1]^3 box. The returned hit point is then already in the
        // model-space convention `get_hit_data` expects.
        if let Some(hit_pos) = raycast_rendered_cube(origin, dir) {
            get_hit_data(hit_pos)
        } else {
            HitData::empty()
        }
    }

    /// Snapshot the orientation quaternion from the shared main camera.
    fn cube_camera_snapshot(&self) -> Quat {
        self.inputs.camera.lock().unwrap().orientation
    }

    /// Push the renderer's custom-render command into the wgpu context,
    /// if the active `DrawCtx` is a `WgpuGfxCtx`. Mirrors
    /// [`crate::Viewport3dWidget::try_push_wgpu_render`].
    fn try_push_wgpu_render(&self, ctx: &mut dyn DrawCtx, w: f64, h: f64) -> bool {
        let any = match ctx.as_any_mut() { Some(a) => a, None => return false };
        let wgpu_ctx = match any.downcast_mut::<demo_wgpu::WgpuGfxCtx>() {
            Some(c) => c,
            None => return false,
        };
        let t = wgpu_ctx.transform();
        let mut x0 = 0.0;
        let mut y0 = 0.0;
        t.transform(&mut x0, &mut y0);
        let mut x1 = w;
        let mut y1 = h;
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
}

/// Intersect a world-space ray with the cube as it is actually drawn:
/// geometry in `[-1, 1]^3` after a uniform model scale of
/// `TUMBLE_CUBE_MODEL_SCALE`.  Returns the model-space hit position
/// because `get_hit_data` expects canonical unit-cube coordinates.
fn raycast_rendered_cube(origin: [f32; 3], dir: [f32; 3]) -> Option<[f32; 3]> {
    let inv_scale = 1.0 / TUMBLE_CUBE_MODEL_SCALE.max(1e-6);
    let model_origin = mul3(origin, inv_scale);
    let model_dir = mul3(dir, inv_scale);
    raycast_unit_cube(model_origin, model_dir)
}

fn mul3(v: [f32; 3], s: f32) -> [f32; 3] {
    [v[0] * s, v[1] * s, v[2] * s]
}

impl Widget for TumbleCubeWidget {
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, b: Rect) { self.bounds = b; }
    fn children(&self) -> &[Box<dyn Widget>] { &self.children }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> { &mut self.children }
    fn type_name(&self) -> &'static str { "TumbleCubeWidget" }
    fn id(&self) -> Option<&str> { Some("tumble-cube") }
    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn min_size(&self) -> Size { self.base.min_size }
    fn max_size(&self) -> Size { self.base.max_size }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        // Fixed 100×100 square — matches MatterCAD's `TumbleCubeControl`
        // size (`100 * GuiWidget.DeviceScale`).
        let w = available.width.min(100.0).max(10.0);
        let h = available.height.min(100.0).max(10.0);
        let side = w.min(h);
        self.bounds = Rect::new(0.0, 0.0, side, side);
        Size::new(side, side)
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        let w = self.bounds.width;
        let h = self.bounds.height;
        if w <= 0.0 || h <= 0.0 { return; }

        // Tick the orient animation using real elapsed wall-clock time.
        // `Instant` is unavailable on WASM but a monotonic counter from
        // the platform's `performance.now()` shim would work; for now we
        // use a constant 16 ms per paint when no timestamp is available
        // — close enough for a 250 ms snap.
        let dt = 0.016f32;
        let changed = self.tick_animation(dt);
        if changed && self.animation.is_some() {
            request_draw();
        }

        // Sync the cube renderer's CPU face buffer to the latest hover
        // state and mirror the main camera's orientation.
        let faces = self.faces_cpu.borrow();
        let orientation = self.cube_camera_snapshot();
        {
            let mut s = self.scene.borrow_mut();
            s.faces_cpu = faces.clone_for_renderer();
            s.set_orientation(orientation);
        }
        drop(faces);

        // Push the wgpu custom render. If we're on the software backend
        // we paint a placeholder background so the user at least sees
        // *something* in that slot.
        let used_wgpu = self.try_push_wgpu_render(ctx, w, h);
        if !used_wgpu {
            ctx.set_fill_color(Color::rgba(0.0, 0.0, 0.0, 0.10));
            ctx.begin_path();
            ctx.rect(0.0, 0.0, w, h);
            ctx.fill();
        }

        let _ = self.last_paint_ms; // reserved for future timestamp-aware stepping
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        match event {
            Event::MouseDown { pos, button, .. } => self.on_mouse_down(*pos, *button),
            Event::MouseMove { pos } => self.on_mouse_move(*pos),
            Event::MouseUp { pos, button, .. } => self.on_mouse_up(*pos, *button),
            _ => EventResult::Ignored,
        }
    }
}

impl TumbleCubeWidget {
    fn on_mouse_down(&mut self, pos: Point, button: MouseButton) -> EventResult {
        if button != MouseButton::Left {
            return EventResult::Ignored;
        }
        self.drag = CubeDrag::Pending { start_local: pos };
        // Cancel any in-flight animation so the user takes direct
        // control of the camera.
        self.animation = None;
        EventResult::Consumed
    }

    fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        // Promote pending to rotating once the user has moved enough,
        // matching MatterCAD's drag threshold.
        if let CubeDrag::Pending { start_local } = self.drag {
            if (pos.x - start_local.x).abs() > 2.0 || (pos.y - start_local.y).abs() > 2.0 {
                self.drag = CubeDrag::Rotating { last_local: start_local };
            }
        }

        match &mut self.drag {
            CubeDrag::Rotating { last_local } => {
                let dx = (pos.x - last_local.x) as f32;
                let dy = (pos.y - last_local.y) as f32;
                let scale = 0.01;
                // Match the viewport's right-drag direction (drag
                // right = world follows finger). The HUD's orbit code
                // applies the same negation; do it here too so the
                // cube's incremental orbit_drag receives consistent
                // signs regardless of `orbit_mode`.
                self.inputs
                    .camera
                    .lock()
                    .unwrap()
                    .orbit_drag(-dx * scale, -dy * scale);
                *last_local = pos;
                // Clear hover overlays during drag — the cursor isn't
                // hovering a tile, it's manipulating the cube.
                if !matches!(self.last_hit, HitData { face_tile: [None, None, None] }) {
                    self.apply_hit_overlay(HitData::empty());
                    self.last_hit = HitData::empty();
                }
                EventResult::Consumed
            }
            CubeDrag::Pending { .. } => EventResult::Consumed,
            CubeDrag::None => {
                // Hover-only path: highlight the tile under the cursor.
                let hit = self.hit_for_local_pos(pos);
                if hit != self.last_hit {
                    self.apply_hit_overlay(hit);
                    self.last_hit = hit;
                }
                EventResult::Ignored
            }
        }
    }

    fn on_mouse_up(&mut self, pos: Point, button: MouseButton) -> EventResult {
        if button != MouseButton::Left {
            return EventResult::Ignored;
        }
        let prev = std::mem::replace(&mut self.drag, CubeDrag::None);
        match prev {
            CubeDrag::Pending { start_local } => {
                // Treat as a click → animate toward the picked face.
                let dx = (pos.x - start_local.x).abs();
                let dy = (pos.y - start_local.y).abs();
                if dx <= 2.0 && dy <= 2.0 {
                    let hit = self.hit_for_local_pos(pos);
                    if let Some(TargetPose { orientation }) = target_for_hit(hit) {
                        let cam = self.inputs.camera.lock().unwrap().clone();
                        self.animation = Some(OrientAnimation::to_orientation(
                            &cam, orientation, 0.25,
                        ));
                        request_draw();
                    }
                }
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }
}

// ---------------------------------------------------------------------------
// Helper for cloning the face-texture vector into the renderer's owned
// storage.  We can't `Clone` `FaceTexture` directly because it carries
// large `Vec<u8>` buffers and we want explicit, named cloning at the
// call sites that need it.
// ---------------------------------------------------------------------------

trait CloneForRenderer {
    fn clone_for_renderer(&self) -> Vec<FaceTexture>;
}

impl CloneForRenderer for Vec<FaceTexture> {
    fn clone_for_renderer(&self) -> Vec<FaceTexture> {
        self.iter()
            .map(|f| FaceTexture {
                face: f.face,
                source: f.source.clone(),
                active: f.active.clone(),
                dirty: f.dirty,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn orient_to_face_runs_to_completion_and_fires_hook_once() {
        let camera = Arc::new(Mutex::new(OrbitCamera::default()));
        {
            let mut c = camera.lock().unwrap();
            // Start away from Front so the test verifies the
            // animation actually moves the orientation.
            c.orientation = Quat::from_rotation_y(1.25) * Quat::from_rotation_x(-0.65);
        }

        let completed = Arc::new(AtomicUsize::new(0));
        let completed_cb = completed.clone();
        let mut widget = TumbleCubeWidget::new(TumbleCubeInputs {
            camera: camera.clone(),
            animation_completed: Some(Arc::new(move || {
                completed_cb.fetch_add(1, Ordering::SeqCst);
            })),
        });

        widget.orient_to_face(Face::Front);
        assert!(widget.animation_active(), "orient_to_face should start an animation");

        // Step past the 0.25s duration in several smaller increments
        // to exercise interpolation rather than only the final clamp.
        for _ in 0..20 {
            widget.step_animation_for_test(0.016);
            if !widget.animation_active() {
                break;
            }
        }

        assert!(!widget.animation_active(), "animation should run to completion");
        assert_eq!(completed.load(Ordering::SeqCst), 1, "completion hook should fire once");

        // Z-up Front view: camera at -Y looking +Y. The orientation's
        // back vector (eye-from-centre) lands on -Y.
        let c = camera.lock().unwrap();
        let back = c.orientation * glam::Vec3::Z;
        assert!(
            (back - glam::Vec3::NEG_Y).length() < 1e-3,
            "Front view should land at back = -Y, got {back:?}"
        );
    }

    #[test]
    fn raycast_uses_rendered_scaled_cube_bounds() {
        // X=0.9 would hit the old unscaled [-1, 1] cube, but should
        // miss the rendered cube because it is scaled to [-0.8, 0.8].
        let miss = raycast_rendered_cube([0.9, 0.0, 5.0], [0.0, 0.0, -1.0]);
        assert!(miss.is_none(), "ray outside the scaled cube should miss");

        let hit = raycast_rendered_cube([0.7, 0.0, 5.0], [0.0, 0.0, -1.0])
            .expect("ray inside the scaled cube should hit");
        // Returned hit is model-space, so the front face is still z=1.
        assert!((hit[2] - 1.0).abs() < 1e-4, "hit z = {}", hit[2]);
    }
}
