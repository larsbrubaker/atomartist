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
        outline_color: [f32; 4],
    ) {
        let sel_id = *self.inputs.selection.lock().unwrap();
        // Index of the body whose `origin` NodeId matches the active
        // selection. The outline pass uses this so the silhouette
        // rims the body the user actually clicked on — not just the
        // first body in the group. `None` falls back to body 0 in
        // the renderer (e.g. selection-active but origin un-matched
        // somehow).
        let outline_body_index = sel_id.and_then(|id| {
            bodies.iter().position(|b| b.origin == Some(id))
        });
        let mut s = self.scene.borrow_mut();
        s.bodies = bodies;
        if let Some(b) = first_body {
            s.base_color = b.color;
        }
        s.camera = self.cam();
        s.outline_enabled = selection_active;
        s.outline_width = outline_width;
        s.outline_body_index = outline_body_index;
        s.gizmo_lines.clear();
        s.gizmo_triangles.clear();
        // Bounds-box gizmo dropped — the selection-outline rim
        // already gives a visible "this is selected" cue. The bounds
        // box added noise without orientation context.
        // Z control gizmo — anchored above the selected body's world
        // AABB. Camera-distance-proportional sizing keeps the gizmo
        // a constant pixel-size at any zoom; idle colour mirrors the
        // active theme's text colour (MatterCAD parity).
        if let Some(sel_id) = *self.inputs.selection.lock().unwrap() {
            let geom = self.current_geometry();
            if let Some(world_aabb) = selected_body_world_aabb(geom.as_deref(), sel_id) {
                let visuals = agg_gui::theme::current_visuals();
                let idle = [
                    visuals.text_color.r,
                    visuals.text_color.g,
                    visuals.text_color.b,
                    1.0,
                ];
                let cam = self.cam();
                let vh = self.bounds.height.max(1.0) as f32;
                let (arrow, sphere) =
                    z_control_gizmo::z_control_for_aabb(world_aabb, &cam, vh, idle);
                s.gizmo_lines.push(arrow);
                s.gizmo_triangles.push(sphere);
            }
        }
        s.outline_color = outline_color;
        s.render_style = *self.inputs.render_style.lock().unwrap();
        s.draw_grid = *self.inputs.show_bed.lock().unwrap();
    }
}
