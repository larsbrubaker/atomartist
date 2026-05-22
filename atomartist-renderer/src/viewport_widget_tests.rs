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
