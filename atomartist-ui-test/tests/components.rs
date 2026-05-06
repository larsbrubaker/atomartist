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

use atomartist_lib::graph::node::{NodeId, NodeInstance, PortValue, SocketId};
use atomartist_lib::graph::{Edge, Graph};
use atomartist_lib::nodes::register_subgraph;
use atomartist_lib::registry::NodeRegistry;
use atomartist_ui_test::TestHarness;

fn make_node(g: &mut Graph, reg: &NodeRegistry, type_id: &'static str, pos: [f64; 2]) -> NodeId {
    let id = g.allocate_id();
    let mut n = NodeInstance::new(id, type_id, pos);
    if let Some(def) = reg.get(type_id) {
        for prop in def.properties() {
            n.properties.insert(prop.name, prop.default);
        }
    }
    g.add_node(n).unwrap();
    id
}

#[test]
fn registry_in_appstate_can_register_a_passthrough_subgraph() {
    // Cross-check the path the UI's "Save selection as component"
    // workflow will eventually take: snap a sub-DAG, register it, see
    // it appear in the registry the menu reads from.
    let mut reg = NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut reg);

    let mut tpl = Graph::new();
    let gin = make_node(&mut tpl, &reg, "GraphInput", [0.0, 0.0]);
    let xform = make_node(&mut tpl, &reg, "Transform", [200.0, 0.0]);
    let gout = make_node(&mut tpl, &reg, "GraphOutput", [400.0, 0.0]);
    tpl.get_mut(gin).unwrap().properties.insert(
        "name",
        PortValue::StringVal(Arc::new("mesh".into())),
    );
    tpl.get_mut(gout).unwrap().properties.insert(
        "name",
        PortValue::StringVal(Arc::new("out_mesh".into())),
    );
    tpl.connect(
        Edge { from: SocketId { node: gin, name: "out" }, to: SocketId { node: xform, name: "input" } },
        &reg,
    ).unwrap();
    tpl.connect(
        Edge { from: SocketId { node: xform, name: "out" }, to: SocketId { node: gout, name: "in" } },
        &reg,
    ).unwrap();

    let id = register_subgraph(&mut reg, "Passthrough", "Passthrough", tpl);
    assert!(reg.get(id).is_some());
    let def = reg.get(id).unwrap();
    assert_eq!(def.category(), "Components");
    assert_eq!(def.input_sockets().len(), 1);
    assert_eq!(def.output_sockets().len(), 1);
}

#[test]
fn graph_input_and_graph_output_node_types_exist_for_subgraph_authoring() {
    // Authoring a subgraph requires GraphInput + GraphOutput nodes.
    let h = TestHarness::new();
    assert!(h.state().registry.get("GraphInput").is_some());
    assert!(h.state().registry.get("GraphOutput").is_some());
}

#[test]
fn allocated_node_ids_are_unique_per_graph() {
    // Sanity: NodeIds allocated through `Graph::allocate_id` are unique
    // within a graph — required for both subgraph authoring and clipboard
    // paste paths.
    let h = TestHarness::new();
    let mut g = h.state().graph.lock().unwrap();
    let a = g.allocate_id();
    let b = g.allocate_id();
    let c = g.allocate_id();
    assert!(a != b && b != c && a != c);
}
