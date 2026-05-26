//! Z control gizmo — vertical arrow + sphere handle anchored above
//! the selected body. Camera-distance proportional sizing so the
//! gizmo stays constant-pixel-size at any zoom; theme-aware colour
//! so it reads against light and dark backgrounds.
//!
//! MatterCAD reference: `PartPreviewWindow/View3D/Gui3D/MoveInZControl.cs`.
//! Mirrors that file's layout: anchor at the bounding box top-face
//! centre, offset above by `10 px + arrowSize/2` worth of world
//! units, idle colour = `theme.TextColor`, hover = `theme.PrimaryAccentColor`.
//!
//! Output: one `GizmoLineSet` for the arrow shaft + arrowhead and
//! one `GizmoTriangleSet` for the sphere handle. Both pure functions
//! of bbox + camera + viewport — easy to unit-test.

use crate::camera::OrbitCamera;
use crate::scene_renderer::gizmo_pass::{sphere_handle, GizmoLineSet, GizmoTriangleSet};

/// One handle id per Z control — single grab target for the
/// translate-Z action. Cube / scale handles can grow id 1 / 2 / etc.
/// when the rest of MoveInZControl ports.
pub const Z_TRANSLATE_HANDLE_ID: u32 = 0;

/// Sizing constants in **screen pixels**. Each is multiplied by the
/// world-units-per-pixel factor at the gizmo's anchor so the
/// on-screen size stays constant across zoom levels.
///
/// Mirrors MatterCAD's `MoveInZControl.cs`:
/// * Arrow shaft + arrowhead total ≈ `70 px`
/// * Sphere handle radius ≈ `8 px`
/// * Anchor sits `10 px` above the bbox top
const ARROW_LENGTH_PX: f32 = 70.0;
const HANDLE_RADIUS_PX: f32 = 8.0;
const ANCHOR_OFFSET_PX: f32 = 10.0;

/// Build a Z control sized for the supplied world-space AABB. The
/// gizmo is anchored above the AABB top-face centre with an offset +
/// arrow length + handle size computed in **screen pixels** — the
/// caller passes the camera + viewport height so the per-frame
/// world-units-per-pixel factor lands inside the math. `idle_color`
/// is the colour both the arrow lines and the sphere handle use.
pub fn z_control_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
    idle_color: [f32; 4],
) -> (GizmoLineSet, GizmoTriangleSet) {
    let ((anchor, arrow_len, handle_r), _) =
        z_control_layout_for_aabb(world_aabb, camera, viewport_height);
    build_z_control(anchor, arrow_len, handle_r, idle_color)
}

/// World-space pose of the Z control's draggable sphere — used by
/// the viewport's mouse-down hit-test to spot a click on the handle
/// before falling through to body-pick. Returns
/// `((anchor, arrow_length, handle_radius), (sphere_center, sphere_radius))`.
/// Both calls share the same math as [`z_control_for_aabb`] /
/// [`build_z_control`] so the rendered geometry and the pick AABB
/// match.
pub fn z_control_layout_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
) -> (([f32; 3], f32, f32), ([f32; 3], f32)) {
    let (mn, mx) = world_aabb;
    let cx = (mn[0] + mx[0]) * 0.5;
    let cy = (mn[1] + mx[1]) * 0.5;
    let top_z = mx[2];
    // World-units-per-pixel at the bbox top. The anchor itself is a
    // few pixels above the top, but at typical CAD camera angles the
    // depth change is negligible — sampling at the top centre is
    // close enough and avoids a circular dependency with the
    // anchor we're computing.
    let upp = camera.world_units_per_pixel_at([cx, cy, top_z], viewport_height);
    let arrow_len = ARROW_LENGTH_PX * upp;
    let handle_r = HANDLE_RADIUS_PX * upp;
    let anchor_offset = ANCHOR_OFFSET_PX * upp;
    let anchor = [cx, cy, top_z + anchor_offset];
    let sphere_center = [
        anchor[0],
        anchor[1],
        anchor[2] + arrow_len + handle_r * 0.5,
    ];
    ((anchor, arrow_len, handle_r), (sphere_center, handle_r))
}

/// Build the Z control's gizmo sets. `anchor` is the world-space
/// point at the *bottom* of the gizmo (typically the selected body's
/// top face); the arrow extends `+Z` by `arrow_length`. `handle_radius`
/// sizes the sphere at the tip.
///
/// Returns `(lines, sphere)`. The host pushes `lines` into
/// `gizmo_lines` and `sphere` into `gizmo_triangles` each frame; both
/// drop out when selection changes.
pub fn build_z_control(
    anchor: [f32; 3],
    arrow_length: f32,
    handle_radius: f32,
    color: [f32; 4],
) -> (GizmoLineSet, GizmoTriangleSet) {
    let tip = [anchor[0], anchor[1], anchor[2] + arrow_length];
    // Arrowhead: four short diagonal segments fanning down from the
    // tip, forming a "spike" silhouette. The angled segments make
    // the gizmo direction obvious at any camera angle.
    let head_h = arrow_length * 0.18;
    let head_r = arrow_length * 0.08;
    let head_base_z = tip[2] - head_h;
    let head_legs: [[f32; 3]; 4] = [
        [anchor[0] + head_r, anchor[1], head_base_z],
        [anchor[0] - head_r, anchor[1], head_base_z],
        [anchor[0], anchor[1] + head_r, head_base_z],
        [anchor[0], anchor[1] - head_r, head_base_z],
    ];
    let mut vertices = vec![anchor, tip];
    for leg in head_legs.iter() {
        vertices.push(*leg);
        vertices.push(tip);
    }
    let lines = GizmoLineSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        // Match NodeDesigner's control-gizmo overlay alpha.
        occluded_alpha: 0.35,
    };
    let sphere_center = [tip[0], tip[1], tip[2] + handle_radius * 0.5];
    let sphere = sphere_handle(sphere_center, handle_radius as f64, color);
    (lines, sphere)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    #[test]
    fn arrow_starts_at_anchor_and_extends_up_by_length() {
        let (lines, _) = build_z_control([1.0, 2.0, 3.0], 10.0, 1.0, RED);
        assert_eq!(lines.vertices[0], [1.0, 2.0, 3.0]);
        assert_eq!(lines.vertices[1], [1.0, 2.0, 13.0]);
    }

    #[test]
    fn sphere_handle_sits_above_tip() {
        let (_, sphere) = build_z_control([0.0, 0.0, 0.0], 10.0, 1.0, RED);
        let mut min_z = f32::INFINITY;
        let mut max_z = f32::NEG_INFINITY;
        for v in &sphere.vertices {
            if v[2] < min_z { min_z = v[2]; }
            if v[2] > max_z { max_z = v[2]; }
        }
        assert!((min_z - 9.5).abs() < 1e-3);
        assert!((max_z - 11.5).abs() < 1e-3);
    }

    #[test]
    fn caller_supplied_color_threads_through() {
        let (lines, sphere) = build_z_control([0.0, 0.0, 0.0], 1.0, 0.1, RED);
        assert_eq!(lines.color, RED);
        assert_eq!(sphere.color, RED);
    }

    #[test]
    fn arrow_emits_one_shaft_plus_four_arrowhead_legs() {
        let (lines, _) = build_z_control([0.0, 0.0, 0.0], 1.0, 0.1, RED);
        assert_eq!(lines.vertices.len(), 10);
    }

    #[test]
    fn layout_anchors_above_bbox_top_by_pixel_offset() {
        // Bounding box from (0,0,0) to (10,10,5). The anchor sits
        // above the top-face centre by 10 pixels worth of world
        // units. With a default camera the upp factor is positive
        // and non-zero, so anchor.z > bbox.max.z.
        let cam = crate::camera::OrbitCamera::default();
        let bbox = ([0.0, 0.0, 0.0], [10.0, 10.0, 5.0]);
        let ((anchor, arrow_len, handle_r), (sphere, sphere_r)) =
            z_control_layout_for_aabb(bbox, &cam, 720.0);
        assert_eq!(anchor[0], 5.0, "anchor X = bbox center X");
        assert_eq!(anchor[1], 5.0, "anchor Y = bbox center Y");
        assert!(anchor[2] > 5.0, "anchor lifts above bbox top");
        assert!(arrow_len > 0.0);
        assert!(handle_r > 0.0);
        // Sphere sits above the arrow tip.
        assert!(sphere[2] > anchor[2] + arrow_len);
        assert_eq!(sphere_r, handle_r);
    }
}
