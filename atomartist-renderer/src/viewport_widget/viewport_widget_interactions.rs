//! Interaction handlers for Viewport3dWidget.
//!
//! Split out of iewport_widget.rs so camera navigation and input
//! handling stay below the repository file-size guardrail.

use super::*;
use crate::picking::raycast_mesh;

impl Viewport3dWidget {
    // `orbit_pivot_from_cursor` used to set
    // `camera.center = pivot` and a fresh `radius` on every rotate
    // mouse-down. That snapped the eye to a new world position —
    // visible to the user as a scene "jump" the instant the
    // rotation drag started, with the rotation cursor circle then
    // sitting nowhere near the cursor. The MatterCAD model leaves
    // the camera centre alone on mouse-down and rotates the whole
    // view (eye + centre) around `mouse_down_world_pos` during the
    // drag via `OrbitCamera::orbit_drag_around`. The function is
    // therefore gone — `refresh_pivot` alone seeds the pivot for
    // both the rotate cursor and the per-move rotation math.

    pub(super) fn on_mouse_down(&mut self, pos: Point, button: MouseButton, mods: Modifiers) -> EventResult {
        match button {
            MouseButton::Right => {
                // Right-drag → orbit, pivoting at the cursor hit
                // point. `refresh_pivot` stores the world pivot in
                // `self.mouse_down_world_pos`; the per-frame
                // rotation in `on_mouse_move` then swings both eye
                // and centre around that point via
                // `OrbitCamera::orbit_drag_around` — so nothing
                // moves on the mouse-down itself.
                self.refresh_pivot(pos);
                self.drag = CameraDrag::Orbit { last_local: pos };
                EventResult::Consumed
            }
            MouseButton::Middle => {
                // Resolve the pivot+plane up-front so the on-move
                // pan math can ray-intersect against the saved
                // plane every frame.
                self.refresh_pivot(pos);
                self.drag = CameraDrag::Pan { last_local: pos };
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
                    self.refresh_pivot(pos);
                    self.drag = CameraDrag::Pan { last_local: pos };
                    EventResult::Consumed
                } else if mods.ctrl {
                    self.refresh_pivot(pos);
                    self.drag = CameraDrag::Orbit { last_local: pos };
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
                            self.refresh_pivot(pos);
                            self.drag = CameraDrag::Orbit { last_local: pos };
                        }
                        ViewportTool::Pan => {
                            self.refresh_pivot(pos);
                            self.drag = CameraDrag::Pan { last_local: pos };
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

    pub(super) fn on_mouse_move(&mut self, pos: Point) -> EventResult {
        // Safety net: if a drag is "in progress" but no mouse button
        // is actually held, we must have missed a MouseUp (e.g.
        // released outside the window, focus loss, an aborted
        // touchpad gesture, etc.). Clear the stale drag state so
        // plain hover never orbits / pans / zooms. The cost is one
        // extra `matches!` per MouseMove; the win is that hover
        // can NEVER trigger a camera change, no matter how the OS
        // delivered the prior events.
        if !matches!(self.drag, CameraDrag::None) && !self.any_mouse_button_held() {
            self.drag = CameraDrag::None;
        }
        match &mut self.drag {
            CameraDrag::None => EventResult::Ignored,
            CameraDrag::Orbit { last_local } => {
                // Incremental rotation around the stored world
                // pivot (`mouse_down_world_pos`). Mirrors
                // MatterCAD's `world.RotateAroundPosition(pivot, q)`
                // — both eye and orbit centre swing around the
                // pivot, so the world point that was under the
                // cursor at mouse-down stays glued to the cursor
                // (no scene jump on the first frame either).
                //
                // Sign convention: agg-gui screen Y is up, so a
                // cursor-up drag has dy > 0. We want cursor-up to
                // tilt the camera UP (back vector picks up more
                // +Z) — natural CAD feel — which means orbit_drag's
                // pitch argument should be POSITIVE for dy > 0.
                // So no negation on dy. dx is still negated because
                // a rightward drag (dx > 0) yaws the camera
                // CCW around world +Z (azimuth decreasing).
                let dx = (pos.x - last_local.x) as f32;
                let dy = (pos.y - last_local.y) as f32;
                let scale = 0.005;
                let pivot = self.mouse_down_world_pos;
                *last_local = pos;
                self.inputs
                    .camera
                    .lock()
                    .unwrap()
                    .orbit_drag_around(pivot, -dx * scale, dy * scale);
                EventResult::Consumed
            }
            CameraDrag::Pan { last_local } => {
                // MatterCAD-style plane-anchored pan: intersect the
                // stored `hit_plane` with both the previous and
                // current cursor rays, then shift `center` by the
                // world delta. The world point that was under the
                // cursor on `last` ends up under the cursor again,
                // no matter how the projection / camera distance
                // changes — port of
                // `TrackballTumbleWidgetExtended.Translate`.
                let last = *last_local;
                *last_local = pos;
                let plane = self.hit_plane;
                let w = self.bounds.width.max(1.0);
                let h = self.bounds.height.max(1.0);
                let last_cursor_td = (last.x, h - last.y);
                let curr_cursor_td = (pos.x, h - pos.y);
                let (last_o, last_d, curr_o, curr_d) = {
                    let cam = self.cam();
                    let (lo, ld) = cam.screen_to_ray(last_cursor_td, (w, h));
                    let (co, cd) = cam.screen_to_ray(curr_cursor_td, (w, h));
                    (lo, ld, co, cd)
                };
                let p_last = plane.ray_intersect(last_o, last_d);
                let p_curr = plane.ray_intersect(curr_o, curr_d);
                if let (Some(p_last), Some(p_curr)) = (p_last, p_curr) {
                    let mut c = self.inputs.camera.lock().unwrap();
                    c.center = [
                        c.center[0] - (p_curr[0] - p_last[0]),
                        c.center[1] - (p_curr[1] - p_last[1]),
                        c.center[2] - (p_curr[2] - p_last[2]),
                    ];
                }
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

    pub(super) fn on_mouse_up(&mut self, pos: Point, _button: MouseButton) -> EventResult {
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

    /// Zoom-to-cursor: re-picks the scene at `pos` (mirrors
    /// MatterCAD's `if (TryResolveSceneOrFallbackHit(...)) {
    /// hitPlane = ...; ZoomToWorldPosition(...); mouseDownWorldPosition = ...; }`
    /// path), then scales the orbit radius about that pivot so the
    /// world point under the cursor stays under the cursor across
    /// the zoom step.
    pub(super) fn on_wheel_at_pos(&mut self, pos: Point, delta_y: f64) -> EventResult {
        if delta_y == 0.0 {
            return EventResult::Ignored;
        }
        let factor = if delta_y > 0.0 { 0.9 } else { 1.0 / 0.9 } as f32;
        // Update pivot + plane from the current wheel position.
        let res = self.refresh_pivot(pos);
        let pivot = res.world_pos;
        {
            let mut c = self.inputs.camera.lock().unwrap();
            // Translate the centre so the pivot stays at the same
            // world-relative-to-centre offset times the new scale.
            // Equivalently: center = pivot + (center - pivot) * factor.
            c.center = [
                pivot[0] + (c.center[0] - pivot[0]) * factor,
                pivot[1] + (c.center[1] - pivot[1]) * factor,
                pivot[2] + (c.center[2] - pivot[2]) * factor,
            ];
            c.zoom(factor);
        }
        EventResult::Consumed
    }

    /// Keyboard navigation. Mirrors MatterCAD's documented shortcuts —
    /// see the file-header table.
    pub(super) fn on_key_down(&mut self, key: &Key, mods: Modifiers) -> EventResult {
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
                    // Same convention as the mouse-drag path: dx
                    // negated, dy not. ArrowUp (dy = +1) tilts the
                    // camera UP, matching the cursor's direction
                    // of travel.
                    self.cam_mut(|c| {
                        c.orbit(-dx * ARROW_ORBIT_PX * scale, dy * ARROW_ORBIT_PX * scale)
                    });
                }
                return EventResult::Consumed;
            }
            _ => {}
        }
        EventResult::Ignored
    }
}
