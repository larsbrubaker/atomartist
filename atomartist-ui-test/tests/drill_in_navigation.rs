//! Drill-in navigation chrome tests — the back button + breadcrumb bar
//! that appears when the user has drilled into a component.
//!
//! There is no direct NodeDesigner analogue (its component editing is a
//! separate panel); these verify the AtomArtist-specific breadcrumb
//! widget in `atomartist-ui/src/breadcrumb_bar.rs` faithfully mirrors
//! `AppState::edit_stack` and drives `exit_one` / `exit_to`.
//!
//! Setup note: these drive the breadcrumb by pushing `EditLevel`s onto
//! the shared `edit_stack` directly (the same field
//! `AppState::enter_component` pushes to), which decouples the chrome
//! test from the concurrently-evolving component-registration path.
//! In production the level `label` is derived from the component
//! `NodeDef::display_name()` (see `AppState::enter_component`), so the
//! rendered trail reads "Top Level > <ComponentDisplayName>".

use std::sync::{Arc, Mutex};

use agg_gui::undo::UndoBuffer;
use agg_gui::{MouseButton, Rect};
use atomartist_lib::Graph;
use atomartist_ui::app_state::EditLevel;
use atomartist_ui::breadcrumb_bar::{BACK_BUTTON_CENTER_X, FIRST_CRUMB_HIT_X};
use atomartist_ui_test::harness::DEFAULT_HEIGHT;
use atomartist_ui_test::TestHarness;

/// Push a synthetic drill-in level with the given breadcrumb label.
fn push_level(h: &TestHarness, label: &str) {
    let level = EditLevel {
        label: label.to_string(),
        type_id: "TestComponent".to_string(),
        graph: Arc::new(Mutex::new(Graph::new())),
        undo: Arc::new(Mutex::new(UndoBuffer::new())),
    };
    h.state().edit_stack.lock().unwrap().push(level);
}

/// Absolute (Y-up) screen bounds of the breadcrumb widget, read from the
/// inspector snapshot the harness exposes.
fn breadcrumb_bounds(h: &TestHarness) -> Rect {
    h.snapshot()
        .into_iter()
        .find(|n| n.type_name == "BreadcrumbBar")
        .expect("breadcrumb bar must be in the widget tree")
        .screen_bounds
}

/// Look up a breadcrumb inspector property by name.
fn breadcrumb_prop(h: &TestHarness, name: &str) -> String {
    h.snapshot()
        .into_iter()
        .find(|n| n.type_name == "BreadcrumbBar")
        .and_then(|n| {
            n.properties
                .into_iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v)
        })
        .unwrap_or_default()
}

/// Convert a widget-local point into a harness screen-space click.
/// `local_x` is measured from the widget's left edge; the click lands on
/// the widget's vertical centre.
fn click_local(h: &mut TestHarness, sb: Rect, local_x: f64) {
    let world_x = sb.x + local_x;
    let world_y = sb.y + sb.height * 0.5;
    let screen_y = DEFAULT_HEIGHT - world_y;
    h.click(world_x, screen_y, MouseButton::Left);
}

#[test]
fn breadcrumb_is_hidden_at_root() {
    let h = TestHarness::new();
    // The widget exists in the tree...
    let bar = h.find_by_id("breadcrumb-bar").expect("bar must exist");
    // ...but at the root it hides itself (is_visible == false), which the
    // enclosing FlexRow honours by giving it a zero-width slot — so it
    // draws nothing and swallows no clicks. (Hidden widgets are also
    // excluded from the inspector snapshot, so `bounds` is the observable
    // signal here.)
    assert_eq!(h.state().edit_depth(), 0);
    assert_eq!(bar.bounds().width, 0.0, "bar should take no slot at root");
}

#[test]
fn breadcrumb_shows_trail_when_drilled_in() {
    let mut h = TestHarness::new();
    push_level(&h, "Gadget");
    // Force a relayout so the now-visible bar rebuilds its segments.
    h.mouse_move(1.0, 1.0);

    let bar = h.find_by_id("breadcrumb-bar").expect("bar must exist");
    assert!(bar.bounds().width > 0.0, "bar should occupy a slot once drilled in");
    assert_eq!(breadcrumb_prop(&h, "depth"), "1");
    assert_eq!(breadcrumb_prop(&h, "trail"), "Top Level > Gadget");
}

#[test]
fn breadcrumb_reflects_nested_levels() {
    let mut h = TestHarness::new();
    push_level(&h, "Outer");
    push_level(&h, "Inner");
    h.mouse_move(1.0, 1.0);
    assert_eq!(breadcrumb_prop(&h, "depth"), "2");
    assert_eq!(breadcrumb_prop(&h, "trail"), "Top Level > Outer > Inner");
}

#[test]
fn back_button_click_exits_one_level() {
    let mut h = TestHarness::new();
    push_level(&h, "Outer");
    push_level(&h, "Inner");
    h.mouse_move(1.0, 1.0);
    assert_eq!(h.state().edit_depth(), 2);

    let sb = breadcrumb_bounds(&h);
    click_local(&mut h, sb, BACK_BUTTON_CENTER_X);

    assert_eq!(h.state().edit_depth(), 1, "back button should pop one level");
}

#[test]
fn top_level_segment_click_exits_to_root() {
    let mut h = TestHarness::new();
    push_level(&h, "Outer");
    push_level(&h, "Inner");
    h.mouse_move(1.0, 1.0);
    assert_eq!(h.state().edit_depth(), 2);

    let sb = breadcrumb_bounds(&h);
    click_local(&mut h, sb, FIRST_CRUMB_HIT_X);

    assert_eq!(
        h.state().edit_depth(),
        0,
        "clicking the 'Top Level' crumb should exit all the way to the root"
    );
}
