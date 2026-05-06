//! Workflow tests — multi-step UX scenarios that span the menu bar, the
//! file dialogs, and the graph state.
//!
//! Equivalents of the following NodeDesigner suites:
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/save-open-dialog.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/singleton-nodes-clipboard.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/file-menu-actions-load-example.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/node-menu-coverage.test.ts`

use atomartist_lib::serialization::{graph_from_json_str, graph_to_json_string};
use atomartist_ui_test::TestHarness;

#[test]
fn save_then_load_round_trips_starter_graph_topology() {
    // Round-tripping through JSON should preserve node count + edge count.
    let h = TestHarness::with_starter_graph();
    let json = {
        let g = h.state().graph.lock().unwrap();
        graph_to_json_string(&g)
    };
    let result = graph_from_json_str(&json, &h.state().registry).expect("parse");
    let nodes_before = h.state().graph.lock().unwrap().nodes().count();
    let edges_before = h.state().graph.lock().unwrap().edges().len();
    let nodes_after = result.graph.nodes().count();
    let edges_after = result.graph.edges().len();
    assert_eq!(nodes_before, nodes_after);
    assert_eq!(edges_before, edges_after);
}

#[test]
fn new_empty_project_clears_graph_and_selection() {
    let h = TestHarness::with_starter_graph();
    h.state().set_selection(Some(atomartist_lib::graph::node::NodeId(1)));
    assert!(h.state().graph.lock().unwrap().nodes().count() > 0);
    assert!(h.state().selection.lock().unwrap().is_some());

    h.state().new_empty_project();

    assert_eq!(h.state().graph.lock().unwrap().nodes().count(), 0);
    assert!(h.state().selection.lock().unwrap().is_none());
    assert!(h.state().last_mesh_output.lock().unwrap().is_none());
}

#[test]
fn evaluate_now_picks_display_node_when_unset() {
    // NodeDesigner's "open a graph and have it auto-display" behaviour.
    // With no display_node set, the evaluator should pick the
    // highest-id node with a Geometry3d output.
    let h = TestHarness::with_starter_graph();
    *h.state().display_node.lock().unwrap() = None;
    h.evaluate_now();
    let mesh = h.state().last_mesh_output.lock().unwrap().clone();
    assert!(mesh.is_some(), "auto-pick should select the Output node's mesh");
}

#[test]
fn registry_exposes_all_built_in_node_types() {
    // node-menu-coverage equivalent: the registry must include every
    // primitive class the user can create from the New-Node menu.
    let h = TestHarness::new();
    let reg = &h.state().registry;
    for ty in &[
        "Box", "Sphere", "Cylinder", "Cone", "Pyramid", "Wedge", "Torus",
        "Rectangle", "Circle", "Ring", "Star",
        "Extrude", "Transform", "Combine", "Boolean",
        "Inflate", "Stroke", "SmoothPaths",
        "Output", "GraphInput", "GraphOutput",
    ] {
        assert!(
            reg.get(ty).is_some(),
            "registry must register '{}' for the New-Node menu",
            ty
        );
    }
}
