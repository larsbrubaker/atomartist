//! Shared placement + auto-wiring for programmatically inserted nodes
//! (`docs/file-browser-design.md` §5b, step 6f-4).
//!
//! Port of NodeDesigner's `static/js/node-editor/core/node-insertion.js`,
//! which its mesh drop, component picker and parts-bar drag-out all call.
//! Ours is called by the favorites-bar drag-insert controller
//! ([`crate::drag_insert`]) for both of its drop targets — the node
//! canvas and the 3-D viewport — so a node that arrives without a
//! user-chosen position lands where the ancestor put it: immediately
//! left of the Output node, vertically centred on it, nudged clear of
//! anything already there, and wired into the Output's first free input.
//!
//! # Why this lives in `atomartist-ui`
//!
//! Placement needs two things at once: node **positions** (an
//! `atomartist-lib` concern) and node **sizes** — which no `Graph` knows,
//! because a node's box is derived at layout time by
//! `agg_gui_node_editor::draw::layout_node` from the [`NodeView`]s the
//! [`AppStateModel`](crate::AppStateModel) projects. Only this crate can
//! see both, so the whole module lives here, next to the adapter, and
//! reads the *real* rendered boxes rather than guessing a constant node
//! size. The wiring half could live in `atomartist-lib`; it stays here so
//! the ancestor's one file stays one file.
//!
//! # Coordinates
//!
//! Canvas-space, **Y-up** (agg-gui / CLAUDE.md). A node's `position` is
//! its **top-left**, so its body spans `y - height ..= y` — the opposite
//! of NodeDesigner's top-down space. Every sign that differs from the
//! ancestor is flagged where it occurs; in particular the free-slot scan
//! tries the slot that looks *upward on screen* first, which is
//! `target + offset` here and `targetY - offset` there.

use std::cell::Cell;

use agg_gui_node_editor::draw::layout_node;

use atomartist_lib::graph::graph::Noodle;
use atomartist_lib::graph::node::NodeId;
use atomartist_lib::graph::socket::SocketUid;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::socket_types::SocketType;
use atomartist_lib::Graph;

/// Type id of the graph terminal every auto-wire targets. NodeDesigner's
/// `graph/output`.
pub const OUTPUT_TYPE_ID: &str = "Output";

/// Vertical step of the free-slot scan, and of the pile-up fallback.
/// ND's `NODE_OFFSET_STEP`.
pub const NODE_OFFSET_STEP: f64 = 30.0;
/// Gap left between the new node's right edge and the Output's left
/// edge. ND's `NODE_HORIZONTAL_GAP`.
pub const NODE_HORIZONTAL_GAP: f64 = 50.0;
/// How much each existing node is inflated by for the occupancy test —
/// the breathing room the ancestor leaves around a placed node.
pub const OCCUPANCY_INFLATE: f64 = 20.0;
/// Furthest the free-slot scan travels from the target before giving up.
pub const MAX_SCAN_OFFSET: f64 = 300.0;
/// The pile-up offset wraps once it passes this, so successive
/// give-up placements fan out instead of marching off the canvas.
pub const PILE_UP_WRAP: f64 = 150.0;
/// Size assumed for a node that does not exist yet (an async import).
/// ND's `newNode.size || [200, 100]`.
pub const DEFAULT_NODE_SIZE: [f64; 2] = [200.0, 100.0];

thread_local! {
    /// ND's module-level `nodeAddOffset.y`. Thread-local rather than a
    /// global: it is pure UI-thread state, and per-thread isolation keeps
    /// parallel tests from reading each other's fan-out.
    static PILE_UP: Cell<f64> = const { Cell::new(0.0) };
}

/// Current pile-up offset — the fan-out applied when the scan found no
/// free slot at all.
pub fn pile_up_offset() -> f64 {
    PILE_UP.with(|c| c.get())
}

/// Advance the pile-up offset one step, wrapping past [`PILE_UP_WRAP`].
/// ND advances after *every* Output-relative placement, not only after
/// the give-up case.
pub fn advance_pile_up_offset() {
    PILE_UP.with(|c| {
        let next = c.get() + NODE_OFFSET_STEP;
        c.set(if next > PILE_UP_WRAP { 0.0 } else { next });
    });
}

/// Reset the fan-out. Test seam; production never needs it.
pub fn reset_pile_up_offset() {
    PILE_UP.with(|c| c.set(0.0));
}

/// One node's canvas-space box: `top_left` (Y-up) plus its rendered
/// `size`. The unit the occupancy test works in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeBox {
    pub id: NodeId,
    pub top_left: [f64; 2],
    pub size: [f64; 2],
}

impl NodeBox {
    pub fn new(id: NodeId, top_left: [f64; 2], size: [f64; 2]) -> Self {
        Self { id, top_left, size }
    }
}

/// Does a candidate box at `top_left` of `size` overlap `other`, once
/// `other` is inflated by [`OCCUPANCY_INFLATE`]? Strict inequalities
/// throughout, matching the ancestor (edge-to-edge contact at exactly
/// the inflated distance is *not* an overlap).
fn overlaps(top_left: [f64; 2], size: [f64; 2], other: &NodeBox) -> bool {
    let (x, y) = (top_left[0], top_left[1]);
    let (w, h) = (size[0], size[1]);
    let left = other.top_left[0] - OCCUPANCY_INFLATE;
    let right = other.top_left[0] + other.size[0] + OCCUPANCY_INFLATE;
    // Y-up: the body hangs *below* its top-left.
    let top = other.top_left[1] + OCCUPANCY_INFLATE;
    let bottom = other.top_left[1] - other.size[1] - OCCUPANCY_INFLATE;
    x < right && x + w > left && y - h < top && y > bottom
}

/// Port of ND's `findPositionLeftOfOutput`.
///
/// `others` is every node the placement must dodge — i.e. the graph
/// minus the Output node itself and minus the node being placed (the
/// ancestor skips exactly those two). `pile_up` is the current
/// [`pile_up_offset`], used only when the scan exhausts itself.
///
/// Y-up notes: the vertical centring subtracts where ND adds, and the
/// scan tries `target + offset` (upward on screen) before
/// `target - offset`, which is ND's `targetY - offset` / `targetY +
/// offset` order expressed in bottom-up coordinates.
pub fn find_position_left_of_output(
    others: &[NodeBox],
    output: NodeBox,
    new_size: [f64; 2],
    pile_up: f64,
) -> [f64; 2] {
    let target_x = output.top_left[0] - new_size[0] - NODE_HORIZONTAL_GAP;
    let target_y = output.top_left[1] - (output.size[1] - new_size[1]) * 0.5;

    let free = |y: f64| !others.iter().any(|n| overlaps([target_x, y], new_size, n));

    if free(target_y) {
        return [target_x, target_y];
    }
    let mut offset = NODE_OFFSET_STEP;
    while offset <= MAX_SCAN_OFFSET {
        if free(target_y + offset) {
            return [target_x, target_y + offset];
        }
        if free(target_y - offset) {
            return [target_x, target_y - offset];
        }
        offset += NODE_OFFSET_STEP;
    }
    // Nothing free anywhere: stack downward on screen, fanned out by the
    // module's advancing offset (ND's `targetY + nodeAddOffset.y`).
    [target_x, target_y - pile_up]
}

/// The Output node of `graph`, if it has one.
pub fn find_output_node(graph: &Graph) -> Option<NodeId> {
    graph
        .nodes()
        .find(|n| n.type_id.as_ref() == OUTPUT_TYPE_ID)
        .map(|n| n.id)
}

/// The noodle that would auto-wire `node` into the graph's Output, or
/// `None` when any link in the chain is missing: no Output node, no
/// geometry output on `node`, or no free input left on the Output.
///
/// Pure — it only *plans* the connection, so the caller can decide
/// whether to apply it directly or wrap it in an undo command. Silent by
/// design (ND returns a bare `false`): a node type that has nothing
/// geometric to say is inserted unwired, not rejected.
pub fn plan_auto_connect(graph: &Graph, node: NodeId) -> Option<AutoWirePlan> {
    let from_socket = graph
        .get(node)?
        .outputs
        .iter()
        .find(|s| s.socket_type == SocketType::Geometry3d)?
        .uid;
    let output = find_output_node(graph)?;
    if output == node {
        return None;
    }
    // The Output must have room *now*; the target socket itself is
    // deliberately not part of the plan (see [`AutoWirePlan`]).
    graph.first_free_input(output)?;
    Some(AutoWirePlan {
        from: node,
        from_socket,
        output,
    })
}

/// The three stable halves of an auto-wire: source node, source socket,
/// and the Output node to feed.
///
/// The **target socket is deliberately absent**. `Output` is a
/// dynamic-input node: disconnecting an input deletes that slot and
/// regrows a trailing empty one with a fresh [`SocketUid`], so a target
/// uid captured here would be dead after one undo. Everything that
/// re-applies a wire (redo, in particular) re-resolves the slot through
/// `Graph::first_free_input` — see
/// [`ConnectToFreeInputCmd`](atomartist_lib::graph::undo_commands::ConnectToFreeInputCmd).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoWirePlan {
    pub from: NodeId,
    pub from_socket: SocketUid,
    pub output: NodeId,
}

impl AutoWirePlan {
    /// The noodle this plan makes *right now*, resolving the Output's
    /// free slot against the graph as it currently stands.
    pub fn noodle(&self, graph: &Graph) -> Option<Noodle> {
        let to = graph.first_free_input(self.output)?;
        Some(Noodle::new(self.from, self.from_socket, self.output, to))
    }
}

/// Apply [`plan_auto_connect`] straight to the graph, with no undo
/// entry. Returns whether a connection was made. ND's
/// `autoConnectToOutput`.
pub fn auto_connect_to_output(graph: &mut Graph, registry: &NodeRegistry, node: NodeId) -> bool {
    let Some(noodle) = plan_auto_connect(graph, node).and_then(|p| p.noodle(graph)) else {
        return false;
    };
    graph.connect(noodle, registry).is_ok()
}

/// Every node of `graph` as a rendered box — positions from the graph,
/// sizes from `agg_gui_node_editor`'s own layout routine.
///
/// The sizes are the ones the canvas would paint **for an expanded
/// node**: whether a node is collapsed is widget state (`NodeEditor`
/// owns it), not model state, so it cannot be read from here. A
/// collapsed neighbour is therefore treated as its full height, which
/// over-reserves space rather than placing a node on top of one — the
/// safe direction to be wrong in.
///
/// Takes an explicit graph because the target of an insertion is not
/// always the active one (the mesh import inserts into the root graph
/// even while the user is drilled in).
pub fn node_boxes(graph: &Graph, registry: &NodeRegistry) -> Vec<NodeBox> {
    // Sizing only — a badge lives inside the title bar and does not
    // change a node's box, so empty error / warning maps are honest here.
    crate::app_state_model::node_views(
        graph,
        registry,
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
    )
    .iter()
    .map(|view| {
        let info = layout_node(view);
        NodeBox::new(NodeId(view.id.0), info.top_left, info.size)
    })
    .collect()
}

/// [`position_for_insertion`] over boxes the caller already projected —
/// the seam that lets a caller size the new node and place it from a
/// single projection pass.
fn position_from_boxes(
    boxes: &[NodeBox],
    output: Option<NodeId>,
    new_size: [f64; 2],
    exclude: Option<NodeId>,
    fallback_center: [f64; 2],
) -> [f64; 2] {
    // No Output (or no box for it): centre on the viewport, ND's
    // fallback. Y-up — the top-left sits *above* the centre.
    let centered = [
        fallback_center[0] - new_size[0] * 0.5,
        fallback_center[1] + new_size[1] * 0.5,
    ];
    let Some(output_id) = output else {
        return centered;
    };
    let Some(output_box) = boxes.iter().copied().find(|b| b.id == output_id) else {
        return centered;
    };
    let others: Vec<NodeBox> = boxes
        .iter()
        .copied()
        .filter(|b| b.id != output_id && Some(b.id) != exclude)
        .collect();
    let pos = find_position_left_of_output(&others, output_box, new_size, pile_up_offset());
    advance_pile_up_offset();
    pos
}

/// Canvas-space top-left for a node of `new_size` being inserted into
/// `graph` without a user-chosen position: left of that graph's Output
/// node when it has one, otherwise centred on `fallback_center` (the
/// caller's node-canvas viewport centre, already mapped through pan and
/// zoom).
///
/// `exclude` is the node being placed, when it is already in the graph —
/// it must not block its own placement.
///
/// Advances the pile-up fan-out on the Output-relative path, exactly
/// where ND's `positionNodeForInsertion` does.
pub fn position_for_insertion(
    graph: &Graph,
    registry: &NodeRegistry,
    new_size: [f64; 2],
    exclude: Option<NodeId>,
    fallback_center: [f64; 2],
) -> [f64; 2] {
    let boxes = node_boxes(graph, registry);
    position_from_boxes(
        &boxes,
        find_output_node(graph),
        new_size,
        exclude,
        fallback_center,
    )
}

/// Place a node that is **already in `graph`** at its insertion position
/// and write it back. Uses the node's own rendered size, so a wide node
/// gets the gap it needs. Returns the position applied.
///
/// One projection pass serves both the size lookup and the occupancy
/// test.
pub fn place_inserted_node(
    graph: &mut Graph,
    registry: &NodeRegistry,
    id: NodeId,
    fallback_center: [f64; 2],
) -> [f64; 2] {
    let boxes = node_boxes(graph, registry);
    let size = boxes
        .iter()
        .find(|b| b.id == id)
        .map(|b| b.size)
        .unwrap_or(DEFAULT_NODE_SIZE);
    let pos = position_from_boxes(
        &boxes,
        find_output_node(graph),
        size,
        Some(id),
        fallback_center,
    );
    let _ = graph.set_position(id, pos);
    pos
}

#[cfg(test)]
#[path = "node_insertion_tests.rs"]
mod tests;
