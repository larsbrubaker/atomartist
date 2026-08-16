//! What differs between the browser's **faces**, and how its grid
//! scrolls (`docs/file-browser-design.md` §5c, step 6g-2).
//!
//! Split off `tests/file_browser_widget.rs` — which owns navigation,
//! selection, search and listing states — to keep both under the
//! project's 800-line cap. No NodeDesigner counterpart file: the
//! ancestor's embedded parts browser (`file-browser-*.js`, mounted by
//! `parts-bar.js`) shows filter tabs + search + nav + grid and has no
//! provider list at all, which is the behaviour pinned here.
//!
//! Like its sibling, this mounts the widget through [`WidgetHarness`] —
//! a real `agg_gui::App` with the browser as its root — and asserts
//! through `properties()` reflection (design §6), never through pixels.

use std::sync::Arc;

use agg_gui::{MouseButton, Size};
use atomartist_storage::{
    MemoryProvider, Precondition, StorageProvider, StorageRegistry, StorageUri,
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

fn mount(registry: StorageRegistry, mode: BrowserMode) -> (WidgetHarness, BrowserModel) {
    let state = state_with(registry);
    let model = BrowserModel::opened_on(&state);
    let browser = FileBrowser::new(state.clone(), model.clone(), ThumbnailCache::new(), mode);
    let harness = WidgetHarness::mount(state, Box::new(browser), W, H);
    (harness, model)
}

fn layout(mode: BrowserMode) -> BrowserLayout {
    BrowserLayout::compute(Size::new(W, H), mode)
}

fn prop(h: &WidgetHarness, key: &str) -> String {
    h.property(key)
        .unwrap_or_else(|| panic!("the browser exposes no `{key}` property"))
}

/// Two providers and a couple of files, so the sidebar has something to
/// list and the grid something to tile.
fn two_providers() -> StorageRegistry {
    let memory = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    put(memory.as_ref(), "mem", "/alpha.atmr");
    put(memory.as_ref(), "mem", "/beta.atmr");
    let other = Arc::new(MemoryProvider::new("other", "Other Store"));
    put(other.as_ref(), "other", "/only-here.atmr");
    let mut registry = StorageRegistry::new();
    registry
        .register(memory as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    registry
        .register(other as Arc<dyn StorageProvider>)
        .expect("second provider registers");
    registry
}

/// The embedded face has no provider sidebar; the modal keeps its
/// (step 6g-2).
///
/// Inside a 380 px favorites panel a 150 px sidebar leaves 218 px of
/// content, i.e. exactly one `minmax(120px, 1fr)` column — the "cards
/// render in one narrow column" the second build review reported.
#[test]
fn the_embedded_face_drops_the_provider_sidebar_the_modal_keeps() {
    let (h, _model) = mount(two_providers(), BrowserMode::Open);
    assert_eq!(prop(&h, "sidebar"), "2", "the modal lists both providers");
    let modal_cols: usize = prop(&h, "grid_cols").parse().unwrap();

    let (mut h, model) = mount(two_providers(), BrowserMode::Embedded);
    assert_eq!(prop(&h, "sidebar"), "0", "the embedded face has no rows");
    assert_eq!(layout(BrowserMode::Embedded).sidebar.width, 0.0);

    // Clicking where the modal's second provider row would be must not
    // navigate — that column is the grid now.
    let rows = geom::sidebar_rows(layout(BrowserMode::Open).sidebar, model.roots().len());
    h.click_local(rows[1].center(), MouseButton::Left);
    assert_eq!(
        prop(&h, "cwd"),
        "mem:///",
        "no provider switch when embedded"
    );

    // And the grid really did take the freed width.
    let embedded_cols: usize = prop(&h, "grid_cols").parse().unwrap();
    assert!(
        embedded_cols > modal_cols,
        "embedded {embedded_cols} columns vs modal {modal_cols} at the same size"
    );
}

/// One wheel notch scrolls the grid one browser-normal step — the 6g-2
/// complaint was that a notch jumped most of a page.
///
/// The number the widget receives is a **notch** count (agg-gui's
/// convention: its `ScrollView` multiplies by 40, and the shells divide
/// their OS pixel deltas down to notches), so this pins the product.
#[test]
fn one_wheel_notch_scrolls_the_grid_by_one_step() {
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    for i in 0..200 {
        put(provider.as_ref(), "mem", &format!("/project-{i:03}.atmr"));
    }
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let (mut h, _model) = mount(registry, BrowserMode::Open);
    let grid = layout(BrowserMode::Open).grid;
    assert_eq!(prop(&h, "scroll"), "0.0");

    h.scroll_at(grid.center(), -1.0);
    assert_eq!(
        prop(&h, "scroll"),
        format!("{:.1}", geom::GRID_SCROLL_STEP),
        "one notch is one step down"
    );
    assert!(
        geom::GRID_SCROLL_STEP < geom::CARD_H,
        "and a step is less than one card, never a page"
    );

    h.scroll_at(grid.center(), -3.0);
    assert_eq!(
        prop(&h, "scroll"),
        format!("{:.1}", geom::GRID_SCROLL_STEP * 4.0),
        "notches accumulate"
    );

    // Clamping still holds at both ends.
    h.scroll_at(grid.center(), 100.0);
    assert_eq!(prop(&h, "scroll"), "0.0", "never negative");
    h.scroll_at(grid.center(), -1000.0);
    assert_eq!(
        prop(&h, "scroll"),
        prop(&h, "max_scroll"),
        "and never past the end of the content"
    );
}
