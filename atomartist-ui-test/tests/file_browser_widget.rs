//! The shared file-browser widget, driven by real events.
//!
//! No NodeDesigner counterpart file: the ancestor's browser is
//! `static/js/node-editor/ui/file-browser-dialog.js` +
//! `file-browser-file-grid.js`, whose behaviour these tests port —
//! sidebar/breadcrumb navigation, click-to-select, double-click-to-enter,
//! first-class loading/empty/error states, and the save-mode name field
//! that fills from the selection.
//!
//! The widget is not in `build_app` yet (its modal host is step 6c), so
//! these mount it through [`WidgetHarness`] — a real `agg_gui::App` with
//! the browser as its root — rather than wiring it into the production
//! tree early. Assertions go through the widget's `properties()`
//! reflection (design §6) and through the model / cache handles the test
//! cloned before mounting; never through pixels.

use std::sync::{Arc, Mutex};

use agg_gui::{MouseButton, Point, Rect, Size};
use atomartist_storage::{
    FlakyConfig, FlakyProvider, MemoryProvider, Precondition, StorageProvider, StorageRegistry,
    StorageUri,
};
use atomartist_ui::file_browser::widget_geom::{self as geom, BrowserLayout};
use atomartist_ui::file_browser::{BrowserMode, BrowserModel, FileBrowser, ThumbnailCache};
use atomartist_ui::AppState;
use atomartist_ui_test::WidgetHarness;

const W: f64 = 900.0;
const H: f64 = 600.0;

fn uri(scheme: &str, path: &str) -> StorageUri {
    StorageUri::new(scheme, path)
}

/// Plant a file (creating its ancestors implicitly).
fn put(provider: &dyn StorageProvider, scheme: &str, path: &str) {
    provider
        .write(
            &uri(scheme, path),
            b"not a real package".to_vec(),
            Precondition::None,
        )
        .take()
        .expect("memory writes settle inline")
        .expect("seed write succeeds");
}

fn state_with(registry: StorageRegistry) -> AppState {
    AppState::with_storage(
        atomartist_lib::Graph::new(),
        atomartist_lib::registry::NodeRegistry::new(),
        Arc::new(registry),
    )
}

/// A memory store seeded with two projects, a text file and a
/// subdirectory holding one more project.
fn seeded_registry() -> (StorageRegistry, Arc<MemoryProvider>) {
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    put(provider.as_ref(), "mem", "/alpha.atmr");
    put(provider.as_ref(), "mem", "/beta.atmr");
    put(provider.as_ref(), "mem", "/notes.txt");
    put(provider.as_ref(), "mem", "/docs/inner.atmr");
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    (registry, provider)
}

/// Mount a browser over `registry` in `mode`, returning the harness plus
/// the model and cache handles the test keeps a view on.
fn mount(
    registry: StorageRegistry,
    mode: BrowserMode,
) -> (WidgetHarness, BrowserModel, ThumbnailCache, AppState) {
    mount_with(registry, mode, |browser| browser)
}

fn mount_with(
    registry: StorageRegistry,
    mode: BrowserMode,
    decorate: impl FnOnce(FileBrowser) -> FileBrowser,
) -> (WidgetHarness, BrowserModel, ThumbnailCache, AppState) {
    let state = state_with(registry);
    let model = BrowserModel::opened_on(&state);
    let cache = ThumbnailCache::new();
    let browser = decorate(FileBrowser::new(
        state.clone(),
        model.clone(),
        cache.clone(),
        mode,
    ));
    let harness = WidgetHarness::mount(state.clone(), Box::new(browser), W, H);
    (harness, model, cache, state)
}

fn layout(mode: BrowserMode) -> BrowserLayout {
    BrowserLayout::compute(Size::new(W, H), mode)
}

/// Centre of the cell showing `index`, in root-local Y-up coordinates.
fn cell_center(model: &BrowserModel, mode: BrowserMode, index: usize) -> Point {
    let layout = layout(mode);
    let count = model.visible_entries().len();
    let geo = geom::grid_geometry(layout.grid, count);
    geom::cell_rect(layout.grid, &geo, index, 0.0).center()
}

/// Index of the entry named `name` in the current listing.
fn index_of(model: &BrowserModel, name: &str) -> usize {
    model
        .visible_entries()
        .iter()
        .position(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("listing has no entry named {name}"))
}

fn entry_names(model: &BrowserModel) -> Vec<String> {
    model
        .visible_entries()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

fn prop(h: &WidgetHarness, key: &str) -> String {
    h.property(key)
        .unwrap_or_else(|| panic!("the browser exposes no `{key}` property"))
}

// ── Navigation ────────────────────────────────────────────────────────────

/// The sidebar is the provider list: clicking a row browses that
/// provider's root, and the row for the provider on screen is the current
/// one.
#[test]
fn sidebar_click_switches_provider_root() {
    let (mut registry, _memory) = seeded_registry();
    let other = Arc::new(MemoryProvider::new("other", "Other Store"));
    put(other.as_ref(), "other", "/only-here.atmr");
    registry
        .register(other as Arc<dyn StorageProvider>)
        .expect("second provider registers");

    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Open);
    assert_eq!(prop(&h, "cwd"), "mem:///");

    let rows = geom::sidebar_rows(layout(BrowserMode::Open).sidebar, model.roots().len());
    h.click_local(rows[1].center(), MouseButton::Left);

    assert_eq!(prop(&h, "cwd"), "other:///");
    assert_eq!(entry_names(&model), vec!["only-here.atmr"]);
}

/// Double-clicking a directory enters it — and the grid immediately shows
/// the new directory's entries, not the old ones.
#[test]
fn double_clicking_a_directory_enters_it_and_the_listing_updates() {
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Open);
    assert_eq!(prop(&h, "listing"), "Ready");
    assert_eq!(prop(&h, "entries"), "4");

    let docs = cell_center(&model, BrowserMode::Open, index_of(&model, "docs"));
    h.double_click_local(docs, MouseButton::Left);

    assert_eq!(prop(&h, "cwd"), "mem:///docs");
    assert_eq!(entry_names(&model), vec!["inner.atmr"]);
    assert_eq!(prop(&h, "entries"), "1");
}

/// Clicking a crumb walks back up the trail the model built.
#[test]
fn breadcrumb_click_navigates_to_that_step() {
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Open);

    let docs = cell_center(&model, BrowserMode::Open, index_of(&model, "docs"));
    h.double_click_local(docs, MouseButton::Left);
    assert_eq!(prop(&h, "cwd"), "mem:///docs");

    // Two crumbs now: the provider root, then "docs". Click the root.
    let crumbs = model.breadcrumbs();
    assert_eq!(crumbs.len(), 2, "root + one level");
    let rects = geom::crumb_rects(layout(BrowserMode::Open).crumbs, &crumbs);
    h.click_local(rects[0].center(), MouseButton::Left);

    assert_eq!(prop(&h, "cwd"), "mem:///");
    assert_eq!(prop(&h, "entries"), "4");
}

/// A single click selects and nothing else — the directory on screen does
/// not change.
#[test]
fn single_click_selects_without_navigating() {
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Open);

    let beta = cell_center(&model, BrowserMode::Open, index_of(&model, "beta.atmr"));
    h.click_local(beta, MouseButton::Left);

    assert_eq!(prop(&h, "selected"), "beta.atmr");
    assert_eq!(prop(&h, "cwd"), "mem:///", "selection must not navigate");
    assert_eq!(model.selected(), Some(uri("mem", "/beta.atmr")));
}

/// A third rapid press must not activate again.
///
/// agg-gui's multi-click tracker counts 1, 2, 3, 1, … inside its window,
/// so a "two or more" guard turns a triple-click into two activations: a
/// folder opened two levels deep, or a file handed to the host twice.
#[test]
fn a_triple_click_activates_exactly_once() {
    let picked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = picked.clone();
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount_with(registry, BrowserMode::Open, move |browser| {
        browser.on_activate(move |entry| sink.lock().unwrap().push(entry.name.clone()))
    });

    // Three presses at the same point, well inside the 400 ms window.
    let alpha = cell_center(&model, BrowserMode::Open, index_of(&model, "alpha.atmr"));
    h.click_local(alpha, MouseButton::Left);
    h.click_local(alpha, MouseButton::Left);
    h.click_local(alpha, MouseButton::Left);
    assert_eq!(
        picked.lock().unwrap().as_slice(),
        ["alpha.atmr"],
        "the third press must not re-activate"
    );

    // Same rule for a directory: one level down, not two. The store is
    // seeded so that the *next* directory sits in the same cell as the
    // one just entered — otherwise a stray third activation would have
    // nothing to enter and the bug would hide.
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    put(provider.as_ref(), "mem", "/outer/inner/leaf.atmr");
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Open);

    let first_cell = cell_center(&model, BrowserMode::Open, index_of(&model, "outer"));
    h.click_local(first_cell, MouseButton::Left);
    h.click_local(first_cell, MouseButton::Left);
    assert_eq!(prop(&h, "cwd"), "mem:///outer");
    assert_eq!(entry_names(&model), vec!["inner"], "`inner` is now cell 0");
    // The third press lands on `inner`, in the very cell `outer` used to
    // occupy, and must not enter it.
    h.click_local(first_cell, MouseButton::Left);
    assert_eq!(prop(&h, "cwd"), "mem:///outer", "one level, not two");
}

/// Double-clicking a *file* is an activation, delivered to the host — the
/// widget itself opens nothing (design §2: embedded mode delivers picks
/// via callback).
#[test]
fn activate_callback_fires_on_file_double_click() {
    let picked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = picked.clone();
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount_with(registry, BrowserMode::Open, move |browser| {
        browser.on_activate(move |entry| sink.lock().unwrap().push(entry.name.clone()))
    });

    let alpha = cell_center(&model, BrowserMode::Open, index_of(&model, "alpha.atmr"));
    h.double_click_local(alpha, MouseButton::Left);

    assert_eq!(picked.lock().unwrap().as_slice(), ["alpha.atmr"]);
    assert_eq!(
        prop(&h, "cwd"),
        "mem:///",
        "activating a file never navigates"
    );

    // A directory is entered instead of activated.
    let docs = cell_center(&model, BrowserMode::Open, index_of(&model, "docs"));
    h.double_click_local(docs, MouseButton::Left);
    assert_eq!(
        picked.lock().unwrap().len(),
        1,
        "directories do not activate"
    );
}

/// A click that arrives in the same event batch as the wheel that
/// scrolled must hit the row it is over *now*.
///
/// agg-gui delivers every queued event between redraws, so hit-testing
/// against the visible range published by the last layout would make this
/// click miss and clear the selection instead.
#[test]
fn a_click_right_after_a_wheel_hits_the_row_that_scrolled_into_view() {
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    for i in 0..200 {
        put(provider.as_ref(), "mem", &format!("/project-{i:03}.atmr"));
    }
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Open);

    let grid = layout(BrowserMode::Open).grid;
    let top_left = geom::cell_rect(
        grid,
        &geom::grid_geometry(grid, model.visible_entries().len()),
        0,
        0.0,
    )
    .center();

    // Wheel and click without a redraw in between: `app.on_mouse_wheel`
    // then `on_mouse_down` at the same point, exactly as a fast user's
    // events queue up.
    let scrolled = {
        let (x, y) = h.to_screen(top_left);
        // Far enough that the row now under the cursor was nowhere near
        // the visible range the last layout published — which is exactly
        // what a hit test against that stale range would miss.
        h.app_mut().on_mouse_wheel(x, y, -20.0);
        h.app_mut()
            .on_mouse_down(x, y, MouseButton::Left, agg_gui::Modifiers::default());
        h.app_mut()
            .on_mouse_up(x, y, MouseButton::Left, agg_gui::Modifiers::default());
        h.frame();
        prop(&h, "selected")
    };

    assert_ne!(scrolled, "", "the click must select the row now under it");
    assert_ne!(
        scrolled, "project-000.atmr",
        "the wheel scrolled that row off the top"
    );
    assert_eq!(
        model.selected_entry().map(|e| e.name),
        Some(scrolled),
        "and the selection must be a real entry, not a stale index"
    );
}

// ── Search ────────────────────────────────────────────────────────────────

/// Typing in the search field filters the grid to matching names.
#[test]
fn typing_in_the_search_field_narrows_the_grid() {
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Open);
    assert_eq!(prop(&h, "entries"), "4");

    let search = layout(BrowserMode::Open).search;
    h.click_local(search.center(), MouseButton::Left);
    h.type_text("alp");

    assert_eq!(prop(&h, "search"), "alp");
    assert_eq!(prop(&h, "entries"), "1");
    assert_eq!(entry_names(&model), vec!["alpha.atmr"]);
}

// ── Listing states ────────────────────────────────────────────────────────

/// An empty directory says so; it never paints as a blank pane.
#[test]
fn an_empty_directory_reports_the_empty_state() {
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let (h, _model, _cache, _state) = mount(registry, BrowserMode::Open);

    assert_eq!(prop(&h, "listing"), "Empty");
    assert_eq!(prop(&h, "entries"), "0");
}

/// A build with no providers has nowhere to browse and says that too.
#[test]
fn a_registry_with_no_providers_reports_the_error_state() {
    let (h, _model, _cache, _state) = mount(StorageRegistry::new(), BrowserMode::Open);

    assert!(
        prop(&h, "listing").starts_with("Error:"),
        "expected an error listing, got {}",
        prop(&h, "listing")
    );
    assert_eq!(prop(&h, "cwd"), "");
}

/// A listing still in flight paints the loading state, and only settles
/// once both clocks advance.
#[test]
fn a_listing_in_flight_reports_the_loading_state() {
    let inner = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    put(inner.as_ref(), "mem", "/alpha.atmr");
    let provider = Arc::new(FlakyProvider::new(
        inner,
        FlakyConfig {
            latency_ticks: 3,
            ..FlakyConfig::default()
        },
    ));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry");

    let (mut h, _model, _cache, state) = mount(registry, BrowserMode::Open);
    assert_eq!(prop(&h, "listing"), "Loading");

    for _ in 0..6 {
        provider.pump();
        state.pump_storage();
    }
    h.frame();
    assert_eq!(prop(&h, "listing"), "Ready");
    assert_eq!(prop(&h, "entries"), "1");
}

// ── Save mode ─────────────────────────────────────────────────────────────

/// Save mode shows a name field, and selecting a file fills it — the
/// ancestors' behaviour, and what makes "click a file, press Save"
/// overwrite that file.
#[test]
fn save_mode_name_field_fills_from_the_selection() {
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Save);
    assert_eq!(prop(&h, "mode"), "Save");
    assert_eq!(prop(&h, "name"), "");

    let beta = cell_center(&model, BrowserMode::Save, index_of(&model, "beta.atmr"));
    h.click_local(beta, MouseButton::Left);
    assert_eq!(prop(&h, "name"), "beta.atmr");

    // Selecting a *directory* is a navigation target, not a file name.
    let docs = cell_center(&model, BrowserMode::Save, index_of(&model, "docs"));
    h.click_local(docs, MouseButton::Left);
    assert_eq!(prop(&h, "name"), "beta.atmr", "a folder is not a file name");
}

/// Open mode has no name field at all, so its region is not carved out of
/// the grid.
#[test]
fn open_mode_has_no_name_field() {
    let (registry, _memory) = seeded_registry();
    let (h, _model, _cache, _state) = mount(registry, BrowserMode::Open);
    assert_eq!(prop(&h, "mode"), "Open");
    assert!(layout(BrowserMode::Open).name.is_none());
    assert_eq!(prop(&h, "name"), "");
}

// ── Thumbnail visibility gating ───────────────────────────────────────────

/// The whole point of the cache's frame gate: a directory far larger than
/// the viewport must only ever cost previews for the rows on screen.
#[test]
fn thumbnails_are_requested_only_for_the_rows_on_screen() {
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    let total = 400;
    for i in 0..total {
        put(provider.as_ref(), "mem", &format!("/project-{i:03}.atmr"));
    }
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry");

    let (mut h, _model, cache, _state) = mount(registry, BrowserMode::Open);
    assert_eq!(prop(&h, "entries"), total.to_string());

    let grid = layout(BrowserMode::Open).grid;
    let window = {
        let cols = ((grid.width / geom::CELL_W).floor() as usize).max(1);
        let rows = (grid.height / geom::CELL_H).ceil() as usize + 1;
        cols * rows
    };
    let after_first_frame = cache.entry_count();
    assert!(
        after_first_frame > 0,
        "the visible rows must actually be requested"
    );
    assert!(
        after_first_frame <= window,
        "one frame asked for {after_first_frame} previews with only {window} cells on screen"
    );
    assert!(
        after_first_frame * 4 < total,
        "the gate must cost far less than the whole directory ({after_first_frame} of {total})"
    );

    // Scrolling brings a new band into view; the cost grows by that band,
    // not by everything scrolled past.
    h.scroll_at(grid.center(), -4.0);
    let after_scroll = cache.entry_count();
    assert!(
        after_scroll > after_first_frame,
        "scrolling must reveal (and request) new rows"
    );
    assert!(
        after_scroll < total / 2,
        "a short scroll requested {after_scroll} of {total} previews"
    );

    // Nothing is left queued for a row that scrolled away: the cache
    // drops those instead of reading them.
    assert_eq!(cache.in_flight(), 0);
    assert_eq!(cache.queued(), 0);
}

/// Every listing state must survive a real software raster pass: the grid
/// is the one place the widget blits image buffers and measures text, and
/// a panic there would only ever show up on a user's frame.
#[test]
fn every_listing_state_paints_without_panicking() {
    // Ready, with a selection and a save-mode name field in play.
    let (registry, _memory) = seeded_registry();
    let (mut h, model, _cache, _state) = mount(registry, BrowserMode::Save);
    let alpha = cell_center(&model, BrowserMode::Save, index_of(&model, "alpha.atmr"));
    h.click_local(alpha, MouseButton::Left);
    h.paint_once();

    // Empty.
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let (mut h, _model, _cache, _state) = mount(registry, BrowserMode::Open);
    assert_eq!(prop(&h, "listing"), "Empty");
    h.paint_once();

    // Error.
    let (mut h, _model, _cache, _state) = mount(StorageRegistry::new(), BrowserMode::Open);
    h.paint_once();

    // Loading, plus the "search matched nothing" pane.
    let (registry, _memory) = seeded_registry();
    let (mut h, _model, _cache, _state) = mount(registry, BrowserMode::Open);
    let search = layout(BrowserMode::Open).search;
    h.click_local(search.center(), MouseButton::Left);
    h.type_text("no-such-file");
    assert_eq!(prop(&h, "entries"), "0");
    h.paint_once();
}

/// A scroll requested *before* the first layout must survive it.
///
/// The grid's extent is zero until a layout has run, so clamping eagerly
/// would silently discard the offset a host sets while building itself
/// (the modal revealing a selection). The clamp is deferred to the frame
/// that actually knows how tall the content is.
#[test]
fn a_scroll_set_before_the_first_layout_is_clamped_later_not_discarded() {
    use agg_gui::Widget;

    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    for i in 0..200 {
        put(provider.as_ref(), "mem", &format!("/project-{i:03}.atmr"));
    }
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let state = state_with(registry);
    let model = BrowserModel::opened_on(&state);
    let mut browser = FileBrowser::new(
        state.clone(),
        model,
        ThumbnailCache::new(),
        BrowserMode::Open,
    );

    browser.set_scroll_offset(500.0);
    assert_eq!(
        browser.scroll_offset(),
        500.0,
        "a pre-layout scroll must not be clamped away against a zero extent"
    );

    browser.layout(Size::new(W, H));
    assert_eq!(browser.scroll_offset(), 500.0, "and it survives the layout");

    // Once the extent is real, an over-scroll is clamped immediately.
    browser.set_scroll_offset(1.0e9);
    let clamped = browser.scroll_offset();
    assert!(
        clamped > 500.0 && clamped < 1.0e9,
        "post-layout scrolls clamp to the content ({clamped})"
    );
    browser.set_scroll_offset(-10.0);
    assert_eq!(browser.scroll_offset(), 0.0, "never negative");
}

/// Sanity: the mounted widget is findable by the id the design fixed for
/// it, so the modal and the bar can reach it the same way.
#[test]
fn the_browser_mounts_under_its_documented_id() {
    let (registry, _memory) = seeded_registry();
    let (h, _model, _cache, _state) = mount(registry, BrowserMode::Open);
    let widget = h.find_by_id("file-browser").expect("id is `file-browser`");
    assert_eq!(widget.type_name(), "FileBrowser");
    assert_eq!(widget.bounds(), Rect::new(0.0, 0.0, W, H));
}
