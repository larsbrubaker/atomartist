//! Per-project view state (design §5d step 6h-4) end-to-end through the
//! live widget tree: save → File → New → open puts the canvas, the
//! splitter and the camera back, moving the view never dirties the
//! project, and a project written without view state still opens.
//!
//! NodeDesigner equivalent: `static/js/node-editor/core/design-io.js`
//! (`viewState` / `dividerPosition` / `cameraState`) plus
//! `graph-manager.js`'s deferred restore.

use atomartist_lib::serialization::{write_project_to_bytes, ProjectView};
use atomartist_renderer::CameraPose;
use atomartist_storage::{Precondition, StorageUri};
use atomartist_ui_test::{memory_uri, TestHarness};

/// Put the harness's view somewhere distinctive and return what a save
/// would capture.
fn stage_a_view(h: &TestHarness) -> ProjectView {
    *h.state().canvas_zoom.lock().unwrap() = 0.65;
    *h.state().canvas_pan.lock().unwrap() = [231.0, -87.0];
    h.state().divider_ratio.set(0.35);
    // Pretend the document has been framed, so the camera is worth
    // saving at all, then orbit somewhere the default never is.
    *h.state().camera_framed.lock().unwrap() = Some(CameraPose {
        position: [11.0, -22.0, 33.0],
        target: [1.0, 2.0, 3.0],
    });
    {
        let mut cam = h.state().camera.lock().unwrap();
        cam.center = [4.0, 5.0, 6.0];
        cam.radius = 123.0;
    }
    h.state().current_project_view()
}

fn save(h: &TestHarness, uri: &StorageUri) {
    h.state().save_project(uri);
    h.pump_until_idle(64);
}

fn open(h: &TestHarness, uri: &StorageUri) {
    h.state().open_project(uri);
    h.pump_until_idle(64);
}

/// The headline round trip: everything the file carries comes back, and
/// the canvas restore reaches the editor on the next frame.
#[test]
fn save_new_open_restores_canvas_divider_and_camera() {
    let mut h = TestHarness::with_starter_graph();
    let uri = memory_uri("view-round-trip.atmr");
    let expected = stage_a_view(&h);
    let expected_eye = h.state().camera.lock().unwrap().eye();
    save(&h, &uri);

    // File → New wipes the document; its camera goes back to default and
    // the workspace (canvas + divider) deliberately does not move.
    h.state().new_empty_project();
    h.state().camera.lock().unwrap().radius = 60.0;
    assert!(h.state().camera_framed.lock().unwrap().is_none());

    // Now move the workspace somewhere else entirely. Without this the
    // canvas / divider assertions below would pass on a no-op restore —
    // File → New deliberately leaves both where the save found them, so
    // they must be perturbed before the open to mean anything.
    *h.state().canvas_zoom.lock().unwrap() = 2.75;
    *h.state().canvas_pan.lock().unwrap() = [-500.0, 900.0];
    h.state().divider_ratio.set(0.75);
    h.frame();
    let _ = h.state().node_editor.take();

    open(&h, &uri);

    let canvas = h.state().current_project_view().view_state.unwrap();
    assert_eq!(canvas.scale, expected.view_state.unwrap().scale);
    assert_eq!(canvas.offset, expected.view_state.unwrap().offset);
    assert_eq!(h.state().divider_ratio.get(), 0.35);

    let eye = h.state().camera.lock().unwrap().eye();
    for k in 0..3 {
        assert!(
            (eye[k] - expected_eye[k]).abs() < 1e-2,
            "camera eye {eye:?} vs {expected_eye:?}"
        );
    }
    assert_eq!(h.state().camera.lock().unwrap().center, [4.0, 5.0, 6.0]);
    assert_eq!(
        h.state().camera_framed.lock().unwrap().map(|p| p.position),
        Some([11.0, -22.0, 33.0]),
        "the initial pose comes back, so the auto-frame stays suppressed"
    );

    // The canvas half is a queued editor command — one frame drains it
    // (the editor's `layout()` is our "wait for layout to settle").
    assert!(h.state().node_editor.is_pending());
    h.frame();
    assert!(
        !h.state().node_editor.is_pending(),
        "the editor adopts the restored view on its next layout"
    );
    // …and the splitter adopts its handle on the same frame.
    let splitter = h.find_by_type("Splitter").expect("splitter in the tree");
    assert!(splitter.bounds().height > 0.0);
}

/// Regression: the auto-frame is per **document**, not per session.
///
/// The gate used to be the viewport widget's own "have I ever seen a
/// mesh" flag, so only the first project opened in a session was ever
/// framed — every one after it inherited the previous project's camera.
/// Driving the real path (open → evaluate → paint, which is what calls
/// `maybe_auto_fit`) with two projects of very different size pins it.
#[test]
fn each_opened_project_without_a_camera_gets_its_own_auto_frame() {
    let mut h = TestHarness::with_starter_graph();
    let small = memory_uri("frame-small.atmr");
    let large = memory_uri("frame-large.atmr");

    // Project A: the starter scene, saved before anything has been
    // framed — so it carries no `camera_state` at all.
    assert!(h.state().camera_framed.lock().unwrap().is_none());
    save(&h, &small);

    // Project B: the same scene, an order of magnitude taller.
    {
        let mut g = h.state().graph.lock().unwrap();
        let id = g
            .nodes()
            .find(|n| n.type_id.as_ref() == "Extrude")
            .expect("starter graph has an Extrude")
            .id;
        g.set_property(
            id,
            "height",
            atomartist_lib::graph::node::PortValue::Number(400.0),
        )
        .expect("Extrude has a height");
    }
    h.state().evaluate_now();
    *h.state().camera_framed.lock().unwrap() = None;
    save(&h, &large);

    // Open A: the first geometry frames the camera and records it.
    h.state().new_empty_project();
    open(&h, &small);
    h.state().evaluate_now();
    h.paint_once();
    let framed_small = h
        .state()
        .camera_framed
        .lock()
        .unwrap()
        .expect("project A was auto-framed");
    let radius_small = h.state().camera.lock().unwrap().radius;

    // Open B in the same session: it must get its *own* framing.
    h.state().new_empty_project();
    assert!(
        h.state().camera_framed.lock().unwrap().is_none(),
        "File → New re-arms the auto-frame"
    );
    open(&h, &large);
    h.state().evaluate_now();
    h.paint_once();
    let framed_large = h
        .state()
        .camera_framed
        .lock()
        .unwrap()
        .expect("project B was auto-framed too");
    let radius_large = h.state().camera.lock().unwrap().radius;

    assert!(
        radius_large > radius_small * 1.5,
        "the taller project must be framed for itself: {radius_large} vs {radius_small}"
    );
    assert_ne!(
        framed_large.position, framed_small.position,
        "each document records its own initial pose"
    );
}

/// Moving the view is not an edit. Nothing here should leave the project
/// with unsaved changes, which is what makes File → New silent.
#[test]
fn view_moves_do_not_dirty_a_saved_project() {
    let mut h = TestHarness::with_starter_graph();
    let uri = memory_uri("view-dirty.atmr");
    save(&h, &uri);
    assert!(!h.state().has_unsaved_changes(), "a fresh save is clean");

    // Pan and zoom through the real widget: a wheel over the canvas.
    let bounds = h.find_by_id("node-canvas").expect("canvas").bounds();
    let (_, height) = h.size();
    let cx = bounds.x + bounds.width * 0.5;
    let cy = height - (bounds.y + bounds.height * 0.5);
    h.mouse_move(cx, cy);
    h.scroll(120.0);
    h.frame();
    // …and drag the splitter + orbit the camera.
    h.state().divider_ratio.set(0.7);
    h.state().camera.lock().unwrap().orbit(0.4, 0.2);

    assert!(
        !h.state().has_unsaved_changes(),
        "pan / zoom / divider / orbit must not mark the project dirty"
    );
}

/// A project written before view state existed opens fine and leaves the
/// live view alone — except for the auto-frame, which it re-arms.
#[test]
fn a_project_without_view_state_opens_and_leaves_the_workspace_alone() {
    let mut h = TestHarness::with_starter_graph();
    let uri = memory_uri("legacy.atmr");

    // Bytes from the view-less encoder — exactly what an older build
    // would have written.
    let bytes = {
        let graph = h.state().graph.lock().unwrap();
        let assets = h.state().assets.lock().unwrap();
        write_project_to_bytes(&graph, &assets).expect("encode legacy project")
    };
    let provider = h.storage().resolve(&uri).expect("memory provider");
    provider
        .write(&uri, bytes, Precondition::None)
        .take()
        .expect("memory provider settles inline")
        .expect("write");

    *h.state().canvas_zoom.lock().unwrap() = 1.25;
    *h.state().canvas_pan.lock().unwrap() = [9.0, 9.0];
    h.state().divider_ratio.set(0.55);
    *h.state().camera_framed.lock().unwrap() = Some(CameraPose {
        position: [1.0, 1.0, 1.0],
        target: [0.0, 0.0, 0.0],
    });

    open(&h, &uri);
    h.frame();

    assert_eq!(*h.state().canvas_zoom.lock().unwrap(), 1.25);
    assert_eq!(*h.state().canvas_pan.lock().unwrap(), [9.0, 9.0]);
    assert_eq!(h.state().divider_ratio.get(), 0.55);
    assert!(
        h.state().camera_framed.lock().unwrap().is_none(),
        "no saved camera means this document has never been framed"
    );
    assert!(!h.state().node_editor.is_pending(), "no canvas command");
}
