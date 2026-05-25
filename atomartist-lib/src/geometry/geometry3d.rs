//! `Geometry3d` — a collection of one-or-more bodies that flows along
//! Geometry3d sockets.
//!
//! Originally a single `(mesh, matrix, color)` triple, the type was
//! widened to a `Vec<Body>` so a single socket can carry multiple
//! distinct meshes — each with its own colour and transform. This
//! matches:
//!
//! * 3MF imports, which routinely encode multiple bodies (each with
//!   its own assigned material) inside one file.
//! * NodeDesigner / Blender-style workflows where downstream nodes
//!   pick or filter individual bodies out of a group.
//! * The Output node, which now concatenates every input's bodies
//!   into one geometry stream instead of merging them into a single
//!   tinted mesh (which lost per-body colour).
//!
//! Single-mesh callers (primitives, most operations) construct a
//! one-body `Geometry3d` via [`Geometry3d::from_mesh`] or
//! [`Geometry3d::from_body`] and continue to treat `.bodies[0]` as
//! "the mesh". Multi-body callers (3MF importer, Output node)
//! construct from a `Vec<Body>` directly.

use std::sync::Arc;

use manifold_rust::types::MeshGL;

use crate::graph::node::identity_matrix;

/// Default mesh tint when a body hasn't been given a colour — matches
/// the renderer's historical `base_color` (a desaturated blue-grey)
/// so unconfigured nodes look identical to the pre-refactor view.
pub const DEFAULT_GEOMETRY_COLOR: [f32; 4] = [0.62, 0.66, 0.78, 1.0];

/// One renderable body: a mesh plus its per-body transform + tint.
///
/// Every body in a `Geometry3d` paints independently — the renderer
/// iterates and emits a draw call per body with its own `base_color`
/// uniform. Multi-body groups can therefore carry different colours
/// per body without per-vertex paint tricks.
#[derive(Clone, Debug)]
pub struct Body {
    /// Triangle mesh. Shared so cloning a `Body` is cheap.
    pub mesh: Arc<MeshGL>,
    /// Column-major 4×4 transform (OpenGL / wgpu convention).
    pub matrix: [f32; 16],
    /// RGBA tint in 0..=1.
    pub color: [f32; 4],
}

impl Body {
    /// Body with identity transform and the default tint.
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

/// Group of one-or-more renderable bodies. Travels along a
/// `Geometry3d` socket between nodes.
#[derive(Clone, Debug, Default)]
pub struct Geometry3d {
    pub bodies: Vec<Body>,
}

impl Geometry3d {
    /// Empty group — no bodies. Used as a "passthrough nothing" value
    /// when an upstream input is unwired.
    pub fn empty() -> Self {
        Self { bodies: Vec::new() }
    }

    /// Single-body group from a mesh + default transform + default
    /// tint. Drop-in replacement for the pre-refactor single-mesh
    /// constructor.
    pub fn from_mesh(mesh: Arc<MeshGL>) -> Self {
        Self {
            bodies: vec![Body::from_mesh(mesh)],
        }
    }

    /// Single-body group wrapping an explicit `Body`.
    pub fn from_body(body: Body) -> Self {
        Self { bodies: vec![body] }
    }

    /// Multi-body group from a list of bodies.
    pub fn from_bodies(bodies: Vec<Body>) -> Self {
        Self { bodies }
    }

    /// Number of bodies in the group.
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    /// True when the group carries no bodies — viewport / pick paths
    /// treat this as "nothing to render."
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// First body in the group, or `None` if empty. Convenience for
    /// single-body callers that don't want to deal with the `Vec`.
    pub fn first(&self) -> Option<&Body> {
        self.bodies.first()
    }

    /// Mutable handle to the first body.
    pub fn first_mut(&mut self) -> Option<&mut Body> {
        self.bodies.first_mut()
    }

    /// Iterate over every body — the renderer's primary entry point.
    pub fn iter(&self) -> std::slice::Iter<'_, Body> {
        self.bodies.iter()
    }

    /// Builder-style override of the first body's transform. Used by
    /// single-body callers; multi-body groups should mutate each body
    /// individually.
    pub fn with_matrix(mut self, matrix: [f32; 16]) -> Self {
        if let Some(b) = self.bodies.first_mut() {
            b.matrix = matrix;
        }
        self
    }

    /// Builder-style override of the first body's colour.
    pub fn with_color(mut self, color: [f32; 4]) -> Self {
        if let Some(b) = self.bodies.first_mut() {
            b.color = color;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_mesh_produces_one_body_with_defaults() {
        let mesh = Arc::new(MeshGL::default());
        let g = Geometry3d::from_mesh(mesh);
        assert_eq!(g.len(), 1);
        let b = g.first().unwrap();
        assert_eq!(b.matrix, identity_matrix());
        assert_eq!(b.color, DEFAULT_GEOMETRY_COLOR);
    }

    #[test]
    fn empty_is_empty() {
        let g = Geometry3d::empty();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
        assert!(g.first().is_none());
    }

    #[test]
    fn from_bodies_preserves_order_and_count() {
        let m = Arc::new(MeshGL::default());
        let bodies = vec![
            Body::from_mesh(m.clone()).with_color([1.0, 0.0, 0.0, 1.0]),
            Body::from_mesh(m.clone()).with_color([0.0, 1.0, 0.0, 1.0]),
            Body::from_mesh(m).with_color([0.0, 0.0, 1.0, 1.0]),
        ];
        let g = Geometry3d::from_bodies(bodies);
        assert_eq!(g.len(), 3);
        assert_eq!(g.iter().nth(1).unwrap().color[1], 1.0);
    }

    #[test]
    fn builders_update_first_body() {
        let mesh = Arc::new(MeshGL::default());
        let m = [
            2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let c = [1.0, 0.5, 0.2, 0.8];
        let g = Geometry3d::from_mesh(mesh).with_matrix(m).with_color(c);
        let b = g.first().unwrap();
        assert_eq!(b.matrix, m);
        assert_eq!(b.color, c);
    }
}
