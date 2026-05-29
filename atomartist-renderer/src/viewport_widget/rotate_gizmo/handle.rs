//! Rotate-handle plate geometry.
//!
//! Stage 1 renders each handle as a flat square plate lying in the
//! plane perpendicular to its axis — a visible, pickable grab target.
//! Stage 3 replaces the plate face with the tess2-triangulated arrow
//! icon built on the same in-plane `(u, v)` basis, so this module owns
//! the basis the icon will reuse.

use crate::scene_renderer::gizmo_pass::GizmoTriangleSet;

/// In-plane orthonormal basis `(u, v)` for the plane perpendicular to
/// `axis`. Chosen so `u × v` points along the axis and the pairing
/// matches the per-axis angle convention in
/// [`atomartist_lib::graph::node::angle_on_axis_plane`].
pub fn plane_basis(axis: u8) -> ([f32; 3], [f32; 3]) {
    match axis {
        0 => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]), // X plane: u = +Y, v = +Z
        1 => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]), // Y plane: u = +Z, v = +X
        _ => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), // Z plane: u = +X, v = +Y
    }
}

/// A flat square plate centred at `center`, lying in the plane
/// perpendicular to `axis`, with side length `size`. Emitted
/// double-sided (front + back winding) so it reads from either side
/// regardless of back-face culling. Placeholder grab target until the
/// arrow icon lands (Stage 3).
pub fn plate_handle(center: [f32; 3], axis: u8, size: f32, color: [f32; 4]) -> GizmoTriangleSet {
    let (u, v) = plane_basis(axis);
    let h = size * 0.5;
    let corner = |su: f32, sv: f32| {
        [
            center[0] + (u[0] * su + v[0] * sv) * h,
            center[1] + (u[1] * su + v[1] * sv) * h,
            center[2] + (u[2] * su + v[2] * sv) * h,
        ]
    };
    let a = corner(-1.0, -1.0);
    let b = corner(1.0, -1.0);
    let c = corner(1.0, 1.0);
    let d = corner(-1.0, 1.0);
    let vertices = vec![
        // front
        a, b, c, a, c, d, // back (reversed winding)
        a, c, b, a, d, c,
    ];
    GizmoTriangleSet {
        vertices,
        color,
        matrix: None,
        draw_solid: true,
        draw_overlay: true,
        // Match the control-gizmo overlay alpha used by the Z control.
        occluded_alpha: 0.35,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    #[test]
    fn plane_basis_is_orthonormal_and_spans_the_axis_plane() {
        for axis in 0..3u8 {
            let (u, v) = plane_basis(axis);
            let dot = u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
            assert!(dot.abs() < 1e-6, "axis {axis}: u·v not orthogonal");
            // u and v must lie in the plane (zero component along the axis).
            assert!(u[axis as usize].abs() < 1e-6, "axis {axis}: u leaves the plane");
            assert!(v[axis as usize].abs() < 1e-6, "axis {axis}: v leaves the plane");
        }
    }

    #[test]
    fn plate_is_double_sided_and_in_plane() {
        let plate = plate_handle([1.0, 2.0, 3.0], 2, 4.0, RED);
        assert_eq!(plate.vertices.len(), 12, "two triangles per side");
        // Z-plane plate: every vertex shares the centre's Z.
        for vtx in &plate.vertices {
            assert!((vtx[2] - 3.0).abs() < 1e-5, "Z plate left its plane");
        }
        assert_eq!(plate.color, RED);
    }
}
