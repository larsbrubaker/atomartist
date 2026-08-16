//! Boolean operation node — Combine / Subtract / Intersect / Subtract &
//! Replace on two `MeshGL` solids via `manifold-rust`.
//!
//! The operation is a first-class enum param serialized **by name**
//! (MatterCAD's `BooleanOperation`, `BooleanObject3D.cs:61-75`), so the
//! variant list can be reordered without rewriting saved graphs. Graphs
//! written before this step stored the operation as a `Number` (0 = union,
//! 1 = difference, 2 = intersection); those load through the generic
//! index → variant migration in
//! [`crate::serialization::prop_migration`], and a stray in-memory
//! `Number` still resolves the same way in [`ParamReader::enum_`].
//!
//! MatterCAD also renames the scene item when the operation changes, but
//! only while the name is still the default one it gave the item
//! (`BooleanObject3D.cs:129-151`). Not ported: a `NodeInstance` has no
//! per-node name at all — the canvas titles every node from its type's
//! `display_name` — so there is neither a name to change nor a
//! user-renamed flag to respect. Inventing that state is a graph-model
//! decision, not a Boolean-node one.
//!
//! [`ParamReader::enum_`]: crate::registry::ParamReader::enum_
//!
//! Inputs are converted to `Manifold` by [`boolean_import::import_operand`]
//! (robust import + seam-weld retry; see that module for the policy), the
//! requested op is performed, and the result is exported back to `MeshGL`.
//! Operands are stripped to positions before import — manifold's
//! property-interpolation across new cut vertices would otherwise yield
//! mid-face-averaged normals.
//!
//! The result comes back with positions only and with vertices shared
//! between faces, so making it render-ready takes three steps: promote to
//! the `num_prop = 6` layout, split every triangle corner onto its own
//! vertex ([`split_for_flat_normals`]), then compute per-face normals. The
//! split is what makes the third step meaningful — writing face normals into
//! shared vertex slots leaves all but the last face visited shading wrong.
//!
//! Every refusal, on import or on the boolean's own result, becomes a
//! [`NodeError`] naming the operand: a boolean that swallowed a bad operand
//! as empty geometry would still report success, and the part would silently
//! vanish from the output.

use std::sync::Arc;

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{Error, MeshGL, OpType};

use super::boolean_import::{import_operand, refusal_message};
use crate::geometry::mesh3d::{compute_flat_normals, split_for_flat_normals, NUM_PROP};
use crate::geometry::{Body, Geometry3d};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    wrap_mesh, EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct BooleanNode;

/// The four operations, in MatterCAD's declaration order
/// (`BooleanObject3D.cs:61-75`). The strings are the **stored** values —
/// the enum is serialized by name — and double as the display labels, so
/// the list may be reordered but not respelled.
///
/// The order also defines the legacy numeric encoding: index 0/1/2 are
/// exactly the `Number`-valued 0 = union, 1 = difference, 2 = intersection
/// this node stored before the enum landed, which is what makes the
/// generic index-based migration correct here.
pub const OPERATIONS: [&str; 4] = ["Combine", "Subtract", "Intersect", "Subtract & Replace"];

/// The Boolean node's parameter schema. Shared `Color` / `Matrix` (via
/// [`ParamSet::geometry`], resolved by [`wrap_mesh`]) lead; `operation`
/// follows.
fn params() -> ParamSet {
    ParamSet::geometry()
        .enum_("operation", "Operation", OPERATIONS[0], &OPERATIONS)
        .editor(EditorKind::EnumButtons {
            variants: OPERATIONS.iter().map(|v| Arc::from(*v)).collect(),
        })
        .description(
            "Combine merges the parts; Subtract cuts 'b' out of 'a'; Intersect keeps \
             only the shared volume; Subtract & Replace cuts 'b' out of 'a' and keeps \
             the removed volume as its own part.",
        )
}

impl NodeDef for BooleanNode {
    fn type_id(&self) -> &'static str { "Boolean" }
    fn display_name(&self) -> &'static str { "Boolean" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        // The two required geometry inputs lead; the schema params
        // (Color / Matrix) follow. `operation` mints no socket.
        params()
            .mint_sockets(
                InstanceTemplate::builder(alloc)
                    .input("a", SocketType::Geometry3d)
                    .input("b", SocketType::Geometry3d),
            )
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        params().prop_defs()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let geom_a = match ctx.input_named("a") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Boolean: input 'a' must be Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        let geom_b = match ctx.input_named("b") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Boolean: input 'b' must be Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        let ps = params();
        let operation = ps.reader(ctx).enum_("operation").to_string();

        // Booleans operate on the first body of each input, and the other
        // bodies are dropped rather than passed through — plan step B-3
        // ("N-ary operands + selection") replaces this with combine_node's
        // trailing-empty input model and per-operand `Body.matrix` baking.
        let mesh_a = match geom_a.first() {
            Some(b) => &b.mesh,
            None => return Ok(NodeOutputs::default()),
        };
        let mesh_b = match geom_b.first() {
            Some(b) => &b.mesh,
            None => return Ok(NodeOutputs::default()),
        };
        let ma = import_operand(mesh_a)
            .map_err(|status| NodeError::msg(refusal_message("a", status)))?;
        let mb = import_operand(mesh_b)
            .map_err(|status| NodeError::msg(refusal_message("b", status)))?;

        // The imported handles are reused across both calls of Subtract &
        // Replace — importing twice would pay the (robust) import cost
        // again for identical input.
        let geometry = if operation == OPERATIONS[3] {
            // Subtract & Replace: the kept body is `a − b`, and the volume
            // the subtraction removed (`a ∩ b`) is kept beside it as its
            // own body rather than discarded.
            //
            // For today's fixed a/b pair that is exactly one keep and one
            // remover; plan step B-6 extends this to the n-ary keep-parts
            // semantics (one replaced body per keep × remover pair) once
            // B-3 lands the operand list.
            let kept = finish(ma.boolean(&mb, OpType::Subtract))?;
            let removed = finish(ma.boolean(&mb, OpType::Intersect))?;
            let template = wrap_mesh(ctx, kept);
            let base = match template.first() {
                Some(b) => b.clone(),
                // `wrap_mesh` always produces exactly one body.
                None => return Ok(NodeOutputs::default()),
            };
            let mut bodies = vec![base.clone()];
            // Operands that never touch have nothing to replace. An
            // empty body is still a *body*: part counts, exports and the
            // viewport's per-body iteration would all see a phantom part
            // with no triangles.
            if !removed.tri_verts.is_empty() {
                bodies.push(Body {
                    mesh: Arc::new(removed),
                    matrix: base.matrix,
                    color: base.color,
                    // Explicitly none rather than inherited: the colour
                    // buffer is indexed per *vertex* of its own mesh, and
                    // the replaced body's vertices are not the keep's.
                    // B-6 gives this body its own colour (MatterCAD tints
                    // a retained remover red).
                    vertex_colors: None,
                    origin: base.origin,
                });
            }
            Geometry3d::from_bodies(bodies)
        } else {
            let op = match operation.as_str() {
                // Subtract is `a - b`; anything unrecognised is Combine.
                v if v == OPERATIONS[1] => OpType::Subtract,
                v if v == OPERATIONS[2] => OpType::Intersect,
                _ => OpType::Add,
            };
            wrap_mesh(ctx, finish(ma.boolean(&mb, op))?)
        };

        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(geometry)));
        Ok(out)
    }
}

/// Turn a boolean result into a render-ready `num_prop = 6` mesh, or a
/// node error when the kernel refused the operation.
///
/// Manifold returns a shared-vertex mesh; flat normals need one vertex per
/// triangle corner or neighbouring faces overwrite each other's normals and
/// the shading goes to mush (the visual half of the B-1 dark-blob report).
fn finish(result: Manifold) -> Result<MeshGL, NodeError> {
    if result.status() != Error::NoError {
        return Err(NodeError::msg(format!(
            "Boolean: the operation failed ({})",
            result.status().to_str()
        )));
    }
    let mut out_mesh = result.get_mesh_gl(-1);
    promote_to_num_prop6(&mut out_mesh);
    out_mesh = split_for_flat_normals(&out_mesh);
    compute_flat_normals(&mut out_mesh);
    Ok(out_mesh)
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(BooleanNode);
}

fn promote_to_num_prop6(mesh: &mut MeshGL) {
    if mesh.num_prop == NUM_PROP {
        return;
    }
    let n = mesh.vert_properties.len() / mesh.num_prop as usize;
    let mut out = Vec::with_capacity(n * NUM_PROP as usize);
    for i in 0..n {
        let off = i * mesh.num_prop as usize;
        out.push(mesh.vert_properties[off]);
        out.push(mesh.vert_properties[off + 1]);
        out.push(mesh.vert_properties[off + 2]);
        out.push(0.0);
        out.push(0.0);
        out.push(0.0);
    }
    mesh.vert_properties = out;
    mesh.num_prop = NUM_PROP;
}

#[cfg(test)]
#[path = "boolean_node_tests.rs"]
mod tests;
