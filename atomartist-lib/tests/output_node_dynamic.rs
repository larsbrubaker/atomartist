//! Ported from NodeDesigner's `tests/unit/graph-io-nodes.test.ts`.
//!
//! Covers the dynamic-input behavior of atomartist's `Output` node — the
//! direct Rust analogue of JS's `graph/output` type. JS tests for
//! `graph/input` and the entire `graph-io-slot-rearrangement.test.ts`
//! file are NOT ported because atomartist intentionally diverges on the
//! input side (see "Diverges from JS" below).
//!
//! ## Diverges from JS — intentional architectural choices
//!
//! - **No dynamic GraphInput node**. atomartist's `GraphInput`
//!   (`nodes/io/graph_input_node.rs`) is a static single-output node
//!   whose subgraph port name comes from a `name` property. JS's
//!   `graph/input` instead built a Blender-style dynamic-output node
//!   that adopted target-side names by connecting outward. We chose the
//!   property-driven design because it survives serialization cleanly
//!   and doesn't require the engine to fan one output into many noodles
//!   while keeping a per-target label.
//! - **No "one output, many fan-out noodles"**. JS lets a single
//!   GraphInput slot drive multiple targets and updates its label as
//!   targets connect / disconnect. atomartist's executor and serializer
//!   both treat noodles as a flat `Vec<Noodle>`; one output can drive
//!   many inputs, but the source-side label is the source-socket name,
//!   period — no relabel-on-fan-out machinery.
//!
//! As a result, the JS file `graph-io-slot-rearrangement.test.ts` is
//! entirely N/A here (every scenario it covers depends on the
//! relabel-on-fan-out feature that doesn't exist in our model).
//!
//! ## Other JS-only surface area we deliberately skip
//!
//! - `properties.outputSockets` / `properties.inputSockets`
//!   serialization — atomartist saves sockets directly on the node
//!   instance in `graph_json.rs`, not into a magic "properties" bag.
//! - `setDirtyCanvas` / `widgets` arrays — UI-layer concerns. The
//!   `agg-gui-node-editor` canvas recomputes layout from the live
//!   socket list every frame ("widget-system migration" in JS terms);
//!   there is no per-node dirty bit to assert.
//! - `graph.inputSockets["Width"]` / `graph.outputSockets["Geometry"]`
//!   subgraph-port registries — exposed in atomartist via the
//!   `SubgraphNodeDef` and tested in `tests/subgraphs.rs`. Not retested
//!   here.

#[path = "common/mod.rs"]
mod common;

use atomartist_lib::graph::graph::{Graph, Noodle};
use atomartist_lib::SocketType;

use common::{registry, BareNode};

const OUTPUT_TYPE: &str = "Output";
const MULTI: &str = "test::MultiOutputSource";

/// Name of the synthetic output that the Output node always mints for
/// the 3D viewport's "first Geometry3d cached output" heuristic. Tests
/// filter it out when counting "user-facing" output mirrors.
const DISPLAY_OUTPUT: &str = "__display__";

fn user_output_count(g: &Graph, node: atomartist_lib::graph::node::NodeId) -> usize {
    g.get(node)
        .unwrap()
        .outputs
        .iter()
        .filter(|s| s.name.as_ref() != DISPLAY_OUTPUT)
        .count()
}

fn configured_input_count(g: &Graph, node: atomartist_lib::graph::node::NodeId) -> usize {
    g.get(node)
        .unwrap()
        .inputs
        .iter()
        .filter(|s| !s.name.as_ref().is_empty())
        .count()
}

// ============================================================================
// Initialization
// ============================================================================

/// JS: GraphOutput "starts with one empty input slot"
#[test]
fn output_node_starts_with_one_empty_input_slot() {
    let reg = registry();
    let mut g = Graph::new();
    let o = g.add_new_node(OUTPUT_TYPE, [0.0, 0.0], &reg).unwrap();

    let n = g.get(o).unwrap();
    assert_eq!(n.inputs.len(), 1);
    assert_eq!(n.inputs[0].name.as_ref(), "");
    assert_eq!(n.inputs[0].socket_type, SocketType::None);
    assert!(n.inputs[0].optional);
}

/// JS: GraphOutput "has correct title". atomartist surfaces this via
/// `NodeDef::display_name`.
#[test]
fn output_node_display_name_is_output() {
    use atomartist_lib::nodes;
    let mut reg = atomartist_lib::registry::NodeRegistry::new();
    nodes::register_all(&mut reg);
    let def = reg.get(OUTPUT_TYPE).unwrap();
    assert_eq!(def.display_name(), "Output");
}

// ============================================================================
// Connection behavior
// ============================================================================

/// JS: GraphOutput "configures slot when connected from source node"
#[test]
fn connecting_a_source_configures_the_input_slot() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node(MULTI, [0.0, 0.0], &reg).unwrap();
    let out = g.add_new_node(OUTPUT_TYPE, [200.0, 0.0], &reg).unwrap();

    let src_geom = g.get(source).unwrap().output_by_name("Geometry").unwrap().uid;
    let empty_slot = g.get(out).unwrap().inputs[0].uid;
    g.connect(Noodle::new(source, src_geom, out, empty_slot), &reg).unwrap();

    let o = g.get(out).unwrap();
    // The placeholder slot is now configured: name == source's socket name,
    // type adopted, display_label formatted as "{SourceType} - {SocketName}".
    let configured = o.input_by_uid(empty_slot).unwrap();
    assert_eq!(configured.name.as_ref(), "Geometry");
    assert_eq!(configured.socket_type, SocketType::Geometry3d);
    assert_eq!(
        configured.display_label.as_deref().map(|a| a.as_ref()),
        Some(format!("{} - Geometry", MULTI).as_str()),
    );

    // A fresh empty trailing slot must be present.
    assert_eq!(o.inputs.len(), 2);
    assert_eq!(o.inputs[1].name.as_ref(), "");
    assert!(o.inputs[1].optional);

    // Mirror output of matching type was minted.
    assert!(o.output_by_name("Geometry").is_some());
    assert_eq!(user_output_count(&g, out), 1);
}

/// JS: GraphOutput multi-input — "always maintains one empty slot at end"
#[test]
fn each_connect_adds_a_new_trailing_empty_slot() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node(MULTI, [0.0, 0.0], &reg).unwrap();
    let out = g.add_new_node(OUTPUT_TYPE, [200.0, 0.0], &reg).unwrap();

    // Connect Geometry → slot 0
    let geom = g.get(source).unwrap().output_by_name("Geometry").unwrap().uid;
    let s0 = g.get(out).unwrap().inputs[0].uid;
    g.connect(Noodle::new(source, geom, out, s0), &reg).unwrap();
    assert_eq!(g.get(out).unwrap().inputs.len(), 2);
    assert_eq!(g.get(out).unwrap().inputs[1].name.as_ref(), "");

    // Connect Paths → next empty slot
    let paths = g.get(source).unwrap().output_by_name("Paths").unwrap().uid;
    let s1 = g.get(out).unwrap().inputs[1].uid;
    g.connect(Noodle::new(source, paths, out, s1), &reg).unwrap();
    assert_eq!(g.get(out).unwrap().inputs.len(), 3);
    assert_eq!(g.get(out).unwrap().inputs[2].name.as_ref(), "");

    // Connect Color → next empty slot
    let color = g.get(source).unwrap().output_by_name("Color").unwrap().uid;
    let s2 = g.get(out).unwrap().inputs[2].uid;
    g.connect(Noodle::new(source, color, out, s2), &reg).unwrap();
    assert_eq!(g.get(out).unwrap().inputs.len(), 4);
    assert_eq!(g.get(out).unwrap().inputs[3].name.as_ref(), "");

    // Each configured slot got its own mirror output.
    assert_eq!(user_output_count(&g, out), 3);
}

// ============================================================================
// Disconnection behavior — middle / first / last
// ============================================================================

/// Wire all three MultiOutputSource sockets into the Output node and
/// return `(output_node_id, slot_uids_in_order)`. Helper shared by the
/// middle/first/last disconnect tests.
fn three_slot_output(
    g: &mut Graph,
    reg: &atomartist_lib::registry::NodeRegistry,
) -> (
    atomartist_lib::graph::node::NodeId,
    Vec<atomartist_lib::graph::socket::SocketUid>,
) {
    let source = g.add_new_node(MULTI, [0.0, 0.0], reg).unwrap();
    let out = g.add_new_node(OUTPUT_TYPE, [200.0, 0.0], reg).unwrap();
    for name in ["Geometry", "Paths", "Color"] {
        let src = g.get(source).unwrap().output_by_name(name).unwrap().uid;
        // Always connect to the trailing empty slot — exactly how the UI
        // drives connections in production.
        let target = g
            .get(out)
            .unwrap()
            .inputs
            .iter()
            .find(|s| s.name.as_ref().is_empty())
            .unwrap()
            .uid;
        g.connect(Noodle::new(source, src, out, target), reg).unwrap();
    }
    let slots: Vec<_> = g
        .get(out)
        .unwrap()
        .inputs
        .iter()
        .filter(|s| !s.name.as_ref().is_empty())
        .map(|s| s.uid)
        .collect();
    (out, slots)
}

/// JS: GraphOutput "removes correct slot when middle slot is disconnected"
#[test]
fn disconnect_middle_slot_removes_that_slot_and_its_mirror() {
    let reg = registry();
    let mut g = Graph::new();
    let (out, slots) = three_slot_output(&mut g, &reg);
    assert_eq!(slots.len(), 3);

    // Disconnect "Paths" (middle).
    let noodle = *g
        .noodles()
        .iter()
        .find(|n| n.to.node == out && n.to.socket == slots[1])
        .unwrap();
    g.disconnect(&noodle, &reg).unwrap();

    let n = g.get(out).unwrap();
    let names: Vec<_> = n
        .inputs
        .iter()
        .filter(|s| !s.name.as_ref().is_empty())
        .map(|s| s.name.to_string())
        .collect();
    assert_eq!(names, vec!["Geometry", "Color"]);
    // Mirror output for "Paths" must be gone, the others survive.
    assert!(n.output_by_name("Paths").is_none());
    assert!(n.output_by_name("Geometry").is_some());
    assert!(n.output_by_name("Color").is_some());
    // Trailing empty invariant holds.
    assert_eq!(n.inputs.last().unwrap().name.as_ref(), "");
}

/// JS: GraphOutput "removes correct slot when first slot is disconnected"
#[test]
fn disconnect_first_slot_removes_only_that_slot() {
    let reg = registry();
    let mut g = Graph::new();
    let (out, slots) = three_slot_output(&mut g, &reg);

    let noodle = *g
        .noodles()
        .iter()
        .find(|n| n.to.node == out && n.to.socket == slots[0])
        .unwrap();
    g.disconnect(&noodle, &reg).unwrap();

    let n = g.get(out).unwrap();
    let names: Vec<_> = n
        .inputs
        .iter()
        .filter(|s| !s.name.as_ref().is_empty())
        .map(|s| s.name.to_string())
        .collect();
    assert_eq!(names, vec!["Paths", "Color"]);
    assert!(n.output_by_name("Geometry").is_none());
}

/// JS: GraphOutput "removes correct slot when last configured slot is disconnected"
#[test]
fn disconnect_last_slot_removes_only_that_slot() {
    let reg = registry();
    let mut g = Graph::new();
    let (out, slots) = three_slot_output(&mut g, &reg);

    let noodle = *g
        .noodles()
        .iter()
        .find(|n| n.to.node == out && n.to.socket == slots[2])
        .unwrap();
    g.disconnect(&noodle, &reg).unwrap();

    let n = g.get(out).unwrap();
    let names: Vec<_> = n
        .inputs
        .iter()
        .filter(|s| !s.name.as_ref().is_empty())
        .map(|s| s.name.to_string())
        .collect();
    assert_eq!(names, vec!["Geometry", "Paths"]);
    assert!(n.output_by_name("Color").is_none());
    assert_eq!(n.inputs.last().unwrap().name.as_ref(), "");
}

/// Down to zero configured slots: the trailing-empty invariant must
/// still leave one empty placeholder so the user can drop another
/// source on it.
#[test]
fn disconnect_all_slots_leaves_one_empty_placeholder() {
    let reg = registry();
    let mut g = Graph::new();
    let (out, _) = three_slot_output(&mut g, &reg);

    // Pull every noodle into a snapshot first to avoid mutating while
    // iterating the live list.
    let noodles: Vec<_> = g
        .noodles()
        .iter()
        .filter(|n| n.to.node == out)
        .copied()
        .collect();
    for n in noodles {
        g.disconnect(&n, &reg).unwrap();
    }

    let n = g.get(out).unwrap();
    assert_eq!(n.inputs.len(), 1);
    assert_eq!(n.inputs[0].name.as_ref(), "");
    assert_eq!(user_output_count(&g, out), 0);
}

// ============================================================================
// Duplicate-source rejection
// ============================================================================

/// JS: "GraphOutput duplicate connection prevention" — JS replaces the
/// existing noodle. atomartist instead refuses the second connect with
/// `ConnectionRejected("source already connected to this Output")`.
/// Documented divergence: refusing keeps `Graph::connect`'s contract
/// straightforward (no implicit side-effect disconnects), and the UI
/// layer is the right place to translate "user dragged a duplicate"
/// into "drop the original then make the new one."
#[test]
fn duplicate_source_to_output_is_rejected() {
    use atomartist_lib::graph::graph::GraphError;
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node(MULTI, [0.0, 0.0], &reg).unwrap();
    let out = g.add_new_node(OUTPUT_TYPE, [200.0, 0.0], &reg).unwrap();

    let src = g.get(source).unwrap().output_by_name("Geometry").unwrap().uid;
    let s0 = g.get(out).unwrap().inputs[0].uid;
    g.connect(Noodle::new(source, src, out, s0), &reg).unwrap();

    // Second attempt from the same (source_node, source_socket) lands
    // on the new trailing empty slot — and is refused by
    // `validate_input_connection`.
    let s1 = g.get(out).unwrap().inputs.last().unwrap().uid;
    let err = g
        .connect(Noodle::new(source, src, out, s1), &reg)
        .unwrap_err();
    assert!(matches!(err, GraphError::ConnectionRejected(_)));

    // The trailing empty slot is still empty after the rejection.
    let n = g.get(out).unwrap();
    assert_eq!(configured_input_count(&g, out), 1);
    assert_eq!(n.inputs.last().unwrap().name.as_ref(), "");
}

/// Connecting two *different* source sockets to the same Output node is
/// allowed (the JS counterpart "multiple slots to different inputs on
/// same target is allowed").
#[test]
fn connecting_distinct_sources_into_output_is_allowed() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node(MULTI, [0.0, 0.0], &reg).unwrap();
    let out = g.add_new_node(OUTPUT_TYPE, [200.0, 0.0], &reg).unwrap();

    let g_uid = g.get(source).unwrap().output_by_name("Geometry").unwrap().uid;
    let s0 = g.get(out).unwrap().inputs[0].uid;
    g.connect(Noodle::new(source, g_uid, out, s0), &reg).unwrap();

    let p_uid = g.get(source).unwrap().output_by_name("Paths").unwrap().uid;
    let s1 = g.get(out).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(source, p_uid, out, s1), &reg).unwrap();

    assert_eq!(configured_input_count(&g, out), 2);
}

// ============================================================================
// Name collision: two sources whose output sockets share a name
// ============================================================================

/// Two different source nodes can both expose an output named
/// `Geometry`. JS would just shrug and overwrite. atomartist's Output
/// node disambiguates by suffixing `_1`, `_2`, …. This isn't covered by
/// the JS suite but is a Rust-side invariant worth pinning so the
/// suffix logic doesn't regress.
#[test]
fn duplicate_source_name_gets_unique_suffix() {
    let reg = registry();
    let mut g = Graph::new();
    let s1 = g.add_new_node(MULTI, [0.0, 0.0], &reg).unwrap();
    let s2 = g.add_new_node(MULTI, [0.0, 0.0], &reg).unwrap();
    let out = g.add_new_node(OUTPUT_TYPE, [200.0, 0.0], &reg).unwrap();

    let geom1 = g.get(s1).unwrap().output_by_name("Geometry").unwrap().uid;
    let slot_a = g.get(out).unwrap().inputs[0].uid;
    g.connect(Noodle::new(s1, geom1, out, slot_a), &reg).unwrap();

    let geom2 = g.get(s2).unwrap().output_by_name("Geometry").unwrap().uid;
    let slot_b = g.get(out).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(s2, geom2, out, slot_b), &reg).unwrap();

    let names: Vec<_> = g
        .get(out)
        .unwrap()
        .inputs
        .iter()
        .filter(|s| !s.name.as_ref().is_empty())
        .map(|s| s.name.to_string())
        .collect();
    assert_eq!(names, vec!["Geometry", "Geometry_1"]);
    // Output mirrors share the same disambiguation.
    assert!(g.get(out).unwrap().output_by_name("Geometry").is_some());
    assert!(g.get(out).unwrap().output_by_name("Geometry_1").is_some());
}

// ============================================================================
// Silence unused-fixture warning
// ============================================================================

#[test]
fn fixture_reachable() {
    // BareNode is shared with other ports but unused here; touch it so
    // adding an import for it doesn't generate a dead-code warning.
    let _ = std::marker::PhantomData::<BareNode>;
}
