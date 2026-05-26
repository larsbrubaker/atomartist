//! Reusable `matrix` + `color` property scaffolding shared by every
//! geometry-producing node.
//!
//! Lives in its own file so [`super`] stays under the 800-line
//! guardrail. Nothing here owns state; these are factory helpers
//! every NodeDef can fold into its `properties()` and `evaluate()`.

use std::sync::Arc;

use manifold_rust::types::MeshGL;

use crate::geometry::{is_inherit_color, Body, Geometry3d, DEFAULT_GEOMETRY_COLOR, INHERIT_COLOR};
use crate::graph::node::{identity_matrix, matmul4x4, PortValue};

use crate::graph::node::PortValue as _PortValue;

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
/// `matrix` + `color` properties for **operation** nodes that act on an
/// upstream `Geometry3d`. Differs from [`geometry_props`] only in the
/// default `color` — operation nodes default to [`INHERIT_COLOR`]
/// (alpha 0 = "use upstream's colour"), matching MatterCAD's
/// `Color.Transparent` convention in `Object3D.WorldColor()`. The user
/// can still override by picking an opaque colour; until then the
/// upstream's colour flows through untouched.
///
/// Primitives (no upstream) keep using [`geometry_props`] with a solid
/// default; ops use this variant.
pub fn op_props() -> Vec<PropDef> {
    vec![
        PropDef::new("color", PortValue::Color(INHERIT_COLOR))
            .with_editor(EditorKind::ColorPicker)
            .with_label("Color"),
        PropDef::new("matrix", PortValue::Matrix4x4(identity_matrix()))
            .with_editor(EditorKind::Matrix)
            .with_label("Matrix"),
    ]
}

/// Read this node's `color` + `matrix` resolution (input socket wins,
/// else property store) and compose them with an `upstream` body:
///
///   - **Matrix**: `ctx_matrix · upstream.matrix` (column-major). The
///     upstream's transform is preserved; this op's transform stacks
///     on top, so dragging a gizmo on a `Box → Transform` chain only
///     updates the matrix — no mesh re-bake.
///   - **Colour**: `ctx_color` wins if it's not the
///     [`INHERIT_COLOR`] sentinel (alpha > 0); otherwise the
///     upstream's colour passes through.
///   - **Mesh / vertex_colors**: passed through from `upstream`
///     unchanged. Geometry-modifying ops (Boolean, Mirror) bake their
///     own mesh first then call [`compose_with_upstream_and_mesh`]
///     instead.
pub fn compose_with_upstream(ctx: &EvalCtx, upstream: &Body) -> Body {
    let ctx_color = match ctx.input_named("Color") {
        _PortValue::Color(c) => *c,
        _ => ctx.properties.color("color", INHERIT_COLOR),
    };
    let ctx_matrix = match ctx.input_named("Matrix") {
        _PortValue::Matrix4x4(m) => *m,
        _ => ctx.properties.matrix4x4("matrix", identity_matrix()),
    };
    let composed_matrix = matmul4x4(&ctx_matrix, &upstream.matrix);
    let composed_color = if is_inherit_color(&ctx_color) {
        upstream.color
    } else {
        ctx_color
    };
    // Origin claim: the consuming op overwrites upstream's claim with
    // its own NodeId. Mirrors NodeDesigner's "click the displayed
    // result of Transform → select Transform" UX. Aggregators (Combine,
    // Output) don't call compose_with_upstream so they preserve
    // per-body claims by construction.
    Body {
        mesh: upstream.mesh.clone(),
        matrix: composed_matrix,
        color: composed_color,
        vertex_colors: upstream.vertex_colors.clone(),
        origin: Some(ctx.instance.id),
    }
}

/// Like [`compose_with_upstream`], but for ops that produce a brand-new
/// mesh (Boolean union, Mirror, Hollow, Bevel). The op has already
/// baked the upstream matrix into `new_mesh` so the composed body's
/// matrix is just this op's own `ctx_matrix`; colour resolves the same
/// way as the pass-through variant.
pub fn compose_with_upstream_and_mesh(
    ctx: &EvalCtx,
    upstream: &Body,
    new_mesh: MeshGL,
) -> Body {
    let ctx_color = match ctx.input_named("Color") {
        _PortValue::Color(c) => *c,
        _ => ctx.properties.color("color", INHERIT_COLOR),
    };
    let ctx_matrix = match ctx.input_named("Matrix") {
        _PortValue::Matrix4x4(m) => *m,
        _ => ctx.properties.matrix4x4("matrix", identity_matrix()),
    };
    let composed_color = if is_inherit_color(&ctx_color) {
        upstream.color
    } else {
        ctx_color
    };
    Body {
        mesh: Arc::new(new_mesh),
        matrix: ctx_matrix,
        color: composed_color,
        vertex_colors: None,
        origin: Some(ctx.instance.id),
    }
}

pub fn wrap_mesh(ctx: &EvalCtx, mesh: MeshGL) -> Geometry3d {
    // Color and Matrix can come from either an upstream input socket
    // (nodes that declared "Color" / "Matrix" inputs in `instantiate`)
    // or the node's own property store. Inputs win when wired —
    // matches the Extrude node's "socket-or-property" pattern so the
    // resolution rule is consistent across every geometry node.
    let color = match ctx.input_named("Color") {
        _PortValue::Color(c) => *c,
        _ => ctx.properties.color("color", DEFAULT_GEOMETRY_COLOR),
    };
    let matrix = match ctx.input_named("Matrix") {
        _PortValue::Matrix4x4(m) => *m,
        _ => ctx.properties.matrix4x4("matrix", identity_matrix()),
    };
    Geometry3d::from_body(Body {
        mesh: Arc::new(mesh),
        matrix,
        color,
        vertex_colors: None,
        origin: Some(ctx.instance.id),
    })
}
