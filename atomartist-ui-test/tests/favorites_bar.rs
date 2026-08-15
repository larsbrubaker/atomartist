//! The left favorites bar, driven through the real widget tree
//! (`docs/file-browser-design.md` §1.2, §2, §6, §5b — steps 6d-2, 6f-1).
//!
//! Ancestor: NodeDesigner's `static/js/node-editor/ui/parts-bar.js`, whose
//! handle semantics (3 px toggle-vs-resize threshold, pull-open, snap-closed
//! below 120 px keeping the stored width) and constants (panel default 380,
//! min 240, max 70 % of the pane, persistent 72 px strip) these tests pin
//! down on our side. There is no NodeDesigner test file to port — the
//! ancestor had none — so the assertions come from the behaviour the design
//! records.
//!
//! Everything here goes through `TestHarness`, i.e. the production
//! `build_app` tree: since 6f-1 the bar is a real child of the *3-D
//! viewport* row.
//!
//! This file owns the **handle** (toggle / resize / snap-closed), the
//! **panel** (embedded browser, opening projects), pinning and
//! persistence. The persistent icon strip — its contents, its docking,
//! its scrolling — lives in `tests/favorites_strip.rs`, which is where
//! the two files were split to stay under the 800-line cap.
//!
//! Coordinates: `find_widget_screen_rect` reports screen-absolute **Y-up**
//! rectangles; `click_local` / `to_screen` flip them into the Y-down space
//! the event helpers take. Bar-local rectangles come from
//! `favorites_bar_geom`, the same arithmetic the widget hit-tests against.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agg_gui::widget::find_widget_screen_rect;
use agg_gui::{MouseButton, Point, Rect, Size};
use atomartist_storage::{
    FlakyConfig, FlakyProvider, Job, MemoryProvider, StorageProvider, StorageRegistry, StorageUri,
};
use atomartist_ui::favorites_bar::{
    BAR_ID, DEFAULT_EXPANDED_W, EMBEDDED_BROWSER_ID, MAX_WIDTH_FRACTION, MIN_EXPANDED_W,
};
use atomartist_ui::favorites_bar_geom::{self as bar_geom, COLLAPSED_W, HANDLE_W};
use atomartist_ui::file_browser::widget_geom::{self as geom, BrowserLayout};
use atomartist_ui::file_browser::{BrowserMode, FavoriteKind, Favorites};
use atomartist_ui::top_menu_bar::{FileDialogProvider, UnsavedChoice};
use atomartist_ui::{fresh_state_with_starter_graph_and_storage, AppState, UiSettings};
use atomartist_ui_test::TestHarness;

const SCHEME: &str = "mem";

fn uri(path: &str) -> StorageUri {
    StorageUri::new(SCHEME, path)
}

/// A registry whose only provider is an empty in-memory store, so the
/// embedded browser's starting directory is unambiguous.
fn memory_registry() -> Arc<StorageRegistry> {
    let mut registry = StorageRegistry::new();
    registry
        .register(Arc::new(MemoryProvider::new(SCHEME, "Test Memory")) as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    Arc::new(registry)
}

/// Dialog provider with a scripted answer to the unsaved-changes prompt,
/// counting how many times it was asked. Mirrors `file_menu_features`'
/// `ScriptedDialogs`; kept local because that one is a test-crate item.
struct ScriptedDialogs {
    unsaved_answer: UnsavedChoice,
    prompts: AtomicUsize,
    errors: Mutex<Vec<String>>,
}

impl ScriptedDialogs {
    fn new(answer: UnsavedChoice) -> Self {
        ScriptedDialogs {
            unsaved_answer: answer,
            prompts: AtomicUsize::new(0),
            errors: Mutex::new(Vec::new()),
        }
    }
    fn prompts(&self) -> usize {
        self.prompts.load(Ordering::SeqCst)
    }
}

impl FileDialogProvider for ScriptedDialogs {
    fn pick_open_project(&self) -> Job<Option<StorageUri>> {
        Job::ready(None)
    }
    fn pick_save_project(&self, _name: &str) -> Job<Option<StorageUri>> {
        Job::ready(None)
    }
    fn pick_save_export(&self, _ext: &str, _name: &str) -> Job<Option<StorageUri>> {
        Job::ready(None)
    }
    fn pick_import_file(&self) -> Job<Option<StorageUri>> {
        Job::ready(None)
    }
    fn confirm_unsaved_changes(&self) -> UnsavedChoice {
        self.prompts.fetch_add(1, Ordering::SeqCst);
        self.unsaved_answer
    }
    fn show_error(&self, message: &str) {
        self.errors.lock().unwrap().push(message.to_string());
    }
    fn show_info(&self, _title: &str, _message: &str) {}
}

// ── Geometry helpers ────────────────────────────────────────────────────

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

/// Width of the *browser panel* — what the handle resizes and what the
/// settings persist since 6f-1 (the strip is a constant beside it).
fn panel_width(h: &TestHarness) -> f64 {
    prop(h, BAR_ID, "panel_width")
        .parse()
        .expect("panel_width is a number")
}

fn expanded(h: &TestHarness) -> bool {
    prop(h, BAR_ID, "expanded") == "true"
}

fn scroll_offset(h: &TestHarness) -> f64 {
    prop(h, BAR_ID, "scroll").parse().unwrap()
}

/// Absolute centre of the handle strip on the bar's right edge.
fn handle_center(h: &TestHarness) -> Point {
    let bar = rect_of(h, BAR_ID);
    Point::new(bar.x + bar.width - HANDLE_W * 0.5, bar.y + bar.height * 0.5)
}

/// Drag the handle by `dx` logical pixels (positive = right = wider).
fn drag_handle(h: &mut TestHarness, dx: f64) {
    drag_handle_via(h, &[dx]);
}

/// Drag the handle through a *sequence* of offsets from the press point,
/// one `MouseMove` each, releasing at the last.
///
/// `TestHarness::drag` emits a single move, which cannot tell "commits on
/// every move" apart from "commits on release" — the distinction the
/// snap-closed rule turns on. Real pointers emit a stream, so tests of
/// that rule drive one.
fn drag_handle_via(h: &mut TestHarness, offsets: &[f64]) {
    let handle = handle_center(h);
    let (x0, y0) = h.to_screen(handle);
    h.mouse_move(x0, y0);
    h.mouse_down(MouseButton::Left);
    for dx in offsets {
        h.mouse_move(x0 + dx, y0);
    }
    h.mouse_up(MouseButton::Left);
}

/// Absolute centre of the embedded browser's grid cell at `index`.
fn embedded_cell(h: &TestHarness, index: usize) -> Point {
    let browser = rect_of(h, EMBEDDED_BROWSER_ID);
    let layout = BrowserLayout::compute(
        Size::new(browser.width, browser.height),
        BrowserMode::Embedded,
    );
    let count: usize = prop(h, EMBEDDED_BROWSER_ID, "entries").parse().unwrap();
    let geo = geom::grid_geometry(layout.grid, count);
    let local = geom::cell_rect(layout.grid, &geo, index, 0.0).center();
    Point::new(browser.x + local.x, browser.y + local.y)
}

/// Write a real project (starter graph plus one node, so it is
/// distinguishable from what is on screen) and report its node count.
fn seed_project(storage: &Arc<StorageRegistry>, at: &StorageUri) -> usize {
    let seed = fresh_state_with_starter_graph_and_storage(storage.clone());
    {
        let mut graph = seed.graph.lock().unwrap();
        graph
            .add_new_node("Box", [400.0, 400.0], &seed.registry)
            .expect("Box is a built-in");
    }
    seed.save_project(at);
    for _ in 0..8 {
        if !seed.pump_storage() {
            break;
        }
    }
    let count = seed.graph.lock().unwrap().node_count();
    count
}

/// Harness over the memory provider with scripted dialogs — the shape
/// every open-through-the-bar test needs.
fn harness_with(dialogs: Arc<ScriptedDialogs>, storage: Arc<StorageRegistry>) -> TestHarness {
    let state = fresh_state_with_starter_graph_and_storage(storage);
    TestHarness::with_dialogs(state, dialogs as Arc<dyn FileDialogProvider>)
}

// ── Handle: toggle and resize ───────────────────────────────────────────

/// A press released in place toggles — the ancestor's first handle rule.
#[test]
fn clicking_the_handle_toggles_the_panel() {
    let mut h = TestHarness::with_starter_graph();
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    assert!(expanded(&h), "a click on the handle opens the panel");
    assert_eq!(
        panel_width(&h),
        DEFAULT_EXPANDED_W as f64,
        "and opens it at the stored (ND default 380) width"
    );
    assert_eq!(bar_width(&h), COLLAPSED_W + DEFAULT_EXPANDED_W as f64);

    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    assert!(!expanded(&h), "a second click closes it");
    assert_eq!(bar_width(&h), COLLAPSED_W, "the strip is still there");
}

/// Past the 3 px threshold the same gesture is a resize, and — because the
/// bar is docked left — dragging right widens it, pulling it open from the
/// collapsed rail in one motion.
#[test]
fn dragging_the_handle_right_pulls_the_bar_open_and_sizes_it() {
    let mut h = TestHarness::with_starter_graph();
    drag_handle(&mut h, 260.0);
    assert!(expanded(&h), "a rightward pull opens the panel");
    let width = panel_width(&h);
    assert!(
        (width - 260.0).abs() < 1.0,
        "the panel follows the pointer, got {width}"
    );
    assert!(
        (bar_width(&h) - (COLLAPSED_W + 260.0)).abs() < 1.0,
        "and the strip and handle ride along"
    );
    assert_eq!(
        prop(&h, BAR_ID, "dragging"),
        "false",
        "the gesture ended on mouse-up"
    );
}

/// Releasing a drag below the minimum snaps the bar closed but **keeps**
/// the stored width, so the next open comes back at the user's size.
#[test]
fn a_narrow_release_snaps_closed_and_keeps_the_stored_width() {
    let mut h = TestHarness::with_starter_graph();
    drag_handle(&mut h, 300.0);
    let opened = panel_width(&h);
    assert!(opened > MIN_EXPANDED_W as f64);

    // Now drag back past the snap threshold (ND: 120 px) in one motion.
    drag_handle(&mut h, -(opened - 40.0));
    assert!(!expanded(&h), "released below 120 px, the panel closes");
    assert_eq!(
        bar_width(&h),
        COLLAPSED_W,
        "and the strip alone is on screen again"
    );
    let stored: f64 = prop(&h, BAR_ID, "stored_width").parse().unwrap();
    assert!(
        (stored - opened).abs() < 1.0,
        "the stored width survives the snap-closed, got {stored} want {opened}"
    );

    // Re-opening uses it.
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    assert!(expanded(&h));
    assert!((panel_width(&h) - opened).abs() < 1.0);
}

/// The same rule under a **real** pointer stream, which is the only way to
/// see it fail: a close-drag passes through every width on its way down,
/// so a bar that commits the stored width on each `MouseMove` "keeps" the
/// last width above the minimum (~120) instead of the one the user
/// actually sized to. The commit belongs on release.
#[test]
fn closing_through_intermediate_widths_keeps_the_sized_width() {
    let mut h = TestHarness::with_starter_graph();
    drag_handle(&mut h, 300.0);
    let sized = panel_width(&h);
    assert!(
        (sized - 300.0).abs() < 1.0,
        "the panel is wide before the close-drag, got {sized}"
    );

    // Down on the handle, sweep left through 200 → 150 → 130 (all still
    // above the 120 px snap threshold), release at 40 (below it).
    drag_handle_via(
        &mut h,
        &[200.0 - sized, 150.0 - sized, 130.0 - sized, 40.0 - sized],
    );

    assert!(!expanded(&h), "released below the minimum, the bar closes");
    let stored: f64 = prop(&h, BAR_ID, "stored_width").parse().unwrap();
    assert!(
        (stored - sized).abs() < 1.0,
        "the widths swept through on the way down must not be committed: \
         stored {stored}, sized {sized}"
    );

    // And the re-open honours it.
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    assert!((panel_width(&h) - sized).abs() < 1.0);
}

/// The bar may never take more than [`MAX_WIDTH_FRACTION`] of the pane it
/// is docked in — including when the pane shrinks to exactly the width the
/// bar was last given, which is the case that breaks any "is this layout
/// call the parent's echo?" guess based on comparing widths. Getting it
/// wrong leaves a stale cap and a zero-width node canvas.
#[test]
fn the_width_cap_reapplies_when_the_pane_matches_the_bar() {
    let mut h = TestHarness::with_starter_graph();
    *h.state().favorites_bar_width.lock().unwrap() = 400.0;
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    assert_eq!(panel_width(&h), 400.0, "wide open in a 1280 px window");

    // Shrink the window until the viewport pane is about the bar's width.
    let mut h = h.with_size(400, 720);
    h.frame();

    // ND's cap, raised to 70 % of the pane in 6f-1.
    let cap = 400.0 * MAX_WIDTH_FRACTION;
    let width = panel_width(&h);
    assert!(
        width <= cap + 1.0,
        "the panel must re-clamp to {cap} of the new pane, got {width}"
    );
    let viewport = rect_of(&h, "viewport-3d");
    assert!(
        viewport.width > 0.0,
        "and must not squeeze the 3-D viewport out of existence"
    );
}

// ── Expanded panel ──────────────────────────────────────────────────────

/// The panel hosts the shared browser in its embedded face — under its own
/// id, so a `find_widget_by_id` walk can never confuse it with the
/// Open/Save modal's instance.
#[test]
fn the_expanded_panel_hosts_the_embedded_browser() {
    let mut h = harness_with(
        Arc::new(ScriptedDialogs::new(UnsavedChoice::Discard)),
        memory_registry(),
    );
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.pump_until_idle(8);
    h.frame();

    assert!(h.find_by_id(EMBEDDED_BROWSER_ID).is_some());
    assert_eq!(prop(&h, EMBEDDED_BROWSER_ID, "mode"), "Embedded");
    assert_eq!(
        prop(&h, EMBEDDED_BROWSER_ID, "name"),
        "",
        "the embedded face has no name field"
    );
    assert!(
        h.find_by_id("file-browser").is_none(),
        "and does not answer to the modal's id"
    );
}

/// Listings are **quiet** storage operations, so a directory still coming
/// in over a slow provider does not make `menu_actions`' busy gate refuse
/// File actions — or the bar's own project opens. Before that change, an
/// asynchronous provider meant the browser was blocking the File menu for
/// most of the time it was open.
#[test]
fn a_listing_in_flight_does_not_refuse_file_actions() {
    let inner = Arc::new(MemoryProvider::new(SCHEME, "Test Memory")) as Arc<dyn StorageProvider>;
    let provider = Arc::new(FlakyProvider::new(
        inner,
        FlakyConfig::default().with_latency(4),
    ));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the flaky provider");
    let storage = Arc::new(registry);

    let dialogs = Arc::new(ScriptedDialogs::new(UnsavedChoice::Discard));
    let mut h = harness_with(dialogs, storage);
    h.state().mark_saved_baseline();

    // Expand: the panel's browser starts a listing that will not settle
    // for several simulated frames.
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.frame();
    assert_eq!(
        h.state().pending_op_count_all(),
        1,
        "the listing is genuinely still in flight"
    );
    assert_eq!(
        h.state().pending_op_count(),
        0,
        "but it is quiet, so the busy gate cannot see it"
    );

    // The gate is what a File action consults; with the listing quiet it
    // has nothing to refuse.
    h.menu_action("file.new");
    assert!(
        h.state()
            .last_notice()
            .map(|n| !n.text.contains("busy"))
            .unwrap_or(true),
        "no 'storage is busy' refusal: {:?}",
        h.state().last_notice()
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        None,
        "File → New ran"
    );

    // And the listing still lands afterwards.
    for _ in 0..8 {
        provider.pump();
        h.pump();
    }
    assert_eq!(h.state().pending_op_count_all(), 0);
}

// ── Opening a project from the bar ──────────────────────────────────────

/// Activating a project in the embedded browser opens it, through the same
/// path File → Open Recent uses.
#[test]
fn activating_a_project_in_the_embedded_browser_opens_it() {
    let storage = memory_registry();
    let saved = uri("/bracket.atmr");
    let seeded_nodes = seed_project(&storage, &saved);

    let dialogs = Arc::new(ScriptedDialogs::new(UnsavedChoice::Discard));
    let mut h = harness_with(dialogs.clone(), storage);
    // A clean project: the gate has nothing to ask about.
    h.state().mark_saved_baseline();
    let before = h.state().graph.lock().unwrap().node_count();
    assert_ne!(before, seeded_nodes, "the fixture must differ from screen");

    // Open the bar wide enough for the grid to have a cell.
    *h.state().favorites_bar_width.lock().unwrap() = 420.0;
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.pump_until_idle(8);
    h.frame();
    assert_eq!(prop(&h, EMBEDDED_BROWSER_ID, "entries"), "1");

    let cell = embedded_cell(&h, 0);
    h.double_click_local(cell, MouseButton::Left);
    h.pump_until_idle(8);

    assert_eq!(dialogs.prompts(), 0, "a clean project prompts nobody");
    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        seeded_nodes,
        "the activated project must be the one on screen"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        Some(saved),
        "and Save must now target it"
    );
}

/// …and it goes through the unsaved-changes gate: answering Cancel leaves
/// the user's work alone. This is why the bar routes through
/// `menu_actions::open_project_gated` instead of calling `AppState`
/// directly.
#[test]
fn activating_a_project_with_unsaved_changes_respects_the_gate() {
    let storage = memory_registry();
    let saved = uri("/bracket.atmr");
    seed_project(&storage, &saved);

    let dialogs = Arc::new(ScriptedDialogs::new(UnsavedChoice::Cancel));
    let mut h = harness_with(dialogs.clone(), storage);
    // Dirty the project so the gate has something to protect.
    {
        let state = h.state().clone();
        let mut graph = state.graph.lock().unwrap();
        graph
            .add_new_node("Sphere", [900.0, 100.0], &state.registry)
            .expect("Sphere is a built-in");
    }
    assert!(h.state().has_unsaved_changes());
    let before = h.state().graph.lock().unwrap().node_count();

    *h.state().favorites_bar_width.lock().unwrap() = 420.0;
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.pump_until_idle(8);
    h.frame();

    let cell = embedded_cell(&h, 0);
    h.double_click_local(cell, MouseButton::Left);
    h.pump_until_idle(8);

    assert_eq!(dialogs.prompts(), 1, "the gate asked");
    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        before,
        "Cancel means the project on screen is untouched"
    );
    assert_eq!(h.state().current_file.lock().unwrap().clone(), None);
}

// ── Pin + persistence ───────────────────────────────────────────────────

/// The strip's bottom "pin current project" item pins the open project —
/// the one way a `Project` favourite gets created. (The strip's own
/// contents, scrolling, and the deliberate absence of an unpin gesture
/// live in `tests/favorites_strip.rs`.)
#[test]
fn the_pin_item_pins_the_open_project() {
    let storage = memory_registry();
    let saved = uri("/bracket.atmr");
    seed_project(&storage, &saved);
    let mut h = harness_with(
        Arc::new(ScriptedDialogs::new(UnsavedChoice::Discard)),
        storage,
    );
    *h.state().current_file.lock().unwrap() = Some(saved.clone());
    h.frame();

    // The pin item is anchored to the strip's bottom; ask the same
    // geometry the widget laid out with.
    let bar = rect_of(&h, BAR_ID);
    let count: usize = prop(&h, BAR_ID, "favorites").parse().unwrap();
    let layout = bar_geom::compute(
        Size::new(bar.width, bar.height),
        expanded(&h),
        count,
        true,
        scroll_offset(&h),
    );
    let local = layout
        .pin
        .expect("an open project offers a pin item")
        .center();
    h.click_local(
        Point::new(bar.x + local.x, bar.y + local.y),
        MouseButton::Left,
    );

    assert!(
        h.state()
            .favorites
            .lock()
            .unwrap()
            .contains(FavoriteKind::Project, &saved.to_string()),
        "the open project is now pinned"
    );
    assert_eq!(
        prop(&h, BAR_ID, "favorites"),
        (count + 1).to_string(),
        "and shows up as a strip item"
    );
}

/// The 6d-1 report's splice trap, closed: `AppState::ui_settings` reads the
/// live favorites slot, so a shell that snapshots-and-writes (both do, once
/// per frame) can no longer clear the user's row. Bar geometry rides along.
#[test]
fn bar_state_and_favorites_survive_the_settings_round_trip() {
    let mut h = TestHarness::with_starter_graph();
    drag_handle(&mut h, 300.0);
    assert!(expanded(&h));

    let live: Favorites = h.state().favorites.lock().unwrap().clone();
    assert!(!live.is_empty(), "there is a row to lose");

    // Exactly what `demo-native::compose_settings_blob` writes.
    let blob = h.state().ui_settings().to_text();
    let reloaded = UiSettings::from_text(&blob);
    assert_eq!(reloaded.favorites, live, "the row survived the snapshot");
    assert!(reloaded.favorites_bar_expanded);
    assert!(reloaded.favorites_bar_width > MIN_EXPANDED_W);

    // And applying the blob back reproduces the same bar.
    let fresh = fresh_state_with_starter_graph_and_storage(memory_registry());
    fresh.apply_ui_settings(&reloaded);
    assert_eq!(*fresh.favorites.lock().unwrap(), live);
    assert!(*fresh.favorites_bar_expanded.lock().unwrap());
    assert_eq!(
        *fresh.favorites_bar_width.lock().unwrap(),
        reloaded.favorites_bar_width
    );
}

/// A row the user deliberately cleared stays cleared: seeding runs once
/// per settings file, and `apply_ui_settings` is where the flag is read.
#[test]
fn an_emptied_row_is_not_reseeded_on_apply() {
    let state: AppState = fresh_state_with_starter_graph_and_storage(memory_registry());
    let mut settings = state.ui_settings();
    settings.favorites.clear();
    assert!(settings.favorites.seeded());
    state.apply_ui_settings(&settings);
    assert!(
        state.favorites.lock().unwrap().is_empty(),
        "an emptied row must not be re-seeded"
    );
}
