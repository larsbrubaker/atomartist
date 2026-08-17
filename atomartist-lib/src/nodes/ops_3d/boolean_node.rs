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
//! | Combine | union of every operand — one body; parts marked as holes are unioned separately and subtracted from that (see below) |
//! | Intersect | intersection of every operand — one body |
//! | Subtract | union of the removers, subtracted from **each** keep; keeps stay separate bodies (`Object3DBooleanOperations.DoSubtract`: union the removers first, then one subtract per keep) |
//! | Subtract & Replace | as Subtract, plus each keep ∩ remover-union kept beside its keep as its own body; empty results are omitted |
//!
//! Every refusal, on import or on the boolean's own result, becomes a
//! [`NodeError`] naming the operand: a boolean that swallowed a bad operand
//! as empty geometry would still report success, and the part would silently
//! vanish from the output.
//!
//! ## Holes
//!
//! A body carrying [`BodyRole::Hole`] (stamped by the Mark Hole node —
//! MatterCAD's per-object `OutputType`, which every scene item has and we
//! give one node) is negative space. **Combine** honours that the way
//! `BooleanMeshBuilder.CombineMeshes` (L104-192) does: union the solids,
//! union the holes, subtract the second from the first. A Combine with
//! *only* holes wired in returns the hole union, still marked as a hole —
//! MatterCAD adds it to the scene as a `PrintOutputTypes.Hole` child, so
//! it can still cut something further downstream instead of vanishing.
//! The other three operations treat a hole as an ordinary participant,
//! also as MatterCAD does.
//!
//! ## Colours (plan step B-6)
//!
//! Each operand is imported as an *original* so the result's run data says
//! which operand every output triangle came from, and the result is
//! painted accordingly: a Combine shows each part's own colour, a Subtract
//! leaves the remover's colour on the faces it cut, and Subtract &
//! Replace's replacement wears the "removed material" red. The node's own
//! `Color` overrides all of it. See [`super::boolean_colors`].
//!
//! ## Toggles (plan step B-4)
//!
//! `keep_inside_out` and `repair_winding` resolve into
//! [`BooleanOptions`] and reach every import and every boolean call;
//! `keep_subtracted` is bookkeeping on the Subtract result. See
//! [`super::boolean_ops::BooleanOptions`] for how the two kernel toggles
//! differ from the mirror rewind that always runs.

use std::sync::Arc;

use manifold_rust::types::OpType;

use super::boolean_colors::{node_color_override, operand_color, Palette, REPLACED_TINT};
use super::boolean_degrade::{self, OperandReport};
use super::boolean_ops::{
    boolean_op, composed_bodies, fold, gather_inputs, import_group, import_groups, operand_sockets,
    painted_result_body, pass_through, tinted_result_body, BooleanOptions, InputGroup,
};
use super::boolean_selection::{self, SUBTRACT_PARTS};
use super::dynamic_inputs;
use crate::geometry::{Body, BodyRole, Geometry3d, DEFAULT_GEOMETRY_COLOR, INHERIT_COLOR};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    enum_variant_for_index, op_props, ConnectCtx, DisconnectCtx, EditorKind, EvalCtx,
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

/// Icon-registry ids for [`OPERATIONS`], in the same order — the row is
/// an icon strip (MatterCAD renders this property as an icon row,
/// `BooleanObject3D.cs:129-151`), and the schema names the artwork
/// instead of carrying it.
///
/// The drawings themselves live in the UI layer
/// (`atomartist-ui/src/boolean_icons.rs`) and are registered into
/// agg-gui's icon registry at startup. An id with nothing registered
/// under it renders as the variant's label, so a headless caller — or a
/// shell that skipped the registration — still gets a usable row.
pub const OPERATION_ICONS: [&str; 4] = [
    "boolean.combine",
    "boolean.subtract",
    "boolean.intersect",
    "boolean.subtract_and_replace",
];

/// The Boolean node's own params — the shared `color` / `matrix` pair is
/// added by [`properties`] from [`op_props`], which strips the socket
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
        .editor(EditorKind::enum_icons(
            OPERATIONS.iter().copied().zip(OPERATION_ICONS),
        ))
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
        // The three toggles, in MatterCAD's declaration order
        // (`BooleanObject3D.cs:163-173`). All three are property-only:
        // a param-minted socket after the placeholder would break the
        // trailing-empty invariant (see the doc on this function).
        .bool_(KEEP_INSIDE_OUT, "Keep Inside Out Geometry", false)
        .no_socket()
        .description(
            "Treat an inside-out shell as solid material instead of letting it cancel \
             out the volume around it. Turn this on when a model that should be solid \
             comes back with parts missing. It forces the slower, robust boolean engine.",
        )
        .bool_(REPAIR_WINDING, "Repair Winding Order", false)
        .no_socket()
        .description(
            "Rewind each part's inside-out shells before the boolean runs. This fixes \
             the geometry once rather than changing what every later operation counts \
             as solid, and is usually the better answer of the two.",
        )
        .bool_(KEEP_SUBTRACTED, "Keep Subtracted Parts", false)
        .no_socket()
        .description(
            "Leave the parts that were cut away in the result rather than discarding them.",
        )
}

/// "Keep Inside Out Geometry" — [`BooleanOptions::keep_inside_out`].
pub const KEEP_INSIDE_OUT: &str = "keep_inside_out";
/// "Repair Winding Order" — [`BooleanOptions::repair_winding`].
pub const REPAIR_WINDING: &str = "repair_winding";
/// "Keep Subtracted Parts" — Subtract-only bookkeeping.
pub const KEEP_SUBTRACTED: &str = "keep_subtracted";

/// The kernel toggles as the geometry code wants them.
fn options_of(props: &NodeProperties) -> BooleanOptions {
    BooleanOptions {
        keep_inside_out: props.bool_(KEEP_INSIDE_OUT, false),
        repair_winding: props.bool_(REPAIR_WINDING, false),
    }
}

/// The full property list: the shared (socket-free) `color` + `matrix`
/// rows first, then this node's own params.
///
/// The **op** pair, not the producer one: Boolean's `Color` defaults to
/// the inherit sentinel, which is what lets the operands' own colours
/// reach the result (B-6, [`super::boolean_colors`]) and what the rescue
/// path has always resolved against (`compose_with_upstream_and_mesh`).
/// Picking a colour on the node still overrides every part of the result.
fn properties() -> Vec<PropDef> {
    let mut props = op_props();
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

/// True when "Keep Subtracted Parts" means anything — plain Subtract only
/// (`KeepSubtractObjectsAvailable`, consumed by `UpdateControls` L397-408).
/// Subtract & Replace already keeps the removed volume, by definition.
pub fn keep_subtracted_row_available(props: &NodeProperties) -> bool {
    operation_of(props) == "Subtract"
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
        normalize_pre_b6_color(graph, node);
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
            KEEP_SUBTRACTED => {
                keep_subtracted_row_available(props) && self.default_row_visible(name, props)
            }
            // Keep Inside Out / Repair Winding are MatterCAD's
            // `KernelOptionsAvailable` rows — true whenever the operands
            // are meshes, which for this node is always (the operand
            // sockets are typed Geometry3d; there is no path content).
            _ => self.default_row_visible(name, props),
        }
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let groups = gather_inputs(ctx)?;
        if groups.is_empty() {
            return Ok(NodeOutputs::default());
        }
        let operation = operation_of(ctx.properties);
        let opts = options_of(ctx.properties);

        let mut out = NodeOutputs::default();
        let geometry = match operation {
            "Subtract" => subtract(ctx, &groups, false, opts)?,
            "Subtract & Replace" => subtract(ctx, &groups, true, opts)?,
            "Intersect" => single_result(ctx, &groups, OpType::Intersect, opts)?,
            // Combine, and anything unrecognised (see `operation_of`).
            _ => {
                let (geometry, report) = combine(ctx, &groups, opts)?;
                // The node succeeded: a degraded union still produced real
                // geometry and everything downstream of it is valid. The
                // parts it could not use are in the output too — the only
                // thing left is to say so, which is what the warning
                // channel is for (B-5; `NodeOutputs::warnings`).
                for message in report.warnings() {
                    out.warn(message);
                }
                geometry
            }
        };

        out.set("out", PortValue::Geometry3d(Arc::new(geometry)));
        Ok(out)
    }
}

/// Intersect: fold one operation over every operand and emit the single
/// resulting body.
fn single_result(
    ctx: &EvalCtx,
    groups: &[InputGroup],
    op: OpType,
    opts: BooleanOptions,
) -> Result<Geometry3d, NodeError> {
    let refs: Vec<&InputGroup> = groups.iter().collect();
    let mut palette = new_palette(ctx);
    // An intersection is one body made of every operand, with no single
    // part to call its own — the first one the user wired stands in
    // wherever the run data cannot answer (B-6).
    let base = first_operand_color(groups);
    let solids = import_groups(&refs, opts, &mut palette)?;
    let result = match fold(solids, op, opts) {
        Some(r) => r,
        None => return Ok(Geometry3d::empty()),
    };
    Ok(match painted_result_body(ctx, &result, &palette, base, BodyRole::Solid)? {
        Some(body) => Geometry3d::from_body(body),
        None => Geometry3d::empty(),
    })
}

/// The colour that stands in for a result with no single source part —
/// the first operand the user wired. See
/// [`painted_body`](super::boolean_colors::painted_body) for why a body
/// needs one even when it carries a per-vertex overlay.
fn first_operand_color(groups: &[InputGroup]) -> [f32; 4] {
    groups
        .first()
        .and_then(|g| g.bodies.first())
        .map(operand_color)
        .unwrap_or(DEFAULT_GEOMETRY_COLOR)
}

/// The colour bookkeeping this evaluation should do: none at all when the
/// node's own `Color` already decides every body's colour (B-6 —
/// re-tagging each operand as an original is not free, and the run data
/// would only be discarded).
fn new_palette(ctx: &EvalCtx) -> Palette {
    if node_color_override(ctx).is_some() {
        Palette::disabled()
    } else {
        Palette::new()
    }
}

/// Combine: union the solids, union the holes, subtract the hole union
/// from the solid union (`BooleanMeshBuilder.CombineMeshes`, L104-192),
/// under the degradation policy — touching sets, partial union, repair
/// triage, rescue. See [`super::boolean_degrade`]; the report it hands
/// back becomes this node's warnings.
///
/// With no holes wired in this is exactly the plain n-ary union. With
/// *only* holes it is the hole union, still marked as a hole — see the
/// module docs.
fn combine(
    ctx: &EvalCtx,
    groups: &[InputGroup],
    opts: BooleanOptions,
) -> Result<(Geometry3d, OperandReport), NodeError> {
    boolean_degrade::combine_degrading(ctx, groups, opts)
}

/// Subtract / Subtract & Replace.
///
/// The removers are unioned once and then subtracted from each keep in
/// turn, so the keeps stay separate bodies — MatterCAD's `DoSubtract`
/// (L166-302) does exactly this, and it is also the cheap order: one union
/// plus one subtract per keep instead of keeps × removers subtracts.
fn subtract(
    ctx: &EvalCtx,
    groups: &[InputGroup],
    replace: bool,
    opts: BooleanOptions,
) -> Result<Geometry3d, NodeError> {
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
        return Ok(pass_through(ctx, groups));
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
        return Ok(pass_through(ctx, groups));
    }
    if keeps.is_empty() {
        // The keeps are all evaluating empty: there is genuinely nothing
        // to cut, and nothing was selected away either.
        return Ok(Geometry3d::empty());
    }

    // One palette for the whole operation: the removers are tagged here
    // and the keeps below, so the cut faces the removers leave behind come
    // out wearing the remover's colour — MatterCAD paints a result face
    // from whichever operand it came from, and a cut face came from the
    // cutter (`ExtractFaceColorsFromRuns`, L870-927).
    let mut palette = new_palette(ctx);
    let remover_union = match fold(
        import_groups(&removers, opts, &mut palette)?,
        OpType::Add,
        opts,
    ) {
        Some(u) => u,
        None => return Ok(pass_through(ctx, groups)),
    };

    let mut bodies: Vec<Body> = Vec::new();
    for keep in keeps {
        // Each imported solid is `keep.bodies[i]`, so its role is to hand
        // on: cutting a hole leaves a hole. MatterCAD carries a child's
        // `OutputType` across a subtract the same way, and without it a
        // `Mark Hole → Subtract → Combine` chain quietly stops cutting.
        // Intersect and Combine deliberately do *not* do this — their
        // result is one body made of several operands, with no single role
        // to inherit, and MatterCAD's Combine likewise emits material.
        for (i, solid) in import_group(keep, opts, &mut palette)?.into_iter().enumerate() {
            let role = keep.bodies[i].role;
            // The keep's own colour is what this body is: it stands in
            // on the surfaces the run data cannot claim, and it is the
            // colour every consumer that drops the per-vertex overlay
            // sees. The remover's colour reaches the cut faces through
            // the run data alone — it must never become the body's.
            let base = operand_color(&keep.bodies[i]);
            let cut = boolean_op(&solid, &remover_union, OpType::Subtract, opts);
            bodies.extend(painted_result_body(ctx, &cut, &palette, base, role)?);
            if replace {
                // The volume the subtraction removed, kept beside its keep
                // as its own body — and tinted red, the "removed material"
                // colour of the operation's own icon, so the replacement
                // reads as what it is rather than as another keep
                // (MatterCAD's retained remover, plan §3.5).
                let removed = boolean_op(&solid, &remover_union, OpType::Intersect, opts);
                bodies.extend(tinted_result_body(ctx, &removed, REPLACED_TINT, role)?);
            }
        }
    }
    // "Keep Subtracted Parts" (plain Subtract only — Subtract & Replace
    // already emits the removed volume). MatterCAD re-adds the remover
    // *meshes* as children of the boolean, with their world properties
    // copied (`BooleanMeshBuilder.SubtractMeshes`, L269-285) — which is
    // what puts them in the boolean's frame rather than their own. Ours
    // does the same through `compose_with_upstream`: the remover keeps its
    // own mesh, colour and role, untouched by the subtraction that used
    // it, but moves with this node like every other body it emits.
    if !replace && ctx.properties.bool_(KEEP_SUBTRACTED, false) {
        bodies.extend(composed_bodies(ctx, &removers));
    }
    Ok(Geometry3d::from_bodies(bodies))
}

/// Turn a **pre-B-6** stored `color` back into "no colour chosen".
///
/// Every node is seeded with its schema's defaults at creation
/// (`Graph::add_new_node`) and those defaults are saved, so a Boolean
/// written before B-6 carries an explicit [`DEFAULT_GEOMETRY_COLOR`] —
/// the producer default this node used to declare. Read literally that is
/// an override, and B-6 would be silently inert on every existing file:
/// the operands' colours would never show, with nothing on screen to say
/// why. Mapping exactly that value back to the inherit sentinel costs one
/// comparison per load and puts old and new files on the same footing.
///
/// The cost is a user who deliberately picked the default grey: their
/// choice becomes "inherit". That is the right trade — the two render
/// identically unless the operands disagree, and when they do, showing
/// the parts' own colours is what the user is now asking for by opening a
/// B-6 build.
fn normalize_pre_b6_color(graph: &mut crate::graph::graph::Graph, node: crate::graph::node::NodeId) {
    let is_default = matches!(
        graph.get(node).and_then(|n| n.properties.get("color")),
        Some(PortValue::Color(c)) if *c == DEFAULT_GEOMETRY_COLOR
    );
    if is_default {
        let _ = graph.set_property(node, "color", PortValue::Color(INHERIT_COLOR));
    }
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

// The operation row's schema shape (icon ids, and that the presentation
// switch left the value semantics alone).
#[cfg(test)]
#[path = "boolean_icons_tests.rs"]
mod icons_tests;

#[cfg(test)]
#[path = "boolean_options_tests.rs"]
mod options_tests;

// The degradation policy's tests live with the node rather than with
// `boolean_degrade` because most of them drive the whole node (the shared
// fixtures are in `boolean_node_tests`, a private sibling module).
#[cfg(test)]
#[path = "boolean_degrade_tests.rs"]
mod degrade_tests;

// Likewise the colour tests (B-6): the run-data mapping is only observable
// through a whole evaluation, and the fixtures for that live here.
#[cfg(test)]
#[path = "boolean_colors_tests.rs"]
mod colors_tests;
