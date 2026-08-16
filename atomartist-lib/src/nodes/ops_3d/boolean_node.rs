//! Boolean operation node — Union / Difference / Intersection on two
//! `MeshGL` solids via `manifold-rust`.
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

use manifold_rust::types::{Error, MeshGL, OpType};

use super::boolean_import::{import_operand, refusal_message};
use crate::geometry::mesh3d::{compute_flat_normals, split_for_flat_normals, NUM_PROP};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet,
    PropDef,
};
use crate::socket_types::SocketType;

pub struct BooleanNode;

/// The Boolean node's parameter schema. Shared `Color` / `Matrix` (via
/// [`ParamSet::geometry`], resolved by [`wrap_mesh`]) lead; `operation`
/// follows. `operation` is genuinely a **Number-encoded** enum (0 = Union,
/// 1 = Difference, 2 = Intersection) — not an `EnumDropdown` string — so
/// it stays a property-only `Number` to preserve saved-graph
/// compatibility; there is no scalar socket type for it.
fn params() -> ParamSet {
    ParamSet::geometry()
        .number("operation", "operation", 0.0, 0.0..=2.0)
        .no_socket()
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
        let op_idx = params().reader(ctx).number("operation").round() as i32;
        let op = match op_idx {
            0 => OpType::Add,         // Union
            1 => OpType::Subtract,    // Difference (a - b)
            2 => OpType::Intersect,
            _ => OpType::Add,
        };

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
        let result = ma.boolean(&mb, op);
        if result.status() != Error::NoError {
            return Err(NodeError::msg(format!(
                "Boolean: the operation failed ({})",
                result.status().to_str()
            )));
        }
        let mut out_mesh = result.get_mesh_gl(-1);
        promote_to_num_prop6(&mut out_mesh);
        // Manifold returns a shared-vertex mesh; flat normals need one
        // vertex per triangle corner or neighbouring faces overwrite each
        // other's normals and the shading goes to mush.
        out_mesh = split_for_flat_normals(&out_mesh);
        compute_flat_normals(&mut out_mesh);

        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, out_mesh))));
        Ok(out)
    }
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
