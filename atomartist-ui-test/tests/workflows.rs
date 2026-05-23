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
    let noodles_before = h.state().graph.lock().unwrap().noodles().len();
    let nodes_after = result.graph.nodes().count();
    let noodles_after = result.graph.noodles().len();
    assert_eq!(nodes_before, nodes_after);
    assert_eq!(noodles_before, noodles_after);
}

#[test]
fn save_then_load_atmr_round_trips_through_app_state() {
    // Exercises the public `AppState::save_graph_to_path` →
    // `AppState::load_graph_from_path` pipeline through the new ATMR
    // (zip) container. Confirms `current_file` is updated, the on-disk
    // file is a real zip (PK header), and the round-tripped graph
    // preserves node + edge counts.
    let h = TestHarness::with_starter_graph();
    let nodes_before = h.state().graph.lock().unwrap().nodes().count();
    let noodles_before = h.state().graph.lock().unwrap().noodles().len();

    // Unique name avoids cross-test interference when run in parallel.
    let path = std::env::temp_dir().join(format!(
        "atomartist_ui_test_{}.atmr",
        std::process::id()
    ));

    h.state()
        .save_graph_to_path(&path)
        .expect("save_graph_to_path");
    assert_eq!(
        h.state().current_file.lock().unwrap().as_deref(),
        Some(path.as_path()),
        "save should record the path on AppState.current_file",
    );

    // Quick smoke check that we wrote a real zip: every zip starts
    // with the local-file-header signature `PK\x03\x04`.
    let bytes = std::fs::read(&path).expect("read saved atmr");
    assert!(bytes.len() >= 4 && &bytes[..4] == b"PK\x03\x04", "expected zip magic");

    // Wipe the in-memory graph, then load back from disk and assert
    // the topology survived the round trip.
    h.state().new_empty_project();
    assert_eq!(h.state().graph.lock().unwrap().nodes().count(), 0);
    h.state()
        .load_graph_from_path(&path)
        .expect("load_graph_from_path");
    let nodes_after = h.state().graph.lock().unwrap().nodes().count();
    let noodles_after = h.state().graph.lock().unwrap().noodles().len();
    assert_eq!(nodes_before, nodes_after);
    assert_eq!(noodles_before, noodles_after);

    let _ = std::fs::remove_file(path);
}

#[test]
fn ui_settings_surface_current_file_as_last_project_path() {
    // The shell's AutoSave loop snapshots `AppState::ui_settings()` on
    // every paint and writes it to disk. The auto-reopen path on next
    // launch reads `last_project_path` back from there, so this is
    // the contract that lets the user resume where they left off.
    let h = TestHarness::with_starter_graph();
    // Before any save, no project path is associated.
    assert_eq!(h.state().ui_settings().last_project_path, None);

    let path = std::env::temp_dir().join(format!(
        "atomartist_ui_test_settings_{}.atmr",
        std::process::id()
    ));
    h.state()
        .save_graph_to_path(&path)
        .expect("save_graph_to_path");

    let surfaced = h.state().ui_settings().last_project_path;
    assert_eq!(surfaced.as_deref(), Some(path.as_path()));

    let _ = std::fs::remove_file(path);
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
