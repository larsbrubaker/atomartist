//! The favorites bar's persistent icon **strip** — its contents, its
//! docking, and its scrolling (`docs/file-browser-design.md` §5b, step
//! 6f-1).
//!
//! Split out of `favorites_bar.rs` (handle, panel, persistence) to keep
//! both files under the project's 800-line cap. Ancestor:
//! NodeDesigner's `static/js/node-editor/ui/parts-bar.js`, whose strip
//! is always on screen and scrolls (`overflow-y: auto`) rather than
//! truncating its palette.
//!
//! Coordinates: `find_widget_screen_rect` reports screen-absolute **Y-up**
//! rectangles; `click_local` / `to_screen` flip them into the Y-down space
//! the event helpers take. Bar-local rectangles come from
//! `favorites_bar_geom`, the same arithmetic the widget hit-tests against.

use agg_gui::widget::find_widget_screen_rect;
use agg_gui::{MouseButton, Point, Rect, Size};
use atomartist_ui::favorites_bar::{BAR_ID, EMBEDDED_BROWSER_ID};
use atomartist_ui::favorites_bar_geom::{self as bar_geom, COLLAPSED_W, HANDLE_W, STRIP_W};
use atomartist_ui::file_browser::{FavoriteKind, SEED_NODE_TYPES};
use atomartist_ui::UiSettings;
use atomartist_ui_test::TestHarness;

// ── Geometry helpers (same shape as tests/favorites_bar.rs) ─────────────

fn rect_of(h: &TestHarness, id: &str) -> Rect {
    find_widget_screen_rect(h.app().root(), id)
        .unwrap_or_else(|| panic!("no visible widget with id `{id}`"))
}

fn prop(h: &TestHarness, id: &str, key: &str) -> String {
    h.find_by_id(id)
        .unwrap_or_else(|| panic!("no widget with id `{id}`"))
        .properties()
        .into_iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("`{id}` exposes no `{key}` property"))
}

fn bar_width(h: &TestHarness) -> f64 {
    prop(h, BAR_ID, "width").parse().expect("width is a number")
}

fn panel_width(h: &TestHarness) -> f64 {
    prop(h, BAR_ID, "panel_width")
        .parse()
        .expect("panel_width is a number")
}

fn expanded(h: &TestHarness) -> bool {
    prop(h, BAR_ID, "expanded") == "true"
}

/// How many favourites are on screen in the strip this frame (the rest
/// are scrolled away, not dropped).
fn strip_items(h: &TestHarness) -> usize {
    prop(h, BAR_ID, "strip_items").parse().unwrap()
}

fn scroll_offset(h: &TestHarness) -> f64 {
    prop(h, BAR_ID, "scroll").parse().unwrap()
}

fn max_scroll(h: &TestHarness) -> f64 {
    prop(h, BAR_ID, "max_scroll").parse().unwrap()
}

/// Wheel `notches` over the middle of the strip. Negative = scroll down
/// = reveal favourites further down the palette (agg-gui's sign rule).
///
/// The strip is at the bar's **right** edge since 6g-2, so the point is
/// measured back from there rather than forward from the bar's left —
/// which is the panel while the bar is expanded.
fn wheel_over_strip(h: &mut TestHarness, notches: f64) {
    let bar = rect_of(h, BAR_ID);
    let over = Point::new(bar.x + bar.width - STRIP_W * 0.5, bar.y + bar.height * 0.5);
    let (x, y) = h.to_screen(over);
    h.mouse_move(x, y);
    h.scroll(notches);
}

/// Absolute centre of the handle strip on the bar's right edge.
fn handle_center(h: &TestHarness) -> Point {
    let bar = rect_of(h, BAR_ID);
    Point::new(bar.x + bar.width - HANDLE_W * 0.5, bar.y + bar.height * 0.5)
}

/// The bar's layout at its current size and scroll offset — the same
/// arithmetic the widget hit-tests with.
fn bar_layout(h: &TestHarness) -> (Rect, bar_geom::BarLayout) {
    let bar = rect_of(h, BAR_ID);
    let count: usize = prop(h, BAR_ID, "favorites").parse().unwrap();
    let layout = bar_geom::compute(
        Size::new(bar.width, bar.height),
        expanded(h),
        count,
        h.state().current_file.lock().unwrap().is_some(),
        scroll_offset(h),
    );
    (bar, layout)
}

/// Absolute rectangle of strip item `index`, at the current scroll
/// offset. May be off-screen — the caller decides whether that matters.
fn item_rect(h: &TestHarness, index: usize) -> Rect {
    let (bar, layout) = bar_layout(h);
    let local = layout.items[index];
    Rect::new(bar.x + local.x, bar.y + local.y, local.width, local.height)
}

/// Is strip item `index` *fully* on screen right now? Partial visibility
/// is not reachability: half an icon is neither clickable across its
/// whole slot nor readable.
fn item_fully_on_screen(h: &TestHarness, index: usize) -> bool {
    let (_, layout) = bar_layout(h);
    let item = layout.items[index];
    let view = layout.items_viewport;
    item.y >= view.y - 0.5 && item.y + item.height <= view.y + view.height + 0.5
}

/// …and at least partly on screen, which is what the bar paints and
/// hit-tests.
fn item_on_screen(h: &TestHarness, index: usize) -> bool {
    let (_, layout) = bar_layout(h);
    bar_geom::item_visible(layout.items[index], layout.items_viewport)
}

// ── Strip ───────────────────────────────────────────────────────────────

/// The deliverable of 6d-2's first half, with 6f-1's structure: the bar is
/// in the production tree, collapsed to strip + handle, showing the seeded
/// primitive palette.
#[test]
fn strip_renders_the_seeded_favorites() {
    let h = TestHarness::with_starter_graph();
    assert!(h.find_by_id(BAR_ID).is_some(), "the bar is in build_app");
    assert_eq!(expanded(&h), false, "a fresh install starts collapsed");
    assert_eq!(
        prop(&h, BAR_ID, "favorites"),
        SEED_NODE_TYPES.len().to_string(),
        "the strip shows the seeded primitive palette"
    );
    assert!(strip_items(&h) > 0, "and places what fits");
    assert_eq!(prop(&h, BAR_ID, "dead"), "0", "seeded types all resolve");
    assert_eq!(
        bar_width(&h),
        COLLAPSED_W,
        "collapsed = the persistent strip alone (6g-2: the grip floats over it)"
    );
    assert_eq!(panel_width(&h), 0.0);
    assert!(
        h.find_by_id(EMBEDDED_BROWSER_ID).is_none(),
        "the panel's browser is mounted lazily, on the first expand"
    );
}

/// 6f-1's docking change: the bar comes out of the **3-D viewport** pane,
/// not the node canvas's, so the canvas keeps the full window width.
#[test]
fn the_bar_docks_left_of_the_3d_viewport() {
    let h = TestHarness::with_starter_graph();
    let bar = rect_of(&h, BAR_ID);
    let canvas = rect_of(&h, "node-canvas");
    let viewport = rect_of(&h, "viewport-3d");
    assert!(
        bar.x + bar.width <= viewport.x + 0.5,
        "bar {bar:?} must sit left of the viewport {viewport:?}"
    );
    assert!(
        canvas.width > viewport.width,
        "the bar comes out of the viewport row, not the canvas: \
         canvas {canvas:?} vs viewport {viewport:?}"
    );
    assert!(
        bar.y >= canvas.y + canvas.height - 0.5,
        "and it lives in the pane above the canvas"
    );
}

/// The 72 px icon strip **never** collapses: the same favourites are on
/// screen open and closed, and only the panel's width changes.
#[test]
fn the_strip_survives_the_toggle() {
    let mut h = TestHarness::with_starter_graph();
    let collapsed_items = strip_items(&h);
    let strip = rect_of(&h, BAR_ID);
    assert!(collapsed_items > 0);

    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.pump_until_idle(8);
    h.frame();

    assert!(expanded(&h));
    assert_eq!(
        strip_items(&h),
        collapsed_items,
        "expanding grows the panel, not the strip"
    );
    let bar = rect_of(&h, BAR_ID);
    assert_eq!(bar.x, strip.x, "the bar still starts at the window edge");
    let browser = rect_of(&h, EMBEDDED_BROWSER_ID);
    assert!(
        (browser.x - bar.x).abs() < 0.5,
        "the panel opens *outboard* of the strip, against the window edge: \
         browser {browser:?}, bar {bar:?}"
    );
    assert!(
        (browser.x + browser.width - (bar.x + bar.width - STRIP_W)).abs() < 0.5,
        "and stops where the strip begins"
    );
}

/// 6g-2's invariant, measured against the thing it is about: the strip
/// stays flush with the 3-D viewport's left edge whether the panel is
/// open or shut. Expanding pushes the *viewport* right, not the strip
/// left.
#[test]
fn the_strip_stays_against_the_viewport_in_both_states() {
    let mut h = TestHarness::with_starter_graph();
    let gap = |h: &TestHarness| {
        let bar = rect_of(h, BAR_ID);
        let viewport = rect_of(h, "viewport-3d");
        // Right edge of the strip == right edge of the bar == the
        // viewport's left edge.
        let (_, layout) = bar_layout(h);
        assert!(
            (layout.strip.x + layout.strip.width - bar.width).abs() < 0.5,
            "the strip must end at the bar's right edge, {layout:?} in {bar:?}"
        );
        viewport.x - (bar.x + bar.width)
    };
    let collapsed_gap = gap(&h);
    assert!(
        collapsed_gap.abs() < 0.5,
        "collapsed: the strip abuts the viewport, gap {collapsed_gap}"
    );

    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.pump_until_idle(8);
    h.frame();
    assert!(expanded(&h));
    let expanded_gap = gap(&h);
    assert!(
        expanded_gap.abs() < 0.5,
        "expanded: the strip still abuts the viewport, gap {expanded_gap}"
    );
}

/// The handle reserves no width of its own (6g-2): a press on the bar's
/// right edge *above* the 56 px grip must reach the strip item under it,
/// not the toggle.
#[test]
fn a_press_beside_the_grip_misses_the_handle() {
    let mut h = TestHarness::with_starter_graph();
    let bar = rect_of(&h, BAR_ID);
    let (_, layout) = bar_layout(&h);
    let grip = layout.handle;
    assert_eq!(grip.height, 56.0, "the grip is ND's 16 x 56");

    // Same column as the grip, 40 px above its top edge — inside the
    // strip, outside the grip.
    let above = Point::new(
        bar.x + grip.x + grip.width * 0.5,
        bar.y + grip.y + grip.height + 40.0,
    );
    assert!(
        layout
            .strip
            .contains(agg_gui::Point::new(above.x - bar.x, above.y - bar.y)),
        "the probe point is over the strip"
    );
    h.click_local(above, MouseButton::Left);
    assert!(
        !expanded(&h),
        "a click 40 px above the grip must not toggle the panel"
    );

    // …and the grip itself still does.
    let center = handle_center(&h);
    h.click_local(center, MouseButton::Left);
    assert!(expanded(&h), "the grip is still the toggle");
}

// ── Scrolling ───────────────────────────────────────────────────────────

/// A palette taller than the pane scrolls instead of silently dropping
/// its tail (ND's `overflow-y: auto`). At the harness's default 1280×720
/// the seeded palette does not fit, which is exactly the case the first
/// 6f-1 draft got wrong.
#[test]
fn every_favorite_is_reachable_by_scrolling() {
    let mut h = TestHarness::with_starter_graph();
    let count: usize = prop(&h, BAR_ID, "favorites").parse().unwrap();
    let last = count - 1;
    assert!(
        !item_fully_on_screen(&h, last),
        "this window must be too short for the whole palette,          or the test proves nothing"
    );
    assert!(max_scroll(&h) > 0.0, "so the strip has a scroll range");

    // Wheel down (negative delta = show content below) until the end.
    wheel_over_strip(&mut h, -8.0);
    assert!(
        item_fully_on_screen(&h, last),
        "scrolling brings the last favourite fully into view"
    );
    assert_eq!(
        strip_items(&h),
        count.min(strip_items(&h)),
        "and the strip is still showing items, not blank space"
    );
    assert!(
        (scroll_offset(&h) - max_scroll(&h)).abs() < 0.5,
        "and eight notches is enough to reach the bottom"
    );
}

/// The offset clamps at both ends: no rubber-banding past the last
/// favourite, no negative scroll above the first.
#[test]
fn the_strip_scroll_clamps_at_both_ends() {
    let mut h = TestHarness::with_starter_graph();
    let max = max_scroll(&h);
    assert!(max > 0.0);

    wheel_over_strip(&mut h, -50.0);
    assert!((scroll_offset(&h) - max).abs() < 0.5, "stops at the bottom");
    wheel_over_strip(&mut h, -50.0);
    assert!((scroll_offset(&h) - max).abs() < 0.5, "and stays there");

    wheel_over_strip(&mut h, 50.0);
    assert_eq!(scroll_offset(&h), 0.0, "stops at the top");
    wheel_over_strip(&mut h, 50.0);
    assert_eq!(scroll_offset(&h), 0.0, "and stays there");
    assert!(
        item_on_screen(&h, 0),
        "the first favourite is back on screen"
    );
}

/// A scrolled-away item is not clickable: the paint clip and the
/// hit-test are the same rectangle.
#[test]
fn a_scrolled_away_item_is_not_clickable() {
    // A short window, so the scroll range is more than one item high and
    // item 0 genuinely leaves the viewport at the bottom.
    let mut h = TestHarness::with_starter_graph().with_size(1280, 560);
    h.frame();
    let count: usize = prop(&h, BAR_ID, "favorites").parse().unwrap();
    wheel_over_strip(&mut h, -8.0);
    assert!(!item_on_screen(&h, 0), "the head has scrolled away");

    // Its rectangle is now above the strip's visible region; a press
    // there must not start a drag-insert.
    let gone = item_rect(&h, 0).center();
    h.click_local(gone, MouseButton::Left);
    assert_eq!(
        prop(&h, BAR_ID, "dragging"),
        "false",
        "a click outside the scroll viewport starts nothing"
    );
    assert_eq!(
        prop(&h, BAR_ID, "favorites"),
        count.to_string(),
        "and changes nothing"
    );
}

// ── Unpin + persistence ─────────────────────────────────────────────────

/// 6f-1 ships **no** unpin gesture, on purpose: seeding runs once ever
/// and a `NodeType` favourite has no re-pin path, so a stray right-click
/// must not be able to destroy one. Unpin returns with 6f-3's
/// context-menu work (a popup needs the floating-overlay host — the bar
/// paints under the 3-D viewport beside it).
#[test]
fn a_right_click_on_a_strip_item_destroys_nothing() {
    let mut h = TestHarness::with_starter_graph();
    let before: usize = prop(&h, BAR_ID, "favorites").parse().unwrap();
    let first_key = h.state().favorites.lock().unwrap().list()[0]
        .stable_key
        .clone();
    let item = item_rect(&h, 0).center();
    h.click_local(item, MouseButton::Right);

    assert_eq!(
        prop(&h, BAR_ID, "favorites"),
        before.to_string(),
        "a right-click is inert until there is a confirm step"
    );
    assert!(
        h.state()
            .favorites
            .lock()
            .unwrap()
            .contains(FavoriteKind::NodeType, &first_key),
        "the favourite the user never asked to lose is still there"
    );
    let reloaded = UiSettings::from_text(&h.state().ui_settings().to_text());
    assert_eq!(reloaded.favorites.len(), before);
}

/// The model-level removal the 6f-3 gesture will drive is still there and
/// still reaches the shells' settings snapshot — only the *gesture* is
/// deferred.
#[test]
fn removing_a_favorite_through_the_model_persists() {
    let h = TestHarness::with_starter_graph();
    let before: usize = prop(&h, BAR_ID, "favorites").parse().unwrap();
    let first = h.state().favorites.lock().unwrap().list()[0].clone();
    h.state()
        .favorites
        .lock()
        .unwrap()
        .remove(first.kind, &first.stable_key);

    let reloaded = UiSettings::from_text(&h.state().ui_settings().to_text());
    assert_eq!(reloaded.favorites.len(), before - 1);
    assert!(!reloaded
        .favorites
        .contains(FavoriteKind::NodeType, &first.stable_key));
}
