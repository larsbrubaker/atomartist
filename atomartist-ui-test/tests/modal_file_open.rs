//! The File menu driven through the in-app browser modal, end to end
//! (step 6c-2 of `docs/file-browser-design.md`).
//!
//! Where `file_browser_modal.rs` tests the dialog on its own — open it
//! through the handle, read the pick out of the job — these tests exercise
//! the *whole* user path the way a shell without OS dialogs runs it:
//!
//! ```text
//! menu action → ModalFileDialogs → browser modal → user's click →
//! the picker's JobOp continuation → AppState actually loads / writes
//! ```
//!
//! No NodeDesigner counterpart: the ancestor's dialog and its file menu
//! were wired through the same JavaScript event loop, so it had no seam
//! like this to test. Everything here goes through `TestHarness`'s
//! [`with_modal_dialogs`](atomartist_ui_test::TestHarness::with_modal_dialogs)
//! constructor, which is `demo-wasm`'s exact wiring.
//!
//! Coordinates: widget rectangles are Y-up and screen-absolute (agg-gui's
//! `find_widget_screen_rect`); `click_local` flips them into the Y-down
//! space events use.

use std::sync::Arc;

use agg_gui::widget::find_widget_screen_rect;
use agg_gui::{Key, MouseButton, Point, Rect, Size};
use atomartist_storage::{MemoryProvider, StorageProvider, StorageRegistry, StorageUri};
use atomartist_ui::file_browser::widget_geom::{self as geom, BrowserLayout};
use atomartist_ui::file_browser::BrowserMode;
use atomartist_ui::{fresh_state_with_starter_graph_and_storage, AppState};
use atomartist_ui_test::TestHarness;

const SCHEME: &str = "mem";

fn uri(path: &str) -> StorageUri {
    StorageUri::new(SCHEME, path)
}

/// A registry whose only provider is an empty in-memory store — one
/// provider so the browser's starting directory is unambiguous.
fn memory_registry() -> Arc<StorageRegistry> {
    let mut registry = StorageRegistry::new();
    registry
        .register(Arc::new(MemoryProvider::new(SCHEME, "Test Memory")) as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    Arc::new(registry)
}

/// Bytes stored at `uri`, or `None` when nothing is there.
fn stored(storage: &Arc<StorageRegistry>, uri: &StorageUri) -> Option<Vec<u8>> {
    storage.resolve(uri)?.read(uri).take()?.ok()
}

/// Run the frame pump until nothing is in flight, naming whatever refuses
/// to settle.
fn settle(state: &AppState) {
    for _ in 0..8 {
        if !state.pump_storage() {
            return;
        }
    }
    let outstanding: Vec<String> = state
        .pending_op_status_all()
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    panic!("storage ops never settled: {}", outstanding.join(", "));
}

// ── Modal geometry helpers (mirrors `file_browser_modal.rs`) ─────────────

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

fn ok_button(h: &TestHarness) -> Point {
    let panel = rect_of(h, "file-browser-modal");
    let local =
        atomartist_ui::file_browser::ModalLayout::compute(Size::new(panel.width, panel.height))
            .ok
            .center();
    Point::new(panel.x + local.x, panel.y + local.y)
}

fn browser_layout(h: &TestHarness, mode: BrowserMode) -> (Rect, BrowserLayout) {
    let browser = rect_of(h, "file-browser");
    (
        browser,
        BrowserLayout::compute(Size::new(browser.width, browser.height), mode),
    )
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

/// Absolute centre of the save-mode name field.
fn name_field(h: &TestHarness) -> Point {
    let (browser, layout) = browser_layout(h, BrowserMode::Save);
    let local = layout.name.expect("save mode has a name field").center();
    Point::new(browser.x + local.x, browser.y + local.y)
}

/// Clear the name field (clicked at its centre, i.e. past the end of the
/// short seeded name, so the caret sits at the end) and type `name`.
fn retype_name(h: &mut TestHarness, name: &str) {
    let field = name_field(h);
    h.click_local(field, MouseButton::Left);
    let seeded = prop(h, "file-browser", "name");
    for _ in 0..seeded.chars().count() {
        h.key_down(Key::Backspace);
    }
    h.type_text(name);
}

/// Write a real project into `storage` at `uri` and report how many nodes
/// it holds. Deliberately the starter graph *plus one node*, so a test can
/// tell "the stored project arrived" from "the graph was already like
/// that".
fn seed_project(storage: &Arc<StorageRegistry>, uri: &StorageUri) -> usize {
    let seed = fresh_state_with_starter_graph_and_storage(storage.clone());
    {
        let mut graph = seed.graph.lock().unwrap();
        graph
            .add_new_node("Box", [400.0, 400.0], &seed.registry)
            .expect("Box is a built-in");
    }
    seed.save_project(uri);
    settle(&seed);
    let count = seed.graph.lock().unwrap().node_count();
    count
}

// ── Open ────────────────────────────────────────────────────────────────

/// The deliverable of step 6c-2: File → Open puts the in-app browser up,
/// and clicking a project in it actually loads that project.
///
/// The stored project is deliberately *not* the starter graph (it carries
/// one extra node), so "the graph changed" cannot be confused with "the
/// graph was already like that".
#[test]
fn file_open_through_the_modal_loads_the_picked_project() {
    let storage = memory_registry();
    let saved = uri("/bracket.atmr");
    let seeded_nodes = seed_project(&storage, &saved);

    let mut h = TestHarness::with_modal_dialogs(fresh_state_with_starter_graph_and_storage(
        storage.clone(),
    ));
    let before = h.state().graph.lock().unwrap().node_count();
    assert_ne!(
        before, seeded_nodes,
        "the fixture must differ from on-screen"
    );

    h.menu_action("file.open");

    assert!(h.browser_modal().is_open(), "File → Open shows the browser");
    assert_eq!(prop(&h, "file-browser", "mode"), "Open");
    assert_eq!(prop(&h, "file-browser", "entries"), "1");
    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        before,
        "and changes nothing until the user answers it"
    );

    // Pick the project and confirm.
    let first = cell(&h, BrowserMode::Open, 0);
    h.click_local(first, MouseButton::Left);
    assert_eq!(prop(&h, "file-browser", "selected"), "bracket.atmr");
    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);
    assert!(!h.browser_modal().is_open(), "the dialog closed");

    h.pump_until_idle(8);

    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        seeded_nodes,
        "the picked project must be the one on screen"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        Some(saved.clone()),
        "and Save must now target it"
    );
    assert_eq!(
        h.state().recent_projects.lock().unwrap().first(),
        Some(&saved),
        "an opened project heads the recent list"
    );
}

/// Cancelling the picker leaves the project alone — the other half of the
/// same path, and the reason a cancelled pick posts no error.
#[test]
fn cancelling_the_open_picker_changes_nothing() {
    let storage = memory_registry();
    let mut h =
        TestHarness::with_modal_dialogs(fresh_state_with_starter_graph_and_storage(storage));
    let before = h.state().graph.lock().unwrap().node_count();

    h.menu_action("file.open");
    h.key_down(Key::Escape);
    h.frame();
    h.pump_until_idle(8);

    assert_eq!(h.state().graph.lock().unwrap().node_count(), before);
    assert_eq!(h.state().current_file.lock().unwrap().clone(), None);
    assert!(
        h.state().last_notice().is_none(),
        "a cancelled picker is not a failure and says nothing"
    );
}

// ── Save As ─────────────────────────────────────────────────────────────

/// File → Save As with a name the user types: the bytes land in the
/// provider under that name, and Save retargets to it.
#[test]
fn file_save_as_through_the_modal_writes_the_typed_name() {
    let storage = memory_registry();
    let mut h = TestHarness::with_modal_dialogs(fresh_state_with_starter_graph_and_storage(
        storage.clone(),
    ));

    h.menu_action("file.save_as");
    assert_eq!(prop(&h, "file-browser", "mode"), "Save");
    assert_eq!(
        prop(&h, "file-browser", "name"),
        "untitled.atmr",
        "the picker is seeded with the suggested name"
    );

    retype_name(&mut h, "gear");
    assert_eq!(prop(&h, "file-browser", "name"), "gear");

    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);
    h.pump_until_idle(8);

    let written = uri("/gear.atmr");
    let bytes = stored(&storage, &written).expect("Save As must write the project");
    assert!(!bytes.is_empty(), "and write something");
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        Some(written),
        "Save As retargets Save"
    );
    assert!(
        !h.state().has_unsaved_changes(),
        "a confirmed write re-baselines the dirty tracker"
    );
}

// ── Import ──────────────────────────────────────────────────────────────

/// File → Import picks with the same browser and merges the picked file
/// into the current scene instead of replacing it. The merge rule
/// (`file_menu_features::import_scene_file_atmr_merges_and_rewires_into_output`)
/// drops the imported Output node, so importing a 5-node project into a
/// 4-node one leaves 8.
#[test]
fn file_import_through_the_modal_merges_the_picked_file() {
    let storage = memory_registry();
    let seeded_nodes = seed_project(&storage, &uri("/part.atmr"));

    let mut h = TestHarness::with_modal_dialogs(fresh_state_with_starter_graph_and_storage(
        storage.clone(),
    ));
    let before = h.state().graph.lock().unwrap().node_count();

    h.menu_action("file.import");
    assert_eq!(prop(&h, "file-browser", "mode"), "Open");

    let first = cell(&h, BrowserMode::Open, 0);
    h.click_local(first, MouseButton::Left);
    assert_eq!(prop(&h, "file-browser", "selected"), "part.atmr");
    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);
    h.pump_until_idle(8);

    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        before + seeded_nodes - 1,
        "import merges the picked project, minus its Output node"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        None,
        "importing is not opening — it must not retarget Save"
    );
    assert!(
        h.state().last_notice().is_none(),
        "a successful import says nothing alarming: {:?}",
        h.state().last_notice()
    );
}

// ── Export ──────────────────────────────────────────────────────────────

/// File → Export → STL opens a picker that forces `.stl`, not the project
/// extension. Before the extension was parameterised, the same flow wrote
/// `export.stl.atmr`.
#[test]
fn file_export_picks_with_the_formats_own_extension() {
    let storage = memory_registry();
    let state = fresh_state_with_starter_graph_and_storage(storage.clone());
    state.evaluate_now();
    let mut h = TestHarness::with_modal_dialogs(state);

    h.menu_action("file.export.stl");
    assert_eq!(prop(&h, "file-browser", "mode"), "Save");
    assert_eq!(prop(&h, "file-browser", "name"), "export.stl");

    // Retype so the extension rule — not the seeded name — is what
    // decides the answer.
    retype_name(&mut h, "bracket");
    let ok = ok_button(&h);
    h.click_local(ok, MouseButton::Left);
    h.pump_until_idle(8);

    let mesh = stored(&storage, &uri("/bracket.stl"))
        .expect("the export must land under the format's extension");
    assert!(!mesh.is_empty());
    assert!(
        stored(&storage, &uri("/bracket.stl.atmr")).is_none(),
        "and must not have been forced to the project extension"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        None,
        "exporting is not saving — it must not retarget Save"
    );
}
