//! Interaction handlers for Viewport3dWidget.
//!
//! Split out of iewport_widget.rs so camera navigation and input
//! handling stay below the repository file-size guardrail.

use super::*;

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

    pub(super) fn on_mouse_down(&mut self, pos: Point, button: MouseButton, mods: Modifiers) -> EventResult {
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

    pub(super) fn on_mouse_move(&mut self, pos: Point) -> EventResult {
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

    pub(super) fn on_wheel(&mut self, delta_y: f64) -> EventResult {
        if delta_y == 0.0 {
            return EventResult::Ignored;
        }
        let factor = if delta_y > 0.0 { 0.9 } else { 1.0 / 0.9 };
        self.cam_mut(|c| c.zoom(factor as f32));
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
