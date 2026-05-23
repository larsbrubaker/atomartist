//! Ported from NodeDesigner's
//! `tests/unit/matter-graph-execution-order.test.ts`.
//!
//! Covers topological-order queries on the graph:
//! [`Graph::execution_order`] and [`Graph::ancestors`].
//!
//! ## Mapping notes — JS → Rust
//!
//! - JS exposes `updateExecutionOrder()` which writes two cached arrays
//!   (`_nodes_in_order` + `_nodes_executable`) onto the graph and
//!   `computeExecutionOrder(only_onExecute, set_level)` which also
//!   stamps `_level` and `order` onto each node. The Rust engine
//!   computes order on demand via `Graph::execution_order()` and does
//!   not maintain a cache — there's nothing equivalent to mutate or
//!   read back.
//! - JS-only surface area we deliberately skip in this port:
//!   - `_nodes_executable` / `only_onExecute` filtering — every
//!     `NodeDef` implements `evaluate`; there is no "non-executable"
//!     class of node.
//!   - `set_level=true` / `node._level` — a UI-side layout hint
//!     produced by `arrange()`. Handled by the canvas, not the engine.
//!   - `node.order` property — same reasoning.
//!   - `node.priority` — JS lets the user tie-break root order via
//!     priority; Rust sorts roots by `NodeId` to keep tests
//!     deterministic. No priority field on `NodeInstance`.
//!   - The entire `MatterGraph.arrange()` test group — node auto-layout
//!     is a UI concern. The 2D node canvas (`agg-gui-node-editor`)
//!     owns positions; engine tests don't model them.
//! - Cycle handling diverges: JS returns "all nodes anyway" so a
//!   misconfigured graph still renders; Rust returns
//!   `Err(GraphError::CycleDetected)` because cycles must be a hard
//!   failure in the engine (the evaluator depends on a valid topo
//!   order). We test the Rust contract here.

#[path = "common/mod.rs"]
mod common;

use atomartist_lib::graph::graph::{Graph, GraphError, Noodle};
use atomartist_lib::SocketType;

use common::{add_input, add_output, registry};

// ============================================================================
// execution_order — replaces JS updateExecutionOrder / computeExecutionOrder
// ============================================================================

/// JS: updateExecutionOrder "updates _nodes_in_order array"
#[test]
fn execution_order_returns_all_nodes() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let order = g.execution_order().unwrap();
    assert_eq!(order.len(), 2);
    assert!(order.contains(&n1));
    assert!(order.contains(&n2));
}

/// JS: updateExecutionOrder "handles empty graph"
#[test]
fn execution_order_on_empty_graph_is_empty() {
    let g = Graph::new();
    let order = g.execution_order().unwrap();
    assert_eq!(order.len(), 0);
}

/// JS: computeExecutionOrder "places nodes with no inputSockets before
/// nodes with inputSockets"
#[test]
fn source_precedes_target_in_execution_order() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let target = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let out = add_output(&mut g, source, "out", SocketType::Number);
    let in_uid = add_input(&mut g, target, "in", SocketType::Number);
    g.connect(Noodle::new(source, out, target, in_uid), &reg).unwrap();

    let order = g.execution_order().unwrap();
    let src_idx = order.iter().position(|&id| id == source).unwrap();
    let tgt_idx = order.iter().position(|&id| id == target).unwrap();
    assert!(src_idx < tgt_idx);
}

/// JS: computeExecutionOrder "handles linear chain of nodes"
#[test]
fn execution_order_preserves_linear_chain() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n3 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n1_out = add_output(&mut g, n1, "out", SocketType::Number);
    let n2_in = add_input(&mut g, n2, "in", SocketType::Number);
    let n2_out = add_output(&mut g, n2, "out", SocketType::Number);
    let n3_in = add_input(&mut g, n3, "in", SocketType::Number);
    g.connect(Noodle::new(n1, n1_out, n2, n2_in), &reg).unwrap();
    g.connect(Noodle::new(n2, n2_out, n3, n3_in), &reg).unwrap();

    let order = g.execution_order().unwrap();
    let i1 = order.iter().position(|&id| id == n1).unwrap();
    let i2 = order.iter().position(|&id| id == n2).unwrap();
    let i3 = order.iter().position(|&id| id == n3).unwrap();
    assert!(i1 < i2);
    assert!(i2 < i3);
}

/// JS: computeExecutionOrder "handles branching graph (one output to multiple inputs)"
#[test]
fn execution_order_branching_source_first() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let t1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let t2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s_out = add_output(&mut g, source, "out", SocketType::Number);
    let t1_in = add_input(&mut g, t1, "in", SocketType::Number);
    let t2_in = add_input(&mut g, t2, "in", SocketType::Number);
    g.connect(Noodle::new(source, s_out, t1, t1_in), &reg).unwrap();
    g.connect(Noodle::new(source, s_out, t2, t2_in), &reg).unwrap();

    let order = g.execution_order().unwrap();
    let src_idx = order.iter().position(|&id| id == source).unwrap();
    let t1_idx = order.iter().position(|&id| id == t1).unwrap();
    let t2_idx = order.iter().position(|&id| id == t2).unwrap();
    assert!(src_idx < t1_idx);
    assert!(src_idx < t2_idx);
}

/// JS: computeExecutionOrder "handles merging graph (multiple outputs to one input)"
#[test]
fn execution_order_merging_both_sources_first() {
    let reg = registry();
    let mut g = Graph::new();
    let s1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let target = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s1_out = add_output(&mut g, s1, "out", SocketType::Number);
    let s2_out = add_output(&mut g, s2, "out", SocketType::Number);
    let t_in1 = add_input(&mut g, target, "in1", SocketType::Number);
    let t_in2 = add_input(&mut g, target, "in2", SocketType::Number);
    g.connect(Noodle::new(s1, s1_out, target, t_in1), &reg).unwrap();
    g.connect(Noodle::new(s2, s2_out, target, t_in2), &reg).unwrap();

    let order = g.execution_order().unwrap();
    let s1_idx = order.iter().position(|&id| id == s1).unwrap();
    let s2_idx = order.iter().position(|&id| id == s2).unwrap();
    let t_idx = order.iter().position(|&id| id == target).unwrap();
    assert!(s1_idx < t_idx);
    assert!(s2_idx < t_idx);
}

/// JS: computeExecutionOrder "handles disconnected nodes"
#[test]
fn execution_order_includes_disconnected_nodes() {
    let reg = registry();
    let mut g = Graph::new();
    let c1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let c2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let isolated = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let c1_out = add_output(&mut g, c1, "out", SocketType::Number);
    let c2_in = add_input(&mut g, c2, "in", SocketType::Number);
    g.connect(Noodle::new(c1, c1_out, c2, c2_in), &reg).unwrap();

    let order = g.execution_order().unwrap();
    assert_eq!(order.len(), 3);
    assert!(order.contains(&isolated));
}

/// JS: computeExecutionOrder "handles cycles gracefully" — JS returns all
/// nodes anyway. atomartist refuses to produce an order for a cyclic
/// graph (cycle is a hard engine error). Documented divergence; we test
/// the Rust contract.
///
/// Note: `Graph::connect` itself rejects the second edge with
/// `CycleDetected`, so we don't even reach a cyclic state through the
/// public API — the cycle test in JS is testing recovery from an
/// invalid graph state that Rust prevents from existing.
#[test]
fn cycle_is_rejected_at_connect_time() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n1_in = add_input(&mut g, n1, "in", SocketType::Number);
    let n1_out = add_output(&mut g, n1, "out", SocketType::Number);
    let n2_in = add_input(&mut g, n2, "in", SocketType::Number);
    let n2_out = add_output(&mut g, n2, "out", SocketType::Number);
    g.connect(Noodle::new(n1, n1_out, n2, n2_in), &reg).unwrap();
    // The cycle-closing edge is rejected; the graph stays acyclic.
    assert_eq!(
        g.connect(Noodle::new(n2, n2_out, n1, n1_in), &reg),
        Err(GraphError::CycleDetected),
    );
    // Execution order is well-defined on the acyclic remainder.
    let order = g.execution_order().unwrap();
    assert_eq!(order.len(), 2);
    let i1 = order.iter().position(|&id| id == n1).unwrap();
    let i2 = order.iter().position(|&id| id == n2).unwrap();
    assert!(i1 < i2);
}

// ============================================================================
// ancestors — replaces JS getAncestors
// ============================================================================

/// JS: getAncestors "returns empty array for node with no inputs"
#[test]
fn ancestors_empty_when_no_incoming_noodles() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    assert_eq!(g.ancestors(n).len(), 0);
}

/// JS: getAncestors "returns direct input node"
#[test]
fn ancestors_returns_direct_predecessor() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let target = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s_out = add_output(&mut g, source, "out", SocketType::Number);
    let t_in = add_input(&mut g, target, "in", SocketType::Number);
    g.connect(Noodle::new(source, s_out, target, t_in), &reg).unwrap();

    let ancestors = g.ancestors(target);
    assert_eq!(ancestors, vec![source]);
}

/// JS: getAncestors "returns all upstream nodes in chain"
#[test]
fn ancestors_walks_full_chain() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n3 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n1_out = add_output(&mut g, n1, "out", SocketType::Number);
    let n2_in = add_input(&mut g, n2, "in", SocketType::Number);
    let n2_out = add_output(&mut g, n2, "out", SocketType::Number);
    let n3_in = add_input(&mut g, n3, "in", SocketType::Number);
    g.connect(Noodle::new(n1, n1_out, n2, n2_in), &reg).unwrap();
    g.connect(Noodle::new(n2, n2_out, n3, n3_in), &reg).unwrap();

    let ancestors = g.ancestors(n3);
    assert_eq!(ancestors.len(), 2);
    assert!(ancestors.contains(&n1));
    assert!(ancestors.contains(&n2));
}

/// JS: getAncestors "returns ancestors from multiple input branches"
#[test]
fn ancestors_includes_all_branches() {
    let reg = registry();
    let mut g = Graph::new();
    let s1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let target = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s1_out = add_output(&mut g, s1, "out", SocketType::Number);
    let s2_out = add_output(&mut g, s2, "out", SocketType::Number);
    let t_in1 = add_input(&mut g, target, "in1", SocketType::Number);
    let t_in2 = add_input(&mut g, target, "in2", SocketType::Number);
    g.connect(Noodle::new(s1, s1_out, target, t_in1), &reg).unwrap();
    g.connect(Noodle::new(s2, s2_out, target, t_in2), &reg).unwrap();

    let ancestors = g.ancestors(target);
    assert_eq!(ancestors.len(), 2);
    assert!(ancestors.contains(&s1));
    assert!(ancestors.contains(&s2));
}

/// JS: getAncestors "does not include the node itself"
#[test]
fn ancestors_excludes_self() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let target = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s_out = add_output(&mut g, source, "out", SocketType::Number);
    let t_in = add_input(&mut g, target, "in", SocketType::Number);
    g.connect(Noodle::new(source, s_out, target, t_in), &reg).unwrap();

    assert!(!g.ancestors(target).contains(&target));
}

/// JS: getAncestors "does not include downstream nodes"
#[test]
fn ancestors_excludes_downstream() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n3 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n1_out = add_output(&mut g, n1, "out", SocketType::Number);
    let n2_in = add_input(&mut g, n2, "in", SocketType::Number);
    let n2_out = add_output(&mut g, n2, "out", SocketType::Number);
    let n3_in = add_input(&mut g, n3, "in", SocketType::Number);
    g.connect(Noodle::new(n1, n1_out, n2, n2_in), &reg).unwrap();
    g.connect(Noodle::new(n2, n2_out, n3, n3_in), &reg).unwrap();

    let ancestors = g.ancestors(n2);
    assert_eq!(ancestors, vec![n1]);
    assert!(!ancestors.contains(&n3));
}

/// JS: getAncestors "ancestors are sorted by execution order"
#[test]
fn ancestors_returned_in_execution_order() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n3 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n1_out = add_output(&mut g, n1, "out", SocketType::Number);
    let n2_in = add_input(&mut g, n2, "in", SocketType::Number);
    let n2_out = add_output(&mut g, n2, "out", SocketType::Number);
    let n3_in = add_input(&mut g, n3, "in", SocketType::Number);
    g.connect(Noodle::new(n1, n1_out, n2, n2_in), &reg).unwrap();
    g.connect(Noodle::new(n2, n2_out, n3, n3_in), &reg).unwrap();

    // Direct upstream pred is n2, transitive is n1. Topological order
    // places n1 before n2.
    let ancestors = g.ancestors(n3);
    assert_eq!(ancestors, vec![n1, n2]);
}

/// JS: getAncestors "handles diamond dependency pattern"
///
/// ```text
///     A
///    / \
///   B   C
///    \ /
///     D
/// ```
#[test]
fn ancestors_diamond_pattern() {
    let reg = registry();
    let mut g = Graph::new();
    let na = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let nb = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let nc = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let nd = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let a_out = add_output(&mut g, na, "out", SocketType::Number);
    let b_in = add_input(&mut g, nb, "in", SocketType::Number);
    let b_out = add_output(&mut g, nb, "out", SocketType::Number);
    let c_in = add_input(&mut g, nc, "in", SocketType::Number);
    let c_out = add_output(&mut g, nc, "out", SocketType::Number);
    let d_in1 = add_input(&mut g, nd, "in1", SocketType::Number);
    let d_in2 = add_input(&mut g, nd, "in2", SocketType::Number);
    g.connect(Noodle::new(na, a_out, nb, b_in), &reg).unwrap();
    g.connect(Noodle::new(na, a_out, nc, c_in), &reg).unwrap();
    g.connect(Noodle::new(nb, b_out, nd, d_in1), &reg).unwrap();
    g.connect(Noodle::new(nc, c_out, nd, d_in2), &reg).unwrap();

    let ancestors = g.ancestors(nd);
    assert_eq!(ancestors.len(), 3);
    assert!(ancestors.contains(&na));
    assert!(ancestors.contains(&nb));
    assert!(ancestors.contains(&nc));
    // A must precede both B and C in the returned list.
    let ia = ancestors.iter().position(|&id| id == na).unwrap();
    let ib = ancestors.iter().position(|&id| id == nb).unwrap();
    let ic = ancestors.iter().position(|&id| id == nc).unwrap();
    assert!(ia < ib);
    assert!(ia < ic);
}

/// Returning empty for a non-existent node id keeps callers from having
/// to special-case it. Not from JS — JS would crash dereferencing a
/// missing node.
#[test]
fn ancestors_of_missing_node_is_empty() {
    let g = Graph::new();
    // A node id from a different graph would never resolve.
    let bogus = atomartist_lib::graph::node::NodeId(9_999);
    assert_eq!(g.ancestors(bogus).len(), 0);
}

// ============================================================================
// Integration — execution order reacts to graph mutations
// ============================================================================

/// JS: "execution order updates when nodes are connected"
#[test]
fn execution_order_changes_after_connect() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let target = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let s_out = add_output(&mut g, source, "out", SocketType::Number);
    let t_in = add_input(&mut g, target, "in", SocketType::Number);

    // Pre-connect: order is just NodeId-stable for the two roots.
    let order_before = g.execution_order().unwrap();
    assert_eq!(order_before.len(), 2);

    g.connect(Noodle::new(source, s_out, target, t_in), &reg).unwrap();

    let order_after = g.execution_order().unwrap();
    let src_idx = order_after.iter().position(|&id| id == source).unwrap();
    let tgt_idx = order_after.iter().position(|&id| id == target).unwrap();
    assert!(src_idx < tgt_idx);
}

/// JS: "execution order updates when nodes are removed"
#[test]
fn execution_order_changes_after_remove_node() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n3 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n1_out = add_output(&mut g, n1, "out", SocketType::Number);
    let n2_in = add_input(&mut g, n2, "in", SocketType::Number);
    let n2_out = add_output(&mut g, n2, "out", SocketType::Number);
    let n3_in = add_input(&mut g, n3, "in", SocketType::Number);
    g.connect(Noodle::new(n1, n1_out, n2, n2_in), &reg).unwrap();
    g.connect(Noodle::new(n2, n2_out, n3, n3_in), &reg).unwrap();

    g.remove_node(n2).unwrap();

    let order = g.execution_order().unwrap();
    assert_eq!(order.len(), 2);
    assert!(order.contains(&n1));
    assert!(order.contains(&n3));
    assert!(!order.contains(&n2));
}
