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
use crate::scene_renderer::gizmo_pass::{cone_handle, GizmoLineSet, GizmoTriangleSet};

/// One handle id per Z control — single grab target for the
/// translate-Z action.
pub const Z_TRANSLATE_HANDLE_ID: u32 = 0;

/// Pixel-based size floors. The arrow line length AND the handle
/// cube each have a minimum size in screen pixels so very small
/// bodies still produce a usable hit target — but past that floor
/// they grow with the body's AABB Z extent so a tall body shows a
/// tall arrow (mirrors MatterCAD's `MoveInZControl`).
const ARROW_LENGTH_MIN_PX: f32 = 60.0;
const HANDLE_SIZE_PX: f32 = 12.0;
const ANCHOR_OFFSET_PX: f32 = 10.0;
/// Arrow line length as a fraction of the AABB Z extent. Caps the
/// gizmo at a sensible proportion of the body — same fraction the
/// MatterCAD measurement controls use.
const ARROW_LENGTH_AABB_FACTOR: f32 = 0.5;

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
    let z_extent = (mx[2] - mn[2]).max(0.0);
    // World-units-per-pixel at the bbox top.
    let upp = camera.world_units_per_pixel_at([cx, cy, top_z], viewport_height);
    // Arrow line length grows with AABB Z extent so tall bodies get
    // a long arrow + short bodies get a short one — both stay
    // visually proportional. Floor at a pixel-based minimum so
    // tiny bodies still produce a usable handle.
    let arrow_len = (z_extent * ARROW_LENGTH_AABB_FACTOR).max(ARROW_LENGTH_MIN_PX * upp);
    // Cube handle stays pixel-sized — predictable hit target
    // regardless of body size, matching MatterCAD's scale-handle
    // sizing rule.
    let handle_size = HANDLE_SIZE_PX * upp;
    let anchor_offset = ANCHOR_OFFSET_PX * upp;
    let anchor = [cx, cy, top_z + anchor_offset];
    // Cube centre sits one half-cube above the arrow tip so the
    // cube's bottom face touches the tip.
    let cube_center = [
        anchor[0],
        anchor[1],
        anchor[2] + arrow_len + handle_size * 0.5,
    ];
    ((anchor, arrow_len, handle_size), (cube_center, handle_size))
}

/// Build the Z control's gizmo sets. `anchor` is the world-space
/// point at the *bottom* of the gizmo (typically the selected body's
/// top face); the arrow line extends `+Z` by `arrow_length`. The
/// handle cube sits one half-cube above the arrow tip — that's the
/// click + drag target. MatterCAD's `MoveInZControl` uses the same
/// shaft + cube layout.
///
/// `handle_size` is the cube's edge length. Use the same value the
/// caller hands to [`pick_handle`] so the click hit-test box matches
/// what's drawn.
pub fn build_z_control(
    anchor: [f32; 3],
    arrow_length: f32,
    handle_size: f32,
    color: [f32; 4],
) -> (GizmoLineSet, GizmoTriangleSet) {
    let tip = [anchor[0], anchor[1], anchor[2] + arrow_length];
    // Single line shaft from anchor to tip; the cube handle on top
    // doubles as the arrowhead so we don't need a separate spike.
    let vertices = vec![anchor, tip];
    let lines = GizmoLineSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        // Match NodeDesigner's control-gizmo overlay alpha.
        occluded_alpha: 0.35,
    };
    // MatterCAD `MoveInZControl` uses a cone arrowhead for the
    // translate-Z handle (a box is reserved for height / scale). Centre
    // the cone at the same point the pick-AABB uses (`cube_center`), so
    // the hit-test box still wraps it; its base sits at the arrow tip
    // and the apex points +Z.
    let cone_center = [tip[0], tip[1], tip[2] + handle_size * 0.5];
    let cone = cone_handle(cone_center, (handle_size * 0.5) as f64, handle_size as f64, color);
    (lines, cone)
}

/// Build the Z-drag **measurement** overlay shown while a `DragBodyZ`
/// is in flight (MatterCAD pops a measure control + distance during the
/// move-in-Z drag). A vertical witness line runs from the bed (`z = 0`)
/// up to the selected body's bottom, offset just past the body's `+X`
/// side so it doesn't overlap the mesh, with short end ticks. Returns
/// the line set, the world point to anchor the 2-D distance label, and
/// the measured height (body bottom above the bed) for the label text.
///
/// Drawn on top (`occluded_alpha = 1.0`) so the dimension reads even
/// where the body would occlude it.
pub fn z_measure(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
    color: [f32; 4],
) -> (GizmoLineSet, [f32; 3], f32) {
    let (mn, mx) = world_aabb;
    let cy = (mn[1] + mx[1]) * 0.5;
    let bottom = mn[2];
    let upp = camera.world_units_per_pixel_at([mx[0], cy, bottom], viewport_height);
    let margin = 20.0 * upp; // push the witness line past the body's side
    let tick = 8.0 * upp; // end-tick half length
    let ox = mx[0] + margin;
    let mut vertices = Vec::with_capacity(6);
    // Main vertical witness line, bed → body bottom.
    vertices.push([ox, cy, 0.0]);
    vertices.push([ox, cy, bottom]);
    // End ticks (along X) at each end.
    vertices.push([ox - tick, cy, 0.0]);
    vertices.push([ox + tick, cy, 0.0]);
    vertices.push([ox - tick, cy, bottom]);
    vertices.push([ox + tick, cy, bottom]);
    let lines = GizmoLineSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        occluded_alpha: 1.0,
    };
    let label = [ox, cy, bottom * 0.5];
    (lines, label, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    #[test]
    fn z_measure_spans_bed_to_body_bottom() {
        let cam = OrbitCamera::default();
        // Body bottom at z=15.55, beside +X face at x=10.
        let aabb = ([0.0, 0.0, 15.55], [10.0, 6.0, 25.0]);
        let (lines, label, value) = z_measure(aabb, &cam, 720.0, RED);
        assert!((value - 15.55).abs() < 1e-3, "measured height = body bottom Z");
        // First segment runs from the bed (z=0) to the bottom (z=15.55).
        assert!((lines.vertices[0][2]).abs() < 1e-4, "witness line starts at the bed");
        assert!((lines.vertices[1][2] - 15.55).abs() < 1e-3, "witness line ends at the body bottom");
        // Offset beyond the +X face, and the label rides the line midpoint.
        assert!(lines.vertices[0][0] > 10.0, "witness line sits past the body's +X side");
        assert!((label[2] - 15.55 * 0.5).abs() < 1e-3, "label anchored at the line midpoint");
        // Drawn on top so the dimension never hides behind the body.
        assert!((lines.occluded_alpha - 1.0).abs() < 1e-6);
    }

    #[test]
    fn arrow_starts_at_anchor_and_extends_up_by_length() {
        let (lines, _) = build_z_control([1.0, 2.0, 3.0], 10.0, 1.0, RED);
        assert_eq!(lines.vertices[0], [1.0, 2.0, 3.0]);
        assert_eq!(lines.vertices[1], [1.0, 2.0, 13.0]);
    }

    #[test]
    fn cone_handle_sits_on_the_arrow_tip_pointing_up() {
        // handle_size = 2 → cone height 2, base at the tip (z=10), apex
        // one handle-size above (z=12).
        let (_, cone) = build_z_control([0.0, 0.0, 0.0], 10.0, 2.0, RED);
        let mut min_z = f32::INFINITY;
        let mut max_z = f32::NEG_INFINITY;
        for v in &cone.vertices {
            if v[2] < min_z { min_z = v[2]; }
            if v[2] > max_z { max_z = v[2]; }
        }
        assert!((min_z - 10.0).abs() < 1e-3, "cone base expected at tip z=10, got {min_z}");
        assert!((max_z - 12.0).abs() < 1e-3, "cone apex expected at z=12, got {max_z}");
    }

    #[test]
    fn caller_supplied_color_threads_through() {
        let (lines, cone) = build_z_control([0.0, 0.0, 0.0], 1.0, 0.1, RED);
        assert_eq!(lines.color, RED);
        assert_eq!(cone.color, RED);
    }

    #[test]
    fn arrow_emits_single_shaft_segment() {
        let (lines, _) = build_z_control([0.0, 0.0, 0.0], 1.0, 0.1, RED);
        // Anchor → tip = one segment = 2 vertices. Cube handle
        // doubles as the arrowhead — no spike geometry needed.
        assert_eq!(lines.vertices.len(), 2);
    }

    #[test]
    fn arrow_length_scales_with_bbox_z_extent() {
        // Tall body → long arrow; short body → minimum-pixel-sized
        // arrow.
        let cam = crate::camera::OrbitCamera::default();
        let short = ([0.0, 0.0, 0.0], [10.0, 10.0, 2.0]);
        let tall = ([0.0, 0.0, 0.0], [10.0, 10.0, 200.0]);
        let ((_, short_len, _), _) =
            z_control_layout_for_aabb(short, &cam, 720.0);
        let ((_, tall_len, _), _) =
            z_control_layout_for_aabb(tall, &cam, 720.0);
        assert!(
            tall_len > short_len,
            "taller body must produce a taller arrow; tall={tall_len} short={short_len}",
        );
    }

    #[test]
    fn layout_anchors_above_bbox_top_by_pixel_offset() {
        let cam = crate::camera::OrbitCamera::default();
        let bbox = ([0.0, 0.0, 0.0], [10.0, 10.0, 5.0]);
        let ((anchor, arrow_len, handle_size), (cube, cube_size)) =
            z_control_layout_for_aabb(bbox, &cam, 720.0);
        assert_eq!(anchor[0], 5.0);
        assert_eq!(anchor[1], 5.0);
        assert!(anchor[2] > 5.0);
        assert!(arrow_len > 0.0);
        assert!(handle_size > 0.0);
        assert!(cube[2] > anchor[2] + arrow_len);
        assert_eq!(cube_size, handle_size);
    }
}
