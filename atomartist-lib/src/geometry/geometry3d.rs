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

use crate::graph::node::{identity_matrix, NodeId};

/// Default mesh tint when a body hasn't been given a colour — matches
/// the renderer's historical `base_color` (a desaturated blue-grey)
/// so unconfigured nodes look identical to the pre-refactor view.
pub const DEFAULT_GEOMETRY_COLOR: [f32; 4] = [0.62, 0.66, 0.78, 1.0];

/// Sentinel that means "inherit from upstream" — alpha = 0 with all
/// channels zero. Matches MatterCAD's `Color.Transparent` convention
/// in `WorldColor()`: an op-node defaults its colour to this sentinel
/// so a downstream `compose_with_upstream` knows to pass the upstream
/// body's colour through unchanged. Primitives default to a solid
/// colour ([`DEFAULT_GEOMETRY_COLOR`]); ops default to this sentinel.
///
/// At render time, a body that still carries alpha = 0 by the time it
/// reaches the renderer means "no node along the chain set an explicit
/// colour" — the renderer substitutes [`DEFAULT_GEOMETRY_COLOR`] so the
/// body still paints visibly. Use [`is_inherit_color`] to test.
pub const INHERIT_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.0];

/// True if `color` is the inherit-from-upstream sentinel — i.e. alpha
/// is effectively zero. Channels other than alpha are ignored; only
/// the alpha channel determines whether the colour is considered set.
#[inline]
pub fn is_inherit_color(color: &[f32; 4]) -> bool {
    color[3] <= 0.0
}

/// One renderable body: a mesh plus its per-body transform + tint and
/// an optional per-vertex colour attribute.
///
/// Every body in a `Geometry3d` paints independently — the renderer
/// iterates and emits a draw call per body with its own `base_color`
/// uniform and `model` matrix. Multi-body groups can therefore carry
/// different colours per body without per-vertex paint tricks.
///
/// ## Per-body vs per-vertex colour
///
/// Both NodeDesigner and MatterCAD support **both** colouring modes:
///
/// * **Per-body** (`color` field) — single RGBA uniform applied across
///   the whole body. This is what colour-override nodes set, what 3MF
///   per-body material entries provide, and what users see when no
///   per-vertex data is present. Default tint.
///
/// * **Per-vertex** (`vertex_colors` field, RGBA flat-packed at
///   `4 * num_verts(mesh)` floats) — used when an operation paints
///   geometry at the vertex level (e.g. Boolean ops where each output
///   vertex inherits colour from whichever input it came from).
///   When present, the renderer multiplies the per-vertex value by
///   `color` (which the operation typically leaves at white so the
///   per-vertex value passes through unmodified).
///
/// The two modes coexist: if `vertex_colors` is `Some`, the shader
/// path is the vertex-colour path; if `None`, the shader uses `color`
/// alone. See [`crate::geometry::geometry3d::Body::with_vertex_colors`].
#[derive(Clone, Debug)]
pub struct Body {
    /// Triangle mesh. Shared so cloning a `Body` is cheap.
    pub mesh: Arc<MeshGL>,
    /// Column-major 4×4 transform (OpenGL / wgpu convention).
    pub matrix: [f32; 16],
    /// RGBA tint in 0..=1. When `vertex_colors` is `Some`, this acts
    /// as a multiplier on the per-vertex value — leave at white
    /// `[1,1,1,1]` to pass the per-vertex colour through unchanged.
    pub color: [f32; 4],
    /// Optional per-vertex RGBA, flat-packed `[r,g,b,a, r,g,b,a, ...]`
    /// with length `4 * num_verts(mesh)`. `Arc` so a body can be
    /// cloned cheaply even with large colour buffers — Boolean ops in
    /// particular allocate the colour buffer once and share it across
    /// the cached output bodies. `None` means "use `color` uniform
    /// for every vertex" — the common case for primitive nodes.
    pub vertex_colors: Option<Arc<Vec<f32>>>,
    /// The node that produced this body — the "claim" used by viewport
    /// click-to-select. Primitives set this to their own `NodeId`;
    /// pure-transform ops (Transform, FitToBounds, Align) *overwrite*
    /// upstream's claim with their own, so clicking the rendered
    /// result of `Box → Transform` selects the Transform node (the
    /// most-downstream op in the chain). Combine / Output preserve
    /// upstream claims since they're aggregators, not operators.
    /// `None` is the "no claim" sentinel — clicking such a body
    /// selects nothing rather than guessing.
    pub origin: Option<NodeId>,
}

impl Body {
    /// Body with identity transform, the default tint, no per-vertex
    /// colour overlay, and no origin claim.
    pub fn from_mesh(mesh: Arc<MeshGL>) -> Self {
        Self {
            mesh,
            matrix: identity_matrix(),
            color: DEFAULT_GEOMETRY_COLOR,
            vertex_colors: None,
            origin: None,
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

    /// Attach a per-vertex RGBA colour buffer. Length must be
    /// `4 * num_verts(self.mesh)`. The renderer takes the per-vertex
    /// path when this is `Some` — see the type-level doc for the
    /// per-body vs per-vertex distinction.
    pub fn with_vertex_colors(mut self, colors: Arc<Vec<f32>>) -> Self {
        self.vertex_colors = Some(colors);
        self
    }

    /// Builder-style override of the origin claim. Primitives and
    /// pure-transform ops set this to `ctx.instance.id` so a click on
    /// the rendered body selects the node that owns it.
    pub fn with_origin(mut self, origin: NodeId) -> Self {
        self.origin = Some(origin);
        self
    }

    /// True when this body should render through the per-vertex
    /// colour path. The renderer mirrors this into its
    /// `use_vertex_colors` shader uniform.
    pub fn has_vertex_colors(&self) -> bool {
        self.vertex_colors.is_some()
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
    fn body_default_has_no_vertex_colors() {
        let mesh = Arc::new(MeshGL::default());
        let b = Body::from_mesh(mesh);
        assert!(b.vertex_colors.is_none());
        assert!(!b.has_vertex_colors());
    }

    #[test]
    fn with_vertex_colors_enables_vertex_path() {
        let mesh = Arc::new(MeshGL::default());
        let colors = Arc::new(vec![1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);
        let b = Body::from_mesh(mesh).with_vertex_colors(colors.clone());
        assert!(b.has_vertex_colors());
        let v = b.vertex_colors.as_ref().unwrap();
        assert_eq!(v.len(), 8);
        // Body keeps an Arc to the original buffer (no copy).
        assert!(Arc::ptr_eq(v, &colors));
    }

    #[test]
    fn inherit_color_sentinel_is_alpha_zero() {
        // Sentinel must have alpha 0 so `is_inherit_color` matches it
        // and so a wgpu render of the raw colour is fully transparent
        // (only reachable if the renderer fallback also fails — should
        // never happen, but reveals the bug clearly if it does).
        assert_eq!(INHERIT_COLOR[3], 0.0);
        assert!(is_inherit_color(&INHERIT_COLOR));
        // DEFAULT_GEOMETRY_COLOR must NOT be treated as inherit (alpha 1).
        assert!(!is_inherit_color(&DEFAULT_GEOMETRY_COLOR));
        // Any non-zero alpha is treated as an explicit colour.
        assert!(!is_inherit_color(&[0.0, 0.0, 0.0, 1.0]));
        assert!(!is_inherit_color(&[0.5, 0.5, 0.5, 0.001]));
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
