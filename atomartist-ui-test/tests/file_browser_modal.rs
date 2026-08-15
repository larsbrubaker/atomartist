//! The Open/Save modal, driven through the real production tree.
//!
//! No single NodeDesigner counterpart file: the ancestor's dialog is
//! `static/js/node-editor/ui/file-browser-dialog.js`, whose modal face
//! these tests port — a picker that mounts over the app, swallows input to
//! everything behind it, confirms on OK / double-click, cancels on Cancel
//! and Escape, and refuses to confirm a pick that isn't one.
//!
//! Unlike `file_browser_widget.rs` (which mounts the bare widget), the
//! modal host *is* part of `build_app` now, so everything here goes
//! through [`TestHarness`]: the dialog is opened through the same
//! `FileBrowserModalHandle` step 6c-2's `FileDialogProvider` will hold,
//! and the result is read out of the `Job` that `open` returns.
//!
//! Coordinates: widget rectangles are Y-up and screen-absolute (via
//! agg-gui's `find_widget_screen_rect`), and the harness's `click_local`
//! flips them into the Y-down screen space events use.

use std::rc::Rc;
use std::sync::Arc;

use agg_gui::widget::find_widget_screen_rect;
use agg_gui::{Key, MouseButton, Point, Rect, Size};
use atomartist_storage::{
    MemoryProvider, Precondition, StorageProvider, StorageRegistry, StorageUri,
};
use atomartist_ui::file_browser::widget_geom::{self as geom, BrowserLayout};
use atomartist_ui::file_browser::{
    BrowserMode, BrowserModel, FileBrowser, FileBrowserModal, ModalLayout, ThumbnailCache,
    PANEL_SIZE,
};
use atomartist_ui::{
    fresh_state_with_builtins_and_storage, fresh_state_with_starter_graph_and_storage,
};
use atomartist_ui_test::{TestHarness, WidgetHarness};

fn uri(path: &str) -> StorageUri {
    StorageUri::new("mem", path)
}

/// A memory store holding one directory and two projects. Sorted by the
/// model's rule (directories first, then case-insensitive by name) the
/// listing is `docs`, `alpha.atmr`, `beta.atmr`.
fn seeded_registry() -> StorageRegistry {
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    for path in ["/alpha.atmr", "/beta.atmr", "/docs/inner.atmr"] {
        provider
            .write(
                &uri(path),
                b"not a real package".to_vec(),
                Precondition::None,
            )
            .take()
            .expect("memory writes settle inline")
            .expect("seed write succeeds");
    }
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    registry
}

const DOCS: usize = 0;
const ALPHA: usize = 1;
const BETA: usize = 2;

fn harness() -> TestHarness {
    TestHarness::with_app_state(fresh_state_with_builtins_and_storage(Arc::new(
        seeded_registry(),
    )))
}

/// Absolute rectangle of the widget carrying `id`, or a panic naming it.
fn rect_of(h: &TestHarness, id: &str) -> Rect {
    find_widget_screen_rect(h.app().root(), id)
        .unwrap_or_else(|| panic!("no visible widget with id `{id}`"))
}

/// A panel-local Y-up point lifted into absolute coordinates.
fn in_panel(h: &TestHarness, local: Point) -> Point {
    let panel = rect_of(h, "file-browser-modal");
    Point::new(panel.x + local.x, panel.y + local.y)
}

fn panel_layout(h: &TestHarness) -> ModalLayout {
    let panel = rect_of(h, "file-browser-modal");
    ModalLayout::compute(Size::new(panel.width, panel.height))
}

fn ok_button(h: &TestHarness) -> Point {
    in_panel(h, panel_layout(h).ok.center())
}

fn cancel_button(h: &TestHarness) -> Point {
    in_panel(h, panel_layout(h).cancel.center())
}

fn browser_layout(h: &TestHarness, mode: BrowserMode) -> (Rect, BrowserLayout) {
    let browser = rect_of(h, "file-browser");
    let layout = BrowserLayout::compute(Size::new(browser.width, browser.height), mode);
    (browser, layout)
}

/// Absolute centre of the grid cell at `index`.
fn cell(h: &TestHarness, mode: BrowserMode, index: usize) -> Point {
    let (browser, layout) = browser_layout(h, mode);
    let count = prop(h, "file-browser", "entries")
        .parse::<usize>()
        .expect("the entry count is a number");
    let geo = geom::grid_geometry(layout.grid, count);
    let local = geom::cell_rect(layout.grid, &geo, index, 0.0).center();
    Point::new(browser.x + local.x, browser.y + local.y)
}

/// Absolute centre of the search box (its text-field part).
fn search_box(h: &TestHarness, mode: BrowserMode) -> Point {
    let (browser, layout) = browser_layout(h, mode);
    let local = layout.search_field.center();
    Point::new(browser.x + local.x, browser.y + local.y)
}

/// Absolute centre of the save-mode name field.
fn name_field(h: &TestHarness) -> Point {
    let (browser, layout) = browser_layout(h, BrowserMode::Save);
    let local = layout.name.expect("save mode has a name field").center();
    Point::new(browser.x + local.x, browser.y + local.y)
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

// ── Mounting ──────────────────────────────────────────────────────────────

/// Opening through the handle mounts the shared browser inside the app's
/// own tree — the modal is not a separate window.
#[test]
fn opening_through_the_handle_mounts_the_browser() {
    let mut h = harness();
    assert!(h.find_by_id("file-browser").is_none(), "hidden by default");
    assert!(!h.browser_modal().is_open());

    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    assert!(h.browser_modal().is_open());
    assert!(job.poll().is_pending(), "nothing picked yet");
    assert!(h.find_by_id("file-browser").is_some());
    assert_eq!(prop(&h, "file-browser-modal", "title"), "Open Project");
    assert_eq!(prop(&h, "file-browser", "mode"), "Open");
    assert_eq!(prop(&h, "file-browser", "listing"), "Ready");
    assert_eq!(prop(&h, "file-browser", "entries"), "3");
}

/// Save mode is titled for saving and its name field is seeded from the
/// caller's default.
#[test]
fn save_mode_opens_titled_and_pre_named() {
    let mut h = harness();
    let _job = h
        .browser_modal()
        .open(BrowserMode::Save, "Untitled Project");
    h.frame();

    assert_eq!(prop(&h, "file-browser-modal", "title"), "Save Project");
    assert_eq!(prop(&h, "file-browser", "mode"), "Save");
    assert_eq!(prop(&h, "file-browser", "name"), "Untitled Project");
}

// ── Open mode ─────────────────────────────────────────────────────────────

/// Select a file, press Open: the job settles with that file's URI and
/// the dialog goes away.
#[test]
fn open_mode_ok_settles_the_selected_file() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    let alpha = cell(&h, BrowserMode::Open, ALPHA);
    h.click_local(alpha, MouseButton::Left);
    assert_eq!(prop(&h, "file-browser", "selected"), "alpha.atmr");
    assert_eq!(prop(&h, "file-browser-modal", "ok_enabled"), "true");

    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);

    assert_eq!(job.take(), Some(Ok(Some(uri("/alpha.atmr")))));
    assert!(h.find_by_id("file-browser").is_none(), "the dialog closed");
    assert!(!h.browser_modal().is_open());
}

/// Open is meaningless without a selection, so it is disabled — and a
/// click on it changes nothing at all.
#[test]
fn open_is_refused_and_disabled_without_a_selection() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();
    assert_eq!(prop(&h, "file-browser-modal", "ok_enabled"), "false");

    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);

    assert!(job.poll().is_pending(), "nothing was picked");
    assert!(h.find_by_id("file-browser").is_some(), "and nothing closed");

    // A *directory* selection is no better: it is a place, not a project.
    let docs = cell(&h, BrowserMode::Open, DOCS);
    h.click_local(docs, MouseButton::Left);
    assert_eq!(prop(&h, "file-browser", "selected"), "docs");
    assert_eq!(prop(&h, "file-browser-modal", "ok_enabled"), "false");
    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);
    assert!(job.poll().is_pending());
}

/// Double-clicking a file is pressing Open — the ancestors' shortcut.
#[test]
fn double_clicking_a_file_settles_the_pick() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    let beta = cell(&h, BrowserMode::Open, BETA);
    h.double_click_local(beta, MouseButton::Left);

    assert_eq!(job.take(), Some(Ok(Some(uri("/beta.atmr")))));
    assert!(h.find_by_id("file-browser").is_none());
}

/// Cancel settles `None` — "the user picked nothing", which every caller
/// already treats as an abort.
#[test]
fn cancel_settles_none() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    // Even with a perfectly good selection in hand.
    let alpha = cell(&h, BrowserMode::Open, ALPHA);
    h.click_local(alpha, MouseButton::Left);
    let cancel = cancel_button(&h);
    h.click_local(cancel, MouseButton::Left);

    assert_eq!(job.take(), Some(Ok(None)));
    assert!(h.find_by_id("file-browser").is_none());
    assert!(!h.browser_modal().is_open());
}

/// Escape is Cancel (agg-gui's `ModalSheet` closes on it; the host settles
/// the job the same way).
#[test]
fn escape_settles_none() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    h.key_down(Key::Escape);
    h.frame();

    assert_eq!(job.take(), Some(Ok(None)));
    assert!(h.find_by_id("file-browser").is_none());
}

/// Escape with a search in the box clears the *search* and leaves the
/// dialog up; only the next Escape cancels (step 6f-3).
///
/// This is the one place the two Escape handlers meet: the browser
/// consumes it while there is a filter to drop, and ignores it otherwise
/// so `ModalSheet` still closes. Asserting the job is *unsettled* in
/// between is what makes the first half meaningful — a dialog that
/// closed would also have "cleared" the search.
#[test]
fn escape_clears_the_search_before_it_cancels_the_dialog() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    h.click_local(search_box(&h, BrowserMode::Open), MouseButton::Left);
    h.type_text("alp");
    assert_eq!(prop(&h, "file-browser", "search"), "alp");
    assert_eq!(prop(&h, "file-browser", "entries"), "1", "the filter bites");

    h.key_down(Key::Escape);
    h.frame();
    assert_eq!(
        prop(&h, "file-browser", "search"),
        "",
        "the first Escape clears the search"
    );
    assert_eq!(prop(&h, "file-browser", "entries"), "3", "and unfilters");
    assert!(
        h.find_by_id("file-browser").is_some(),
        "…and the dialog stays up"
    );
    assert!(
        job.poll().is_pending(),
        "…with its job still unsettled — the browser consumed the key"
    );
    assert!(h.browser_modal().is_open());

    // Nothing left to clear: the second Escape belongs to the sheet.
    h.key_down(Key::Escape);
    h.frame();
    assert_eq!(job.take(), Some(Ok(None)));
    assert!(h.find_by_id("file-browser").is_none());
    assert!(!h.browser_modal().is_open());
}

/// The job settles exactly once, whatever the user does afterwards: a
/// second press on OK (or an Escape after a confirm) must not overwrite
/// the pick or panic on an already-consumed completer.
#[test]
fn the_job_settles_exactly_once() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    let alpha = cell(&h, BrowserMode::Open, ALPHA);
    h.click_local(alpha, MouseButton::Left);
    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);
    // Second press at the same place, plus an Escape for good measure.
    h.click_local(ok, MouseButton::Left);
    h.key_down(Key::Escape);
    h.frame();

    assert_eq!(job.take(), Some(Ok(Some(uri("/alpha.atmr")))));
    assert_eq!(job.take(), None, "and there is nothing left to take");
}

/// A pick job can be settled by someone who never saw the dialog — the
/// status bar's "cancel all storage activity", or the shutdown drain,
/// reaching the `JobOp` that wraps it. The dialog must then take itself
/// down: an OK the completer would silently ignore is a picker that eats
/// the user's answer.
#[test]
fn a_job_cancelled_from_outside_takes_the_dialog_down() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();
    assert!(h.find_by_id("file-browser").is_some(), "the dialog is up");

    // Whoever holds the job gives up on it.
    job.cancel();
    h.frame();

    assert!(h.find_by_id("file-browser").is_none(), "the dialog closed");
    assert!(!h.browser_modal().is_open());
    assert_eq!(
        job.take(),
        Some(Err(atomartist_storage::StorageError::Cancelled)),
        "and the cancellation stands — the close must not settle over it"
    );

    // The handle is free again, which it would not be if the host had
    // kept the stranded session.
    let next = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();
    assert!(h.find_by_id("file-browser").is_some());
    assert!(next.poll().is_pending());
}

/// Every open lists afresh: where the *previous* dialog was browsing has
/// no bearing on where this one starts. The model is rebuilt per open
/// precisely so a directory that changed (or vanished) between two opens
/// is never shown from a cache.
#[test]
fn each_open_starts_from_a_fresh_listing() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();

    let docs = cell(&h, BrowserMode::Open, DOCS);
    h.double_click_local(docs, MouseButton::Left);
    assert_eq!(prop(&h, "file-browser", "cwd"), "mem:///docs");
    assert_eq!(prop(&h, "file-browser", "entries"), "1");

    let cancel = cancel_button(&h);
    h.click_local(cancel, MouseButton::Left);
    assert_eq!(job.take(), Some(Ok(None)));

    let _next = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();
    assert_eq!(prop(&h, "file-browser", "cwd"), "mem:///");
    assert_eq!(prop(&h, "file-browser", "entries"), "3");
    assert_eq!(
        prop(&h, "file-browser", "selected"),
        "",
        "and with nothing carried over from last time"
    );
}

// ── Save mode ─────────────────────────────────────────────────────────────

/// Save joins the typed name onto the directory on screen, appending the
/// project extension when the user typed none.
#[test]
fn save_mode_ok_joins_the_typed_name_onto_the_directory() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Save, "");
    h.frame();
    assert_eq!(prop(&h, "file-browser-modal", "ok_enabled"), "false");

    let field = name_field(&h);
    h.click_local(field, MouseButton::Left);
    h.type_text("design");
    assert_eq!(prop(&h, "file-browser", "name"), "design");
    assert_eq!(prop(&h, "file-browser-modal", "ok_enabled"), "true");

    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);

    assert_eq!(job.take(), Some(Ok(Some(uri("/design.atmr")))));
}

/// A seeded name is enough on its own — open, press Save.
#[test]
fn save_mode_ok_uses_the_seeded_default_name() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Save, "seeded");
    h.frame();

    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);

    assert_eq!(job.take(), Some(Ok(Some(uri("/seeded.atmr")))));
}

/// Double-clicking a file in *save* mode fills the name field and stops
/// there. Confirming an overwrite is a deliberate act — the user still
/// has to press Save — which is the ancestors' behaviour and the reason
/// activation is a host decision rather than a widget one.
#[test]
fn save_mode_double_click_fills_the_name_without_confirming() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Save, "");
    h.frame();

    let alpha = cell(&h, BrowserMode::Save, ALPHA);
    h.double_click_local(alpha, MouseButton::Left);

    assert_eq!(prop(&h, "file-browser", "name"), "alpha.atmr");
    assert!(job.poll().is_pending(), "a double click is not a Save");
    assert!(h.find_by_id("file-browser").is_some(), "and nothing closed");

    // …and the deliberate press does confirm, over the existing file.
    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);
    assert_eq!(job.take(), Some(Ok(Some(uri("/alpha.atmr")))));
}

/// An empty name is not a destination: Save stays disabled and refuses.
#[test]
fn save_mode_refuses_an_empty_name() {
    let mut h = harness();
    let job = h.browser_modal().open(BrowserMode::Save, "");
    h.frame();

    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);

    assert!(job.poll().is_pending(), "nothing was picked");
    assert!(h.find_by_id("file-browser").is_some(), "and nothing closed");
}

// ── Painting ──────────────────────────────────────────────────────────────

/// The panel chrome (title text, separator, the two buttons) must survive
/// a real software raster pass in both modes — a panic there would only
/// ever show up on a user's frame.
///
/// The whole-app harness never paints (no GPU), so this mounts just the
/// panel in a `WidgetHarness`, which does.
#[test]
fn the_panel_chrome_paints_in_both_modes() {
    // Constructing a `TestHarness` is what installs the bundled font.
    let _fonts = harness();
    let font = agg_gui::font_settings::current_system_font().expect("the harness installs one");

    for mode in [BrowserMode::Open, BrowserMode::Save] {
        let state = fresh_state_with_builtins_and_storage(Arc::new(seeded_registry()));
        let model = BrowserModel::opened_on(&state);
        let browser = FileBrowser::new(state.clone(), model, ThumbnailCache::new(), mode);
        let gate: Rc<dyn Fn() -> bool> = Rc::new(|| true);
        let panel = FileBrowserModal::new(mode, font.clone(), browser, gate, || {}, || {});
        WidgetHarness::mount(state, Box::new(panel), PANEL_SIZE.width, PANEL_SIZE.height)
            .paint_once();
    }
}

// ── Modality ──────────────────────────────────────────────────────────────

/// While the dialog is up, nothing behind it reacts — the scrim swallows
/// the click that would otherwise clear the canvas selection.
#[test]
fn an_open_modal_swallows_input_to_the_app_behind_it() {
    let state = fresh_state_with_starter_graph_and_storage(Arc::new(seeded_registry()));
    let mut h = TestHarness::with_app_state(state);

    // A point in the empty right-hand part of the node canvas — the same
    // spot `empty_canvas_click_clears_selection` uses, well outside the
    // centred dialog panel.
    let canvas = h.find_by_id("node-canvas").expect("canvas must exist");
    let b = canvas.bounds();
    let target = (b.x + b.width * 0.95, h.size().1 - (b.y + b.height * 0.5));

    let selected = Some(atomartist_lib::graph::node::NodeId(99));
    h.state().set_selection(selected);

    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();
    h.click(target.0, target.1, MouseButton::Left);

    assert_eq!(
        *h.state().selection.lock().unwrap(),
        selected,
        "the canvas must not see a click landing on the scrim"
    );
    assert!(job.poll().is_pending(), "and the dialog is still up");

    // Close it and repeat: the same click now reaches the canvas, which
    // proves the point really was a live target all along.
    h.key_down(Key::Escape);
    h.frame();
    h.click(target.0, target.1, MouseButton::Left);
    assert_eq!(*h.state().selection.lock().unwrap(), None);
}

/// Keys are swallowed too, not just clicks.
///
/// Delete on the focused node canvas removes the selected node — a
/// destructive key that must not fire while a picker is up. The canvas is
/// clicked (and so focused, and so armed) *before* the dialog opens,
/// because agg-gui offers keys to the focused widget first and only
/// re-routes them when a modal is active.
#[test]
fn an_open_modal_swallows_keys_to_the_app_behind_it() {
    let state = fresh_state_with_starter_graph_and_storage(Arc::new(seeded_registry()));
    let mut h = TestHarness::with_app_state(state);

    let victim = {
        let graph = h.state().graph.lock().unwrap();
        let node = graph.nodes().next().expect("the starter graph has nodes");
        node.id
    };
    let before = h.state().graph.lock().unwrap().nodes().count();

    // Select the node by clicking it, which is also what focuses the
    // canvas so its key handler is live.
    let target = node_center(&h, victim);
    h.click(target.0, target.1, MouseButton::Left);

    let job = h.browser_modal().open(BrowserMode::Open, "");
    h.frame();
    h.key_down(Key::Delete);

    assert_eq!(
        h.state().graph.lock().unwrap().nodes().count(),
        before,
        "a key must not reach the app while the dialog is up"
    );
    assert!(job.poll().is_pending(), "and the dialog is still up");

    // Close it and repeat: the same key now deletes, which proves the
    // canvas was armed all along.
    h.key_down(Key::Escape);
    h.frame();
    h.key_down(Key::Delete);
    assert_eq!(
        h.state().graph.lock().unwrap().nodes().count(),
        before - 1,
        "and lands the moment the dialog is gone"
    );
}

/// Screen (Y-down) centre of the canvas widget drawn for `node_id`.
///
/// The node-editor bakes each node's pan/zoom-transformed bounds into a
/// child `NodeWidget`, which `snapshot()` surfaces with absolute Y-up
/// `screen_bounds` — the same route `input_and_drill_e2e.rs` uses to
/// click node interiors.
fn node_center(h: &TestHarness, node_id: atomartist_lib::graph::node::NodeId) -> (f64, f64) {
    let want = node_id.0.to_string();
    let bounds = h
        .snapshot()
        .into_iter()
        .find_map(|n| {
            let is_node = n.type_name == "NodeWidget"
                && n.properties
                    .iter()
                    .any(|(key, value)| *key == "node_id" && *value == want);
            is_node.then_some(n.screen_bounds)
        })
        .expect("the node has a widget on the canvas");
    let center = Point::new(
        bounds.x + bounds.width * 0.5,
        bounds.y + bounds.height * 0.5,
    );
    h.to_screen(center)
}
