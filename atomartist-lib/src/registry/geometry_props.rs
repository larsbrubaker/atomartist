//! Reusable `matrix` + `color` property scaffolding shared by every
//! geometry-producing node.
//!
//! Lives in its own file so [`super`] stays under the 800-line
//! guardrail. Nothing here owns state; these are factory helpers
//! every NodeDef can fold into its `properties()` and `evaluate()`.

use std::sync::Arc;

use manifold_rust::types::MeshGL;

use crate::geometry::{Geometry3d, DEFAULT_GEOMETRY_COLOR};
use crate::graph::node::{identity_matrix, PortValue};

use super::{EditorKind, EvalCtx, PropDef};

/// Standard `matrix` + `color` properties every geometry-producing
/// node should include. Mirrors NodeDesigner's "every geometry node
/// carries a transform and a colour" model so handles and gizmos can
/// drive them without per-node plumbing.
///
/// Convention is to **prepend** these to a node's `properties()`
/// return value so `Color` is the first row and `Matrix` the second —
/// matching the MatterCAD-inspired property panel ordering:
///
/// ```ignore
/// fn properties(&self) -> Vec<PropDef> {
///     let mut props = geometry_props();
///     props.push(PropDef::new("size", PortValue::Number(10.0)));
///     props
/// }
/// ```
pub fn geometry_props() -> Vec<PropDef> {
    vec![
        PropDef::new("color", PortValue::Color(DEFAULT_GEOMETRY_COLOR))
            .with_editor(EditorKind::ColorPicker)
            .with_label("Color"),
        PropDef::new("matrix", PortValue::Matrix4x4(identity_matrix()))
            .with_editor(EditorKind::Matrix)
            .with_label("Matrix"),
    ]
}

/// Bundle a mesh with the node's `matrix` + `color` properties into a
/// [`Geometry3d`] ready to wrap in a `PortValue::Geometry3d`. Used by
/// every geometry-producing node's `evaluate`:
///
/// ```ignore
/// out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, mesh))));
/// ```
pub fn wrap_mesh(ctx: &EvalCtx, mesh: MeshGL) -> Geometry3d {
    let matrix = ctx.properties.matrix4x4("matrix", identity_matrix());
    let color = ctx.properties.color("color", DEFAULT_GEOMETRY_COLOR);
    Geometry3d {
        mesh: Arc::new(mesh),
        matrix,
        color,
    }
}
