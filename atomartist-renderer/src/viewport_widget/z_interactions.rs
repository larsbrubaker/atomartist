//! Z-translate control interactions for `Viewport3dWidget` — the
//! hover pick and drag-start hit-test for the move-in-Z cone
//! (MatterCAD's `MoveInZControl`).
//!
//! Split out of `viewport_widget_interactions.rs` to keep that file
//! under the line guardrail. The drag state machine (`on_mouse_*`)
//! stays there and calls into these; the per-frame Z translation
//! (with grid snap) lives in the `DragBodyZ` mouse-move arm; the cone
//! geometry lives in `viewport_widget/z_control_gizmo.rs`.

use super::body_drag;
use super::viewport_widget_helpers::selected_body_world_aabb;
use super::z_control_gizmo;
use super::*;
use crate::picking::{pick_handle, GizmoHandle};

impl Viewport3dWidget {
    /// The Z-control's pick handle (the cube AABB around the cone) for
    /// the currently-selected body, plus that body's `NodeId`. Shared by
    /// the hover highlight ([`Self::pick_z_hover`]) and the drag-start
    /// hit-test ([`Self::try_start_z_drag`]) so both target exactly the
    /// same target. `None` when nothing is selected or the selection has
    /// no world AABB.
    fn z_control_handle(&self) -> Option<(NodeId, GizmoHandle)> {
        let sel_id = (*self.inputs.selection.lock().unwrap())?;
        let geom = self.current_geometry();
        let world_aabb = selected_body_world_aabb(geom.as_deref(), sel_id)?;
        let cam = self.cam();
        let vh = self.bounds.height.max(1.0) as f32;
        let (cube_center, cube_size) =
            z_control_gizmo::z_control_layout_for_aabb(world_aabb, &cam, vh);
        let half = cube_size * 0.5;
        Some((
            sel_id,
            GizmoHandle {
                id: z_control_gizmo::Z_TRANSLATE_HANDLE_ID,
                center: cube_center,
                half_extent: [half, half, half],
            },
        ))
    }

    /// Whether the cursor at `pos` is over the Z-control handle. Drives
    /// the hover highlight, mirroring [`Self::pick_rotate_hover`] for the
    /// rotate handles. `false` when nothing is selected.
    pub(super) fn pick_z_hover(&self, pos: Point) -> bool {
        let Some((_, handle)) = self.z_control_handle() else {
            return false;
        };
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let cursor_td = (pos.x, h - pos.y);
        let (ray_o, ray_d) = self.cam().screen_to_ray(cursor_td, (w, h));
        pick_handle(std::slice::from_ref(&handle), ray_o, ray_d).is_some()
    }

    /// If the click at `pos` lands on the Z-control handle of the
    /// currently-selected body, return a pending `DragBodyZ` state.
    /// `None` otherwise.
    pub(super) fn try_start_z_drag(&self, pos: Point) -> Option<CameraDrag> {
        let (sel_id, handle) = self.z_control_handle()?;
        let cube_center = handle.center;
        let w = self.bounds.width.max(1.0);
        let h = self.bounds.height.max(1.0);
        let cursor_td = (pos.x, h - pos.y);
        let (ray_o, ray_d) = self.cam().screen_to_ray(cursor_td, (w, h));
        pick_handle(std::slice::from_ref(&handle), ray_o, ray_d)?;
        let start_matrix = self.read_node_matrix(sel_id)?;
        // Anchor Z = projection of the drag-start ray onto the
        // vertical line through (cube_center.xy). The drag math
        // subtracts this to get the per-frame delta.
        let anchor_z = body_drag::z_axis_translation(
            ray_o,
            ray_d,
            [cube_center[0], cube_center[1]],
        )?;
        // AABB bottom / top at drag start: the snap target (bottom
        // position rounds to the grid) and the drag-time anchor for
        // the cone + measure bars, so they ride the cursor instead of
        // the async-rebuilt geometry.
        let geom = self.current_geometry();
        let (mn, mx) = selected_body_world_aabb(geom.as_deref(), sel_id)?;
        Some(CameraDrag::DragBodyZ {
            node_id: sel_id,
            anchor_xy: [cube_center[0], cube_center[1]],
            anchor_z,
            start_matrix,
            start_bottom_z: mn[2],
            start_top_z: mx[2],
            live_dz: 0.0,
        })
    }
}
