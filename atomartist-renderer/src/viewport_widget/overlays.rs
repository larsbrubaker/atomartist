//! 2-D drag-readout overlays for `Viewport3dWidget` — the live value
//! labels painted on top of the 3-D scene while a gizmo drag is in
//! flight: the rotation angle, the Z-move distance, and the height.
//!
//! Extracted from `viewport_widget.rs` so that file stays under the
//! 800-line guardrail. Each is a no-op unless its matching drag is
//! active; all share [`super::paint_text_pill`] for the MatterCAD-style
//! rounded label. Called from `Viewport3dWidget::paint`.

use super::viewport_widget_helpers::paint_text_pill;
use super::*;

impl Viewport3dWidget {
    /// During a rotate-handle drag, paint the live angle readout — a
    /// port of MatterCAD's `angleTextControl`. Sits along the drag's
    /// anchor direction, ~100 px beyond the compass ring, showing the
    /// snapped rotation in degrees on a rounded theme-coloured pill.
    /// No-op unless a `RotateBodyAxis` drag is in flight.
    pub(super) fn paint_rotation_readout(&self, ctx: &mut dyn DrawCtx, w: f64, h: f64) {
        let CameraDrag::RotateBodyAxis {
            axis,
            center,
            anchor_angle,
            snapped,
            radius,
            ..
        } = self.drag.clone()
        else {
            return;
        };
        let cam = self.cam();
        let upp = cam.world_units_per_pixel_at(center, h.max(1.0) as f32);
        let world = rotate_gizmo::readout_position(center, axis, anchor_angle, radius, upp);
        let view = Mat4::from_cols_array(&cam.view_matrix());
        let proj = Mat4::from_cols_array(&cam.projection_matrix((w / h.max(1.0)) as f32));
        let mvp = (proj * view).to_cols_array();
        let Some((sx, sy)) = project(&mvp, world, w, h) else {
            return;
        };
        paint_text_pill(ctx, sx, sy, &rotate_gizmo::format_rotation_degrees(snapped));
    }

    /// While dragging in Z, paint the measurement distance — the body's
    /// height above the bed — beside the witness line the scene pass
    /// draws (see [`z_control_gizmo::z_measure`]). Mirrors MatterCAD's
    /// measure readout during move-in-Z. No-op unless a `DragBodyZ` is
    /// active.
    pub(super) fn paint_z_measure_readout(&self, ctx: &mut dyn DrawCtx, w: f64, h: f64) {
        let CameraDrag::DragBodyZ { node_id, .. } = self.drag.clone() else {
            return;
        };
        let geom = self.current_geometry();
        let Some(world_aabb) = selected_body_world_aabb(geom.as_deref(), node_id) else {
            return;
        };
        let cam = self.cam();
        let vh = h.max(1.0) as f32;
        let idle = {
            let v = ctx.visuals();
            [v.text_color.r, v.text_color.g, v.text_color.b, 1.0]
        };
        let (_, label_world, value) = z_control_gizmo::z_measure(world_aabb, &cam, vh, idle);
        let view = Mat4::from_cols_array(&cam.view_matrix());
        let proj = Mat4::from_cols_array(&cam.projection_matrix((w / h.max(1.0)) as f32));
        let mvp = (proj * view).to_cols_array();
        let Some((sx, sy)) = project(&mvp, label_world, w, h) else {
            return;
        };
        paint_text_pill(ctx, sx, sy, &format!("{value:.2}"));
    }

    /// While dragging the height/scale-Z box, paint the body's current
    /// world height beside the box — MatterCAD's `ScaleHeightControl`
    /// height readout. No-op unless a `DragBodyHeight` is active.
    pub(super) fn paint_height_readout(&self, ctx: &mut dyn DrawCtx, w: f64, h: f64) {
        let CameraDrag::DragBodyHeight { node_id, .. } = self.drag.clone() else {
            return;
        };
        let geom = self.current_geometry();
        let Some((mn, mx)) = selected_body_world_aabb(geom.as_deref(), node_id) else {
            return;
        };
        let height = mx[2] - mn[2];
        let cam = self.cam();
        let vh = h.max(1.0) as f32;
        let (box_center, _) = z_control_gizmo::height_control_layout_for_aabb((mn, mx), &cam, vh);
        let view = Mat4::from_cols_array(&cam.view_matrix());
        let proj = Mat4::from_cols_array(&cam.projection_matrix((w / h.max(1.0)) as f32));
        let mvp = (proj * view).to_cols_array();
        let Some((sx, sy)) = project(&mvp, box_center, w, h) else {
            return;
        };
        // Offset to the right so the pill clears the box + arrow.
        paint_text_pill(ctx, sx + 34.0, sy, &format!("{height:.2}"));
    }
}
