//! Unit tests for `viewport_overlay`. Split out of
//! `viewport_overlay.rs` so the main source file stays under the
//! repository line-count guardrail. Pulled in via `#[path]` so it
//! sits next to its sibling `viewport_overlay.rs` rather than
//! nesting under `viewport_overlay/`.

use super::*;
use crate::app_state::AppState;
use agg_gui::{Modifiers, MouseButton};
use glam::Quat;

const FONT_BYTES: &[u8] =
    include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");

fn make_font() -> Arc<Font> {
    agg_gui::font_settings::current_system_font().unwrap_or_else(|| {
        Arc::new(Font::from_bytes(FONT_BYTES.to_vec()).expect("bundled NotoSans"))
    })
}

fn fresh_state() -> AppState {
    AppState::new(
        atomartist_lib::Graph::new(),
        atomartist_lib::registry::NodeRegistry::new(),
    )
}

/// Click at the given parent-local position. Synthetic
/// MouseDown + MouseUp pair, modifiers = none.
fn click_at(overlay: &mut ViewportOverlay, pos: Point) {
    overlay.on_event(&Event::MouseDown {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    overlay.on_event(&Event::MouseUp {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
}

fn build_at_size(state: AppState, w: f64, h: f64) -> Box<dyn Widget> {
    let mut overlay = build_viewport_overlay(state, make_font());
    overlay.layout(Size::new(w, h));
    overlay
}

#[test]
fn overlay_constructs_with_minimum_children() {
    let state = fresh_state();
    let viewport_inputs = ViewportInputs::empty();
    let cube_inputs = TumbleCubeInputs {
        camera: state.camera.clone(),
        animation_completed: None,
    };
    let viewport = Box::new(Viewport3dWidget::new(viewport_inputs));
    let cube = Box::new(TumbleCubeWidget::new(cube_inputs));
    let overlay = ViewportOverlay::new(viewport, cube);
    // viewport + HUD bay layer + cube — three fixed children before
    // any ring or bottom widgets are attached.
    assert_eq!(overlay.children.len(), 3);
    assert_eq!(overlay.placements.len(), 0);
}

#[test]
fn build_viewport_overlay_has_8_ring_and_3_bottom_widgets() {
    let state = fresh_state();
    let mut overlay = build_viewport_overlay(state, make_font());
    overlay.layout(Size::new(800.0, 600.0));
    assert_eq!(overlay.type_name(), "ViewportOverlay");
    // 3 fixed (viewport + bay + cube) + 8 ring + 3 bottom = 14.
    assert_eq!(overlay.children().len(), 14);
}

#[test]
fn home_button_starts_camera_animation_to_home_orientation() {
    let state = fresh_state();
    // Move the camera off-default first so Home has somewhere
    // visible to tween to.
    {
        let mut c = state.camera.lock().unwrap();
        c.orientation = Quat::from_rotation_y(1.234) * Quat::from_rotation_x(-0.789);
    }
    let mut overlay = build_at_size(state.clone(), 800.0, 600.0);
    let h = 600.0_f64;
    let w = 800.0_f64;
    let cube_cx = w - CUBE_MARGIN_RIGHT - CUBE_SIZE * 0.5;
    let cube_cy = h - CUBE_MARGIN_TOP - CUBE_SIZE * 0.5;
    let angle = TAU * 0.30;
    let dx = -angle.sin() * RING_RADIUS;
    let dy = angle.cos() * RING_RADIUS;
    let center = Point::new(cube_cx + dx, cube_cy + dy);
    overlay.layout(Size::new(w, h));

    overlay.on_event(&Event::MouseDown {
        pos: center,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    overlay.on_event(&Event::MouseUp {
        pos: center,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });

    assert!(
        state.camera_animation.lock().unwrap().is_some(),
        "home button should tween via camera_animation rather than jumping"
    );
}

#[test]
fn bed_toggle_flips_show_bed_state() {
    let state = fresh_state();
    let initial = *state.show_bed.lock().unwrap();
    let mut overlay = build_at_size(state.clone(), 800.0, 600.0);
    let h = 600.0_f64;
    let w = 800.0_f64;
    let cube_cx = w - CUBE_MARGIN_RIGHT - CUBE_SIZE * 0.5;
    let cube_y = h - CUBE_MARGIN_TOP - CUBE_SIZE; // bottom of cube
    let dy_below = BOTTOM_ROW_TOP_OFFSET - CUBE_MARGIN_TOP - CUBE_SIZE;
    let center = Point::new(cube_cx, cube_y - dy_below);
    overlay.layout(Size::new(w, h));
    click_at_widget(&mut *overlay, center);
    let after = *state.show_bed.lock().unwrap();
    assert_ne!(initial, after, "bed click should flip show_bed");
}

fn click_at_widget(w: &mut dyn Widget, pos: Point) {
    w.on_event(&Event::MouseDown {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    w.on_event(&Event::MouseUp {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
}

#[test]
fn click_at_helper_is_callable() {
    let state = fresh_state();
    let viewport_inputs = ViewportInputs::empty();
    let cube_inputs = TumbleCubeInputs {
        camera: state.camera.clone(),
        animation_completed: None,
    };
    let viewport = Box::new(Viewport3dWidget::new(viewport_inputs));
    let cube = Box::new(TumbleCubeWidget::new(cube_inputs));
    let mut overlay = ViewportOverlay::new(viewport, cube);
    overlay.layout(Size::new(800.0, 600.0));
    click_at(&mut overlay, Point::new(10.0, 10.0));
}
