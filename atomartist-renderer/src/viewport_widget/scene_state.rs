//! Per-frame `WgpuSceneRenderer` state population — extracted from
//! `viewport_widget.rs`'s `paint()` so the parent file stays under
//! the 800-line guardrail. Side-effect-only helpers; everything they
//! touch already lives on the parent `Viewport3dWidget` struct.

use super::*;

impl Viewport3dWidget {
    /// Snapshot the current frame's host state into the scene
    /// renderer's mutable inputs — bodies, camera, gizmos, style
    /// flags. Per-frame side effects only; no allocations beyond the
    /// gizmo set pushes.
    pub(super) fn populate_scene_state(
        &self,
        bodies: Vec<atomartist_lib::geometry::Body>,
        first_body: Option<&atomartist_lib::geometry::Body>,
        selection_active: bool,
        outline_width: f32,
        bounds_aabb: Option<([f32; 3], [f32; 3])>,
        outline_color: [f32; 4],
    ) {
        let mut s = self.scene.borrow_mut();
        s.bodies = bodies;
        if let Some(b) = first_body {
            s.base_color = b.color;
        }
        s.camera = self.cam();
        s.outline_enabled = selection_active;
        s.outline_width = outline_width;
        s.gizmo_lines.clear();
        s.gizmo_triangles.clear();
        if let Some((mn, mx)) = bounds_aabb {
            let center = [
                (mn[0] + mx[0]) * 0.5,
                (mn[1] + mx[1]) * 0.5,
                (mn[2] + mx[2]) * 0.5,
            ];
            let size = [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]];
            s.gizmo_lines
                .push(GizmoLineSet::bounds_box(center, size, None));
        }
        // Z control gizmo — anchored above the selected body's world
        // AABB. Stage 1: geometry only; mouse-drag wiring follows.
        if let Some(sel_id) = *self.inputs.selection.lock().unwrap() {
            let geom = self.current_geometry();
            if let Some(world_aabb) = selected_body_world_aabb(geom.as_deref(), sel_id) {
                let (arrow, sphere) = z_control_gizmo::z_control_for_aabb(world_aabb);
                s.gizmo_lines.push(arrow);
                s.gizmo_triangles.push(sphere);
            }
        }
        s.outline_color = outline_color;
        s.render_style = *self.inputs.render_style.lock().unwrap();
        s.draw_grid = *self.inputs.show_bed.lock().unwrap();
    }
}
