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
}
