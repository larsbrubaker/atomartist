//! Unit tests for the shared insertion helper ([`crate::node_insertion`],
//! design step 6f-4).
//!
//! Two halves, tested independently:
//!
//! * **Placement** — pure arithmetic over [`NodeBox`]es, so every
//!   NodeDesigner constant (gap 50, step 30, inflate 20, scan ≤ 300,
//!   wrap 150) is pinned without a graph, a registry or a frame. The
//!   Y-up flips against `node-insertion.js` get their own assertions:
//!   "up on screen first" is `+offset` here.
//! * **Auto-wiring** — over a real [`AppState`] graph, because the
//!   Output node's dynamic-input behaviour (trailing empty slot) is what
//!   "first free input" actually means in AtomArtist.

use super::*;

use crate::top_level::{fresh_state_with_builtins, fresh_state_with_starter_graph};

/// Output node of the fixtures: 200 × 300 with its top-left at (700, 240).
fn output_box() -> NodeBox {
    NodeBox::new(NodeId(u64::MAX), [700.0, 240.0], [200.0, 300.0])
}

const NEW_SIZE: [f64; 2] = [200.0, 100.0];

/// Target the ancestor's formula produces for the fixtures above.
fn target() -> [f64; 2] {
    [
        700.0 - NEW_SIZE[0] - NODE_HORIZONTAL_GAP,
        240.0 - (300.0 - NEW_SIZE[1]) * 0.5,
    ]
}

// ── Placement ───────────────────────────────────────────────────────────

/// Empty area: exactly the target — a gap of 50 to the Output's left
/// edge, and the two boxes share a centre line.
#[test]
fn an_empty_area_places_the_node_at_the_exact_target() {
    let out = output_box();
    let pos = find_position_left_of_output(&[], out, NEW_SIZE, 0.0);
    assert_eq!(pos, target());
    assert_eq!(
        pos[0] + NEW_SIZE[0] + NODE_HORIZONTAL_GAP,
        out.top_left[0],
        "the gap is measured between the facing edges"
    );
    // Centre lines agree (Y-up: the body hangs below its top-left).
    let new_center = pos[1] - NEW_SIZE[1] * 0.5;
    let out_center = out.top_left[1] - out.size[1] * 0.5;
    assert!((new_center - out_center).abs() < 1e-9);
}

/// A blocker on the target pushes the node to the first free 30-step
/// slot — and the *upward on screen* one first, which is `+offset` in
/// our bottom-up canvas (ND's `targetY - offset`).
#[test]
fn an_occupied_target_takes_the_first_free_slot_upward_on_screen() {
    let t = target();
    let blocker = NodeBox::new(NodeId(1), t, NEW_SIZE);
    let pos = find_position_left_of_output(&[blocker], output_box(), NEW_SIZE, 0.0);

    // Clearing an inflated 100-tall blocker with a 100-tall node needs
    // 120 px, i.e. the fourth 30-px step — free in both directions, so
    // the tie is decided by the up-first rule.
    assert_eq!(pos, [t[0], t[1] + 120.0]);
    assert!(
        pos[1] > t[1],
        "the scan must try the slot that looks upward on screen first"
    );
}

/// The scan really is a 30-px ladder: a blocker that only just covers
/// the target is dodged by exactly one step.
#[test]
fn the_free_slot_scan_steps_in_thirty_pixel_increments() {
    let t = target();
    // A thin blocker just low enough that one step clears it: it spans
    // t-93 ..= t-91, i.e. t-113 ..= t-71 inflated, while the candidate
    // at +30 spans t-70 ..= t+30. The candidate at the target (t-100 ..=
    // t) still overlaps it, so the scan must run exactly one step.
    let blocker = NodeBox::new(NodeId(1), [t[0], t[1] - 91.0], [200.0, 2.0]);
    let pos = find_position_left_of_output(&[blocker], output_box(), NEW_SIZE, 0.0);
    assert_eq!(pos, [t[0], t[1] + NODE_OFFSET_STEP]);
}

/// Every slot within ±300 occupied: the pile-up fallback, stacking
/// *downward on screen* by the module's advancing offset (ND stacks
/// downward too — `targetY + nodeAddOffset.y` in its top-down space).
#[test]
fn a_fully_blocked_column_falls_back_to_the_pile_up_offset() {
    let t = target();
    // One wall, far taller than the ±300 scan range.
    let wall = NodeBox::new(NodeId(1), [t[0], t[1] + 1000.0], [200.0, 2000.0]);
    let pos = find_position_left_of_output(&[wall], output_box(), NEW_SIZE, 60.0);
    assert_eq!(pos, [t[0], t[1] - 60.0]);
    // …and with no fan-out yet it lands right on the target.
    let pos0 = find_position_left_of_output(&[wall], output_box(), NEW_SIZE, 0.0);
    assert_eq!(pos0, t);
}

/// The fan-out advances 30 at a time and wraps past 150, so repeated
/// give-up placements spread out instead of piling on one point.
#[test]
fn the_pile_up_offset_advances_and_wraps() {
    reset_pile_up_offset();
    assert_eq!(pile_up_offset(), 0.0);
    let seen: Vec<f64> = (0..6)
        .map(|_| {
            advance_pile_up_offset();
            pile_up_offset()
        })
        .collect();
    assert_eq!(seen, vec![30.0, 60.0, 90.0, 120.0, 150.0, 0.0]);
    assert!(
        seen.iter().all(|v| *v <= PILE_UP_WRAP),
        "the offset never marches past the wrap point"
    );
    reset_pile_up_offset();
}

/// The occupancy test inflates its neighbours by 20 px — a node that
/// would *touch* is still "occupied", one 20 px clear is not.
#[test]
fn occupancy_inflates_neighbours_by_twenty_pixels() {
    let probe = |gap: f64| {
        let other = NodeBox::new(NodeId(1), [0.0, 0.0], [100.0, 100.0]);
        overlaps([100.0 + gap, 0.0], [100.0, 100.0], &other)
    };
    assert!(probe(0.0), "flush against a neighbour is occupied");
    assert!(probe(19.0), "inside the inflation is occupied");
    assert!(!probe(OCCUPANCY_INFLATE), "exactly clear is free");
}

// ── Placement against a live AppState ───────────────────────────────────

/// With the starter graph's Output present, a fresh insert lands to its
/// left at the helper's target — using the node's own *rendered* size.
#[test]
fn position_for_insertion_lands_left_of_the_starter_graphs_output() {
    reset_pile_up_offset();
    let state = fresh_state_with_starter_graph();
    let g = state.graph.lock().unwrap();
    let boxes = node_boxes(&g, &state.registry);
    let output_id = find_output_node(&g).expect("the starter graph has an Output");
    let out = boxes
        .iter()
        .copied()
        .find(|b| b.id == output_id)
        .expect("the Output node has a rendered box");

    let pos = position_for_insertion(&g, &state.registry, NEW_SIZE, None, [0.0, 0.0]);
    assert!(
        pos[0] < out.top_left[0],
        "an inserted node goes to the Output's left, got {pos:?}"
    );
    assert_eq!(pos[0], out.top_left[0] - NEW_SIZE[0] - NODE_HORIZONTAL_GAP);
    reset_pile_up_offset();
}

/// No Output node at all: the node centres on the caller's canvas
/// viewport centre (which the caller has already mapped through pan and
/// zoom). Y-up — the top-left sits above the centre.
#[test]
fn without_an_output_node_the_insert_centers_on_the_viewport() {
    let state = fresh_state_with_builtins();
    let g = state.graph.lock().unwrap();
    assert_eq!(g.node_count(), 0);
    let center = [320.0, 180.0];
    let pos = position_for_insertion(&g, &state.registry, NEW_SIZE, None, center);
    assert_eq!(pos, [320.0 - 100.0, 180.0 + 50.0]);
    // The node's own centre is the point we were given.
    assert_eq!(pos[0] + NEW_SIZE[0] * 0.5, center[0]);
    assert_eq!(pos[1] - NEW_SIZE[1] * 0.5, center[1]);
}

/// `place_inserted_node` writes the position back and never lets the
/// node block its own placement.
#[test]
fn place_inserted_node_writes_the_position_back() {
    reset_pile_up_offset();
    let state = fresh_state_with_starter_graph();
    let mut g = state.graph.lock().unwrap();
    let id = g
        .add_new_node("Box", [0.0, 0.0], &state.registry)
        .expect("Box is a built-in");
    let pos = place_inserted_node(&mut g, &state.registry, id, [0.0, 0.0]);
    assert_eq!(g.get(id).unwrap().position, pos);
    let output_id = find_output_node(&g).unwrap();
    let out = node_boxes(&g, &state.registry)
        .into_iter()
        .find(|b| b.id == output_id)
        .unwrap();
    assert!(pos[0] < out.top_left[0]);
    reset_pile_up_offset();
}

// ── Auto-wiring ─────────────────────────────────────────────────────────

/// The deliverable: a geometry node's first geometry output wires into
/// the Output node's first free input.
#[test]
fn auto_connect_wires_a_geometry_node_into_the_output() {
    let state = fresh_state_with_starter_graph();
    let mut g = state.graph.lock().unwrap();
    let id = g
        .add_new_node("Box", [0.0, 0.0], &state.registry)
        .expect("Box is a built-in");
    let before = g.noodle_count();
    assert!(auto_connect_to_output(&mut g, &state.registry, id));
    assert_eq!(g.noodle_count(), before + 1);

    let output_id = find_output_node(&g).unwrap();
    let wired = g
        .noodles()
        .iter()
        .any(|n| n.from.node == id && n.to.node == output_id);
    assert!(wired, "the new node feeds the Output");
}

/// A node with no geometry output is inserted unwired — silently, the
/// way the ancestor does it (`findGeometryOutputSocket` returns -1).
#[test]
fn a_node_without_a_geometry_output_is_not_wired() {
    let state = fresh_state_with_starter_graph();
    let mut g = state.graph.lock().unwrap();
    // Rectangle outputs a Path2d, not a Geometry3d.
    let id = g
        .add_new_node("Rectangle", [0.0, 0.0], &state.registry)
        .expect("Rectangle is a built-in");
    let before = g.noodle_count();
    assert!(plan_auto_connect(&g, id).is_none());
    assert!(!auto_connect_to_output(&mut g, &state.registry, id));
    assert_eq!(g.noodle_count(), before, "and nothing was wired anyway");
}

/// An Output with no free input leaves the node unwired — the insert
/// itself still stands.
#[test]
fn an_output_with_no_free_input_leaves_the_node_unwired() {
    let state = fresh_state_with_starter_graph();
    let mut g = state.graph.lock().unwrap();
    let output_id = find_output_node(&g).unwrap();
    // The real Output always regrows a trailing empty slot, so fill it
    // by hand: drop every input that has no incoming noodle.
    let connected: Vec<_> = g
        .noodles()
        .iter()
        .filter(|n| n.to.node == output_id)
        .map(|n| n.to.socket)
        .collect();
    if let Some(node) = g.get_mut(output_id) {
        node.inputs.retain(|s| connected.contains(&s.uid));
    }
    assert!(g.first_free_input(output_id).is_none());

    let id = g
        .add_new_node("Box", [0.0, 0.0], &state.registry)
        .expect("Box is a built-in");
    let before = g.noodle_count();
    assert!(!auto_connect_to_output(&mut g, &state.registry, id));
    assert_eq!(g.noodle_count(), before);
    assert!(g.get(id).is_some(), "the node is still inserted");
}

/// No Output node: nothing to wire to, and no complaint.
#[test]
fn without_an_output_node_nothing_is_wired() {
    let state = fresh_state_with_builtins();
    let mut g = state.graph.lock().unwrap();
    let id = g
        .add_new_node("Box", [0.0, 0.0], &state.registry)
        .expect("Box is a built-in");
    assert!(find_output_node(&g).is_none());
    assert!(!auto_connect_to_output(&mut g, &state.registry, id));
    assert_eq!(g.noodle_count(), 0);
}
