//! `Geometry3d` — the bundle that flows along Geometry3d sockets.
//!
//! Inspired by NodeDesigner's mesh+`_data.matrix`+`_data.color` model.
//! Every geometry-producing node emits a [`Geometry3d`] carrying:
//!
//! * `mesh`  — the actual triangle mesh (shared via `Arc` so downstream
//!   nodes don't pay copy cost).
//! * `matrix` — the node's own transform (column-major 4×4). Identity
//!   by default; updated when the user drags a control gizmo (Z, XY,
//!   rotate-corner).
//! * `color` — RGBA tint used by the renderer as `base_color` for the
//!   shaded surface.
//!
//! Downstream operations (Transform, Align, FitToBounds, Combine,
//! Boolean) compose the upstream geometry's matrix into their own
//! before re-emitting a new `Geometry3d`, so a downstream node can
//! read the cumulative world transform via `geom.matrix` without
//! walking the graph.

use std::sync::Arc;

use manifold_rust::types::MeshGL;

use crate::graph::node::identity_matrix;

/// Default mesh tint when a node hasn't been given a colour — matches
/// the renderer's historical `base_color` (a desaturated blue-grey)
/// so unconfigured nodes look identical to the pre-refactor view.
pub const DEFAULT_GEOMETRY_COLOR: [f32; 4] = [0.62, 0.66, 0.78, 1.0];

/// Mesh + per-node transform + per-node colour, bundled into a single
/// value that travels along a Geometry3d socket. See the module-level
/// doc comment for the full rationale.
#[derive(Clone, Debug)]
pub struct Geometry3d {
    /// Triangle mesh. Shared so cloning a `Geometry3d` is cheap.
    pub mesh: Arc<MeshGL>,
    /// Column-major 4×4 transform (OpenGL / wgpu convention). Applied
    /// by the renderer / by downstream operations.
    pub matrix: [f32; 16],
    /// RGBA tint in 0..=1 — feeds the renderer's `base_color`.
    pub color: [f32; 4],
}

impl Geometry3d {
    /// Wrap a mesh with the identity transform and the default tint.
    /// Convenient for primitives that haven't been routed through a
    /// transform / colouring node yet.
    pub fn from_mesh(mesh: Arc<MeshGL>) -> Self {
        Self {
            mesh,
            matrix: identity_matrix(),
            color: DEFAULT_GEOMETRY_COLOR,
        }
    }

    /// Builder-style override of the transform.
    pub fn with_matrix(mut self, matrix: [f32; 16]) -> Self {
        self.matrix = matrix;
        self
    }

    /// Builder-style override of the colour.
    pub fn with_color(mut self, color: [f32; 4]) -> Self {
        self.color = color;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_mesh_uses_identity_and_default_color() {
        let mesh = Arc::new(MeshGL::default());
        let g = Geometry3d::from_mesh(mesh);
        assert_eq!(g.matrix, identity_matrix());
        assert_eq!(g.color, DEFAULT_GEOMETRY_COLOR);
    }

    #[test]
    fn builders_round_trip() {
        let mesh = Arc::new(MeshGL::default());
        let m = [
            2.0, 0.0, 0.0, 0.0,
            0.0, 2.0, 0.0, 0.0,
            0.0, 0.0, 2.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        let c = [1.0, 0.5, 0.2, 0.8];
        let g = Geometry3d::from_mesh(mesh).with_matrix(m).with_color(c);
        assert_eq!(g.matrix, m);
        assert_eq!(g.color, c);
    }
}
