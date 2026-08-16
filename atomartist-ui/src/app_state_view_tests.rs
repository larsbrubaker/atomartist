//! Unit tests for [`crate::app_state_view`] — capture, restore and the
//! File → New reset, driven straight against `AppState` with no widget
//! tree. The end-to-end "save, New, reopen, everything is back" flow
//! lives in `atomartist-ui-test/tests/view_state.rs`.

use super::*;
use crate::top_level::fresh_state_with_builtins;
use agg_gui_node_editor::NodeEditorCommand;
use atomartist_lib::serialization::view_state::{DIVIDER_MAX, DIVIDER_MIN};

fn a_view() -> ProjectView {
    ProjectView {
        view_state: Some(CanvasView {
            scale: 0.75,
            offset: [140.0, -60.0],
        }),
        divider_position: Some(0.35),
        camera_state: Some(CameraState {
            position: [0.0, -100.0, 0.0],
            target: [0.0, 0.0, 0.0],
            initial_position: Some([5.0, -50.0, 10.0]),
            initial_target: Some([1.0, 1.0, 1.0]),
        }),
    }
}

/// Capture reads the three live slots — and reports no camera at all
/// until the document has been framed.
#[test]
fn capture_reads_the_live_view_and_omits_an_unframed_camera() {
    let state = fresh_state_with_builtins();
    *state.canvas_zoom.lock().unwrap() = 1.5;
    *state.canvas_pan.lock().unwrap() = [-20.0, 30.0];
    state.divider_ratio.set(0.45);

    let view = state.current_project_view();
    let canvas = view.view_state.expect("canvas captured");
    assert_eq!(canvas.scale, 1.5);
    assert_eq!(canvas.offset, [-20.0, 30.0]);
    assert_eq!(view.divider_position, Some(0.45));
    assert!(
        view.camera_state.is_none(),
        "a project that has never been framed pins no camera"
    );

    // Once framed, the camera is captured with its initial pose.
    let initial = CameraPose {
        position: [1.0, 2.0, 3.0],
        target: [0.0, 0.0, 0.0],
    };
    *state.camera_framed.lock().unwrap() = Some(initial);
    let camera = state
        .current_project_view()
        .camera_state
        .expect("camera captured once framed");
    assert_eq!(camera.initial_position, Some(initial.position));
    assert_eq!(camera.initial_target, Some(initial.target));
    assert_eq!(camera.position, state.camera.lock().unwrap().eye());
}

/// The captured divider is clamped into NodeDesigner's 20…80 % window.
#[test]
fn capture_clamps_the_divider() {
    let state = fresh_state_with_builtins();
    state.divider_ratio.set(0.05);
    assert_eq!(
        state.current_project_view().divider_position,
        Some(DIVIDER_MIN)
    );
    state.divider_ratio.set(0.95);
    assert_eq!(
        state.current_project_view().divider_position,
        Some(DIVIDER_MAX)
    );
}

/// Restore moves all three: the camera directly, the divider through its
/// handle, the canvas through the editor's command queue.
#[test]
fn restore_applies_every_group() {
    let state = fresh_state_with_builtins();
    let view = a_view();
    state.apply_project_view(Some(&view));

    assert_eq!(*state.canvas_zoom.lock().unwrap(), 0.75);
    assert_eq!(*state.canvas_pan.lock().unwrap(), [140.0, -60.0]);
    assert_eq!(
        state.node_editor.take(),
        vec![NodeEditorCommand::SetView {
            scale: 0.75,
            offset: [140.0, -60.0],
        }],
        "the canvas restore is deferred to the editor's next layout"
    );
    assert_eq!(state.divider_ratio.get(), 0.35);

    let camera = state.camera.lock().unwrap();
    let eye = camera.eye();
    for k in 0..3 {
        assert!(
            (eye[k] - view.camera_state.unwrap().position[k]).abs() < 1e-3,
            "eye {eye:?}"
        );
    }
    assert_eq!(camera.center, [0.0, 0.0, 0.0]);
    drop(camera);

    // The framed slot carries the *initial* pose, which is what
    // suppresses the auto-frame on the first evaluation.
    assert_eq!(
        state.camera_framed.lock().unwrap().map(|p| p.position),
        Some([5.0, -50.0, 10.0])
    );
}

/// A file with no view at all leaves the canvas and splitter alone — and
/// re-arms the auto-frame, because a document with no saved camera has
/// never been framed.
#[test]
fn a_missing_view_leaves_the_workspace_alone_and_rearms_the_auto_frame() {
    let state = fresh_state_with_builtins();
    *state.canvas_zoom.lock().unwrap() = 2.0;
    *state.canvas_pan.lock().unwrap() = [7.0, 8.0];
    state.divider_ratio.set(0.4);
    *state.camera_framed.lock().unwrap() = Some(CameraPose {
        position: [9.0, 9.0, 9.0],
        target: [0.0, 0.0, 0.0],
    });

    state.apply_project_view(None);

    assert_eq!(*state.canvas_zoom.lock().unwrap(), 2.0, "canvas untouched");
    assert_eq!(*state.canvas_pan.lock().unwrap(), [7.0, 8.0]);
    assert_eq!(state.divider_ratio.get(), 0.4, "divider untouched");
    assert!(state.node_editor.take().is_empty(), "no canvas command");
    assert!(
        state.camera_framed.lock().unwrap().is_none(),
        "the next geometry must auto-frame"
    );
}

/// A file carrying only one group restores only that group.
#[test]
fn a_partial_view_restores_only_what_it_carries() {
    let state = fresh_state_with_builtins();
    state.divider_ratio.set(0.4);
    let view = ProjectView {
        view_state: Some(CanvasView {
            scale: 0.5,
            offset: [1.0, 2.0],
        }),
        ..Default::default()
    };
    state.apply_project_view(Some(&view));
    assert_eq!(*state.canvas_zoom.lock().unwrap(), 0.5);
    assert_eq!(state.divider_ratio.get(), 0.4, "divider left as it was");
}

/// A degenerate saved camera (eye == target — a file written by a build
/// with a bug, or hand-edited) must not be treated as a restore: the
/// pose is unusable, so the document counts as never framed and the
/// auto-frame stays armed.
#[test]
fn a_degenerate_camera_state_is_treated_as_no_camera() {
    let state = fresh_state_with_builtins();
    let before = state.camera.lock().unwrap().pose();
    let view = ProjectView {
        camera_state: Some(CameraState {
            position: [7.0, 7.0, 7.0],
            target: [7.0, 7.0, 7.0],
            initial_position: None,
            initial_target: None,
        }),
        ..Default::default()
    };
    state.apply_project_view(Some(&view));

    assert!(
        state.camera_framed.lock().unwrap().is_none(),
        "an unusable pose must leave the auto-frame armed"
    );
    let after = state.camera.lock().unwrap().pose();
    assert_eq!(after.target, before.target, "camera untouched");
}

/// A *usable* camera with a degenerate initial pair still restores — the
/// framed slot falls back to the live pose, which is always usable here.
#[test]
fn a_degenerate_initial_pose_falls_back_to_the_restored_one() {
    let state = fresh_state_with_builtins();
    let view = ProjectView {
        camera_state: Some(CameraState {
            position: [0.0, -100.0, 0.0],
            target: [0.0, 0.0, 0.0],
            initial_position: Some([2.0, 2.0, 2.0]),
            initial_target: Some([2.0, 2.0, 2.0]),
        }),
        ..Default::default()
    };
    state.apply_project_view(Some(&view));

    assert_eq!(
        state.camera_framed.lock().unwrap().map(|p| p.position),
        Some([0.0, -100.0, 0.0]),
        "the framed slot holds a pose the camera can actually be put on"
    );
}

/// File → New resets camera positioning only.
#[test]
fn new_resets_the_camera_but_not_the_workspace() {
    let state = fresh_state_with_builtins();
    state.apply_project_view(Some(&a_view()));
    let _ = state.node_editor.take();

    state.new_empty_project();

    assert_eq!(
        *state.canvas_zoom.lock().unwrap(),
        0.75,
        "canvas zoom survives New"
    );
    assert_eq!(state.divider_ratio.get(), 0.35, "so does the divider");
    assert!(state.node_editor.take().is_empty());
    assert!(
        state.camera_framed.lock().unwrap().is_none(),
        "New re-arms the auto-frame"
    );
    let camera = state.camera.lock().unwrap();
    let default = atomartist_renderer::OrbitCamera::default();
    assert_eq!(camera.center, default.center);
    assert_eq!(camera.orientation, default.orientation);
}

/// Moving the view is never an edit: the change tracker only ever sees
/// the graph.
#[test]
fn view_moves_do_not_mark_the_project_dirty() {
    let state = fresh_state_with_builtins();
    state.mark_saved_baseline();
    assert!(!state.has_unsaved_changes());

    *state.canvas_zoom.lock().unwrap() = 0.3;
    *state.canvas_pan.lock().unwrap() = [500.0, 500.0];
    state.divider_ratio.set(0.8);
    state.camera.lock().unwrap().orbit(0.5, 0.2);
    state.apply_project_view(Some(&a_view()));

    assert!(
        !state.has_unsaved_changes(),
        "panning, resizing and orbiting are not edits"
    );
}
