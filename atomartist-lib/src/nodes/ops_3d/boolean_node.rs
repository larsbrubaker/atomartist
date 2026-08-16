//! Boolean operation node — Combine / Subtract / Intersect / Subtract &
//! Replace over **any number** of `MeshGL` solids via `manifold-rust`.
//!
//! The operation is a first-class enum param serialized **by name**
//! (MatterCAD's `BooleanOperation`, `BooleanObject3D.cs:61-75`), so the
//! variant list can be reordered without rewriting saved graphs. Graphs
//! written before this step stored the operation as a `Number` (0 = union,
//! 1 = difference, 2 = intersection); those load through the generic
//! index → variant migration in
//! [`crate::serialization::prop_migration`], and a stray in-memory
//! `Number` still resolves the same way in [`operation_of`].
//!
//! MatterCAD also renames the scene item when the operation changes, but
//! only while the name is still the default one it gave the item
//! (`BooleanObject3D.cs:129-151`). Not ported: a `NodeInstance` has no
//! per-node name at all — the canvas titles every node from its type's
//! `display_name` — so there is neither a name to change nor a
//! user-renamed flag to respect. Inventing that state is a graph-model
//! decision, not a Boolean-node one.
//!
//! ## Operands
//!
//! Inputs follow [`super::dynamic_inputs`]' trailing-empty model (shared
//! with Combine): every connected slot is an operand, and each body inside
//! a slot is its own participant with its `Body::matrix` baked in. See
//! [`super::boolean_ops`] for the gathering / import / fold plumbing and
//! [`super::boolean_selection`] for "Part(s) to Subtract".
//!
//! Graphs saved before the dynamic inputs landed carry fixed `a` / `b`
//! sockets. Nothing special is needed to read them: the sockets are
//! restored from the file, and the evaluate path treats every named
//! `Geometry3d` input as an operand, so `a` and `b` simply *are* the first
//! two dynamic inputs, and [`NodeDef::on_loaded`] appends the trailing
//! empty slot such a file cannot carry, so the node is immediately
//! connectable.
//!
//! ## The four operations, n-ary
//!
//! | Operation | Result |
//! |---|---|
//! | Combine | union of every operand — one body. Hole/solid participation is plan step B-4 |
//! | Intersect | intersection of every operand — one body |
//! | Subtract | union of the removers, subtracted from **each** keep; keeps stay separate bodies (`Object3DBooleanOperations.DoSubtract`: union the removers first, then one subtract per keep) |
//! | Subtract & Replace | as Subtract, plus each keep ∩ remover-union kept beside its keep as its own body; empty results are omitted |
//!
//! Every refusal, on import or on the boolean's own result, becomes a
//! [`NodeError`] naming the operand: a boolean that swallowed a bad operand
//! as empty geometry would still report success, and the part would silently
//! vanish from the output.

use std::sync::Arc;

use manifold_rust::types::OpType;

use super::boolean_ops::{
    finish_non_empty, fold, gather_inputs, import_group, import_groups, operand_sockets,
    pass_through, result_body, InputGroup,
};
use super::boolean_selection::{self, SUBTRACT_PARTS};
use super::dynamic_inputs;
use crate::geometry::{Body, Geometry3d};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    enum_variant_for_index, geometry_props, ConnectCtx, DisconnectCtx, EditorKind, EvalCtx,
    InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeProperties, NodeRegistry, ParamSet,
    PropDef, VisibleWhen,
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

/// The Boolean node's own params — the shared `color` / `matrix` pair is
/// added by [`properties`] from [`geometry_props`], which strips the socket
/// binding.
///
/// Nothing here mints an input socket, and neither does the node: its input
/// list is dynamic, and the trailing-empty invariant needs the *last* input
/// slot to be the placeholder, which a param-minted `Color` / `Matrix`
/// socket after it would break. Combine (the other dynamic-input node)
/// carries the same property-only pair for the same reason.
fn own_params() -> ParamSet {
    ParamSet::new()
        .enum_("operation", "Operation", OPERATIONS[0], &OPERATIONS)
        .editor(EditorKind::EnumButtons {
            variants: OPERATIONS.iter().map(|v| Arc::from(*v)).collect(),
        })
        .description(
            "Combine merges the parts; Subtract cuts the selected parts out of the \
             others; Intersect keeps only the shared volume; Subtract & Replace cuts \
             the selected parts out and keeps the removed volume as its own part.",
        )
        .string(SUBTRACT_PARTS, "Part(s) to Subtract", boolean_selection::AUTO)
        .no_socket()
        // Hidden for now: the value is a list of input-socket uids, and
        // there is no editor that can render a checkbox per *connected
        // input* — `NodeDef::properties()` is a per-type schema with no
        // access to the instance, so a dynamic row count is not
        // expressible today. The semantics (default, pruning, per-input
        // keep/remove) are live; only the row is missing.
        .visible_when(VisibleWhen::Never)
        .description(
            "Which inputs are cut out of the others. Empty means the last connected \
             input is the remover.",
        )
}

/// The full property list: the shared (socket-free) `color` + `matrix`
/// rows first, then this node's own params.
fn properties() -> Vec<PropDef> {
    let mut props = geometry_props();
    props.extend(own_params().prop_defs());
    props
}

/// Resolve the stored operation to one of [`OPERATIONS`], accepting the
/// legacy `Number` index and falling back to the default for anything
/// unrecognised. Shared by `evaluate` and `row_visible`, which must agree.
pub fn operation_of(props: &NodeProperties) -> &'static str {
    let variants: Vec<Arc<str>> = OPERATIONS.iter().map(|v| Arc::from(*v)).collect();
    match props.get("operation") {
        PortValue::StringVal(s) => OPERATIONS
            .iter()
            .find(|v| **v == s.as_str())
            .copied()
            .unwrap_or(OPERATIONS[0]),
        PortValue::Number(n) => match enum_variant_for_index(&variants, *n) {
            Some(name) => OPERATIONS
                .iter()
                .find(|v| **v == name)
                .copied()
                .unwrap_or(OPERATIONS[0]),
            None => OPERATIONS[0],
        },
        _ => OPERATIONS[0],
    }
}

/// True when the operation is one that cuts, so the part-selection row is
/// meaningful (MatterCAD's `SelectedChildrenAvailable`, consumed by
/// `UpdateControls`, L397-408).
pub fn selection_row_available(props: &NodeProperties) -> bool {
    matches!(operation_of(props), "Subtract" | "Subtract & Replace")
}

impl NodeDef for BooleanNode {
    fn type_id(&self) -> &'static str { "Boolean" }
    fn display_name(&self) -> &'static str { "Boolean" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        // One typed-empty placeholder input; one static output. No param
        // mints a socket (see `own_params`), so the placeholder stays last.
        InstanceTemplate::builder(alloc)
            .input_opt("", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        properties()
    }

    fn on_input_connected(&self, ctx: &mut ConnectCtx) {
        dynamic_inputs::adopt_connected_slot(ctx);
    }

    fn on_loaded(&self, graph: &mut crate::graph::graph::Graph, node: crate::graph::node::NodeId) {
        // A graph saved with the fixed `a` / `b` sockets has no trailing
        // empty slot, and every slot it does have is occupied — without
        // this the node could never take a third operand.
        dynamic_inputs::ensure_trailing_empty_input(graph, node);
    }

    fn on_input_disconnected(&self, ctx: &mut DisconnectCtx) {
        dynamic_inputs::collapse_disconnected_slot(ctx);
        // MatterCAD's `CleanUpSelectedChildrenIDs` (L435-450): a part that
        // is gone cannot stay selected. Left in place, the uid would
        // re-attach itself to a future socket that happened to be minted
        // with the same number.
        let node = ctx.this_node;
        let (stored, live) = match ctx.graph.get(node) {
            Some(n) => (
                boolean_selection::stored(&properties_of(n)),
                n.inputs.iter().map(|s| s.uid).collect::<Vec<_>>(),
            ),
            None => return,
        };
        if let Some(pruned) = boolean_selection::prune(&stored, &live) {
            let _ = ctx.graph.set_property(
                node,
                SUBTRACT_PARTS,
                PortValue::StringVal(Arc::new(pruned)),
            );
        }
    }

    fn row_visible(&self, name: &str, props: &NodeProperties) -> bool {
        match name {
            // Both gates apply: the operation must be one that cuts
            // (UpdateControls parity), and the row itself is still
            // declared hidden because no editor can render it yet.
            SUBTRACT_PARTS => {
                selection_row_available(props) && self.default_row_visible(name, props)
            }
            _ => self.default_row_visible(name, props),
        }
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let groups = gather_inputs(ctx)?;
        if groups.is_empty() {
            return Ok(NodeOutputs::default());
        }
        let operation = operation_of(ctx.properties);

        let geometry = match operation {
            "Subtract" => subtract(ctx, &groups, false)?,
            "Subtract & Replace" => subtract(ctx, &groups, true)?,
            "Intersect" => single_result(ctx, &groups, OpType::Intersect)?,
            // Combine, and anything unrecognised (see `operation_of`).
            _ => single_result(ctx, &groups, OpType::Add)?,
        };

        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(geometry)));
        Ok(out)
    }
}

/// Combine / Intersect: fold one operation over every operand and emit the
/// single resulting body.
fn single_result(
    ctx: &EvalCtx,
    groups: &[InputGroup],
    op: OpType,
) -> Result<Geometry3d, NodeError> {
    let refs: Vec<&InputGroup> = groups.iter().collect();
    let solids = import_groups(&refs)?;
    let result = match fold(solids, op) {
        Some(r) => r,
        None => return Ok(Geometry3d::empty()),
    };
    Ok(match finish_non_empty(&result)? {
        Some(mesh) => match result_body(ctx, mesh) {
            Some(b) => Geometry3d::from_body(b),
            None => Geometry3d::empty(),
        },
        None => Geometry3d::empty(),
    })
}

/// Subtract / Subtract & Replace.
///
/// The removers are unioned once and then subtracted from each keep in
/// turn, so the keeps stay separate bodies — MatterCAD's `DoSubtract`
/// (L166-302) does exactly this, and it is also the cheap order: one union
/// plus one subtract per keep instead of keeps × removers subtracts.
fn subtract(ctx: &EvalCtx, groups: &[InputGroup], replace: bool) -> Result<Geometry3d, NodeError> {
    // Resolved against every operand *slot*, not just the ones that
    // evaluated to something this frame: an input that is momentarily
    // empty must not hand its role to a neighbour (the default remover is
    // "the last input", and that must not move under the user).
    let sockets = operand_sockets(ctx);
    let remover_sockets = boolean_selection::removers(&sockets, ctx.properties);

    let (removers, keeps): (Vec<&InputGroup>, Vec<&InputGroup>) = groups
        .iter()
        .partition(|g| remover_sockets.contains(&g.socket));

    if remover_sockets.is_empty() {
        // Nothing was chosen to cut with — a lone operand, or a selection
        // that named only parts which are gone. The parts pass through
        // untouched rather than vanishing (`GetSubtractItems` returns the
        // source itself for a single child).
        return Ok(pass_through(groups));
    }
    if sockets.iter().all(|u| remover_sockets.contains(u)) {
        // Every operand is a remover: there is nothing left to cut *from*.
        // Returning empty geometry here would be the silent-vanish this
        // node exists to prevent — the user asked to cut and would get a
        // blank viewport with no explanation.
        return Err(NodeError::msg(
            "Boolean: every part is selected as a remover — nothing remains to cut from",
        ));
    }
    if removers.is_empty() {
        // The chosen removers are all evaluating empty right now (an
        // upstream node produced nothing). Cutting with nothing removes
        // nothing, so the keeps — which are all `groups` holds at this
        // point — pass through unchanged.
        return Ok(pass_through(groups));
    }
    if keeps.is_empty() {
        // The keeps are all evaluating empty: there is genuinely nothing
        // to cut, and nothing was selected away either.
        return Ok(Geometry3d::empty());
    }

    let remover_union = match fold(import_groups(&removers)?, OpType::Add) {
        Some(u) => u,
        None => return Ok(pass_through(groups)),
    };

    let mut bodies: Vec<Body> = Vec::new();
    for keep in keeps {
        for solid in import_group(keep)? {
            let cut = solid.boolean(&remover_union, OpType::Subtract);
            if let Some(mesh) = finish_non_empty(&cut)? {
                bodies.extend(result_body(ctx, mesh));
            }
            if replace {
                // The volume the subtraction removed, kept beside its keep
                // as its own body. B-6 gives it its own colour (MatterCAD
                // tints a retained remover red).
                let removed = solid.boolean(&remover_union, OpType::Intersect);
                if let Some(mesh) = finish_non_empty(&removed)? {
                    bodies.extend(result_body(ctx, mesh));
                }
            }
        }
    }
    Ok(Geometry3d::from_bodies(bodies))
}

/// A [`NodeProperties`] view of a live instance's property map, so the
/// selection helpers can read the same way during a graph hook as they do
/// during evaluation.
fn properties_of(node: &crate::graph::node::NodeInstance) -> NodeProperties {
    let mut props = NodeProperties::default();
    for (k, v) in &node.properties {
        props.insert(k.clone(), v.clone());
    }
    props
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(BooleanNode);
}

#[cfg(test)]
#[path = "boolean_node_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "boolean_nary_tests.rs"]
mod nary_tests;
