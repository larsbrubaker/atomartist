//! Drag-math helpers for the viewport's body-drag interactions.
//!
//! Two interactions land here:
//!
//! * **XY bed-plane drag** (`bed_plane_translation`) — mouse-down on a
//!   body's mesh, drag to translate it across the bed (Z=0 plane).
//!   Pattern follows NodeDesigner's bed-plane translate + MatterCAD's
//!   `TranslateObject3D` gizmo: the world point under the cursor at
//!   drag start stays under the cursor for the rest of the drag.
//!
//! * **Z control drag** (`z_axis_translation`) — mouse-down on the
//!   green Z-control sphere handle above a selected body, drag to
//!   translate it along world Z. Projects the mouse ray onto the
//!   vertical line through the body's anchor; the handle stays under
//!   the cursor when the camera is at a typical CAD angle.
//!
//! * **Rotate-Z drag** (`bed_angle_about_center` + `rotate_about_world_z`)
//!   — mouse-down on the rotate gizmo's ring handle, swing the cursor
//!   around the body to spin it about the world vertical axis through
//!   the body's footprint centre. Pattern follows MatterCAD's
//!   `RotateObject3D` corner control: read the pointer's angle on a
//!   horizontal plane, then pre-multiply that rotation onto the node
//!   matrix so the spin happens in world space.
//!
//! Both helpers are pure functions of camera + cursor + matrix snapshot
//! so they can be unit-tested without a viewport widget.

use atomartist_lib::graph::node::matmul4x4;
use std::f32::consts::PI;

/// Translate `start_matrix` so its translation column lands at
/// `start_matrix.t + (current_bed_pt - anchor_bed_pt)` in the XY plane.
/// Z is left untouched — that's the Z control's job. Returns the
/// updated 4×4 column-major matrix.
pub fn bed_plane_translation(
    start_matrix: [f32; 16],
    anchor_bed_pt: [f32; 3],
    current_bed_pt: [f32; 3],
) -> [f32; 16] {
    let mut m = start_matrix;
    m[12] = start_matrix[12] + (current_bed_pt[0] - anchor_bed_pt[0]);
    m[13] = start_matrix[13] + (current_bed_pt[1] - anchor_bed_pt[1]);
    // Z column [14] stays at the start value — bed-plane drag doesn't
    // lift the body off the bed by accident.
    m
}

/// Project ray `(origin, dir)` onto the vertical line through
/// `anchor_xy` (world line `(anchor_xy[0], anchor_xy[1], t)` for
/// `t ∈ ℝ`). Returns the world Z of the closest point on that line
/// to the ray. `None` when the ray is degenerate (parallel to the Z
/// axis — the closest-point math is undefined).
///
/// The math: closest point between two 3-D lines reduces, when one
/// of them is purely vertical, to projecting the ray onto the
/// vertical line via the standard skew-line formula.
///
/// Used by Z-control drag — every mouse-move feeds the screen ray
/// through here and the result becomes the body's new world Z.
pub fn z_axis_translation(
    origin: [f32; 3],
    dir: [f32; 3],
    anchor_xy: [f32; 2],
) -> Option<f32> {
    // Standard skew-line closest-point formula. Lines:
    //   Ray R(s)      = origin + s * dir
    //   Vertical P(t) = anchor + t * z_hat, anchor.z = 0
    // Let w = origin - anchor. Then:
    //   a = dir · dir
    //   b = dir · z_hat = dir.z
    //   c = z_hat · z_hat = 1
    //   d = dir · w
    //   e = z_hat · w = w.z = origin.z
    //   denom = a * c - b² = a - b²
    //   t (on vertical line) = (a * e - b * d) / denom
    let dir_sq = dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2];
    let z_dir_dot = dir[2]; // dir · z_hat
    let denom = dir_sq - z_dir_dot * z_dir_dot;
    if denom.abs() < 1e-6 {
        return None;
    }
    let w = [
        origin[0] - anchor_xy[0],
        origin[1] - anchor_xy[1],
        origin[2],
    ];
    let d = dir[0] * w[0] + dir[1] * w[1] + dir[2] * w[2];
    let e = w[2];
    Some((dir_sq * e - z_dir_dot * d) / denom)
}

/// Translate `start_matrix` so its Z column lands at
/// `start_matrix.tz + (current_z - anchor_z)`. Mirrors
/// [`bed_plane_translation`] but for the single Z axis.
pub fn z_translation(start_matrix: [f32; 16], anchor_z: f32, current_z: f32) -> [f32; 16] {
    let mut m = start_matrix;
    m[14] = start_matrix[14] + (current_z - anchor_z);
    m
}

/// Intersect ray `(origin, dir)` with the horizontal plane `z =
/// plane_z` and return the angle (radians, CCW from world +X) of the
/// hit point about `center_xy`. `None` when the ray is parallel to
/// the plane (no intersection) or the hit lands exactly on the centre
/// (angle undefined).
///
/// Used by the rotate-Z gizmo: the handle lives on a ring in this
/// plane, so the pointer's angle about the centre is exactly the
/// "where am I dragging the handle to" signal. We don't reject a
/// negative ray parameter — at any sane CAD viewing angle the plane
/// is in front of the eye, and accepting either sign keeps the drag
/// from dropping frames near a grazing camera angle.
pub fn bed_angle_about_center(
    origin: [f32; 3],
    dir: [f32; 3],
    plane_z: f32,
    center_xy: [f32; 2],
) -> Option<f32> {
    if dir[2].abs() < 1e-9 {
        // Ray runs parallel to the horizontal plane — no single hit.
        return None;
    }
    let t = (plane_z - origin[2]) / dir[2];
    if !t.is_finite() {
        return None;
    }
    let px = origin[0] + t * dir[0];
    let py = origin[1] + t * dir[1];
    let dx = px - center_xy[0];
    let dy = py - center_xy[1];
    if dx * dx + dy * dy < 1e-12 {
        // Hit landed on the axis — angle is undefined.
        return None;
    }
    Some(dy.atan2(dx))
}

/// Wrap `angle` into the half-open interval `(-π, π]`. Used to turn a
/// raw `current - previous` pointer-angle difference into the shortest
/// signed step, so integrating those steps tracks continuous rotation
/// across the ±π atan2 seam instead of snapping a half-turn.
pub fn normalize_angle(angle: f32) -> f32 {
    let two_pi = 2.0 * PI;
    let mut a = angle % two_pi;
    if a > PI {
        a -= two_pi;
    } else if a <= -PI {
        a += two_pi;
    }
    a
}

/// Pre-multiply a world-space rotation of `angle` radians about the
/// vertical (world Z) axis through `center_xy` onto `start_matrix`.
///
/// The rendered `Body.matrix` is `node_matrix * upstream` (see
/// `geometry_props::compose_with_upstream`). To rotate the body about a
/// world axis we therefore apply the rotation on the **left** of the
/// node matrix: `R * node_matrix * upstream` rotates the composed
/// result while leaving `upstream` untouched, so the spin lands about
/// `center_xy` in world space regardless of any upstream transform.
///
/// `R` is the world-Z rotation about `center_xy`, i.e.
/// `T(center) · Rz(angle) · T(-center)`, expressed directly in
/// column-major form. Only the XY of the axis matters — a Z rotation
/// leaves the Z translation alone, so the centre's height is irrelevant.
pub fn rotate_about_world_z(
    start_matrix: [f32; 16],
    center_xy: [f32; 2],
    angle: f32,
) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    let (cx, cy) = (center_xy[0], center_xy[1]);
    // Column-major: col0 = (c, s, 0, 0), col1 = (-s, c, 0, 0),
    // col2 = Z axis, col3 = the translation that keeps `center_xy`
    // fixed under the rotation.
    let r: [f32; 16] = [
        c, s, 0.0, 0.0,
        -s, c, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        cx - (c * cx - s * cy),
        cy - (s * cx + c * cy),
        0.0,
        1.0,
    ];
    matmul4x4(&r, &start_matrix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> [f32; 16] {
        let mut m = [0.0_f32; 16];
        m[0] = 1.0; m[5] = 1.0; m[10] = 1.0; m[15] = 1.0;
        m
    }

    #[test]
    fn bed_plane_translation_applies_delta_in_xy_only() {
        let start = identity();
        let anchor = [1.0, 2.0, 0.0];
        let current = [3.5, 4.0, 0.0];
        let out = bed_plane_translation(start, anchor, current);
        assert!((out[12] - 2.5).abs() < 1e-6, "X translated by 2.5, got {}", out[12]);
        assert!((out[13] - 2.0).abs() < 1e-6, "Y translated by 2.0, got {}", out[13]);
        assert_eq!(out[14], 0.0, "Z untouched by bed-plane drag");
    }

    #[test]
    fn bed_plane_translation_preserves_rotation_and_scale() {
        // Non-identity start matrix: rotation + scale columns stay
        // exactly as they were; only the translation column moves.
        let start: [f32; 16] = [
            2.0, 0.0, 0.0, 0.0,   // col 0
            0.0, 3.0, 0.0, 0.0,   // col 1
            0.0, 0.0, 4.0, 0.0,   // col 2
            5.0, 6.0, 7.0, 1.0,   // col 3 (translation)
        ];
        let anchor = [0.0, 0.0, 0.0];
        let current = [1.0, -1.0, 0.0];
        let out = bed_plane_translation(start, anchor, current);
        // Rot/scale unchanged.
        assert_eq!(out[0], 2.0);
        assert_eq!(out[5], 3.0);
        assert_eq!(out[10], 4.0);
        assert_eq!(out[15], 1.0);
        // Translation X/Y shifted by delta, Z preserved.
        assert!((out[12] - 6.0).abs() < 1e-6);
        assert!((out[13] - 5.0).abs() < 1e-6);
        assert_eq!(out[14], 7.0);
    }

    #[test]
    fn z_axis_translation_handles_vertical_line() {
        // Ray pointing straight at the +X axis from (5, 0, 5):
        // direction (-1, 0, 0). Vertical line at (0, 0).
        // Closest point on the vertical line is (0, 0, 5) — t_on_z = 5.
        let out = z_axis_translation([5.0, 0.0, 5.0], [-1.0, 0.0, 0.0], [0.0, 0.0]);
        let z = out.expect("ray not parallel to Z");
        assert!((z - 5.0).abs() < 1e-5, "expected z=5, got {}", z);
    }

    #[test]
    fn z_axis_translation_rejects_vertical_ray() {
        // Ray straight up matches the vertical line direction —
        // closest-point math is degenerate. Caller must treat None
        // as "skip this frame, mouse drag is straight up the handle".
        let out = z_axis_translation([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        assert!(out.is_none(), "vertical ray returns None");
    }

    #[test]
    fn z_translation_applies_delta_in_z_only() {
        let start = identity();
        let out = z_translation(start, 5.0, 12.0);
        assert_eq!(out[12], 0.0);
        assert_eq!(out[13], 0.0);
        assert!((out[14] - 7.0).abs() < 1e-6);
    }

    /// Transform a world point through a column-major 4×4 matrix —
    /// test-only helper so the rotation assertions can check where a
    /// known point lands.
    fn xform(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
        [
            m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
            m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
            m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
        ]
    }

    #[test]
    fn bed_angle_reads_pointer_angle_about_center() {
        // Straight-down ray hitting the bed at (2, 0) relative to a
        // centre at the origin → angle 0 (along +X).
        let east = bed_angle_about_center([2.0, 0.0, 5.0], [0.0, 0.0, -1.0], 0.0, [0.0, 0.0])
            .expect("ray hits the plane");
        assert!(east.abs() < 1e-5, "expected angle 0, got {east}");
        // Hit at (0, 3) → +Y → angle +π/2.
        let north = bed_angle_about_center([0.0, 3.0, 5.0], [0.0, 0.0, -1.0], 0.0, [0.0, 0.0])
            .expect("ray hits the plane");
        assert!((north - std::f32::consts::FRAC_PI_2).abs() < 1e-5, "expected π/2, got {north}");
    }

    #[test]
    fn bed_angle_rejects_parallel_ray_and_centre_hit() {
        // Ray parallel to the horizontal plane never intersects it.
        assert!(bed_angle_about_center([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.0, [0.0, 0.0]).is_none());
        // Hit lands exactly on the rotation axis → angle undefined.
        assert!(bed_angle_about_center([0.0, 0.0, 5.0], [0.0, 0.0, -1.0], 0.0, [0.0, 0.0]).is_none());
    }

    #[test]
    fn normalize_angle_keeps_shortest_signed_step() {
        let eps = 1e-5;
        assert!(normalize_angle(0.0).abs() < eps);
        // Just past +π wraps to just past -π (shortest path).
        let almost = std::f32::consts::PI + 0.1;
        assert!((normalize_angle(almost) - (-std::f32::consts::PI + 0.1)).abs() < eps);
        // A full turn is a no-op.
        assert!(normalize_angle(2.0 * std::f32::consts::PI).abs() < eps);
        // Exactly +π stays +π; exactly -π wraps to +π (half-open at -π).
        assert!((normalize_angle(std::f32::consts::PI) - std::f32::consts::PI).abs() < eps);
        assert!((normalize_angle(-std::f32::consts::PI) - std::f32::consts::PI).abs() < eps);
    }

    #[test]
    fn rotate_about_world_z_spins_point_about_center() {
        // +90° about the origin maps (+X) → (+Y).
        let out = rotate_about_world_z(identity(), [0.0, 0.0], std::f32::consts::FRAC_PI_2);
        let p = xform(&out, [1.0, 0.0, 0.0]);
        assert!((p[0]).abs() < 1e-5, "x→0, got {}", p[0]);
        assert!((p[1] - 1.0).abs() < 1e-5, "y→1, got {}", p[1]);
        assert!((p[2]).abs() < 1e-5, "z untouched, got {}", p[2]);
    }

    #[test]
    fn rotate_about_world_z_leaves_center_fixed() {
        // The rotation axis passes through (3, 4): that point must not
        // move for any angle.
        let center = [3.0_f32, 4.0];
        let out = rotate_about_world_z(identity(), center, 1.234);
        let p = xform(&out, [center[0], center[1], 7.0]);
        assert!((p[0] - center[0]).abs() < 1e-5, "cx fixed, got {}", p[0]);
        assert!((p[1] - center[1]).abs() < 1e-5, "cy fixed, got {}", p[1]);
        assert!((p[2] - 7.0).abs() < 1e-5, "z preserved, got {}", p[2]);
    }

    #[test]
    fn rotate_about_world_z_zero_angle_is_identity() {
        // A no-op drag step must leave the matrix exactly as it was so
        // a coalesced rotate-then-release doesn't drift the body.
        let start: [f32; 16] = [
            2.0, 0.0, 0.0, 0.0,
            0.0, 3.0, 0.0, 0.0,
            0.0, 0.0, 4.0, 0.0,
            5.0, 6.0, 7.0, 1.0,
        ];
        let out = rotate_about_world_z(start, [1.0, 2.0], 0.0);
        for i in 0..16 {
            assert!((out[i] - start[i]).abs() < 1e-5, "entry {i} drifted: {} vs {}", out[i], start[i]);
        }
    }

    #[test]
    fn rotate_about_world_z_premultiplies_so_world_axis_wins() {
        // Body already translated to (10, 0). A +90° rotation about the
        // world origin must swing it to (0, 10) — i.e. the rotation is
        // about the world axis, NOT the body's local origin (which would
        // leave it at (10, 0)).
        let start = z_translation(bed_plane_translation(identity(), [0.0, 0.0, 0.0], [10.0, 0.0, 0.0]), 0.0, 0.0);
        let out = rotate_about_world_z(start, [0.0, 0.0], std::f32::consts::FRAC_PI_2);
        let origin_of_body = xform(&out, [0.0, 0.0, 0.0]);
        assert!((origin_of_body[0]).abs() < 1e-4, "x→0, got {}", origin_of_body[0]);
        assert!((origin_of_body[1] - 10.0).abs() < 1e-4, "y→10, got {}", origin_of_body[1]);
    }
}
