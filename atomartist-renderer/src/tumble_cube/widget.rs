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
    Color, DrawCtx, Event, EventResult, HAnchor, MouseButton, Point, Rect, Size,
    VAnchor, Widget, WidgetBase,
};

use super::cube_geometry::Face;
use super::face_textures::{
    apply_hover_overlay, build_face_textures, clear_hover_overlay, default_hover_overlay,
    FaceTexture,
};
use super::hit_test::{get_hit_data, raycast_unit_cube, HitData};
use super::orient::{target_for_hit, TargetPose};
use super::renderer::TumbleCubeRenderer;
use crate::camera::{OrbitCamera, OrientAnimation};

/// External hooks the cube widget needs.  Identical-in-spirit to
/// [`crate::ViewportInputs`] but trimmed to the surfaces the cube
/// actually touches.
pub struct TumbleCubeInputs {
    /// Shared orbit camera — same `Arc<Mutex<>>` the viewport uses.  The
    /// cube reads `(az, el)` each paint and writes the animation step
    /// result on each subsequent paint.
    pub camera: Arc<Mutex<OrbitCamera>>,
}

#[derive(Clone, Copy, Debug)]
enum CubeDrag {
    None,
    /// Mouse-down recorded but no significant movement yet — promotes to
    /// `Rotating` once threshold passed, or treated as a click on
    /// mouse-up otherwise.
    Pending { start_local: Point },
    /// Drag is rotating the main camera; `start_az` / `start_el` capture
    /// the orientation at the drag's start so the math is stable across
    /// the gesture.
    Rotating {
        start_local: Point,
        start_az: f32,
        start_el: f32,
    },
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
    /// verify the cube can converge a Face::Front click to `(0, 0)`.
    pub fn orient_to_face(&mut self, face: Face) {
        let cam = self.inputs.camera.lock().unwrap().clone();
        let hit = HitData::single(face as u8, 4);
        let Some(target) = target_for_hit(hit) else { return };
        self.animation = Some(OrientAnimation::to_orientation(
            &cam,
            target.azimuth,
            target.elevation,
            0.25,
        ));
    }

    /// Step any in-flight animation toward completion. Returns `true` if
    /// any state changed (the caller should request a repaint).
    fn tick_animation(&mut self, dt_seconds: f32) -> bool {
        let Some(anim) = self.animation.as_mut() else { return false };
        let mut cam = self.inputs.camera.lock().unwrap();
        let finished = anim.step(&mut cam, dt_seconds);
        if finished {
            self.animation = None;
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
        let cam_state = self.cube_camera_snapshot();
        // Build a tiny `OrbitCamera` configured the same way the cube
        // renderer is, then reuse `screen_to_ray` for the unprojection.
        let mut tmp = OrbitCamera::default();
        tmp.azimuth = cam_state.0;
        tmp.elevation = cam_state.1;
        tmp.radius = 3.0;
        tmp.center = [0.0, 0.0, 0.0];
        // Flip Y because screen_to_ray expects top-down coords.
        let cursor_top_down = (pos.x, h - pos.y);
        let (origin, dir) = tmp.screen_to_ray(cursor_top_down, (w, h));
        if let Some(hit_pos) = raycast_unit_cube(origin, dir) {
            get_hit_data(hit_pos)
        } else {
            HitData::empty()
        }
    }

    /// Snapshot `(azimuth, elevation)` from the shared main camera.
    fn cube_camera_snapshot(&self) -> (f32, f32) {
        let c = self.inputs.camera.lock().unwrap();
        (c.azimuth, c.elevation)
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
        // The cube wants a fixed 100×100 square; use the smaller of the
        // requested size or our max.
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
        let _ = self.tick_animation(dt);

        // Sync the cube renderer's CPU face buffer to the latest hover
        // state and mirror the main camera's orientation.
        let faces = self.faces_cpu.borrow();
        let (az, el) = self.cube_camera_snapshot();
        {
            let mut s = self.scene.borrow_mut();
            s.faces_cpu = faces.clone_for_renderer();
            s.set_orientation(az, el);
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
        let (start_az, start_el) = self.cube_camera_snapshot();
        let _ = start_el; // kept for clarity; pending uses local pos only
        self.drag = CubeDrag::Pending { start_local: pos };
        let _ = start_az;
        // Cancel any in-flight animation so the user takes direct
        // control of the camera.
        self.animation = None;
        EventResult::Consumed
    }

    fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        // Promote pending to rotating once the user has moved enough,
        // matching MatterCAD's drag threshold.
        let promote = matches!(
            self.drag,
            CubeDrag::Pending { start_local } if (pos.x - start_local.x).abs() > 2.0
                || (pos.y - start_local.y).abs() > 2.0
        );
        if promote {
            let (az, el) = self.cube_camera_snapshot();
            if let CubeDrag::Pending { start_local } = self.drag {
                self.drag = CubeDrag::Rotating {
                    start_local,
                    start_az: az,
                    start_el: el,
                };
            }
        }

        match self.drag {
            CubeDrag::Rotating { start_local, start_az, start_el } => {
                let dx = (pos.x - start_local.x) as f32;
                let dy = (pos.y - start_local.y) as f32;
                let scale = 0.01;
                let mut c = self.inputs.camera.lock().unwrap();
                c.azimuth = start_az + dx * scale;
                c.elevation = start_el - dy * scale;
                let limit = std::f32::consts::PI * 0.49;
                c.elevation = c.elevation.clamp(-limit, limit);
                // Clear hover overlays during drag — the cursor isn't
                // hovering a tile, it's manipulating the cube.
                drop(c);
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
                    if let Some(TargetPose { azimuth, elevation }) = target_for_hit(hit) {
                        let cam = self.inputs.camera.lock().unwrap().clone();
                        self.animation = Some(OrientAnimation::to_orientation(
                            &cam, azimuth, elevation, 0.25,
                        ));
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
