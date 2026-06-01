//! Height / scale-Z control interactions for `Viewport3dWidget` —
//! hover, drag-start, and the per-frame scale for MatterCAD's
//! `ScaleHeightControl` (the top-face box).
//!
//! Split out of `viewport_widget_interactions.rs` to keep that file
//! under the line guardrail. The drag *state machine* entry points
//! (`on_mouse_down` / `on_mouse_move` / `on_mouse_up`) live there and
//! call into the methods here; the box geometry lives in
//! `viewport_widget/z_control_gizmo.rs`; the scale matrix math is shared
//! from `viewport_widget/body_drag.rs`.
//!
//! Two drag paths, picked at drag-start by whether the node exposes an
//! editable `height` parameter (the "has a Height field" test, via
//! `ViewportInputs::read_node_number`):
//!
//! * **Field path** — edit the `height` property each frame, then
//!   re-plant the base on the following evaluation. The mesh rebuilds
//!   asynchronously, so the base is re-anchored a frame later with a
//!   world-Z translate (which shifts the AABB min-Z by exactly that
//!   amount, rotation-independent).
//! * **Matrix path** — no height field, so scale the node matrix in Z
//!   about the base plane (`scale_z_about_bottom`): exact, synchronous,
//!   base stays planted.

use super::body_drag;
use super::viewport_widget_helpers::selected_body_world_aabb;
use super::z_control_gizmo;
use super::*;
use crate::picking::{pick_handle, GizmoHandle};

/// Smallest world height a drag will scale to — avoids divide-by-zero
/// and a collapsed body.
const MIN_HEIGHT_WORLD: f32 = 0.01;

impl Viewport3dWidget {
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
        let (center, size) = z_control_gizmo::height_control_layout_for_aabb(world_aabb, &cam, vh);
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
    /// `DragBodyHeight` state. Captures the base Z, start world height,
    /// the vertical-line anchor, and the node's `height` parameter (if
    /// any — its presence selects the field vs matrix path).
    pub(super) fn try_start_height_drag(&self, pos: Point) -> Option<CameraDrag> {
        let (sel_id, handle) = self.height_control_handle()?;
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let (ray_o, ray_d) = self.cam().screen_to_ray((pos.x, h - pos.y), (w, h));
        pick_handle(std::slice::from_ref(&handle), ray_o, ray_d)?;
        let start_matrix = self.read_node_matrix(sel_id)?;
        let geom = self.current_geometry();
        let (mn, mx) = selected_body_world_aabb(geom.as_deref(), sel_id)?;
        let bottom_z = mn[2];
        let start_height_world = (mx[2] - mn[2]).max(MIN_HEIGHT_WORLD);
        let anchor_xy = [handle.center[0], handle.center[1]];
        let anchor_z = body_drag::z_axis_translation(ray_o, ray_d, anchor_xy)?;
        let start_height = self.inputs.read_node_number(sel_id, "height");
        Some(CameraDrag::DragBodyHeight {
            node_id: sel_id,
            start_matrix,
            start_height,
            bottom_z,
            start_height_world,
            anchor_xy,
            anchor_z,
        })
    }

    /// Per-frame update of an in-flight `DragBodyHeight`: project the
    /// cursor onto the vertical line through the box, derive the new
    /// world height, and apply it via the field or matrix path.
    pub(super) fn drag_height(&mut self, pos: Point) {
        if let CameraDrag::DragBodyHeight {
            node_id,
            start_matrix,
            start_height,
            bottom_z,
            start_height_world,
            anchor_xy,
            anchor_z,
        } = self.drag.clone()
        {
            let w = self.bounds.width.max(1.0);
            let h = self.bounds.height.max(1.0);
            let (ray_o, ray_d) = {
                let cam = self.cam();
                cam.screen_to_ray((pos.x, h - pos.y), (w, h))
            };
            let Some(cur_z) = body_drag::z_axis_translation(ray_o, ray_d, anchor_xy) else {
                return;
            };
            let new_height_world = (start_height_world + (cur_z - anchor_z)).max(MIN_HEIGHT_WORLD);
            let scale = new_height_world / start_height_world;

            match start_height {
                Some(h0) => {
                    // Field path: re-plant the base (the prior frame's
                    // regenerated mesh arrives async and may have shifted
                    // it), then edit the `height` parameter. Assumes the
                    // parameter maps ~linearly to world height (true when
                    // the matrix carries no extra Z scale, the common
                    // case for parametric primitives).
                    self.reanchor_height_base(node_id, bottom_z, start_matrix);
                    self.inputs
                        .push_node_number(node_id, "height", h0 * scale as f64);
                }
                None => {
                    // Matrix path: scale Z about the base plane — exact
                    // and synchronous, base stays planted.
                    let m = body_drag::scale_z_about_bottom(start_matrix, scale, bottom_z);
                    self.inputs.push_node_matrix(node_id, m);
                }
            }
        }
    }

    /// Nudge the node's matrix in world Z so the live body's base
    /// returns to `target_bottom_z`. The height-field path uses this to
    /// keep the base planted after the async mesh rebuild: a world-Z
    /// translate moves the AABB min-Z by exactly that amount regardless
    /// of rotation, so it re-anchors precisely (one frame behind the
    /// height change).
    fn reanchor_height_base(&self, node_id: NodeId, target_bottom_z: f32, fallback: [f32; 16]) {
        let geom = self.current_geometry();
        let Some((mn, _)) = selected_body_world_aabb(geom.as_deref(), node_id) else {
            return;
        };
        let fix = target_bottom_z - mn[2];
        if fix.abs() < 1e-4 {
            return;
        }
        let mut m = self.read_node_matrix(node_id).unwrap_or(fallback);
        m[14] += fix;
        self.inputs.push_node_matrix(node_id, m);
    }
}
