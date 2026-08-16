//! The n-ary operand plumbing behind [`super::boolean_node`]: collecting
//! the dynamic inputs, baking each body's matrix, importing every operand,
//! and folding the four operations over the resulting solids.
//!
//! Split out of `boolean_node.rs` so both stay well under the 800-line
//! limit; the node file keeps the [`NodeDef`](crate::registry::NodeDef)
//! surface (schema, hooks, evaluate glue) and this one keeps the geometry.
//!
//! ## Inputs, bodies, and operands
//!
//! An input slot carries a whole [`Geometry3d`] group, which may hold
//! several bodies. Each **body** is its own operand of the boolean — a
//! two-box group unions as two boxes, exactly as MatterCAD's participants
//! are per-mesh (`Object3DBooleanOperations.CombineParticipants`). The
//! keep/remove **selection**, though, is per *input socket*: MatterCAD's
//! "Part(s) to Subtract" is a list of children, and our children are the
//! things the user wired up, not the bodies that happen to be inside one.
//! So an input marked as a remover contributes all of its bodies as
//! removers.
//!
//! ## Baking the matrix
//!
//! [`Body::matrix`] is a transform the mesh has *not* been through:
//! Transform composes matrices rather than re-baking vertices, so a moved
//! part still arrives centred on its own origin. Every operand's matrix is
//! therefore applied to its mesh before import, mirroring MatterCAD's use
//! of each participant's `WorldMatrix` in `BooleanProcessing.Do`. Skipping
//! this made `Box → Transform → Boolean` subtract a box from itself.
//!
//! ### Mirrored operands
//!
//! A matrix with a **negative determinant** (a mirror — reachable today
//! through a negative scale on Transform) turns the mesh inside out:
//! every triangle's winding is reversed, and the kernel would read the
//! shell as the *complement* of the part, so a subtract would eat
//! everything except the box. [`import_group`] therefore reverses the
//! winding of a mirrored operand as part of the bake — the cheap, local
//! fix, and one that leaves the import seeing exactly the solid the user
//! sees.
//!
//! That is only the transform-induced case. An operand whose *source*
//! geometry is inside out (a badly authored or imported mesh) still
//! computes against its complement; that is what plan step B-4's
//! "Repair Winding Order" / "Keep Inside Out Geometry" rows are for
//! (`repair_orientation()`, `WindingRule::Nonzero`), and it is not
//! detectable from the matrix.

use std::sync::Arc;

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{Error, MeshGL, OpType};

use super::boolean_import::{import_operand, refusal_message};
use crate::geometry::mesh3d::{
    apply_transform, compute_flat_normals, split_for_flat_normals, NUM_PROP,
};
use crate::geometry::{num_tris, num_verts, Body, Geometry3d};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUid;
use crate::registry::{wrap_mesh, EvalCtx, NodeError};
use crate::socket_types::SocketType;

/// One connected input slot and the bodies it carries.
pub struct InputGroup {
    pub socket: SocketUid,
    /// Input socket name — what the user sees, and what a refusal message
    /// names so they know which part of the graph to fix.
    pub name: Arc<str>,
    pub bodies: Vec<Body>,
}

/// The uids of the slots that *can* carry an operand, in display order:
/// named (so not the trailing placeholder) and typed `Geometry3d`.
///
/// Membership is decided by socket **type**, not by name — a graph saved
/// while this node still minted `Color` / `Matrix` param sockets can have
/// one of them wired, and a value-carrying slot that is not geometry is
/// simply not an operand.
///
/// This list is deliberately computed *before* the inputs are evaluated:
/// it is what the part selection is resolved against, so an input that
/// happens to evaluate empty this frame cannot silently hand its role
/// (in particular "last input, therefore the default remover") to a
/// neighbour.
pub fn operand_sockets(ctx: &EvalCtx) -> Vec<SocketUid> {
    ctx.instance
        .inputs
        .iter()
        .filter(|s| !s.name.as_ref().is_empty() && s.socket_type == SocketType::Geometry3d)
        .map(|s| s.uid)
        .collect()
}

/// Collect the node's connected geometry inputs, in display order.
///
/// Slots that are not operand slots ([`operand_sockets`]) are skipped, as
/// is any slot whose bodies are all empty: an empty body is not an
/// operand, and handing one to the import would refuse it as "no solid
/// geometry".
pub fn gather_inputs(ctx: &EvalCtx) -> Result<Vec<InputGroup>, NodeError> {
    let mut groups = Vec::new();
    for slot in &ctx.instance.inputs {
        if slot.name.as_ref().is_empty() || slot.socket_type != SocketType::Geometry3d {
            continue;
        }
        let geom = match ctx.input(slot.uid) {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => continue,
            // The slot is typed Geometry3d, so the graph's own
            // compatibility check should have refused anything else.
            other => {
                return Err(NodeError::msg(format!(
                    "Boolean: input '{}' must be Geometry3d, got {:?}",
                    slot.name,
                    other.socket_type()
                )))
            }
        };
        let bodies: Vec<Body> = geom
            .iter()
            .filter(|b| num_verts(&b.mesh) > 0 && num_tris(&b.mesh) > 0)
            .cloned()
            .collect();
        if bodies.is_empty() {
            continue;
        }
        groups.push(InputGroup {
            socket: slot.uid,
            name: slot.name.clone(),
            bodies,
        });
    }
    Ok(groups)
}

/// Import every body of `group` as a boolean operand, with its matrix
/// baked in. A refusal becomes a [`NodeError`] naming the input (and the
/// part within it, when the input carries more than one body).
pub fn import_group(group: &InputGroup) -> Result<Vec<Manifold>, NodeError> {
    let multi = group.bodies.len() > 1;
    let mut solids = Vec::with_capacity(group.bodies.len());
    for (i, body) in group.bodies.iter().enumerate() {
        let mut baked = apply_transform(&body.mesh, &body.matrix);
        if is_mirroring(&body.matrix) {
            reverse_winding(&mut baked);
        }
        let label = if multi {
            format!("{} (part {})", group.name, i + 1)
        } else {
            group.name.to_string()
        };
        let solid =
            import_operand(&baked).map_err(|f| NodeError::msg(refusal_message(&label, f)))?;
        solids.push(solid);
    }
    Ok(solids)
}

/// Import several groups' bodies into one flat operand list.
pub fn import_groups(groups: &[&InputGroup]) -> Result<Vec<Manifold>, NodeError> {
    let mut all = Vec::new();
    for g in groups {
        all.extend(import_group(g)?);
    }
    Ok(all)
}

/// Fold `op` over the operands left-to-right. `None` for an empty list.
///
/// The intermediate statuses are not inspected: a failed step propagates
/// through every later one, and [`finish`] reports it once with the same
/// message it would have carried here. Plan step B-5 replaces this fold
/// with touching-set batching and per-operand triage.
pub fn fold(solids: Vec<Manifold>, op: OpType) -> Option<Manifold> {
    let mut it = solids.into_iter();
    let first = it.next()?;
    Some(it.fold(first, |acc, next| acc.boolean(&next, op)))
}

/// Turn a boolean result into a render-ready `num_prop = 6` mesh, or a
/// node error when the kernel refused the operation.
///
/// Manifold returns a shared-vertex mesh; flat normals need one vertex per
/// triangle corner or neighbouring faces overwrite each other's normals and
/// the shading goes to mush (the visual half of the B-1 dark-blob report).
pub fn finish(result: &Manifold) -> Result<MeshGL, NodeError> {
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

/// [`finish`], but an empty result yields `None` instead of a body: an
/// empty body is still a *body*, and part counts, exports and the
/// viewport's per-body iteration would all see a phantom part with no
/// triangles.
pub fn finish_non_empty(result: &Manifold) -> Result<Option<MeshGL>, NodeError> {
    let mesh = finish(result)?;
    if mesh.tri_verts.is_empty() {
        return Ok(None);
    }
    Ok(Some(mesh))
}

/// Wrap a result mesh in a [`Body`] carrying the node's own colour +
/// matrix resolution — the same rule [`wrap_mesh`] applies, reused here so
/// the multi-body results agree with the single-body ones.
///
/// The operand matrices were baked into the mesh, so the body's matrix is
/// the Boolean node's own transform and nothing else.
pub fn result_body(ctx: &EvalCtx, mesh: MeshGL) -> Option<Body> {
    wrap_mesh(ctx, mesh).bodies.into_iter().next()
}

/// The bodies of every group, passed through untouched. Used when an
/// operation has nothing to do (Subtract with no removers): the parts keep
/// their own meshes, matrices, colours and origin claims rather than being
/// re-imported and re-emitted as this node's own geometry.
pub fn pass_through(groups: &[InputGroup]) -> Geometry3d {
    Geometry3d::from_bodies(groups.iter().flat_map(|g| g.bodies.iter().cloned()).collect())
}

/// True when a column-major transform flips handedness — its upper-3×3
/// determinant is negative — which is exactly when baking it reverses
/// every triangle's winding. See the module docs.
pub fn is_mirroring(m: &[f32; 16]) -> bool {
    let a = [m[0], m[1], m[2]];
    let b = [m[4], m[5], m[6]];
    let c = [m[8], m[9], m[10]];
    let det = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
        + a[2] * (b[0] * c[1] - b[1] * c[0]);
    det < 0.0
}

/// Swap two corners of every triangle, restoring outward-facing winding
/// after a mirroring bake. Positions are untouched; the import strips
/// normals anyway and the result recomputes its own.
fn reverse_winding(mesh: &mut MeshGL) {
    for tri in mesh.tri_verts.chunks_exact_mut(3) {
        tri.swap(1, 2);
    }
}

/// Promote a positions-only mesh to the `num_prop = 6` layout the renderer
/// expects, leaving the normal slots zeroed for [`compute_flat_normals`].
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
