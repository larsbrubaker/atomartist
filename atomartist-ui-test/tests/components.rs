//! Component / subgraph tests.
//!
//! Equivalents of the following NodeDesigner suites:
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/component-instance.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/component-utils.test.ts`
//!
//! Cross-references `atomartist-lib/tests/subgraphs.rs` (lib-level eval) —
//! these tests verify that a subgraph type registered with the same
//! mechanism is reachable from the production AppState used by the UI.

use std::sync::Arc;

use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::{Noodle, Graph};
use atomartist_lib::nodes::register_subgraph;
use atomartist_lib::registry::NodeRegistry;
use atomartist_ui_test::TestHarness;

fn connect_by_name(g: &mut Graph, from: NodeId, from_name: &str, to: NodeId, to_name: &str, reg: &NodeRegistry) {
    let from_uid = g.get(from).unwrap().output_by_name(from_name).unwrap().uid;
    let to_uid = g.get(to).unwrap().input_by_name(to_name).unwrap().uid;
    g.connect(Noodle::new(from, from_uid, to, to_uid), reg).unwrap();
}

#[test]
fn registry_in_appstate_can_register_a_passthrough_subgraph() {
    // Cross-check the path the UI's "Save selection as component"
    // workflow will eventually take: snap a sub-DAG, register it, see
    // it appear in the registry the menu reads from.
    let mut reg = NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut reg);

    let mut tpl = Graph::new();
    let gin = tpl.add_new_node("GraphInput", [0.0, 0.0], &reg).unwrap();
    let xform = tpl.add_new_node("Transform", [200.0, 0.0], &reg).unwrap();
    let gout = tpl.add_new_node("GraphOutput", [400.0, 0.0], &reg).unwrap();
    tpl.set_property(gin, "name", PortValue::StringVal(Arc::new("mesh".into()))).unwrap();
    tpl.set_property(gout, "name", PortValue::StringVal(Arc::new("out_mesh".into()))).unwrap();
    connect_by_name(&mut tpl, gin, "out", xform, "input", &reg);
    connect_by_name(&mut tpl, xform, "out", gout, "in", &reg);

    let id = register_subgraph(&mut reg, "Passthrough", "Passthrough", tpl);
    assert!(reg.get(id).is_some());
    let def = reg.get(id).unwrap();
    assert_eq!(def.category(), "Components");
    // Sockets surface through a freshly-instantiated instance — that's
    // how the engine sees a node's socket list now.
    let mut probe = Graph::new();
    let probe_id = probe.add_new_node("Passthrough", [0.0, 0.0], &reg).unwrap();
    let probe_inst = probe.get(probe_id).unwrap();
    assert_eq!(probe_inst.inputs.len(), 1);
    assert_eq!(probe_inst.outputs.len(), 1);
}

#[test]
fn graph_input_and_graph_output_node_types_exist_for_subgraph_authoring() {
    let h = TestHarness::new();
    assert!(h.state().registry.get("GraphInput").is_some());
    assert!(h.state().registry.get("GraphOutput").is_some());
}

#[test]
fn allocated_node_ids_are_unique_per_graph() {
    let h = TestHarness::new();
    let mut g = h.state().graph.lock().unwrap();
    let a = g.allocate_id();
    let b = g.allocate_id();
    let c = g.allocate_id();
    assert!(a != b && b != c && a != c);
}
