//! Unit tests for `Viewport3dWidget`. Split out of `viewport_widget.rs`
//! so the main source file stays under the repository line-count
//! guardrail. Pulled in via `#[path]` so it sits next to its sibling
//! `viewport_widget.rs` rather than nesting under `viewport_widget/`.

use super::*;

fn empty_inputs() -> ViewportInputs {
    ViewportInputs::empty()
}

#[test]
fn project_returns_none_for_point_behind_camera() {
    let cam = OrbitCamera::default();
    let view = cam.view_matrix();
    let proj = cam.projection_matrix(1.0);
    let mvp = mul4(&proj, &view);
    // Point behind the camera (w ends up <= 0).
    let p = [
        cam.eye()[0] * 2.0 - cam.center[0],
        cam.eye()[1] * 2.0 - cam.center[1],
        cam.eye()[2] * 2.0 - cam.center[2],
    ];
    let result = project(&mvp, p, 100.0, 100.0);
    assert!(result.is_none());
}

#[test]
fn project_origin_lands_near_center_of_widget() {
    let cam = OrbitCamera::default();
    let mvp = mul4(&cam.projection_matrix(1.0), &cam.view_matrix());
    let s = project(&mvp, [0.0, 0.0, 0.0], 200.0, 200.0).unwrap();
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
