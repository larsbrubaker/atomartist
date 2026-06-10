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
//! Rotation drag lives elsewhere: the angle + matrix math is shared
//! from `atomartist_lib::graph::node`
//! (`angle_on_axis_plane`, `rotate_about_world_axis`, `normalize_angle`)
//! and the gizmo geometry in `viewport_widget/rotate_gizmo/`.
//!
//! Both helpers are pure functions of camera + cursor + matrix snapshot
//! so they can be unit-tested without a viewport widget.

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

/// Parameter `t` of the closest point on the line
/// `L(t) = line_origin + t · line_dir` to the mouse ray — the
/// arbitrary-axis generalisation of [`z_axis_translation`]. The height
/// control's field path drags along the body's **rotated** local-Z
/// axis (MatterCAD measures `(newPosition - bottom).Length` along
/// `top - bottom`), so the projection line is the rotated axis, not
/// world Z. `line_dir` need not be unit length, but `t` is in units of
/// `|line_dir|`; pass a unit vector to get world units. `None` when
/// the ray is (near-)parallel to the line.
pub fn axis_param(
    ray_o: [f32; 3],
    ray_d: [f32; 3],
    line_origin: [f32; 3],
    line_dir: [f32; 3],
) -> Option<f32> {
    // Skew-line closest point: lines P(t) = O + t·u (axis, param
    // wanted) and Q(s) = ro + s·v (mouse ray), w = O − ro:
    //   t = (b·e − c·d) / (a·c − b²)
    // with a = u·u, b = u·v, c = v·v, d = u·w, e = v·w.
    let u = line_dir;
    let v = ray_d;
    let w = [
        line_origin[0] - ray_o[0],
        line_origin[1] - ray_o[1],
        line_origin[2] - ray_o[2],
    ];
    let dot = |p: [f32; 3], q: [f32; 3]| p[0] * q[0] + p[1] * q[1] + p[2] * q[2];
    let (a, b, c, d, e) = (dot(u, u), dot(u, v), dot(v, v), dot(u, w), dot(v, w));
    let denom = a * c - b * b;
    if denom.abs() < 1e-6 {
        return None;
    }
    Some((b * e - c * d) / denom)
}

/// Scale `start_matrix` in **world Z** about the plane `z = bottom_z`,
/// keeping that plane fixed so the body's base stays planted while it
/// grows / shrinks upward. World-space pre-multiply by
/// `Translate(0,0,b) · Scale(1,1,s) · Translate(0,0,-b)`, which leaves
/// every row but Z untouched and maps `z' = b + s·(z − b)`.
///
/// Used by the height control's matrix path — objects with no editable
/// height parameter scale their transform instead of a mesh field.
/// Mirrors MatterCAD's `ScaleMatrixTopControl` (scale Z, lock bottom).
pub fn scale_z_about_bottom(start_matrix: [f32; 16], scale: f32, bottom_z: f32) -> [f32; 16] {
    let mut m = start_matrix;
    // Only the Z row changes: for each column c, z' = s·z + b·(1−s)·w.
    for c in 0..4 {
        let z = start_matrix[c * 4 + 2];
        let w = start_matrix[c * 4 + 3];
        m[c * 4 + 2] = scale * z + bottom_z * (1.0 - scale) * w;
    }
    m
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

    #[test]
    fn scale_z_about_bottom_keeps_the_base_planted() {
        // Identity start, scale ×2 about z = 10. A point at the bottom
        // plane (z = 10) must stay; a point at z = 20 must map to z = 30
        // (height from the base doubles).
        let m = scale_z_about_bottom(identity(), 2.0, 10.0);
        let z_of = |p: [f32; 4]| {
            // column-major matrix · point
            m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14] * p[3]
        };
        assert!((z_of([0.0, 0.0, 10.0, 1.0]) - 10.0).abs() < 1e-5, "base plane stays planted");
        assert!((z_of([0.0, 0.0, 20.0, 1.0]) - 30.0).abs() < 1e-5, "top doubles its height");
        // X / Y rows are untouched.
        assert_eq!(m[0], 1.0);
        assert_eq!(m[5], 1.0);
    }

    #[test]
    fn scale_z_about_bottom_preserves_rotation_columns_xy() {
        // A rotation+translation start matrix: scaling Z about the base
        // must not disturb the X/Y rows of any column.
        let start: [f32; 16] = [
            0.0, 1.0, 0.0, 0.0,
            -1.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            3.0, 4.0, 5.0, 1.0,
        ];
        let out = scale_z_about_bottom(start, 3.0, 0.0);
        // bottom_z = 0 → z' = 3·z, translation Z scales too: 5 → 15.
        assert!((out[14] - 15.0).abs() < 1e-5);
        // X/Y of every column unchanged.
        for c in 0..4 {
            assert_eq!(out[c * 4], start[c * 4], "col {c} X row");
            assert_eq!(out[c * 4 + 1], start[c * 4 + 1], "col {c} Y row");
        }
    }
}
