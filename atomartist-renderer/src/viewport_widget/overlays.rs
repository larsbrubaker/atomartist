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

    /// While dragging in Z — or hovering the Z control — paint the
    /// measured distance (the body's bottom above the bed) beside the
    /// measure bars the scene pass draws (see
    /// [`z_control_gizmo::measure_bars`]). Mirrors MatterCAD's
    /// `MoveInZControl` readout, which appears on `MouseIsOver` as
    /// well as during the drag.
    pub(super) fn paint_z_measure_readout(&self, ctx: &mut dyn DrawCtx, w: f64, h: f64) {
        // During the drag the span comes from the drag state (so the
        // label tracks the cursor, not the lagging rebuild); on hover
        // it comes from the body's current AABB. Value shown signed (a
        // body below the bed reads negative, like MatterCAD).
        let (cx, cy, bottom_z) = match self.drag.clone() {
            CameraDrag::DragBodyZ {
                anchor_xy,
                start_bottom_z,
                live_dz,
                ..
            } => (anchor_xy[0], anchor_xy[1], start_bottom_z + live_dz),
            CameraDrag::None if self.hovered_z_control => {
                let Some(id) = *self.inputs.selection.lock().unwrap() else {
                    return;
                };
                let geom = self.current_geometry();
                let Some((mn, mx)) = selected_body_world_aabb(geom.as_deref(), id) else {
                    return;
                };
                ((mn[0] + mx[0]) * 0.5, (mn[1] + mx[1]) * 0.5, mn[2])
            }
            _ => return,
        };
        let cam = self.cam();
        let vh = h.max(1.0) as f32;
        let idle = {
            let v = ctx.visuals();
            [v.text_color.r, v.text_color.g, v.text_color.b, 1.0]
        };
        let (_, label_world, _) = z_control_gizmo::measure_bars(
            [cx, cy, 0.0],
            [cx, cy, bottom_z],
            &cam,
            vh,
            idle,
        );
        let value = bottom_z;
        let view = Mat4::from_cols_array(&cam.view_matrix());
        let proj = Mat4::from_cols_array(&cam.projection_matrix((w / h.max(1.0)) as f32));
        let mvp = (proj * view).to_cols_array();
        let Some((sx, sy)) = project(&mvp, label_world, w, h) else {
            return;
        };
        // MatterCAD parks the value a few px right of the measure line.
        paint_text_pill(ctx, sx + 10.0, sy, &format!("{value:.2}"));
    }

    /// While dragging the height/scale-Z box — or hovering it — paint
    /// the live height beside the measure bars (MatterCAD's
    /// `ScaleHeightControl` readout, shown on `MouseIsOver` too).
    /// During a drag the value and anchors come from the drag state
    /// (`live_len`, scaled into the height parameter on the field
    /// path) so the label tracks the cursor without rebuild lag; on
    /// hover they come from the body's current measure anchors.
    pub(super) fn paint_height_readout(&self, ctx: &mut dyn DrawCtx, w: f64, h: f64) {
        let (value, base, top) = match self.drag.clone() {
            CameraDrag::DragBodyHeight {
                start_height,
                start_len,
                live_len,
                axis_origin,
                axis_dir,
                ..
            } => {
                // Field path shows the height *parameter* (what's
                // being edited); matrix path shows the world height.
                let value = match start_height {
                    Some(h0) => h0 * (live_len / start_len) as f64,
                    None => live_len as f64,
                };
                let top = [
                    axis_origin[0] + axis_dir[0] * live_len,
                    axis_origin[1] + axis_dir[1] * live_len,
                    axis_origin[2] + axis_dir[2] * live_len,
                ];
                (value, axis_origin, top)
            }
            CameraDrag::None if self.hovered_height_control => {
                let Some(sel_id) = *self.inputs.selection.lock().unwrap() else {
                    return;
                };
                let Some((base, top)) = self.height_measure_anchors(sel_id) else {
                    return;
                };
                let d = sub3(top, base);
                let len = dot3(d, d).sqrt() as f64;
                let value = self
                    .inputs
                    .read_node_number(sel_id, "height")
                    .unwrap_or(len);
                (value, base, top)
            }
            _ => return,
        };
        let cam = self.cam();
        let vh = h.max(1.0) as f32;
        let idle = {
            let v = ctx.visuals();
            [v.text_color.r, v.text_color.g, v.text_color.b, 1.0]
        };
        // Same span the scene pass draws — label rides the measure
        // line's midpoint.
        let (_, label_world, _) = z_control_gizmo::measure_bars(base, top, &cam, vh, idle);
        let view = Mat4::from_cols_array(&cam.view_matrix());
        let proj = Mat4::from_cols_array(&cam.projection_matrix((w / h.max(1.0)) as f32));
        let mvp = (proj * view).to_cols_array();
        let Some((sx, sy)) = project(&mvp, label_world, w, h) else {
            return;
        };
        // MatterCAD parks the value a few px right of the measure line.
        paint_text_pill(ctx, sx + 10.0, sy, &format!("{value:.2}"));
    }
}
