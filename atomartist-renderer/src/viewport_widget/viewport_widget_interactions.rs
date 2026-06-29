//! Interaction handlers for Viewport3dWidget.
//!
//! Split out of iewport_widget.rs so camera navigation and input
//! handling stay below the repository file-size guardrail.

use super::*;
use super::body_drag;
use super::viewport_widget_helpers::selected_body_world_aabb;
use crate::picking::{pick_origin, HitPlane};

// Rotate-gizmo interaction logic (hover pick, drag-start, per-frame
// rotation, angle snapping) lives in `rotate_interactions.rs` to keep
// this file under the line guardrail.

/// Pixel distance the cursor must travel before a `Selecting` state
/// promotes to a body drag. 5 px lets real human clicks (which can
/// jitter 2-4 px during the press+release window on Windows) commit
/// as click-selects instead of accidentally translating the body.
const DRAG_THRESHOLD_PX: f64 = 5.0;

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
                            // Hit-test in priority order:
                            //   1. Z control sphere — small target,
                            //      must win over the body underneath.
                            //   2. Rotate ring handle — also a small
                            //      target sitting off to the side of
                            //      the body; must win over the body.
                            //   3. Anything else (body or empty) →
                            //      Selecting variant with the body
                            //      pick captured so mouse-up can
                            //      reliably select, and mouse-move
                            //      past threshold can promote to
                            //      `DragBodyXY` for translate.
                            if let Some(pending) = self.try_start_z_drag(pos) {
                                self.drag = pending;
                            } else if let Some(pending) = self.try_start_height_drag(pos) {
                                self.drag = pending;
                            } else if let Some(pending) = self.try_start_rotate_drag(pos) {
                                self.drag = pending;
                            } else {
                                let (picked_body, anchor_bed_pt) =
                                    self.pick_body_and_bed_anchor(pos);
                                self.drag = CameraDrag::Selecting {
                                    start_local: pos,
                                    moved: false,
                                    picked_body,
                                    anchor_bed_pt,
                                };
                            }
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
        // Per-frame rotate-handle hover (MatterCAD's `MouseIsOver`),
        // tracked only while idle. A hovered handle paints accent and
        // shows its rotation compass. During any drag the hovered axis
        // is meaningless (the handles are moving / a rotation is in
        // flight), so clear it. Done before the `&mut self.drag` match
        // so the immutable hover pick doesn't fight the borrow.
        // Hover priority matches the mouse-down hit-test order:
        // Z-translate cone → height box → rotate handles. Only one
        // control lights at a time.
        let (new_z_hover, new_height_hover, new_axis_hover) =
            if matches!(self.drag, CameraDrag::None) {
                let z = self.pick_z_hover(pos);
                let height = if z { false } else { self.pick_height_hover(pos) };
                let axis = if z || height { None } else { self.pick_rotate_hover(pos) };
                (z, height, axis)
            } else {
                (false, false, None)
            };
        if new_z_hover != self.hovered_z_control
            || new_height_hover != self.hovered_height_control
            || new_axis_hover != self.hovered_rotate_axis
        {
            self.hovered_z_control = new_z_hover;
            self.hovered_height_control = new_height_hover;
            self.hovered_rotate_axis = new_axis_hover;
            agg_gui::animation::request_draw();
        }
        // Orbit / pan / zoom drag branches all mutate camera state
        // and need to claim a redraw — the native shell now runs
        // a fully reactive loop (matches agg-gui's demo) and won't
        // pump a frame just because the cursor moved.  Without this
        // call, drags would stop visibly updating between mouse
        // events.  The plain `CameraDrag::None` / `Selecting`
        // branches are NOT camera-mutating, so we skip the call
        // there to avoid spuriously re-painting on hover.
        let did_change = !matches!(
            self.drag,
            CameraDrag::None | CameraDrag::Selecting { .. },
        );
        if did_change {
            agg_gui::animation::request_draw();
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
            CameraDrag::Selecting {
                start_local,
                moved,
                picked_body,
                anchor_bed_pt,
            } => {
                let dx = (pos.x - start_local.x).abs();
                let dy = (pos.y - start_local.y).abs();
                let past_threshold = dx > DRAG_THRESHOLD_PX || dy > DRAG_THRESHOLD_PX;
                if !*moved && past_threshold {
                    // Crossed the 2-px threshold for the first time —
                    // try promoting to `DragBodyXY` so the body
                    // follows the cursor. Promotion requires:
                    //   * A picked body (mouse-down landed on one),
                    //   * A bed-plane anchor (the click ray crossed
                    //     Z = 0 somewhere),
                    //   * A writable matrix on that body.
                    // If any check fails we stay in `Selecting` with
                    // `moved = true` — selection still works on
                    // mouse-up, just no translate.
                    if let (Some(node_id), Some(anchor)) =
                        (*picked_body, *anchor_bed_pt)
                    {
                        if let Some(start_matrix) =
                            self.inputs.read_node_matrix(node_id)
                        {
                            // Grid snap aligns the AABB side nearest
                            // the grab point (MatterCAD's HitQuadrant
                            // logic): grab the right half → the right
                            // edge snaps, etc. (Field access instead of
                            // `current_geometry()` — the enclosing
                            // match still mutably borrows `self.drag`.)
                            let geom = self
                                .inputs
                                .last_mesh_output
                                .lock()
                                .ok()
                                .and_then(|g| g.clone());
                            let snap_edge_xy =
                                selected_body_world_aabb(geom.as_deref(), node_id)
                                    .map(|(mn, mx)| {
                                        let cx = (mn[0] + mx[0]) * 0.5;
                                        let cy = (mn[1] + mx[1]) * 0.5;
                                        [
                                            if anchor[0] > cx { mx[0] } else { mn[0] },
                                            if anchor[1] > cy { mx[1] } else { mn[1] },
                                        ]
                                    })
                                    .unwrap_or([0.0, 0.0]);
                            self.drag = CameraDrag::DragBodyXY {
                                node_id,
                                start_local: *start_local,
                                moved: true,
                                anchor_bed_pt: anchor,
                                start_matrix,
                                snap_edge_xy,
                            };
                            return EventResult::Consumed;
                        }
                    }
                }
                *moved = *moved || past_threshold;
                EventResult::Consumed
            }
            CameraDrag::DragBodyZ { .. } => {
                if let CameraDrag::DragBodyZ {
                    node_id,
                    anchor_xy,
                    anchor_z,
                    start_matrix,
                    start_bottom_z,
                    ..
                } = self.drag.clone()
                {
                    let w = self.bounds.width.max(1.0);
                    let h = self.bounds.height.max(1.0);
                    let cursor_td = (pos.x, h - pos.y);
                    let (ray_o, ray_d) = {
                        let cam = self.cam();
                        cam.screen_to_ray(cursor_td, (w, h))
                    };
                    if let Some(cur_z) =
                        body_drag::z_axis_translation(ray_o, ray_d, anchor_xy)
                    {
                        let mut dz = cur_z - anchor_z;
                        // Grid snap aligns the body's *bottom position*
                        // (MatterCAD `MoveInZControl`: `newZPosition`
                        // rounds to the grid).
                        let snap = self.inputs.snap();
                        if snap > 0.0 {
                            let bottom =
                                ((start_bottom_z + dz) / snap).round() * snap;
                            dz = bottom - start_bottom_z;
                        }
                        let new_matrix =
                            body_drag::z_translation(start_matrix, 0.0, dz);
                        self.inputs.push_node_matrix(node_id, new_matrix);
                        if let CameraDrag::DragBodyZ { live_dz, .. } = &mut self.drag {
                            *live_dz = dz;
                        }
                    }
                }
                EventResult::Consumed
            }
            CameraDrag::RotateBodyAxis { .. } => {
                // Per-frame rotation lives in `rotate_interactions.rs`.
                self.drag_rotate(pos);
                EventResult::Consumed
            }
            CameraDrag::DragBodyHeight { .. } => {
                // Per-frame height/scale-Z lives in `scale_interactions.rs`.
                self.drag_height(pos);
                EventResult::Consumed
            }
            CameraDrag::DragBodyXY { .. } => {
                // Branch-local borrow management: pull the fields out
                // so the mutable `drag` borrow is dropped before we
                // call back into `self.cam()` / `self.inputs.*`.
                if let CameraDrag::DragBodyXY {
                    node_id,
                    start_local,
                    moved,
                    anchor_bed_pt,
                    start_matrix,
                    snap_edge_xy,
                } = self.drag.clone()
                {
                    let dx = (pos.x - start_local.x).abs();
                    let dy = (pos.y - start_local.y).abs();
                    let new_moved = moved || dx > DRAG_THRESHOLD_PX || dy > DRAG_THRESHOLD_PX;
                    let w = self.bounds.width.max(1.0);
                    let h = self.bounds.height.max(1.0);
                    let cursor_td = (pos.x, h - pos.y);
                    let (ray_o, ray_d) = {
                        let cam = self.cam();
                        cam.screen_to_ray(cursor_td, (w, h))
                    };
                    // Drag-anchor plane uses the body's Z at drag
                    // start — anchor_bed_pt[2] — so cursor and body
                    // stay locked at any camera angle.
                    let plane = HitPlane {
                        point: [0.0, 0.0, anchor_bed_pt[2]],
                        normal: [0.0, 0.0, 1.0],
                    };
                    if let Some(mut cur) = plane.ray_intersect(ray_o, ray_d) {
                        // Grid snap aligns the grabbed AABB side
                        // (MatterCAD `DragSelectedObject`): the edge's
                        // landing position rounds to the grid, and the
                        // delta is adjusted to put it there.
                        let snap = self.inputs.snap();
                        if snap > 0.0 {
                            for k in 0..2 {
                                let delta = cur[k] - anchor_bed_pt[k];
                                let landed = snap_edge_xy[k] + delta;
                                let snapped = (landed / snap).round() * snap;
                                cur[k] = anchor_bed_pt[k] + (snapped - snap_edge_xy[k]);
                            }
                        }
                        let new_matrix = body_drag::bed_plane_translation(
                            start_matrix,
                            anchor_bed_pt,
                            cur,
                        );
                        self.inputs.push_node_matrix(node_id, new_matrix);
                    }
                    // Stash moved-flag update back.
                    self.drag = CameraDrag::DragBodyXY {
                        node_id,
                        start_local,
                        moved: new_moved,
                        anchor_bed_pt,
                        start_matrix,
                        snap_edge_xy,
                    };
                }
                EventResult::Consumed
            }
        }
    }

    pub(super) fn on_mouse_up(&mut self, _pos: Point, _button: MouseButton) -> EventResult {
        let prev = std::mem::replace(&mut self.drag, CameraDrag::None);
        match prev {
            CameraDrag::None => EventResult::Ignored,
            CameraDrag::Selecting {
                moved,
                picked_body,
                ..
            } => {
                // Selection commits whenever a body was under the
                // cursor at mouse-down — independent of whether the
                // mouse jittered past the 2-px drag threshold. The
                // "moved past threshold but didn't promote to
                // DragBodyXY" case happens when the picked node's
                // `matrix` property isn't writable; we still want a
                // selection. Real human clicks regularly include 1-3
                // px jitter, so a strict `!moved` guard would lose
                // most real clicks.
                //
                // Empty-space click (`picked_body = None`) without
                // movement clears the selection — matches the
                // historical behaviour. Empty-space drag leaves
                // selection alone (the user was probably aiming for
                // a camera gesture and missed the modifier).
                if picked_body.is_some() {
                    self.commit_selection(picked_body);
                } else if !moved {
                    self.commit_selection(None);
                }
                EventResult::Consumed
            }
            CameraDrag::DragBodyXY { node_id, .. } => {
                // Drag committed — the body's new position is
                // already in the graph + on the undo stack. Make
                // sure selection follows the dragged body (matches
                // NodeDesigner: pick-up + drop → that node becomes
                // the active selection).
                self.commit_selection(Some(node_id));
                EventResult::Consumed
            }
            CameraDrag::DragBodyZ { node_id, .. } => {
                self.commit_selection(Some(node_id));
                EventResult::Consumed
            }
            CameraDrag::RotateBodyAxis { node_id, .. } => {
                // Rotation committed — the body's new orientation is
                // already in the graph + coalesced onto the undo stack.
                // Keep the rotated body selected (matches the translate
                // gizmos).
                self.commit_selection(Some(node_id));
                EventResult::Consumed
            }
            CameraDrag::DragBodyHeight { node_id, .. } => {
                // Height/scale committed (height param or matrix Z is
                // already coalesced on the undo stack). Keep selected.
                self.commit_selection(Some(node_id));
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }

    /// Write the viewport-driven selection through the shared
    /// `selection` mutex AND request a global redraw so the canvas
    /// widget (which reads `primary_selection()` at paint time)
    /// picks up the change. agg-gui's reactive paint loop does NOT
    /// poll mutex-wrapped state — a write without an explicit
    /// request goes invisible until the next unrelated event.
    fn commit_selection(&self, id: Option<NodeId>) {
        *self.inputs.selection.lock().unwrap() = id;
        agg_gui::animation::request_draw();
    }

    /// Cancel an in-flight body drag — XY bed-plane, Z-control, or a
    /// rotation — restoring the node's matrix to the snapshot captured
    /// at drag-start and clearing the drag. Mirrors MatterCAD's `Esc`
    /// → `CancelOperation`. Returns `true` when a cancelable drag was
    /// active (so the caller can consume the key); `false` for camera
    /// drags / `Selecting` / no drag (Esc should fall through to e.g.
    /// dismiss a menu).
    pub(super) fn cancel_active_drag(&mut self) -> bool {
        // Push the pre-drag value(s) back through the same coalesced
        // write the drag used, so the in-progress stroke collapses to a
        // no-op and the body snaps home; then drop the drag state.
        match self.drag.clone() {
            CameraDrag::DragBodyXY { node_id, start_matrix, .. }
            | CameraDrag::DragBodyZ { node_id, start_matrix, .. }
            | CameraDrag::RotateBodyAxis { node_id, start_matrix, .. } => {
                self.inputs.push_node_matrix(node_id, start_matrix);
            }
            CameraDrag::DragBodyHeight { node_id, start_matrix, start_height, .. } => {
                // Field path restores through the same atomic pair
                // write the drag used, so the revert coalesces into the
                // stroke's single ChangePropsCmd (one no-op undo entry);
                // matrix path restores the matrix alone.
                match start_height {
                    Some(h0) => self.inputs.push_node_number_and_matrix(
                        node_id,
                        "height",
                        h0,
                        start_matrix,
                    ),
                    None => self.inputs.push_node_matrix(node_id, start_matrix),
                }
            }
            _ => return false,
        }
        self.drag = CameraDrag::None;
        agg_gui::animation::request_draw();
        true
    }

    pub(super) fn read_node_matrix(&self, id: NodeId) -> Option<[f32; 16]> {
        self.inputs.read_node_matrix(id)
    }

    /// Mouse-down body pick + bed-plane anchor. Captures both pieces
    /// up front so the `Selecting` state can either:
    ///
    /// * Become a click-select on mouse-up `!moved` (uses
    ///   `picked_body`), or
    /// * Promote to `DragBodyXY` on mouse-move past the 2-px
    ///   threshold (uses `picked_body` + `anchor_bed_pt`).
    ///
    /// Both fields are independently optional: empty-space click has
    /// neither (returns `(None, Some)` or `(None, None)`), and a
    /// camera angle that's exactly parallel to the bed leaves
    /// `anchor_bed_pt` `None` even when a body was hit (the drag
    /// just doesn't promote — selection still works).
    pub(super) fn pick_body_and_bed_anchor(
        &self,
        pos: Point,
    ) -> (Option<NodeId>, Option<[f32; 3]>) {
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let cursor_td = (pos.x, h - pos.y);
        let (origin, dir) = self.cam().screen_to_ray(cursor_td, (w, h));
        let geom = self.current_geometry();
        let picked = geom.as_ref().and_then(|g| pick_origin(g, origin, dir));
        // Drag-anchor plane: a HORIZONTAL plane at the picked body's
        // current world Z. Using the bed (z=0) instead caused gigantic
        // drag deltas whenever the body floated above the floor — a
        // tiny mouse jitter cast a ray that crossed z=0 far away
        // from the body's screen position, and the drag math
        // translated the body across the scene. Anchoring at the
        // body's own Z makes the cursor + body stay locked
        // regardless of camera angle.
        let plane_z = picked
            .and_then(|id| geom.as_ref().and_then(|g| {
                g.bodies
                    .iter()
                    .find(|b| b.origin == Some(id))
                    .map(|b| b.matrix[14])
            }))
            .unwrap_or(0.0);
        let plane = HitPlane {
            point: [0.0, 0.0, plane_z],
            normal: [0.0, 0.0, 1.0],
        };
        let anchor = plane.ray_intersect(origin, dir);
        (picked, anchor)
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
        agg_gui::animation::request_draw();
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

        // Esc cancels an in-flight body / rotation drag (MatterCAD
        // parity). Only consume the key when there was actually a drag
        // to cancel — otherwise let it bubble (close a menu, clear
        // selection elsewhere, …).
        if matches!(key, Key::Escape) {
            return if self.cancel_active_drag() {
                EventResult::Consumed
            } else {
                EventResult::Ignored
            };
        }

        // Every match arm below mutates camera state and ends with
        // `return EventResult::Consumed`; we centralise the
        // `request_draw` here so the keyboard branches don't each
        // need their own copy.  The `_ => {}` fall-through that
        // returns `Ignored` legitimately skips the redraw — no
        // state changed.
        match key {
            Key::Char(c) => {
                if c.eq_ignore_ascii_case(&'w') || c.eq_ignore_ascii_case(&'f') {
                    // W = canonical fit-all (MatterCAD); F kept as legacy alias.
                    // Use `fit_all` directly so this is an EXPLICIT
                    // user request — `maybe_auto_fit` no longer
                    // refits on graph re-evaluation, so the old
                    // "force a refit by resetting last_mesh_ptr"
                    // trick is gone (and was always a misuse of
                    // that field).
                    self.fit_all();
                    agg_gui::animation::request_draw();
                    return EventResult::Consumed;
                }
                if c.eq_ignore_ascii_case(&'z') {
                    // Z = zoom-to-selected. With no per-node mesh
                    // tracking yet, fall through to fit-all (Phase
                    // A4 will tighten this to use the selected
                    // node's bounds when one is selected).
                    self.fit_all();
                    agg_gui::animation::request_draw();
                    return EventResult::Consumed;
                }
                // Ctrl + +/- → zoom in/out.
                if mods.ctrl {
                    if *c == '+' || *c == '=' {
                        self.cam_mut(|c| c.zoom(1.0 / KEYBOARD_ZOOM_FACTOR));
                        agg_gui::animation::request_draw();
                        return EventResult::Consumed;
                    }
                    if *c == '-' || *c == '_' {
                        self.cam_mut(|c| c.zoom(KEYBOARD_ZOOM_FACTOR));
                        agg_gui::animation::request_draw();
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
                agg_gui::animation::request_draw();
                return EventResult::Consumed;
            }
            _ => {}
        }
        EventResult::Ignored
    }
}
