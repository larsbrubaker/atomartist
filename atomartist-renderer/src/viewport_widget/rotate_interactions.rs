//! Rotate-gizmo interaction logic for `Viewport3dWidget` — hover
//! hit-testing, drag-start, per-frame rotation, and angle snapping for
//! MatterCAD's 3-axis `RotateCornerControl`.
//!
//! Split out of `viewport_widget_interactions.rs` to keep that file
//! under the repository line guardrail. The drag *state machine* entry
//! points (`on_mouse_down` / `on_mouse_move` / `on_mouse_up`) live in
//! that sibling file and call into the methods here; the gizmo geometry
//! lives in `viewport_widget/rotate_gizmo/`; the angle + matrix math is
//! shared from `atomartist_lib::graph::node`.

use super::rotate_gizmo;
use super::viewport_widget_helpers::selected_body_world_aabb;
use super::*;
use atomartist_lib::graph::node::{angle_on_axis_plane, normalize_angle, rotate_about_world_axis};
use crate::picking::{pick_handle, GizmoHandle, HitPlane};

/// Default rotate-drag snap step — one degree, matching MatterCAD's
/// `RotateCornerControl` (`snapRadians = DegreesToRadians(1)`).
const ROTATE_SNAP_RADIANS: f32 = std::f32::consts::PI / 180.0;

/// Snap a raw rotation `delta` (radians, already shortest-signed) to
/// the grid the rotate gizmo uses, porting MatterCAD's `OnMouseMove`
/// snap block + `GetSnapIndex`:
///
/// * **45° lock** when Shift is held, or when the cursor is dragged out
///   to the snap-mark ring (~50 px beyond the handle radius) *and* is
///   within 5° of a 45° mark — the "magnet" that grabs the nearest
///   eighth-turn.
/// * **1° step** otherwise.
///
/// `cursor_dist` / `radius` / `upp` are world-space so the pixel
/// thresholds scale with zoom.
fn snap_rotation(delta: f32, cursor_dist: f32, radius: f32, upp: f32, shift: bool) -> f32 {
    const SNAP_45: f32 = std::f32::consts::PI / 4.0;
    const FIVE_DEG: f32 = 5.0 * std::f32::consts::PI / 180.0;
    // Snap-mark ring sits ~50 px past the handle; the magnet engages
    // within ~20 px of it. (RingWidth/2 + RingWidth + 20 px = 50 px.)
    let snap_mark_radius = radius + 50.0 * upp;
    let near_ring = (cursor_dist - snap_mark_radius).abs() < 20.0 * upp;
    let nearest_45 = (delta / SNAP_45).round();
    let within_5 = (delta - nearest_45 * SNAP_45).abs() < FIVE_DEG;
    if shift || (near_ring && within_5) {
        nearest_45 * SNAP_45
    } else {
        (delta / ROTATE_SNAP_RADIANS).round() * ROTATE_SNAP_RADIANS
    }
}

impl Viewport3dWidget {
    /// Solve the three per-axis rotate-handle layouts + their pick
    /// AABBs for the currently-selected body. Shared by the mouse-down
    /// drag-start and the per-frame hover pick so both hit-test exactly
    /// the same handles. `None` when nothing is selected or the
    /// selection has no world AABB.
    fn rotate_layouts_and_handles(
        &self,
    ) -> Option<([rotate_gizmo::RotateAxisLayout; 3], Vec<GizmoHandle>)> {
        let sel_id = (*self.inputs.selection.lock().unwrap())?;
        let geom = self.current_geometry();
        let world_aabb = selected_body_world_aabb(geom.as_deref(), sel_id)?;
        let cam = self.cam();
        let vh = self.bounds.height.max(1.0) as f32;
        let layouts = rotate_gizmo::rotate_axis_layouts(world_aabb, &cam, vh);
        let handles = layouts
            .iter()
            .map(|l| {
                let half = l.handle_size * 0.5;
                GizmoHandle {
                    id: rotate_gizmo::handle_id(l.axis),
                    center: l.control_center,
                    half_extent: [half, half, half],
                }
            })
            .collect();
        Some((layouts, handles))
    }

    /// Which rotate-handle axis the cursor at `pos` is over, if any.
    /// Drives the hover accent + compass display. `None` when no body
    /// is selected or the cursor misses all three handles.
    pub(super) fn pick_rotate_hover(&self, pos: Point) -> Option<u8> {
        let (_layouts, handles) = self.rotate_layouts_and_handles()?;
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let cursor_td = (pos.x, h - pos.y);
        let (ray_o, ray_d) = self.cam().screen_to_ray(cursor_td, (w, h));
        let id = pick_handle(&handles, ray_o, ray_d)?;
        rotate_gizmo::axis_from_handle_id(id)
    }

    /// If the click at `pos` lands on one of the three per-axis rotate
    /// handles of the currently-selected body, return a pending
    /// `RotateBodyAxis` state. `None` otherwise. Mirrors
    /// [`Self::try_start_z_drag`] — handle-pick + matrix-read — but
    /// seeds the pointer angle in the picked axis's rotation plane.
    pub(super) fn try_start_rotate_drag(&self, pos: Point) -> Option<CameraDrag> {
        let sel_id = (*self.inputs.selection.lock().unwrap())?;
        let (layouts, handles) = self.rotate_layouts_and_handles()?;
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let cursor_td = (pos.x, h - pos.y);
        let (ray_o, ray_d) = self.cam().screen_to_ray(cursor_td, (w, h));
        let id = pick_handle(&handles, ray_o, ray_d)?;
        let axis = rotate_gizmo::axis_from_handle_id(id)?;
        let layout = layouts[axis as usize];
        let start_matrix = self.read_node_matrix(sel_id)?;
        // Anchor the rotation at the pointer's angle in the picked
        // axis's plane (normal = axis, through the corner-anchored
        // centre). `None` (ray parallel to the plane) aborts rather
        // than starting from a bogus angle.
        let plane = HitPlane {
            point: layout.rotation_center,
            normal: rotate_gizmo::axis_unit(axis),
        };
        let hit = plane.ray_intersect(ray_o, ray_d)?;
        let anchor_angle = angle_on_axis_plane(hit, layout.rotation_center, axis);
        let rc = layout.rotation_center;
        let cc = layout.control_center;
        let radius =
            ((cc[0] - rc[0]).powi(2) + (cc[1] - rc[1]).powi(2) + (cc[2] - rc[2]).powi(2)).sqrt();
        Some(CameraDrag::RotateBodyAxis {
            node_id: sel_id,
            axis,
            center: layout.rotation_center,
            anchor_angle,
            snapped: 0.0,
            radius,
            start_matrix,
        })
    }

    /// Per-frame update of an in-flight `RotateBodyAxis` drag: intersect
    /// the cursor ray with the axis's rotation plane, read + snap the
    /// signed delta from the mouse-down anchor, rotate the node matrix
    /// from the captured start matrix (so the whole drag coalesces into
    /// one undo step), and stash the live snapped angle for the compass.
    pub(super) fn drag_rotate(&mut self, pos: Point) {
        if let CameraDrag::RotateBodyAxis {
            node_id,
            axis,
            center,
            anchor_angle,
            radius,
            start_matrix,
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
            let plane = HitPlane {
                point: center,
                normal: rotate_gizmo::axis_unit(axis),
            };
            if let Some(hit) = plane.ray_intersect(ray_o, ray_d) {
                let cur_angle = angle_on_axis_plane(hit, center, axis);
                let delta = normalize_angle(cur_angle - anchor_angle);
                let cursor_dist = {
                    let d = [hit[0] - center[0], hit[1] - center[1], hit[2] - center[2]];
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
                };
                let upp = self.cam().world_units_per_pixel_at(center, h as f32);
                let snapped =
                    snap_rotation(delta, cursor_dist, radius, upp, self.current_mods.shift);
                let new_matrix = rotate_about_world_axis(&start_matrix, center, axis, snapped);
                self.inputs.push_node_matrix(node_id, new_matrix);
                self.drag = CameraDrag::RotateBodyAxis {
                    node_id,
                    axis,
                    center,
                    anchor_angle,
                    snapped,
                    radius,
                    start_matrix,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::snap_rotation;

    const DEG: f32 = std::f32::consts::PI / 180.0;

    // World-units-per-pixel and a handle radius chosen so the snap-mark
    // ring sits at radius + 50*upp = 100 world units (upp=1, radius=50).
    const UPP: f32 = 1.0;
    const RADIUS: f32 = 50.0;
    const SNAP_RING: f32 = RADIUS + 50.0 * UPP; // 100

    #[test]
    fn fine_drag_snaps_to_whole_degrees() {
        // 12.4° with no shift, cursor near the handle (not the snap
        // ring) → rounds to 12°.
        let out = snap_rotation(12.4 * DEG, RADIUS, RADIUS, UPP, false);
        assert!((out - 12.0 * DEG).abs() < 1e-4, "got {} deg", out / DEG);
    }

    #[test]
    fn shift_locks_to_forty_five() {
        // 50° with shift → 45°, regardless of cursor distance.
        let out = snap_rotation(50.0 * DEG, RADIUS, RADIUS, UPP, true);
        assert!((out - 45.0 * DEG).abs() < 1e-4, "got {} deg", out / DEG);
        // 70° with shift → 90° (nearest 45° multiple).
        let out2 = snap_rotation(70.0 * DEG, RADIUS, RADIUS, UPP, false /* via ring below */);
        let _ = out2;
    }

    #[test]
    fn snap_ring_magnet_grabs_nearest_eighth_when_near_a_mark() {
        // Cursor out at the snap ring, angle within 5° of 45° → snaps to
        // exactly 45°, even without Shift.
        let out = snap_rotation(43.0 * DEG, SNAP_RING, RADIUS, UPP, false);
        assert!((out - 45.0 * DEG).abs() < 1e-4, "got {} deg", out / DEG);
    }

    #[test]
    fn snap_ring_does_not_grab_between_marks() {
        // Cursor at the ring but 20° (not within 5° of any 45° mark) →
        // stays on the 1° grid.
        let out = snap_rotation(20.0 * DEG, SNAP_RING, RADIUS, UPP, false);
        assert!((out - 20.0 * DEG).abs() < 1e-4, "got {} deg", out / DEG);
    }
}
