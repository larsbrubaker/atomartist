//! Unit tests for `Viewport3dWidget`. Split out of `viewport_widget.rs`
//! so the main source file stays under the repository line-count
//! guardrail. Pulled in via `#[path]` so it sits next to its sibling
//! `viewport_widget.rs` rather than nesting under `viewport_widget/`.

use super::*;
use glam::Mat4 as GlamMat4;

fn empty_inputs() -> ViewportInputs {
    ViewportInputs::empty()
}

fn mvp(cam: &OrbitCamera, aspect: f32) -> [f32; 16] {
    let view = GlamMat4::from_cols_array(&cam.view_matrix());
    let proj = GlamMat4::from_cols_array(&cam.projection_matrix(aspect));
    (proj * view).to_cols_array()
}

#[test]
fn project_returns_none_for_point_behind_camera() {
    let cam = OrbitCamera::default();
    let m = mvp(&cam, 1.0);
    // Point behind the camera (w ends up <= 0).
    let p = [
        cam.eye()[0] * 2.0 - cam.center[0],
        cam.eye()[1] * 2.0 - cam.center[1],
        cam.eye()[2] * 2.0 - cam.center[2],
    ];
    let result = project(&m, p, 100.0, 100.0);
    assert!(result.is_none());
}

#[test]
fn project_origin_lands_near_center_of_widget() {
    let cam = OrbitCamera::default();
    let m = mvp(&cam, 1.0);
    let s = project(&m, [0.0, 0.0, 0.0], 200.0, 200.0).unwrap();
    // Center is somewhere in the middle of the widget within tolerance.
    assert!(s.0 > 60.0 && s.0 < 140.0);
    assert!(s.1 > 60.0 && s.1 < 140.0);
}

#[test]
fn widget_constructs_and_lays_out() {
    let inputs = empty_inputs();
    let mut w = Viewport3dWidget::new(inputs);
    let s = w.layout(Size::new(400.0, 300.0));
    assert_eq!(s.width, 400.0);
    assert_eq!(s.height, 300.0);
}

/// Build a viewport with `bounds` set and a single body wired
/// through `last_mesh_output`. Used by the click-select tests
/// below — they need a real Geometry3d to ray-test against and a
/// known viewport rect for the cursor coords.
fn viewport_with_one_body(node_id: atomartist_lib::graph::node::NodeId) -> Viewport3dWidget {
    use atomartist_lib::geometry::{Body, Geometry3d};
    let inputs = empty_inputs();
    let mesh = std::sync::Arc::new(atomartist_lib::geometry::generate_box(20.0, 20.0, 20.0));
    let mut body = Body::from_mesh(mesh);
    body.origin = Some(node_id);
    *inputs.last_mesh_output.lock().unwrap() =
        Some(std::sync::Arc::new(Geometry3d::from_body(body)));
    let mut w = Viewport3dWidget::new(inputs);
    let _ = w.layout(Size::new(400.0, 300.0));
    w
}

/// Synthesise the full Widget event flow (note_mouse_down/up +
/// on_mouse_X) so the safety net inside `on_mouse_move` sees the
/// real pressed-buttons state. Calling the inner `on_mouse_*`
/// methods directly skips `note_mouse_down` — the safety net then
/// clears the drag on the very next move, which doesn't match
/// production behaviour.
fn fire(w: &mut Viewport3dWidget, ev: Event) {
    use agg_gui::Widget;
    w.on_event(&ev);
}

#[test]
fn click_without_drag_selects_clicked_body() {
    use agg_gui::Modifiers;
    let node_id = atomartist_lib::graph::node::NodeId(42);
    let mut w = viewport_with_one_body(node_id);
    assert_eq!(*w.inputs.selection.lock().unwrap(), None);
    let centre = Point { x: 200.0, y: 150.0 };
    fire(&mut w, Event::MouseDown { pos: centre, button: MouseButton::Left, modifiers: Modifiers::default() });
    fire(&mut w, Event::MouseUp { pos: centre, button: MouseButton::Left, modifiers: Modifiers::default() });
    assert_eq!(
        *w.inputs.selection.lock().unwrap(),
        Some(node_id),
        "click on a body must select that body's origin NodeId",
    );
}

#[test]
fn click_with_micro_jitter_still_selects() {
    // Real human mouse-clicks include 1-2 px jitter between
    // mouse-down and mouse-up. Selection must still land —
    // otherwise users have to hold the mouse rock-still to pick
    // anything.
    use agg_gui::Modifiers;
    let node_id = atomartist_lib::graph::node::NodeId(7);
    let mut w = viewport_with_one_body(node_id);
    let down = Point { x: 200.0, y: 150.0 };
    // Past the 5-px drag threshold so we exercise the
    // moved-past-threshold path. Real human click jitter rarely
    // exceeds this, but selection must still commit if it does.
    let jitter = Point { x: 207.0, y: 156.0 };
    let up = Point { x: 200.0, y: 150.0 };
    fire(&mut w, Event::MouseDown { pos: down, button: MouseButton::Left, modifiers: Modifiers::default() });
    fire(&mut w, Event::MouseMove { pos: jitter });
    fire(&mut w, Event::MouseUp { pos: up, button: MouseButton::Left, modifiers: Modifiers::default() });
    assert_eq!(
        *w.inputs.selection.lock().unwrap(),
        Some(node_id),
        "jitter past the 2-px threshold must still commit selection",
    );
}

/// Build a viewport whose selected body's origin node exposes a
/// writable `matrix`, backed by a shared cell that the read + write
/// callbacks both touch. Mirrors how `atomartist-ui` wires
/// `read_node_matrix` / `write_node_matrix` to the node's `matrix`
/// property + undo stack — the existing `viewport_with_one_body`
/// leaves both `None`, so the body there can never promote past
/// `Selecting` into a real `DragBodyXY`. This helper is what lets a
/// test exercise the drag-and-release path the user actually hits.
fn viewport_with_draggable_body(
    node_id: atomartist_lib::graph::node::NodeId,
) -> (Viewport3dWidget, std::sync::Arc<std::sync::Mutex<[f32; 16]>>) {
    use atomartist_lib::geometry::{Body, Geometry3d};
    let mut identity = [0.0_f32; 16];
    identity[0] = 1.0;
    identity[5] = 1.0;
    identity[10] = 1.0;
    identity[15] = 1.0;
    let matrix = std::sync::Arc::new(std::sync::Mutex::new(identity));
    let mut inputs = empty_inputs();
    let read_cell = matrix.clone();
    let write_cell = matrix.clone();
    inputs.read_node_matrix =
        Some(std::sync::Arc::new(move |_id| Some(*read_cell.lock().unwrap())));
    inputs.write_node_matrix =
        Some(std::sync::Arc::new(move |_id, m| *write_cell.lock().unwrap() = m));
    let mesh = std::sync::Arc::new(atomartist_lib::geometry::generate_box(20.0, 20.0, 20.0));
    let mut body = Body::from_mesh(mesh);
    body.origin = Some(node_id);
    *inputs.last_mesh_output.lock().unwrap() =
        Some(std::sync::Arc::new(Geometry3d::from_body(body)));
    let mut w = Viewport3dWidget::new(inputs);
    let _ = w.layout(Size::new(400.0, 300.0));
    (w, matrix)
}

/// Full press → drag → release on a draggable body: the release MUST
/// (a) clear the drag state so the body stops following the cursor and
/// (b) leave the dragged translation committed to the node matrix.
/// This is the path the bare `viewport_with_one_body` tests can't
/// reach (their matrix callbacks are `None`, so the drag never
/// promotes to `DragBodyXY`).
#[test]
fn drag_then_release_commits_position_and_releases() {
    use agg_gui::Modifiers;
    let node_id = atomartist_lib::graph::node::NodeId(99);
    let (mut w, matrix) = viewport_with_draggable_body(node_id);
    let down = Point { x: 200.0, y: 150.0 };
    let mid = Point { x: 260.0, y: 150.0 };
    let up = Point { x: 280.0, y: 160.0 };
    fire(&mut w, Event::MouseDown { pos: down, button: MouseButton::Left, modifiers: Modifiers::default() });
    fire(&mut w, Event::MouseMove { pos: mid });
    // Drag must have promoted to a body translation by now.
    assert!(
        matches!(w.drag, CameraDrag::DragBodyXY { .. }),
        "a past-threshold drag on a body with a writable matrix must promote to DragBodyXY, got {:?}",
        w.drag,
    );
    fire(&mut w, Event::MouseMove { pos: up });
    fire(&mut w, Event::MouseUp { pos: up, button: MouseButton::Left, modifiers: Modifiers::default() });

    // (a) Released: drag state cleared so a later hover can't keep
    //     moving the body.
    assert!(
        matches!(w.drag, CameraDrag::None),
        "mouse-up during a body drag must release the object, got {:?}",
        w.drag,
    );
    // (b) Position set: the dragged translation persists on the node
    //     matrix after release (it is NOT reset back to the origin).
    let m = *matrix.lock().unwrap();
    let moved = m[12].abs() > 1e-4 || m[13].abs() > 1e-4;
    assert!(moved, "released body must keep its dragged position, got translation [{}, {}]", m[12], m[13]);
    // And the dragged body stays selected.
    assert_eq!(*w.inputs.selection.lock().unwrap(), Some(node_id));
}

/// Esc during a body drag must cancel it: clear the drag state AND
/// restore the node matrix to its pre-drag value (MatterCAD parity).
#[test]
fn esc_cancels_body_drag_and_restores_matrix() {
    use agg_gui::Modifiers;
    let node_id = atomartist_lib::graph::node::NodeId(77);
    let (mut w, matrix) = viewport_with_draggable_body(node_id);
    let down = Point { x: 200.0, y: 150.0 };
    let mid = Point { x: 270.0, y: 160.0 };
    let far = Point { x: 300.0, y: 175.0 };
    fire(&mut w, Event::MouseDown { pos: down, button: MouseButton::Left, modifiers: Modifiers::default() });
    // First move past threshold promotes Selecting → DragBodyXY (no
    // matrix write yet); the second move actually translates the body.
    fire(&mut w, Event::MouseMove { pos: mid });
    fire(&mut w, Event::MouseMove { pos: far });
    assert!(matches!(w.drag, CameraDrag::DragBodyXY { .. }), "drag should be active before Esc");
    let moved = *matrix.lock().unwrap();
    assert!(moved[12].abs() > 1e-4 || moved[13].abs() > 1e-4, "drag should have moved the body");

    // Esc → cancel.
    fire(&mut w, Event::KeyDown { key: agg_gui::Key::Escape, modifiers: Modifiers::default() });
    assert!(matches!(w.drag, CameraDrag::None), "Esc must clear the drag, got {:?}", w.drag);
    let restored = *matrix.lock().unwrap();
    assert!(restored[12].abs() < 1e-6, "X restored to start, got {}", restored[12]);
    assert!(restored[13].abs() < 1e-6, "Y restored to start, got {}", restored[13]);
}

/// With no drag active, Esc must NOT be consumed by the viewport — it
/// should bubble so menus / other widgets can act on it.
#[test]
fn esc_with_no_drag_is_not_consumed() {
    use agg_gui::{Modifiers, Widget};
    let node_id = atomartist_lib::graph::node::NodeId(78);
    let (mut w, _matrix) = viewport_with_draggable_body(node_id);
    let r = w.on_event(&Event::KeyDown { key: agg_gui::Key::Escape, modifiers: Modifiers::default() });
    assert_eq!(r, EventResult::Ignored, "idle Esc must fall through");
}

/// Hovering the Z-control handle must set `hovered_z_control` (which
/// drives the accent highlight, MatterCAD parity); moving off it clears
/// the flag. Drives the real `on_mouse_move` hover path.
#[test]
fn z_control_highlights_on_hover() {
    let node_id = atomartist_lib::graph::node::NodeId(55);
    let mut w = viewport_with_one_body(node_id);
    *w.inputs.selection.lock().unwrap() = Some(node_id);

    // Project the Z-control handle to screen using the same camera +
    // layout the widget's hover pick uses, then hover there.
    let aabb = selected_body_world_aabb(w.current_geometry().as_deref(), node_id)
        .expect("selected body has a world AABB");
    let cam = w.cam();
    let (_, (cube_center, _)) = z_control_gizmo::z_control_layout_for_aabb(aabb, &cam, 300.0);
    let m = mvp(&cam, 400.0_f32 / 300.0);
    let (sx, sy) = project(&m, cube_center, 400.0, 300.0).expect("Z control projects on screen");

    fire(&mut w, Event::MouseMove { pos: Point { x: sx, y: sy } });
    assert!(w.hovered_z_control, "hovering the Z control must highlight it");

    // Move to a far corner — off the control — and the highlight clears.
    fire(&mut w, Event::MouseMove { pos: Point { x: 2.0, y: 2.0 } });
    assert!(!w.hovered_z_control, "moving off the Z control clears the highlight");
}

#[test]
fn mouse_up_clears_drag_state() {
    use agg_gui::Modifiers;
    let node_id = atomartist_lib::graph::node::NodeId(13);
    let mut w = viewport_with_one_body(node_id);
    let down = Point { x: 200.0, y: 150.0 };
    let dragged = Point { x: 240.0, y: 180.0 };
    fire(&mut w, Event::MouseDown { pos: down, button: MouseButton::Left, modifiers: Modifiers::default() });
    fire(&mut w, Event::MouseMove { pos: dragged });
    fire(&mut w, Event::MouseUp { pos: dragged, button: MouseButton::Left, modifiers: Modifiers::default() });
    assert!(
        matches!(w.drag, CameraDrag::None),
        "mouse-up must always reset drag to None, got {:?}",
        w.drag,
    );
    // Drag fully released: a subsequent hover event with no button
    // held must not re-enter any drag state.
    fire(&mut w, Event::MouseMove { pos: Point { x: 250.0, y: 200.0 } });
    assert!(matches!(w.drag, CameraDrag::None));
}

/// `show_bed` lives in a shared `Arc<Mutex<>>` between the host UI
/// (which drives the toolbar toggle) and the viewport widget (which
/// mirrors it into [`crate::scene_renderer::WgpuSceneRenderer::draw_grid`]
/// each paint). Clone the handle, build a widget that owns the
/// inputs, and assert the flag the widget sees follows the host's
/// writes — proves the toggle path doesn't get truncated when the
/// widget moves out the inputs.
#[test]
fn show_bed_flag_round_trips_through_inputs() {
    let inputs = empty_inputs();
    let handle = inputs.show_bed.clone();
    let _w = Viewport3dWidget::new(inputs);
    *handle.lock().unwrap() = false;
    assert!(!*handle.lock().unwrap());
    *handle.lock().unwrap() = true;
    assert!(*handle.lock().unwrap());
}
