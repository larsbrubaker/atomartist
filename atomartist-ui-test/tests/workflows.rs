//! Workflow tests — multi-step UX scenarios that span the menu bar, the
//! file dialogs, and the graph state.
//!
//! Equivalents of the following NodeDesigner suites:
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/save-open-dialog.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/singleton-nodes-clipboard.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/file-menu-actions-load-example.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/node-menu-coverage.test.ts`

use atomartist_lib::serialization::{graph_from_json_str, graph_to_json_string};
use atomartist_storage::StorageUri;
use atomartist_ui_test::{memory_uri, TestHarness};

/// Bytes the harness's storage provider holds at `uri`. Jobs from the
/// in-memory provider are always settled on return.
fn read_stored(h: &TestHarness, uri: &StorageUri) -> Vec<u8> {
    let provider = h.storage().resolve(uri).expect("provider for test URI");
    provider
        .read(uri)
        .take()
        .expect("memory provider completes synchronously")
        .expect("stored project readable")
}

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
    // Exercises the public `AppState::save_graph_to_uri` →
    // `AppState::load_graph_from_uri` pipeline through the ATMR (zip)
    // container. Confirms `current_file` is updated, the stored blob is
    // a real zip (PK header), and the round-tripped graph preserves node
    // + edge counts. Storage is the harness's `MemoryProvider`, so this
    // never touches the filesystem.
    let h = TestHarness::with_starter_graph();
    let nodes_before = h.state().graph.lock().unwrap().nodes().count();
    let noodles_before = h.state().graph.lock().unwrap().noodles().len();

    let uri = memory_uri("round_trip.atmr");

    h.state().save_graph_to_uri(&uri).expect("save_graph_to_uri");
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        Some(uri.clone()),
        "save should record the URI on AppState.current_file",
    );

    // Quick smoke check that we wrote a real zip: every zip starts with
    // the four-byte local-file-header signature 50 4B 03 04 ("PK" plus
    // two control bytes), spelled here with explicit escapes so the
    // source stays plain ASCII.
    let bytes = read_stored(&h, &uri);
    assert!(
        bytes.len() >= 4 && &bytes[..4] == b"PK\x03\x04",
        "expected the 4-byte zip signature"
    );

    // Wipe the in-memory graph, then load back from storage and assert
    // the topology survived the round trip.
    h.state().new_empty_project();
    assert_eq!(h.state().graph.lock().unwrap().nodes().count(), 0);
    h.state()
        .load_graph_from_uri(&uri)
        .expect("load_graph_from_uri");
    let nodes_after = h.state().graph.lock().unwrap().nodes().count();
    let noodles_after = h.state().graph.lock().unwrap().noodles().len();
    assert_eq!(nodes_before, nodes_after);
    assert_eq!(noodles_before, noodles_after);
}

#[test]
fn ui_settings_surface_current_file_as_last_project_path() {
    // The shell's AutoSave loop snapshots `AppState::ui_settings()` on
    // every paint and writes it to disk. The auto-reopen path on next
    // launch reads `last_project_path` back from there, so this is
    // the contract that lets the user resume where they left off.
    let h = TestHarness::with_starter_graph();
    // Before any save, no project location is associated.
    assert_eq!(h.state().ui_settings().last_project_path, None);

    let uri = memory_uri("settings_surface.atmr");
    h.state().save_graph_to_uri(&uri).expect("save_graph_to_uri");

    assert_eq!(h.state().ui_settings().last_project_path, Some(uri));
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
        // Unified Output node plays both the viewport-display anchor
        // role and the subgraph-output declarator role. GraphInput
        // remains for subgraph inputs; the legacy GraphOutput was
        // removed.
        "Output", "GraphInput",
    ] {
        assert!(
            reg.get(ty).is_some(),
            "registry must register '{}' for the New-Node menu",
            ty
        );
    }
}
