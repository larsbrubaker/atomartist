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

fn connect_by_name(
    g: &mut Graph,
    from: NodeId,
    from_name: &str,
    to: NodeId,
    to_name: &str,
    reg: &NodeRegistry,
) {
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

    // Template: GraphInput("mesh") → Transform → Output. The Output's
    // mirror output (adopted from Transform's "out") becomes the
    // subgraph's published port.
    let mut tpl = Graph::new();
    let gin = tpl.add_new_node("GraphInput", [0.0, 0.0], &reg).unwrap();
    let xform = tpl.add_new_node("Transform", [200.0, 0.0], &reg).unwrap();
    let out_node = tpl.add_new_node("Output", [400.0, 0.0], &reg).unwrap();
    tpl.set_property(gin, "name", PortValue::StringVal(Arc::new("mesh".into()))).unwrap();
    connect_by_name(&mut tpl, gin, "out", xform, "input", &reg);
    // Transform.out → Output's trailing empty slot.
    let xform_out = tpl.get(xform).unwrap().output_by_name("out").unwrap().uid;
    let out_empty = tpl.get(out_node).unwrap().inputs[0].uid;
    tpl.connect(Noodle::new(xform, xform_out, out_node, out_empty), &reg)
        .unwrap();

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
fn graph_input_node_type_exists_for_subgraph_authoring() {
    // Authoring a subgraph requires the GraphInput marker; subgraph
    // outputs come from the unified Output node, not a separate
    // GraphOutput.
    let h = TestHarness::new();
    assert!(h.state().registry.get("GraphInput").is_some());
    assert!(h.state().registry.get("Output").is_some());
    // The legacy GraphOutput node was removed when the unified Output
    // node took over its role.
    assert!(h.state().registry.get("GraphOutput").is_none());
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
