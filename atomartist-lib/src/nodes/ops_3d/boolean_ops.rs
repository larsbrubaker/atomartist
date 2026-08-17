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
//! That is only the transform-induced case, and it is deterministic: the
//! sign of the determinant says exactly how many times the winding was
//! flipped on the way here, so the rewind is always right and always
//! applied. An operand whose *source* geometry is inside out (a badly
//! authored or imported mesh) is a different problem — nothing in the
//! matrix reveals it, and the fix has to look at the geometry. That is
//! what [`BooleanOptions`] carries: **Repair Winding Order**
//! (`repair_orientation()` on the imported handle, fixing the data) and
//! **Keep Inside Out Geometry** ([`WindingRule::Nonzero`], redefining
//! what counts as material). The two are independent of the mirror
//! rewind and of each other, and all three can apply to the same operand.

use std::sync::Arc;

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{BooleanEngine, Error, MeshGL, OpType, WindingRule};

use super::boolean_colors::{painted_body, tag_original, Palette};
use super::boolean_import::{import_operand, refusal_message};
use crate::geometry::mesh3d::{
    apply_transform, compute_flat_normals, split_for_flat_normals, NUM_PROP,
};
use crate::geometry::{num_tris, num_verts, Body, BodyRole, Geometry3d};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUid;
use crate::registry::{compose_with_upstream, wrap_mesh, EvalCtx, NodeError};
use crate::socket_types::SocketType;

/// The two kernel toggles from the Boolean node's property panel
/// (`BooleanObject3D.cs:163-170`), resolved once per evaluation and passed
/// to every import and every boolean call so a single operation cannot run
/// half under one policy and half under the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BooleanOptions {
    /// "Keep Inside Out Geometry" — [`WindingRule::Nonzero`] instead of
    /// the default [`WindingRule::Positive`], so a region wound the wrong
    /// way counts as material (`w != 0`) rather than cancelling the volume
    /// around it. A winding rule is a robust-engine semantic, so asking
    /// for it also forces the slower robust pipeline — manifold-rust's
    /// `Auto` resolves `Nonzero` to `Robust` for us
    /// (`boolean3::boolean_dispatch_full`), which is why this stays on
    /// `Auto` rather than pinning the engine here.
    pub keep_inside_out: bool,
    /// "Repair Winding Order" — `repair_orientation()` on each imported
    /// operand, before any boolean runs. Fixes the data once instead of
    /// redefining "solid" for the whole operation; MatterCAD calls it the
    /// better answer of the two.
    pub repair_winding: bool,
}

impl BooleanOptions {
    /// Which winding numbers the kernel counts as solid
    /// (`BooleanMeshBuilder.WindingRule`, L71-73).
    pub fn winding_rule(&self) -> WindingRule {
        if self.keep_inside_out {
            WindingRule::Nonzero
        } else {
            WindingRule::Positive
        }
    }
}

/// One boolean step under `opts`.
///
/// Every boolean in this module goes through here: the engine stays
/// `Auto` (see [`BooleanOptions::keep_inside_out`]) and the rule is the
/// node's, so no call site can forget the toggle.
///
/// ## Cancel and progress (B-6): deliberately not wired
///
/// manifold-rust also offers `boolean_with_engine_rule_and_progress`,
/// taking a cancel token and a progress reporter. Neither has anything to
/// connect to here yet, and both would cost something to fake:
///
///   * **Cancel.** Evaluation runs on `AppState`'s `EvalTask`, which has
///     no supersede signal to derive a token from. Its `ticket` /
///     `published` pair guards the *publication* of a stale result, not
///     its computation, and the task holds the graph mutex for the whole
///     pass — so a newer evaluation cannot even start until the older one
///     has finished. Cancellation is therefore a change to the evaluator
///     (a shared "newest ticket" the node can watch, a cancelled outcome
///     the executor must distinguish from a failure so nothing is badged
///     or blocked, and a token on `EvalCtx` to carry it), not to this
///     module. Left to that step.
///   * **Progress.** Nothing consumes evaluation progress — the status
///     bar's notices are storage operations. MatterCAD's `ScaledReporter`
///     (`Object3DBooleanOperations.cs:93-101`) is explicit that a
///     non-null reporter turns the batch boolean into a pairwise fold, so
///     passing one nobody listens to is pure cost. We already fold
///     pairwise, so the cost is smaller — but it is still a cost with no
///     benefit.
pub fn boolean_op(a: &Manifold, b: &Manifold, op: OpType, opts: BooleanOptions) -> Manifold {
    a.boolean_with_engine_and_rule(b, op, BooleanEngine::Auto, opts.winding_rule())
}

/// One connected input slot and the bodies it carries.
pub struct InputGroup {
    pub socket: SocketUid,
    /// Input socket name — what the user sees, and what a refusal message
    /// names so they know which part of the graph to fix.
    pub name: Arc<str>,
    pub bodies: Vec<Body>,
    /// The 1-based part number each body had **in the user's input**,
    /// parallel to `bodies`. Carried explicitly because [`split_roles`]
    /// hands out subsets: a refusal that renumbered "part 3" to "part 1"
    /// after the hole/solid split would send the user counting parts that
    /// only exist inside this node.
    pub parts: Vec<usize>,
    /// How many parts the input carried in total — what decides whether a
    /// refusal names a part at all. Also a split survivor: a one-body
    /// subset of a three-body input still says "part 2".
    pub total_parts: usize,
}

impl InputGroup {
    /// A sub-group of `self` holding only the listed body/part pairs.
    fn subset(&self, bodies: Vec<Body>, parts: Vec<usize>) -> Self {
        Self {
            socket: self.socket,
            name: self.name.clone(),
            bodies,
            parts,
            total_parts: self.total_parts,
        }
    }
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
    operand_sockets_of(ctx.instance)
}

/// [`operand_sockets`] against a bare instance, for the callers that have
/// no evaluation in hand — the property-row projection (B-3b) builds one
/// "Part(s) to Subtract" checkbox per entry of this list, and it must be
/// the *same* list the evaluation resolves the selection against or a row
/// could name a slot the boolean does not treat as an operand.
pub fn operand_sockets_of(instance: &crate::graph::node::NodeInstance) -> Vec<SocketUid> {
    instance
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
        // Part numbers count every body the input *carried*, including the
        // empty ones dropped here — the user counts what they wired up.
        let (bodies, parts): (Vec<Body>, Vec<usize>) = geom
            .iter()
            .enumerate()
            .filter(|(_, b)| num_verts(&b.mesh) > 0 && num_tris(&b.mesh) > 0)
            .map(|(i, b)| (b.clone(), i + 1))
            .unzip();
        if bodies.is_empty() {
            continue;
        }
        groups.push(InputGroup {
            socket: slot.uid,
            name: slot.name.clone(),
            bodies,
            parts,
            total_parts: geom.len(),
        });
    }
    Ok(groups)
}

/// Import every body of `group` as a boolean operand, with its matrix
/// baked in. A refusal becomes a [`NodeError`] naming the input (and the
/// part within it, when the input carries more than one body).
///
/// With [`BooleanOptions::repair_winding`] set, each imported handle is
/// rewound before it is handed back — MatterCAD repairs the operand right
/// after import too, so every consumer of the handle sees the repaired
/// geometry and no boolean can run against a half-repaired operand list.
///
/// Each handle is then re-tagged as an original ([`tag_original`]) so the
/// result's run data can say which operand a triangle came from, and
/// `palette` learns that operand's colour (B-6). Repair first, tag last:
/// the tag has to be on the handle the boolean actually sees.
pub fn import_group(
    group: &InputGroup,
    opts: BooleanOptions,
    palette: &mut Palette,
) -> Result<Vec<Manifold>, NodeError> {
    let mut solids = Vec::with_capacity(group.bodies.len());
    for operand in baked_operands(group) {
        let solid = import_operand(&operand.mesh)
            .map_err(|f| NodeError::msg(refusal_message(&operand.label, f)))?;
        let color = super::boolean_colors::operand_color(&operand.source);
        solids.push(tag_original(apply_repair(solid, opts), color, palette));
    }
    Ok(solids)
}

/// One boolean participant: the operand's mesh in **world space** (its
/// [`Body::matrix`] baked in, a mirror rewound) and the name the user knows
/// it by.
///
/// The pair that [`import_group`] used to build inline, named because the
/// degradation policy ([`super::boolean_degrade`]) needs the same baked
/// mesh both to import *and* to hand back untouched when the import is
/// refused — a rescued part is the geometry the user modelled, in the place
/// they modelled it.
#[derive(Clone)]
pub struct Operand {
    /// The input socket's name, plus the part number when the input
    /// carried more than one body.
    pub label: String,
    pub mesh: MeshGL,
    /// The body this operand was baked from — kept whole because a
    /// rescued operand has to come back out looking like *itself*:
    /// [`compose_with_upstream_and_mesh`](crate::registry::compose_with_upstream_and_mesh)
    /// resolves its colour and role
    /// from here, the same rule
    /// [`pass_through`] and Keep Subtracted Parts already follow. A part
    /// that visibly failed to combine must not also change colour.
    pub source: Body,
}

/// Bake every body of `group` into a world-space [`Operand`].
pub fn baked_operands(group: &InputGroup) -> Vec<Operand> {
    let multi = group.total_parts > 1;
    group
        .bodies
        .iter()
        .enumerate()
        .map(|(i, body)| {
            let mut mesh = apply_transform(&body.mesh, &body.matrix);
            if is_mirroring(&body.matrix) {
                reverse_winding(&mut mesh);
            }
            let label = if multi {
                format!("{} (part {})", group.name, group.parts[i])
            } else {
                group.name.to_string()
            };
            Operand { label, mesh, source: body.clone() }
        })
        .collect()
}

/// Every group's operands in one flat list, in wiring order.
pub fn baked_operands_of(groups: &[&InputGroup]) -> Vec<Operand> {
    groups.iter().flat_map(|g| baked_operands(g)).collect()
}

/// "Repair Winding Order" applied to one imported handle, or the handle
/// unchanged when the toggle is off.
pub fn apply_repair(solid: Manifold, opts: BooleanOptions) -> Manifold {
    if opts.repair_winding {
        solid.repair_orientation()
    } else {
        solid
    }
}

/// Import several groups' bodies into one flat operand list.
pub fn import_groups(
    groups: &[&InputGroup],
    opts: BooleanOptions,
    palette: &mut Palette,
) -> Result<Vec<Manifold>, NodeError> {
    let mut all = Vec::new();
    for g in groups {
        all.extend(import_group(g, opts, palette)?);
    }
    Ok(all)
}

/// Fold `op` over the operands left-to-right. `None` for an empty list.
///
/// The intermediate statuses are not inspected: a failed step propagates
/// through every later one, and [`finish`] reports it once with the same
/// message it would have carried here. Plan step B-5 replaces this fold
/// with touching-set batching and per-operand triage.
pub fn fold(solids: Vec<Manifold>, op: OpType, opts: BooleanOptions) -> Option<Manifold> {
    let mut it = solids.into_iter();
    let first = it.next()?;
    Some(it.fold(first, |acc, next| boolean_op(&acc, &next, op, opts)))
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
    Ok(finish_mesh(result.get_mesh_gl(-1)))
}

/// Make one mesh render-ready: `num_prop = 6`, one vertex per triangle
/// corner, flat normals recomputed.
///
/// Shared by [`finish`] and by the rescue path
/// ([`super::boolean_degrade`]), which emits a mesh the kernel never saw:
/// its normals came through a matrix bake and possibly a winding rewind, so
/// recomputing them is the only way the rescued part shades like everything
/// else the node emits.
pub fn finish_mesh(mesh: MeshGL) -> MeshGL {
    let mut out_mesh = mesh;
    promote_to_num_prop6(&mut out_mesh);
    out_mesh = split_for_flat_normals(&out_mesh);
    compute_flat_normals(&mut out_mesh);
    out_mesh
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

/// Finish one boolean result into an output body, painted from the run
/// data (B-6) — the one path every operation's result goes through.
///
/// `None` when the result bounds no volume: an empty body is still a
/// *body*, and part counts, exports and the viewport's per-body iteration
/// would all see a phantom part with no triangles.
///
/// The mesh is exported **once**, before it is made render-ready: run data
/// is the kernel's, and [`finish_mesh`]'s vertex split drops it (it is
/// meaningless once every triangle owns its corners). The per-triangle
/// colours are read off the raw export and re-attached to the split mesh,
/// whose triangle order is the same.
/// `base` is the colour of the part this body *is* — the keep a Subtract
/// cut, the first operand of a union set. It stands in wherever the run
/// data cannot answer (an operand that could not be tagged) and, more
/// importantly, it is the body's own `Body::color` for every consumer
/// that cannot read the per-vertex overlay. See [`painted_body`].
pub fn painted_result_body(
    ctx: &EvalCtx,
    result: &Manifold,
    palette: &Palette,
    base: [f32; 4],
    role: BodyRole,
) -> Result<Option<Body>, NodeError> {
    if result.status() != Error::NoError {
        return Err(NodeError::msg(format!(
            "Boolean: the operation failed ({})",
            result.status().to_str()
        )));
    }
    let raw = result.get_mesh_gl(-1);
    let paint = palette.paint_for(&raw, base);
    let mesh = finish_mesh(raw);
    if mesh.tri_verts.is_empty() {
        return Ok(None);
    }
    Ok(Some(painted_body(ctx, mesh, paint, None, base, role)))
}

/// [`painted_result_body`] with a colour the operation itself assigns —
/// Subtract & Replace's red for the volume it kept in place of the one it
/// cut away. The tint beats the run colours and loses to the node's own
/// `Color`; see [`painted_body`].
pub fn tinted_result_body(
    ctx: &EvalCtx,
    result: &Manifold,
    tint: [f32; 4],
    role: BodyRole,
) -> Result<Option<Body>, NodeError> {
    let mesh = match finish_non_empty(result)? {
        Some(m) => m,
        None => return Ok(None),
    };
    Ok(Some(painted_body(ctx, mesh, None, Some(tint), tint, role)))
}

/// The bodies of every group, handed back as they arrived — same meshes,
/// same colours (unless this node sets one), same solid/hole roles — but
/// **in this node's frame**. Used when an operation has nothing to do
/// (Subtract with no removers) and by Keep Subtracted Parts.
///
/// The frame is the whole subtlety. A body that comes out of the boolean
/// itself is baked and carries the node's matrix ([`result_body`]), so a
/// part passed through with only its upstream matrix would sit in a
/// different place the moment the user dragged the node's gizmo: the
/// result would move and its untouched neighbours would stay behind.
/// [`compose_with_upstream`] is the shared rule for exactly this —
/// `node_matrix · upstream_matrix`, upstream colour unless the node
/// overrides it, role preserved. The origin claim moves to this node, as
/// it does for every body the boolean emits ([`result_body`]) — the parts
/// are this node's output now, whatever it did or did not do to them.
pub fn pass_through(ctx: &EvalCtx, groups: &[InputGroup]) -> Geometry3d {
    Geometry3d::from_bodies(composed_bodies(ctx, &groups.iter().collect::<Vec<_>>()))
}

/// [`pass_through`]'s body list, for callers that already hold references
/// and want to append rather than build a group.
pub fn composed_bodies(ctx: &EvalCtx, groups: &[&InputGroup]) -> Vec<Body> {
    groups
        .iter()
        .flat_map(|g| g.bodies.iter())
        .map(|b| compose_with_upstream(ctx, b))
        .collect()
}

/// Split every group's bodies by [`crate::geometry::BodyRole`], keeping the
/// group identity (socket + name) on both sides so a refusal message still
/// names the input the part came from. Groups that end up with no bodies on
/// a side are dropped from that side.
///
/// Holes are per-*body*, unlike the keep/remove selection, which is per
/// input socket: the role travels with a body from whatever node marked it,
/// and one input may well carry both a solid and a hole.
pub fn split_roles(groups: &[InputGroup]) -> (Vec<InputGroup>, Vec<InputGroup>) {
    let mut solids = Vec::new();
    let mut holes = Vec::new();
    for g in groups {
        let (h, s): (Vec<(Body, usize)>, Vec<(Body, usize)>) = g
            .bodies
            .iter()
            .cloned()
            .zip(g.parts.iter().copied())
            .partition(|(b, _)| b.is_hole());
        if !s.is_empty() {
            let (bodies, parts) = s.into_iter().unzip();
            solids.push(g.subset(bodies, parts));
        }
        if !h.is_empty() {
            let (bodies, parts) = h.into_iter().unzip();
            holes.push(g.subset(bodies, parts));
        }
    }
    (solids, holes)
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
