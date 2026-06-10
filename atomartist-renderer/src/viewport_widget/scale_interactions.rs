//! Height / scale-Z control interactions for `Viewport3dWidget` —
//! hover, drag-start, and the per-frame scale for MatterCAD's
//! `ScaleHeightControl` (the top-face box).
//!
//! Split out of `viewport_widget_interactions.rs` to keep that file
//! under the line guardrail. The drag *state machine* entry points
//! (`on_mouse_down` / `on_mouse_move` / `on_mouse_up`) live there and
//! call into the methods here; the box mesh lives in
//! `viewport_widget/z_control_gizmo.rs`; the matrix math is shared
//! from `viewport_widget/body_drag.rs`.
//!
//! Two drag paths, picked at drag-start by whether the node exposes an
//! editable `height` parameter (the "has a Height field" test, via
//! `ViewportInputs::read_node_number`):
//!
//! * **Field path** — the box rides the *object's* top-face centre
//!   (local bounds top transformed by the body matrix — MatterCAD's
//!   `GetTopPosition`), the drag measures along the rotated local-Z
//!   axis, and every frame writes the `height` parameter **together
//!   with** a predicted matrix translation that keeps the rotated base
//!   point locked — one atomic graph update, so the async rebuild
//!   never paints a scaled-but-unanchored frame (no bounce). The
//!   prediction assumes the mesh's local Z scales proportionally with
//!   the parameter about local `z = 0` (true for our parametric
//!   primitives).
//! * **Matrix path** — no height field: box on the world-AABB top
//!   centre, scale the node matrix in world Z about the base plane
//!   (`scale_z_about_bottom`): exact, synchronous, base stays planted.

use super::body_drag;
use super::viewport_widget_helpers::{
    mat4_transform_point, selected_body_local_aabb_and_matrix, selected_body_world_aabb,
};
use super::z_control_gizmo;
use super::*;
use crate::picking::{pick_handle, GizmoHandle};

/// Smallest world height a drag will scale to — avoids divide-by-zero
/// and a collapsed body.
const MIN_HEIGHT_WORLD: f32 = 0.01;

impl Viewport3dWidget {
    /// World pose `(center, size, axes)` of the height box for the
    /// selected body — mode-aware. Field mode (node has a `height`
    /// parameter) anchors to the **object's** top-face centre (local
    /// bounds top through the body matrix, riding the rotated height
    /// axis) and returns the body's rotation basis so the cube tilts
    /// with the top face — MatterCAD rotates the `ScaleHeightControl`
    /// mesh with the selection. Matrix mode anchors to the
    /// **world-AABB** top centre, axis-aligned
    /// (`ScaleMatrixTopControl`). Shared by the scene draw, the
    /// hover/hit-test, and the readout so all three agree.
    pub(super) fn height_box_layout(
        &self,
        sel_id: NodeId,
        world_aabb: ([f32; 3], [f32; 3]),
        cam: &crate::camera::OrbitCamera,
        vh: f32,
    ) -> ([f32; 3], f32, [[f32; 3]; 3]) {
        if self.inputs.read_node_number(sel_id, "height").is_some() {
            let geom = self.current_geometry();
            if let Some(((mn, mx), m)) =
                selected_body_local_aabb_and_matrix(geom.as_deref(), sel_id)
            {
                let cx = (mn[0] + mx[0]) * 0.5;
                let cy = (mn[1] + mx[1]) * 0.5;
                let top = mat4_transform_point(&m, [cx, cy, mx[2]]);
                let bottom = mat4_transform_point(&m, [cx, cy, mn[2]]);
                let axis = normalize3(sub3(top, bottom));
                let axis = if axis[0].is_finite() { axis } else { [0.0, 0.0, 1.0] };
                // Cube basis = the matrix's normalised rotation columns
                // (scale stripped — the quaternion-extraction analog),
                // with Z snapped to the drag axis so box and drag agree.
                let xa = normalize3([m[0], m[1], m[2]]);
                let ya = normalize3([m[4], m[5], m[6]]);
                let axes = [
                    if xa[0].is_finite() { xa } else { [1.0, 0.0, 0.0] },
                    if ya[0].is_finite() { ya } else { [0.0, 1.0, 0.0] },
                    axis,
                ];
                let upp = cam.world_units_per_pixel_at(top, vh);
                let size = z_control_gizmo::HEIGHT_BOX_PX * upp;
                let half = size * 0.5;
                let center = [
                    top[0] + axis[0] * half,
                    top[1] + axis[1] * half,
                    top[2] + axis[2] * half,
                ];
                return (center, size, axes);
            }
        }
        let (center, size) =
            z_control_gizmo::height_control_layout_for_aabb(world_aabb, cam, vh);
        (center, size, z_control_gizmo::AXIS_ALIGNED)
    }

    /// World-space measure anchors `(base, top)` for the height
    /// control — the span the measure bars run between. Field mode:
    /// the object's transformed local bottom/top centres (rotation-
    /// aware); matrix mode: the world-AABB bottom/top centres. Used by
    /// the hover-time measure draw + readout (drag-time anchors come
    /// from the live drag state instead).
    pub(super) fn height_measure_anchors(&self, sel_id: NodeId) -> Option<([f32; 3], [f32; 3])> {
        let geom = self.current_geometry();
        if self.inputs.read_node_number(sel_id, "height").is_some() {
            let ((mn, mx), m) = selected_body_local_aabb_and_matrix(geom.as_deref(), sel_id)?;
            let cx = (mn[0] + mx[0]) * 0.5;
            let cy = (mn[1] + mx[1]) * 0.5;
            Some((
                mat4_transform_point(&m, [cx, cy, mn[2]]),
                mat4_transform_point(&m, [cx, cy, mx[2]]),
            ))
        } else {
            let (wmn, wmx) = selected_body_world_aabb(geom.as_deref(), sel_id)?;
            let cx = (wmn[0] + wmx[0]) * 0.5;
            let cy = (wmn[1] + wmx[1]) * 0.5;
            Some(([cx, cy, wmn[2]], [cx, cy, wmx[2]]))
        }
    }

    /// The height box's pick handle for the selected body, plus its
    /// `NodeId`. Shared by the hover highlight and the drag-start
    /// hit-test so both target the same box. `None` when nothing is
    /// selected or the selection has no world AABB.
    fn height_control_handle(&self) -> Option<(NodeId, GizmoHandle)> {
        let sel_id = (*self.inputs.selection.lock().unwrap())?;
        let geom = self.current_geometry();
        let world_aabb = selected_body_world_aabb(geom.as_deref(), sel_id)?;
        let cam = self.cam();
        let vh = self.bounds.height.max(1.0) as f32;
        let (center, size, _) = self.height_box_layout(sel_id, world_aabb, &cam, vh);
        let half = size * 0.5;
        Some((
            sel_id,
            GizmoHandle {
                id: z_control_gizmo::HEIGHT_HANDLE_ID,
                center,
                half_extent: [half, half, half],
            },
        ))
    }

    /// Whether the cursor at `pos` is over the height box — drives the
    /// hover highlight. `false` when nothing is selected.
    pub(super) fn pick_height_hover(&self, pos: Point) -> bool {
        let Some((_, handle)) = self.height_control_handle() else {
            return false;
        };
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let (ray_o, ray_d) = self.cam().screen_to_ray((pos.x, h - pos.y), (w, h));
        pick_handle(std::slice::from_ref(&handle), ray_o, ray_d).is_some()
    }

    /// If the click at `pos` lands on the height box, return a pending
    /// `DragBodyHeight`. Captures the drag axis (rotated local Z in
    /// field mode, world Z in matrix mode), the locked base point, the
    /// start length along the axis, and — for the field path — the
    /// local bottom anchor + body matrix that drive the base-lock
    /// prediction.
    pub(super) fn try_start_height_drag(&self, pos: Point) -> Option<CameraDrag> {
        let (sel_id, handle) = self.height_control_handle()?;
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let (ray_o, ray_d) = self.cam().screen_to_ray((pos.x, h - pos.y), (w, h));
        pick_handle(std::slice::from_ref(&handle), ray_o, ray_d)?;
        let start_matrix = self.read_node_matrix(sel_id)?;
        let start_height = self.inputs.read_node_number(sel_id, "height");
        let geom = self.current_geometry();

        let (axis_origin, axis_dir, start_len, bottom_local, start_body_matrix) =
            if start_height.is_some() {
                // Field mode: anchors from the object's local bounds
                // through the body matrix (rotation-aware).
                let ((mn, mx), m) =
                    selected_body_local_aabb_and_matrix(geom.as_deref(), sel_id)?;
                let cx = (mn[0] + mx[0]) * 0.5;
                let cy = (mn[1] + mx[1]) * 0.5;
                let bl = [cx, cy, mn[2]];
                let bottom = mat4_transform_point(&m, bl);
                let top = mat4_transform_point(&m, [cx, cy, mx[2]]);
                let d = sub3(top, bottom);
                let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if len < MIN_HEIGHT_WORLD {
                    return None;
                }
                let axis = [d[0] / len, d[1] / len, d[2] / len];
                (bottom, axis, len, bl, m)
            } else {
                // Matrix mode: world AABB, world-Z axis.
                let (wmn, wmx) = selected_body_world_aabb(geom.as_deref(), sel_id)?;
                let origin = [(wmn[0] + wmx[0]) * 0.5, (wmn[1] + wmx[1]) * 0.5, wmn[2]];
                let len = (wmx[2] - wmn[2]).max(MIN_HEIGHT_WORLD);
                let identity = {
                    let mut m = [0.0_f32; 16];
                    m[0] = 1.0;
                    m[5] = 1.0;
                    m[10] = 1.0;
                    m[15] = 1.0;
                    m
                };
                (origin, [0.0, 0.0, 1.0], len, [0.0; 3], identity)
            };

        let anchor_t = body_drag::axis_param(ray_o, ray_d, axis_origin, axis_dir)?;
        Some(CameraDrag::DragBodyHeight {
            node_id: sel_id,
            start_matrix,
            start_height,
            axis_origin,
            axis_dir,
            start_len,
            anchor_t,
            bottom_local,
            start_body_matrix,
            live_len: start_len,
        })
    }

    /// Per-frame update of an in-flight `DragBodyHeight`: project the
    /// cursor onto the drag-axis line, derive the new length, and
    /// apply it via the field or matrix path. The field path writes
    /// the height parameter and its base-lock matrix compensation as
    /// ONE atomic graph update — never two visible steps.
    pub(super) fn drag_height(&mut self, pos: Point) {
        let CameraDrag::DragBodyHeight {
            node_id,
            start_matrix,
            start_height,
            axis_origin,
            axis_dir,
            start_len,
            anchor_t,
            bottom_local,
            start_body_matrix,
            ..
        } = self.drag.clone()
        else {
            return;
        };
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let (ray_o, ray_d) = {
            let cam = self.cam();
            cam.screen_to_ray((pos.x, h - pos.y), (w, h))
        };
        let Some(t) = body_drag::axis_param(ray_o, ray_d, axis_origin, axis_dir) else {
            return;
        };
        let new_len = (start_len + (t - anchor_t)).max(MIN_HEIGHT_WORLD);
        let scale = new_len / start_len;
        if let CameraDrag::DragBodyHeight { live_len, .. } = &mut self.drag {
            *live_len = new_len;
        }

        match start_height {
            Some(h0) => {
                // Field path. Predict where the rebuilt mesh's base
                // lands: local Z scales about local 0 with the height
                // parameter, so the new local bottom is
                // `(bl.x, bl.y, bl.z · s)`. Translate the matrix so
                // that point maps back onto the locked world base, and
                // send parameter + matrix atomically.
                let predicted = mat4_transform_point(
                    &start_body_matrix,
                    [bottom_local[0], bottom_local[1], bottom_local[2] * scale],
                );
                let fix = sub3(axis_origin, predicted);
                let mut m = start_matrix;
                m[12] += fix[0];
                m[13] += fix[1];
                m[14] += fix[2];
                self.inputs
                    .push_node_number_and_matrix(node_id, "height", h0 * scale as f64, m);
            }
            None => {
                // Matrix path: scale Z about the base plane — exact
                // and synchronous, base stays planted.
                let m = body_drag::scale_z_about_bottom(start_matrix, scale, axis_origin[2]);
                self.inputs.push_node_matrix(node_id, m);
            }
        }
    }
}
