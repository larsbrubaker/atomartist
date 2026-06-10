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
use crate::scene_renderer::gizmo_pass::{
    cone_handle, oriented_cube_handle, GizmoLineSet, GizmoTriangleSet,
};

/// One handle id per Z control — single grab target for the
/// translate-Z action.
pub const Z_TRANSLATE_HANDLE_ID: u32 = 0;

/// Handle id for the height / scale-Z box (top-face centre). Distinct
/// from the Z-translate cone (0) and the rotate handles (10..12) so a
/// combined hit-test tells them apart.
pub const HEIGHT_HANDLE_ID: u32 = 1;

/// Height-box edge length in screen pixels — a small cube on the top
/// face (MatterCAD's `ScaleHeightControl` uses a 7px cube; we use a
/// slightly larger one for grabbability, sitting just under the
/// move-Z cone's arrow). Pub so the widget's mode-aware
/// `height_box_layout` (field path: object-top placement) sizes its
/// box identically to the AABB-mode layout below.
pub(crate) const HEIGHT_BOX_PX: f32 = 11.0;

/// Z-translate cone sizing, in screen pixels. MatterCAD's
/// `MoveInZControl.SetPosition` parks the up-arrow `10 px +
/// upArrowSize/2` above the AABB top — close to the body, no long
/// shaft. Our cone must additionally clear the height box (which
/// spans `0..HEIGHT_BOX_PX` above the top face, taller than
/// MatterCAD's 7 px cube), so its base starts a clear gap above the
/// box top — in MatterCAD the two controls never touch.
const HANDLE_SIZE_PX: f32 = 12.0;
const Z_CONE_GAP_PX: f32 = 8.0;
const ANCHOR_OFFSET_PX: f32 = HEIGHT_BOX_PX + Z_CONE_GAP_PX;

/// Build the Z-translate cone for the supplied world-space AABB —
/// hovering just above the top-face centre (MatterCAD's
/// `MoveInZControl` up arrow). `idle_color` is the cone colour.
pub fn z_control_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
    idle_color: [f32; 4],
) -> GizmoTriangleSet {
    let (center, size) = z_control_layout_for_aabb(world_aabb, camera, viewport_height);
    cone_handle(center, (size * 0.5) as f64, size as f64, idle_color)
}

/// World pose `(cone_center, size)` of the Z-translate cone — used by
/// the draw AND the viewport's mouse-down/hover hit-tests so the
/// rendered geometry and the pick AABB match. The cone floats
/// `10 px + size/2` above the AABB top-face centre, constant
/// pixel-size via the camera's world-units-per-pixel factor.
pub fn z_control_layout_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
) -> ([f32; 3], f32) {
    let (mn, mx) = world_aabb;
    let cx = (mn[0] + mx[0]) * 0.5;
    let cy = (mn[1] + mx[1]) * 0.5;
    z_control_layout_at([cx, cy, mx[2]], camera, viewport_height)
}

/// [`z_control_layout_for_aabb`] from an explicit top-face-centre
/// point. The drag-time draw anchors the cone to the drag state
/// (`start_top_z + live_dz`) instead of the async-rebuilt geometry,
/// so the control tracks the cursor without rebuild lag.
pub fn z_control_layout_at(
    top_center: [f32; 3],
    camera: &OrbitCamera,
    viewport_height: f32,
) -> ([f32; 3], f32) {
    let upp = camera.world_units_per_pixel_at(top_center, viewport_height);
    let size = HANDLE_SIZE_PX * upp;
    let center = [
        top_center[0],
        top_center[1],
        top_center[2] + (ANCHOR_OFFSET_PX + HANDLE_SIZE_PX * 0.5) * upp,
    ];
    (center, size)
}

/// Build the Z-translate cone at an explicit pose (see
/// [`z_control_layout_at`]).
pub fn z_cone(center: [f32; 3], size: f32, color: [f32; 4]) -> GizmoTriangleSet {
    cone_handle(center, (size * 0.5) as f64, size as f64, color)
}

/// World pose of the height / scale-Z box — a small cube centred on the
/// selection's top-face centre, its base sitting on the top face. The
/// box is the grab target for the height control (edits the body's
/// `height` parameter, or scales the matrix in Z when there is none).
/// Returns `(box_center, box_size)`; the box is constant pixel-size via
/// the camera's world-units-per-pixel factor. Shared by the renderer
/// (draw) and the viewport (hit-test) so both match.
pub fn height_control_layout_for_aabb(
    world_aabb: ([f32; 3], [f32; 3]),
    camera: &OrbitCamera,
    viewport_height: f32,
) -> ([f32; 3], f32) {
    let (mn, mx) = world_aabb;
    let cx = (mn[0] + mx[0]) * 0.5;
    let cy = (mn[1] + mx[1]) * 0.5;
    let top_z = mx[2];
    let upp = camera.world_units_per_pixel_at([cx, cy, top_z], viewport_height);
    let size = HEIGHT_BOX_PX * upp;
    // Base of the cube on the top face → centre half a box above it.
    let center = [cx, cy, top_z + size * 0.5];
    (center, size)
}

/// Build the height-box gizmo (a cube handle) at `center` with edge
/// length `size`, oriented along `axes` so the box tilts with the
/// body's rotated top face in field mode (MatterCAD rotates the
/// `ScaleHeightControl` mesh with the selection). Pass identity axes
/// for the axis-aligned matrix mode. `color` is idle (theme text) or
/// accent on hover.
pub fn height_control(
    center: [f32; 3],
    size: f32,
    axes: [[f32; 3]; 3],
    color: [f32; 4],
) -> GizmoTriangleSet {
    oriented_cube_handle(center, size as f64, axes, color)
}

/// Identity basis for [`height_control`] — the axis-aligned matrix
/// mode / fallback orientation.
pub const AXIS_ALIGNED: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Measure-bars layout, in screen pixels — MatterCAD's
/// `DrawMeasurementLines3D` (identical in `ScaleHeightControl` and
/// `MoveInZControl`): bars start a small gap from the measured points
/// and extend `MEASURE_BAR_PX` toward screen-right.
const MEASURE_GAP_PX: f32 = 5.0;
const MEASURE_BAR_PX: f32 = 55.0;
const MEASURE_ARROW_PX: f32 = 8.0;

/// Build a MatterCAD-style **measurement** between two world points:
/// a perpendicular bar at each end (gap 5 px, length 55 px, pointing
/// screen-right) and a connecting line between the bar midpoints with
/// arrowheads at both ends. Used by the move-in-Z drag (bed → body
/// bottom) and the height drag (body bottom → top) — both controls
/// share this exact pattern in MatterCAD (`DrawMeasurementLines3D`).
///
/// Returns the line set, the world point at the connecting line's
/// midpoint (anchor for the 2-D value label), and the measured
/// distance `|end − start|`.
pub fn measure_bars(
    start: [f32; 3],
    end: [f32; 3],
    camera: &OrbitCamera,
    viewport_height: f32,
    color: [f32; 4],
) -> (GizmoLineSet, [f32; 3], f32) {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
    let mul = |a: [f32; 3], k: f32| [a[0] * k, a[1] * k, a[2] * k];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let d = sub(end, start);
    let length = dot(d, d).sqrt();
    let axis = if length > 1e-6 { mul(d, 1.0 / length) } else { [0.0, 0.0, 1.0] };
    // Camera basis from the view matrix's rotation rows: row 0 is the
    // camera-right direction in world space, row 2 points from the
    // scene toward the eye. Bars run perpendicular to both the measure
    // axis and the view (so they face the camera), flipped to point
    // screen-right — MatterCAD's tick-direction logic.
    let view = camera.view_matrix();
    let cam_right = [view[0], view[4], view[8]];
    let to_eye = [view[2], view[6], view[10]];
    let mut tick = cross(axis, to_eye);
    let tick_len = dot(tick, tick).sqrt();
    tick = if tick_len > 1e-4 { mul(tick, 1.0 / tick_len) } else { cam_right };
    if dot(tick, cam_right) < 0.0 {
        tick = mul(tick, -1.0);
    }

    let upp_s = camera.world_units_per_pixel_at(start, viewport_height);
    let upp_e = camera.world_units_per_pixel_at(end, viewport_height);
    let bar = |p: [f32; 3], upp: f32| {
        (
            add(p, mul(tick, MEASURE_GAP_PX * upp)),
            add(p, mul(tick, (MEASURE_GAP_PX + MEASURE_BAR_PX) * upp)),
        )
    };
    let (s0, s1) = bar(start, upp_s);
    let (e0, e1) = bar(end, upp_e);
    let mid_s = mul(add(s0, s1), 0.5);
    let mid_e = mul(add(e0, e1), 0.5);

    let mut vertices = Vec::with_capacity(14);
    // End bars.
    vertices.push(s0);
    vertices.push(s1);
    vertices.push(e0);
    vertices.push(e1);
    // Connecting line between bar midpoints.
    vertices.push(mid_s);
    vertices.push(mid_e);
    // Arrowhead wings at both tips (MatterCAD's startArrow/endArrow):
    // two short segments per end angling back from the tip.
    let mut wings = |tip: [f32; 3], back: [f32; 3], upp: f32| {
        let base = add(tip, mul(back, MEASURE_ARROW_PX * upp));
        for side in [1.0, -1.0] {
            vertices.push(tip);
            vertices.push(add(base, mul(tick, side * MEASURE_ARROW_PX * 0.45 * upp)));
        }
    };
    wings(mid_e, mul(axis, -1.0), upp_e);
    wings(mid_s, axis, upp_s);

    let lines = GizmoLineSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        // Dim where occluded, like MatterCAD's non-depth-tested pass
        // (`theme.TextColor.WithAlpha(Constants.LineAlpha)`).
        occluded_alpha: 0.35,
    };
    let label = mul(add(mid_s, mid_e), 0.5);
    (lines, label, length)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    #[test]
    fn height_box_sits_on_the_top_face_center() {
        let cam = OrbitCamera::default();
        let aabb = ([0.0, 0.0, 0.0], [10.0, 6.0, 4.0]);
        let (center, size) = height_control_layout_for_aabb(aabb, &cam, 720.0);
        assert!((center[0] - 5.0).abs() < 1e-4, "box centred on top-face X");
        assert!((center[1] - 3.0).abs() < 1e-4, "box centred on top-face Y");
        assert!(center[2] > 4.0, "box sits above the top face (z = 4)");
        assert!(size > 0.0, "box must have a usable size");
        // Cube geometry round-trips through the handle builder.
        let g = height_control(center, size, AXIS_ALIGNED, RED);
        assert_eq!(g.vertices.len(), 36, "cube = 12 triangles");
        assert_eq!(g.color, RED);
    }

    /// The cube must tilt with the basis it's given (field mode aligns
    /// it to the body's rotated top face). A 45°-about-X basis pushes
    /// corners out to `half·√2` along Y; the axis-aligned cube never
    /// exceeds `half`.
    #[test]
    fn height_control_cube_tilts_with_axes() {
        let center = [5.0, 3.0, 10.0];
        let size = 2.0_f32;
        let half = size * 0.5;
        let max_dy = |g: &GizmoTriangleSet| {
            g.vertices
                .iter()
                .map(|v| (v[1] - center[1]).abs())
                .fold(0.0_f32, f32::max)
        };
        let aligned = height_control(center, size, AXIS_ALIGNED, RED);
        assert!(max_dy(&aligned) <= half * 1.01, "axis-aligned cube stays within half");
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let rot45_x = [[1.0, 0.0, 0.0], [0.0, s, s], [0.0, -s, s]];
        let tilted = height_control(center, size, rot45_x, RED);
        assert!(
            max_dy(&tilted) > half * 1.3,
            "45°-tilted cube corners must exceed the axis-aligned extent, got {}",
            max_dy(&tilted),
        );
    }

    /// The measure pattern: a bar at each end offset toward
    /// screen-right, a connecting line between bar midpoints, and
    /// arrowhead wings at both tips — MatterCAD's
    /// `DrawMeasurementLines3D`, here spanning bed → body bottom.
    #[test]
    fn measure_bars_span_between_the_two_points() {
        let cam = OrbitCamera::default();
        let start = [5.0, 3.0, 0.0]; // bed
        let end = [5.0, 3.0, 15.55]; // body bottom
        let (lines, label, value) = measure_bars(start, end, &cam, 720.0, RED);
        assert!((value - 15.55).abs() < 1e-3, "measured value = |end - start|");
        // 2 bars + connecting line + 4 arrow wings = 7 segments.
        assert_eq!(lines.vertices.len(), 14);
        // Bars sit at the two measured heights, offset away from the
        // measured axis (never touching the points themselves).
        assert!((lines.vertices[0][2]).abs() < 1e-3, "start bar at the bed");
        assert!((lines.vertices[2][2] - 15.55).abs() < 1e-3, "end bar at the body bottom");
        let off = |v: [f32; 3]| ((v[0] - 5.0).powi(2) + (v[1] - 3.0).powi(2)).sqrt();
        assert!(off(lines.vertices[0]) > 0.0, "bar starts a gap away from the axis");
        assert!(
            off(lines.vertices[1]) > off(lines.vertices[0]),
            "bar extends outward from the gap",
        );
        // Connecting line spans the two bar midpoints vertically.
        assert!((lines.vertices[4][2]).abs() < 1e-3);
        assert!((lines.vertices[5][2] - 15.55).abs() < 1e-3);
        // Label anchored at the connecting line's midpoint.
        assert!((label[2] - 15.55 * 0.5).abs() < 1e-3);
        // Dimmed where occluded (MatterCAD's alpha overlay pass).
        assert!(lines.occluded_alpha > 0.0 && lines.occluded_alpha < 1.0);
    }

    /// The Z-translate cone stays close to the AABB top (MatterCAD
    /// `MoveInZControl.SetPosition` — no long shaft) but its base must
    /// clear the height box below it by a visible gap; in MatterCAD
    /// the two controls never touch.
    #[test]
    fn z_cone_floats_just_above_the_aabb_top() {
        let cam = crate::camera::OrbitCamera::default();
        let bbox = ([0.0, 0.0, 0.0], [10.0, 10.0, 5.0]);
        let (center, size) = z_control_layout_for_aabb(bbox, &cam, 720.0);
        assert_eq!(center[0], 5.0);
        assert_eq!(center[1], 5.0);
        assert!(size > 0.0);
        let upp = cam.world_units_per_pixel_at([5.0, 5.0, 5.0], 720.0);
        let expected_z = 5.0 + (ANCHOR_OFFSET_PX + 6.0) * upp; // gap above box + half cone
        assert!(
            (center[2] - expected_z).abs() < 1e-4,
            "cone must hug the top: expected z {expected_z}, got {}",
            center[2],
        );
        // The cone's base must sit clearly above the height box's top
        // (box spans 0..HEIGHT_BOX_PX above the top face).
        let cone_bottom = center[2] - size * 0.5;
        let box_top = 5.0 + HEIGHT_BOX_PX * upp;
        assert!(
            cone_bottom - box_top >= 7.0 * upp,
            "cone base must clear the height box by a visible gap; \
             cone bottom {cone_bottom}, box top {box_top}",
        );
        // Geometry: apex up, base down, caller colour threads through.
        let cone = z_control_for_aabb(bbox, &cam, 720.0, RED);
        assert_eq!(cone.color, RED);
        let max_z = cone.vertices.iter().map(|v| v[2]).fold(f32::NEG_INFINITY, f32::max);
        let min_z = cone.vertices.iter().map(|v| v[2]).fold(f32::INFINITY, f32::min);
        assert!((max_z - (center[2] + size * 0.5)).abs() < 1e-3, "apex half a size above centre");
        assert!((min_z - (center[2] - size * 0.5)).abs() < 1e-3, "base half a size below centre");
    }
}
