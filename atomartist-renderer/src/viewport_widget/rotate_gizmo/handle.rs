//! Rotate-handle geometry.
//!
//! Each handle renders MatterCAD's curved double-arrow rotation glyph —
//! the [`super::arrow`] icon — triangulated and laid flat in the plane
//! perpendicular to its axis. This module owns the in-plane `(u, v)`
//! basis the glyph (and the compass in [`super::compass`]) map through,
//! and assembles the icon's 2-D triangle soup into a double-sided
//! [`GizmoTriangleSet`] at the handle's world centre.
//!
//! The clickable region stays a square AABB (see the `GizmoHandle`
//! built in `rotate_interactions.rs`) — only the *visual* is the arrow
//! glyph; picking a slightly larger square than the glyph's ink is the
//! same forgiving target MatterCAD's textured cube face gives.

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

/// The rotate-arrow glyph centred at `center`, lying in the plane
/// perpendicular to `axis`, sized so it fills a `size × size` square
/// (matching the old plate's footprint). Emitted double-sided (front +
/// reversed-winding back) so it reads from either side regardless of
/// back-face culling.
///
/// The glyph's 2-D triangle soup (in `[-0.5, 0.5]`, Y-up) comes from
/// [`super::arrow::arrow_icon_triangles`]; here we just scale by `size`
/// and lift each vertex into the axis plane via the `(u, v)` basis.
pub fn arrow_handle(center: [f32; 3], axis: u8, size: f32, color: [f32; 4]) -> GizmoTriangleSet {
    let (u, v) = plane_basis(axis);
    // Map a normalised 2-D glyph point (`[-0.5, 0.5]`) into the world
    // plane: scale by `size`, then ride the in-plane basis vectors.
    let map = |p: &[f32; 2]| {
        let su = p[0] * size;
        let sv = p[1] * size;
        [
            center[0] + u[0] * su + v[0] * sv,
            center[1] + u[1] * su + v[1] * sv,
            center[2] + u[2] * su + v[2] * sv,
        ]
    };
    let icon = super::arrow::arrow_icon_triangles();
    let mut vertices = Vec::with_capacity(icon.len() * 2);
    // Front winding, as triangulated.
    for tri in icon.chunks_exact(3) {
        vertices.push(map(&tri[0]));
        vertices.push(map(&tri[1]));
        vertices.push(map(&tri[2]));
    }
    // Back winding (reversed) so the glyph reads from behind too.
    for tri in icon.chunks_exact(3) {
        vertices.push(map(&tri[0]));
        vertices.push(map(&tri[2]));
        vertices.push(map(&tri[1]));
    }
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
    fn arrow_is_double_sided_and_in_plane() {
        let handle = arrow_handle([1.0, 2.0, 3.0], 2, 4.0, RED);
        // Whole triangles, and an even split between the front + reversed
        // back windings.
        assert!(handle.vertices.len() >= 6, "arrow must have geometry");
        assert_eq!(handle.vertices.len() % 6, 0, "front + back windings, 3 verts each");
        // Z-plane handle: every vertex shares the centre's Z.
        for vtx in &handle.vertices {
            assert!((vtx[2] - 3.0).abs() < 1e-5, "Z-plane handle left its plane");
        }
        assert_eq!(handle.color, RED);
    }

    #[test]
    fn arrow_fits_within_the_handle_footprint() {
        // The glyph maps from `[-0.5, 0.5]` scaled by `size`, so every
        // vertex must stay within ±size/2 of the centre in the plane.
        let size = 4.0_f32;
        let handle = arrow_handle([0.0, 0.0, 0.0], 2, size, RED);
        for vtx in &handle.vertices {
            assert!(vtx[0].abs() <= size * 0.5 + 1e-4, "x {} outside footprint", vtx[0]);
            assert!(vtx[1].abs() <= size * 0.5 + 1e-4, "y {} outside footprint", vtx[1]);
        }
    }
}
