//! Drag-and-drop integration: dropping a mesh file on the node canvas
//! should add a `MeshNode` that immediately produces geometry.
//!
//! These keep using real temp files (and `file:` URIs) on purpose: a
//! drag-and-drop payload arrives from the OS as a `PathBuf`, so the
//! filesystem is genuinely part of what is under test here. Every other
//! file test runs against the in-memory provider.
//!
//! Tests against the real `App` event path — `App::on_file_dropped` is
//! the same entry point `demo-native`'s `WindowEvent::DroppedFile`
//! handler invokes — so a passing test here means dragging the same
//! file into the running desktop app will behave identically.

use std::path::PathBuf;

use atomartist_lib::graph::node::PortValue;
use atomartist_storage::StorageUri;
use atomartist_lib::nodes::mesh::mesh_node;
use atomartist_ui_test::TestHarness;

/// Path to a bundled mesh fixture. The fixture files live in
/// `atomartist-lib/tests/meshes/` so both lib-level and UI-level
/// integration tests share one copy.
fn mesh_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("atomartist-lib")
        .join("tests")
        .join("meshes")
        .join(name)
}

#[test]
fn dropping_an_stl_on_the_canvas_adds_a_mesh_node() {
    let mut h = TestHarness::new();
    let nodes_before = h.state().graph.lock().unwrap().node_count();

    // Drop onto the visible centre of the node canvas.
    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    let drop_x = b.x + b.width * 0.5;
    let drop_y_screen = 720.0 - (b.y + b.height * 0.5);

    h.drop_file(drop_x, drop_y_screen, mesh_fixture("simple_box.stl"));

    let nodes_after = h.state().graph.lock().unwrap().node_count();
    assert_eq!(
        nodes_after,
        nodes_before + 1,
        "drop should spawn exactly one MeshNode",
    );

    // The new node must be a MeshNode with its asset ref populated AND
    // its runtime mesh cache pre-filled so `evaluate_now` has geometry
    // to emit on the very first frame after the drop.
    let graph = h.state().graph.lock().unwrap();
    let mesh_node_instance = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == mesh_node::TYPE_ID)
        .expect("graph should contain a MeshNode after the drop");
    let asset_ref = match mesh_node_instance.properties.get("asset") {
        Some(PortValue::StringVal(s)) => s.as_str().to_string(),
        other => panic!("expected asset property as StringVal, got {:?}", other),
    };
    assert!(
        asset_ref.starts_with("sha256-"),
        "asset property should hold a sha256 ref, got `{}`",
        asset_ref
    );
    match mesh_node_instance.properties.get("mesh") {
        Some(PortValue::Geometry3d(g)) => {
            assert_eq!(
                g.first().unwrap().mesh.tri_verts.len() / 3,
                12,
                "Simple Box has 12 triangles"
            );
        }
        other => panic!(
            "mesh cache should be populated immediately on drop, got {:?}",
            other
        ),
    }
}

#[test]
fn dropped_mesh_lands_at_canvas_space_drop_position() {
    // The drop position should map to canvas-space coordinates on the
    // new node so the user gets it where they aimed. We can't easily
    // assert the absolute canvas pos (depends on pan/zoom/widget
    // layout), but we *can* assert the new node's position isn't (0,0)
    // when the drop happens far from the canvas origin.
    let mut h = TestHarness::new();
    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    let drop_x = b.x + b.width * 0.75;
    let drop_y_screen = 720.0 - (b.y + b.height * 0.25);

    h.drop_file(drop_x, drop_y_screen, mesh_fixture("simple_box.stl"));

    let graph = h.state().graph.lock().unwrap();
    let mesh_node = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == mesh_node::TYPE_ID)
        .expect("MeshNode added");
    let [px, py] = mesh_node.position;
    assert!(
        px.abs() > f64::EPSILON || py.abs() > f64::EPSILON,
        "dropped node should not land at canvas origin (0,0) — got ({}, {})",
        px,
        py,
    );
}

#[test]
fn dropped_mesh_propagates_through_evaluate_now() {
    // The user-visible payoff: after the drop, the viewport sees a
    // mesh. Verify the evaluator picks up the MeshNode's `out` socket
    // when it's the display target.
    let mut h = TestHarness::new();
    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    let drop_x = b.x + b.width * 0.5;
    let drop_y_screen = 720.0 - (b.y + b.height * 0.5);

    h.drop_file(drop_x, drop_y_screen, mesh_fixture("simple_box.stl"));

    // Designate the new MeshNode as the display node so the evaluator
    // pushes its mesh into `last_mesh_output`.
    let new_id = {
        let graph = h.state().graph.lock().unwrap();
        let id = graph
            .nodes()
            .find(|n| n.type_id.as_ref() == mesh_node::TYPE_ID)
            .expect("MeshNode exists")
            .id;
        id
    };
    h.state().set_display_node(Some(new_id));
    h.state().evaluate_now();

    let out = h.state().last_mesh_output.lock().unwrap();
    let geom = out.as_ref().expect("viewport geometry must be populated");
    assert_eq!(
        geom.first().unwrap().mesh.tri_verts.len() / 3,
        12,
        "viewport should receive the 12-triangle cube",
    );
}

#[test]
fn dropping_over_the_3d_viewport_still_imports() {
    // Real-world drops rarely land on the node canvas: users aim at
    // the 3D viewport, and on Windows the shell can't even trust the
    // drop position (winit discards it). The App re-offers unconsumed
    // drops to the whole tree, so a drop anywhere in the window must
    // reach the canvas's file-drop handler.
    let mut h = TestHarness::new();
    let nodes_before = h.state().graph.lock().unwrap().node_count();

    let viewport = h.find_by_id("viewport-3d").expect("viewport widget");
    let b = viewport.bounds();
    let drop_x = b.x + b.width * 0.5;
    let drop_y_screen = 720.0 - (b.y + b.height * 0.5);

    h.drop_file(drop_x, drop_y_screen, mesh_fixture("simple_box.stl"));

    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        nodes_before + 1,
        "a drop over the viewport must still spawn a MeshNode",
    );
}

#[test]
fn dropped_mesh_is_wired_to_output_and_visible() {
    // The user-visible contract: drop an STL, see it in the viewport.
    // That requires the import to connect itself to the Output node —
    // the viewport renders only what's wired to Output.
    let mut h = TestHarness::with_starter_graph();
    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    h.drop_file(
        b.x + b.width * 0.5,
        720.0 - (b.y + b.height * 0.5),
        mesh_fixture("simple_box.stl"),
    );
    h.state().evaluate_now();

    let graph = h.state().graph.lock().unwrap();
    let mesh_id = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == mesh_node::TYPE_ID)
        .expect("MeshNode added")
        .id;
    let output_id = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == "Output")
        .expect("starter graph has an Output node")
        .id;
    assert!(
        graph
            .noodles()
            .iter()
            .any(|n| n.from.node == mesh_id && n.to.node == output_id),
        "imported mesh must be connected into the Output node",
    );
}

#[test]
fn dropping_an_atmr_project_merges_into_the_scene() {
    // Scene formats route through import_scene_file: dropping a saved
    // project merges its nodes (minus Output) into the current graph.
    let mut h = TestHarness::with_starter_graph();
    // OS drops deliver a `PathBuf`, so this project has to be a real
    // file: save it through the harness's `file:` provider and drop the
    // same path the shell would hand us.
    let proj = std::env::temp_dir().join("__atmr_drop_project.atmr");
    let proj_uri = StorageUri::from_local_path(&proj).expect("temp path has a URI form");
    h.state().save_graph_to_uri(&proj_uri).unwrap();
    let before = h.state().graph.lock().unwrap().node_count();

    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    h.drop_file(
        b.x + b.width * 0.5,
        720.0 - (b.y + b.height * 0.5),
        proj.clone(),
    );

    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        before + 3,
        "starter pipeline (minus Output) must merge in",
    );
    let _ = std::fs::remove_file(proj);
}

#[test]
fn dropping_an_unsupported_extension_is_a_silent_no_op() {
    // The drop handler only knows mesh + scene formats. A dropped
    // .txt must not crash and must not add a node.
    let mut h = TestHarness::new();
    let before = h.state().graph.lock().unwrap().node_count();

    let dir = std::env::temp_dir();
    let path = dir.join("__atmr_drop_unsupported.txt");
    std::fs::write(&path, b"not a mesh").unwrap();

    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    let b = canvas.bounds();
    h.drop_file(
        b.x + b.width * 0.5,
        720.0 - (b.y + b.height * 0.5),
        path.clone(),
    );

    assert_eq!(h.state().graph.lock().unwrap().node_count(), before);
    let _ = std::fs::remove_file(path);
}
