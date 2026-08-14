//! File-menu feature coverage: unsaved-changes gate, recent-projects
//! list (state + persistence + menu rendering), scene import
//! (.stl / .mcx / .atmr into the current graph), and the Export
//! format family.
//!
//! These test the production `AppState` / menu-composition code
//! directly — no windowing needed.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use atomartist_storage::{Precondition, StorageRegistry, StorageUri};
use atomartist_ui::menu_actions::confirm_discard_unsaved;
use atomartist_ui::top_menu_bar::{FileDialogProvider, UnsavedChoice};
use atomartist_ui::{
    fresh_state_with_starter_graph_and_storage, uri_exists, AppState, MeshExportFormat, UiSettings,
};
use atomartist_ui_test::{memory_uri, test_storage_registry};

/// Starter-graph state over the shared test registry (an in-memory
/// `mem:` store plus, on native, the real filesystem for `file:` URIs).
fn starter_state() -> AppState {
    fresh_state_with_starter_graph_and_storage(test_storage_registry())
}

/// `file:` URI for a bundled mesh fixture. Fixtures are real checked-in
/// files, so these stay `file:`; every project the tests *write* lives
/// in `mem:` and never touches the disk.
fn mesh_fixture(name: &str) -> StorageUri {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("atomartist-lib")
        .join("tests")
        .join("meshes")
        .join(name);
    StorageUri::from_local_path(path).expect("fixture path has a URI form")
}

/// Plant bytes in the state's storage so production code can read them
/// back — the in-memory stand-in for "write a fixture to a temp file".
fn put(storage: &Arc<StorageRegistry>, uri: &StorageUri, bytes: Vec<u8>) {
    let provider = storage.resolve(uri).expect("provider for test URI");
    provider
        .write(uri, bytes, Precondition::None)
        .take()
        .expect("test provider completes synchronously")
        .expect("write succeeds");
}

/// Size of the blob stored at `uri`, or `None` when nothing is there.
fn stored_len(storage: &Arc<StorageRegistry>, uri: &StorageUri) -> Option<usize> {
    let provider = storage.resolve(uri)?;
    provider.read(uri).take()?.ok().map(|bytes| bytes.len())
}

/// Scripted dialog provider: answers the unsaved prompt with a fixed
/// choice, hands out a fixed save path, and counts prompt invocations.
struct ScriptedDialogs {
    unsaved_answer: UnsavedChoice,
    save_path: Option<StorageUri>,
    prompts: AtomicUsize,
    errors: Mutex<Vec<String>>,
}

impl ScriptedDialogs {
    fn new(answer: UnsavedChoice) -> Self {
        Self {
            unsaved_answer: answer,
            save_path: None,
            prompts: AtomicUsize::new(0),
            errors: Mutex::new(Vec::new()),
        }
    }
    fn with_save_path(mut self, uri: StorageUri) -> Self {
        self.save_path = Some(uri);
        self
    }
}

impl FileDialogProvider for ScriptedDialogs {
    fn pick_open_project(&self) -> Option<StorageUri> {
        None
    }
    fn pick_save_project(&self, _name: &str) -> Option<StorageUri> {
        self.save_path.clone()
    }
    fn pick_save_export(&self, _ext: &str, _name: &str) -> Option<StorageUri> {
        None
    }
    fn pick_import_file(&self) -> Option<StorageUri> {
        None
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

/// Make the graph differ from its saved baseline by adding a node.
fn dirty_the_graph(state: &AppState) {
    let mut g = state.graph.lock().unwrap();
    g.add_new_node("Box", [400.0, 400.0], &state.registry)
        .unwrap();
}

// ── Unsaved-changes gate ────────────────────────────────────────────────

#[test]
fn clean_state_proceeds_without_prompting() {
    let state = starter_state();
    let dialogs = ScriptedDialogs::new(UnsavedChoice::Cancel);
    assert!(confirm_discard_unsaved(&state, &dialogs));
    assert_eq!(dialogs.prompts.load(Ordering::SeqCst), 0);
}

#[test]
fn cancel_blocks_and_discard_proceeds_when_dirty() {
    let state = starter_state();
    dirty_the_graph(&state);
    assert!(state.has_unsaved_changes());

    let cancel = ScriptedDialogs::new(UnsavedChoice::Cancel);
    assert!(!confirm_discard_unsaved(&state, &cancel));
    assert_eq!(cancel.prompts.load(Ordering::SeqCst), 1);

    let discard = ScriptedDialogs::new(UnsavedChoice::Discard);
    assert!(confirm_discard_unsaved(&state, &discard));
    // Discard must not have saved anything or touched the graph.
    assert!(state.has_unsaved_changes());
}

#[test]
fn save_choice_saves_then_proceeds() {
    let state = starter_state();
    dirty_the_graph(&state);
    let uri = memory_uri("save_choice.atmr");

    let dialogs = ScriptedDialogs::new(UnsavedChoice::Save).with_save_path(uri.clone());
    assert!(confirm_discard_unsaved(&state, &dialogs));
    assert!(
        uri_exists(&state.storage, &uri),
        "Save choice must write the project"
    );
    assert!(!state.has_unsaved_changes(), "save re-baselines the tracker");
}

#[test]
fn save_choice_with_cancelled_picker_blocks() {
    let state = starter_state();
    dirty_the_graph(&state);
    // Save chosen but the file picker returns None → action must block.
    let dialogs = ScriptedDialogs::new(UnsavedChoice::Save);
    assert!(!confirm_discard_unsaved(&state, &dialogs));
}

// ── Recent projects ─────────────────────────────────────────────────────

#[test]
fn save_and_load_maintain_mru_order_and_dedupe() {
    let state = starter_state();
    let a = memory_uri("recent_a.atmr");
    let b = memory_uri("recent_b.atmr");
    state.save_graph_to_uri(&a).unwrap();
    state.save_graph_to_uri(&b).unwrap();
    state.load_graph_from_uri(&a).unwrap();

    let recent = state.recent_projects.lock().unwrap().clone();
    assert_eq!(recent[0], a, "most recent first");
    assert_eq!(recent[1], b);
    assert_eq!(recent.len(), 2, "re-opening a must dedupe, not duplicate");
}

#[test]
fn recent_projects_round_trip_through_settings_text() {
    let state = starter_state();
    let a = memory_uri("rt_a.atmr");
    let b = memory_uri("rt_b.atmr");
    state.save_graph_to_uri(&a).unwrap();
    state.save_graph_to_uri(&b).unwrap();

    let text = state.ui_settings().to_text();
    let parsed = UiSettings::from_text(&text);
    assert_eq!(parsed.recent_projects, vec![b.clone(), a.clone()]);
}

#[test]
fn file_menu_renders_recent_items_and_import_export() {
    use agg_gui::MenuEntry;
    let state = starter_state();
    let proj = memory_uri("menu_recent.atmr");
    state.save_graph_to_uri(&proj).unwrap();

    // Build the chrome and force a snapshot refresh via layout.
    agg_gui::set_device_scale(1.0);
    let font = test_font();
    let dialogs: std::sync::Arc<dyn FileDialogProvider> =
        std::sync::Arc::new(atomartist_ui::top_menu_bar::NoFileDialogs);
    let debug = atomartist_ui::DebugWindowHandles::new(Default::default());
    let mut chrome =
        atomartist_ui::top_menu_bar::build_menu_bar(state, font, dialogs, debug);
    use agg_gui::Widget as _;
    chrome.layout(agg_gui::Size::new(1280.0, 32.0));

    let file = chrome
        .menus()
        .iter()
        .find(|m| m.label == "File")
        .expect("File menu");
    let labels: Vec<String> = file
        .items
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Item(i) => Some(i.label.clone()),
            _ => None,
        })
        .collect();
    assert!(labels.iter().any(|l| l == "Open Recent"));
    assert!(labels.iter().any(|l| l.starts_with("Import")));
    assert!(labels.iter().any(|l| l == "Export"));

    // The Open Recent submenu holds the saved project by file name.
    let recent_item = file
        .items
        .iter()
        .find_map(|e| match e {
            MenuEntry::Item(i) if i.label == "Open Recent" => Some(i),
            _ => None,
        })
        .unwrap();
    let recent_labels: Vec<&str> = recent_item
        .submenu
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Item(i) => Some(i.label.as_str()),
            _ => None,
        })
        .collect();
    // The entry is labelled by the URI's last segment, whatever the
    // scheme — the same user-visible name the path version showed.
    assert_eq!(recent_labels, vec!["menu_recent.atmr"]);

    // The Export submenu offers all four formats.
    let export_item = file
        .items
        .iter()
        .find_map(|e| match e {
            MenuEntry::Item(i) if i.label == "Export" => Some(i),
            _ => None,
        })
        .unwrap();
    let export_actions: Vec<&str> = export_item
        .submenu
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Item(i) => i.action.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(
        export_actions,
        vec![
            "file.export.stl",
            "file.export.3mf",
            "file.export.obj",
            "file.export.atmr"
        ]
    );
}

fn test_font() -> std::sync::Arc<agg_gui::text::Font> {
    const FONT_BYTES: &[u8] =
        include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");
    std::sync::Arc::new(agg_gui::text::Font::from_bytes(FONT_BYTES.to_vec()).expect("font"))
}

// ── Import into current scene ───────────────────────────────────────────

#[test]
fn import_scene_file_stl_adds_connected_mesh_node() {
    let state = starter_state();
    let before = state.graph.lock().unwrap().node_count();
    let added = state
        .import_scene_file(&mesh_fixture("simple_box.stl"))
        .unwrap();
    assert_eq!(added, 1);
    assert_eq!(state.graph.lock().unwrap().node_count(), before + 1);
}

#[test]
fn import_scene_file_atmr_merges_and_rewires_into_output() {
    // Save the starter scene, then import it into itself: 4 nodes → 7
    // (the imported Output is dropped), and the imported Extrude must
    // be rewired into the surviving Output node.
    let state = starter_state();
    let proj = memory_uri("merge_me.atmr");
    state.save_graph_to_uri(&proj).unwrap();

    let added = state.import_scene_file(&proj).unwrap();
    assert_eq!(added, 3, "Output node must not be duplicated");
    let graph = state.graph.lock().unwrap();
    assert_eq!(graph.node_count(), 7);
    let output_id = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == "Output")
        .unwrap()
        .id;
    let feeders = graph
        .noodles()
        .iter()
        .filter(|n| n.to.node == output_id)
        .count();
    assert_eq!(
        feeders, 2,
        "original + imported pipelines both feed the Output"
    );
}

#[test]
fn import_scene_file_mcx_spawns_mesh_nodes_with_transform() {
    use atomartist_lib::graph::node::PortValue;
    // Synthesize a minimal .mcx: scene.mcx + one STL asset, translated.
    let state = starter_state();
    let stl_bytes = {
        let fixture = mesh_fixture("simple_box.stl");
        let provider = state.storage.resolve(&fixture).expect("file provider");
        provider.read(&fixture).take().unwrap().unwrap()
    };
    let scene = serde_json::json!({
        "Name": "t.mcx",
        "Children": [{
            "Name": "Part",
            "Matrix": "[1,0,0,0, 0,1,0,0, 0,0,1,0, 5,6,7,1]",
            "MeshPath": "M.stl",
            "Color": "#00FF00"
        }]
    });
    // Build the .mcx zip in memory and plant it in the provider — the
    // import path reads bytes from storage, so no temp file is needed.
    let mcx_bytes = {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("scene.mcx", opts).unwrap();
        zip.write_all(scene.to_string().as_bytes()).unwrap();
        zip.start_file("Assets/M.stl", opts).unwrap();
        zip.write_all(&stl_bytes).unwrap();
        zip.finish().unwrap().into_inner()
    };
    let mcx_uri = memory_uri("import_me.mcx");
    put(&state.storage, &mcx_uri, mcx_bytes);

    let added = state.import_scene_file(&mcx_uri).unwrap();
    assert_eq!(added, 1);

    let graph = state.graph.lock().unwrap();
    let node = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == "Mesh")
        .expect("MeshNode spawned");
    match node.properties.get("matrix") {
        Some(PortValue::Matrix4x4(m)) => assert_eq!(&m[12..15], &[5.0, 6.0, 7.0]),
        other => panic!("matrix property expected, got {other:?}"),
    }
    match node.properties.get("color") {
        Some(PortValue::Color(c)) => assert_eq!(*c, [0.0, 1.0, 0.0, 1.0]),
        other => panic!("color property expected, got {other:?}"),
    }
}

#[test]
fn import_scene_file_rejects_unknown_extension() {
    let state = starter_state();
    let uri = memory_uri("nope.xyz");
    put(&state.storage, &uri, b"?".to_vec());
    assert!(state.import_scene_file(&uri).is_err());
}

// ── Export formats ──────────────────────────────────────────────────────

#[test]
fn export_all_mesh_formats_write_nonempty_files() {
    let state = starter_state();
    state.evaluate_now();
    for (format, name) in [
        (MeshExportFormat::Stl, "out.stl"),
        (MeshExportFormat::ThreeMf, "out.3mf"),
        (MeshExportFormat::Obj, "out.obj"),
    ] {
        let uri = memory_uri(name);
        state.export_mesh_to_uri(&uri, format).unwrap();
        let len = stored_len(&state.storage, &uri).expect("export must be stored");
        assert!(len > 0, "{name} must not be empty");
    }
}

#[test]
fn export_project_copy_keeps_current_file_untouched() {
    let state = starter_state();
    let home = memory_uri("home.atmr");
    state.save_graph_to_uri(&home).unwrap();

    let copy = memory_uri("copy.atmr");
    state.export_project_copy_to_uri(&copy).unwrap();
    assert!(uri_exists(&state.storage, &copy));
    assert_eq!(
        state.current_file.lock().unwrap().clone(),
        Some(home.clone()),
        "export-a-copy must not retarget Save"
    );
    // The copy must itself be a loadable project — reload it through a
    // fresh state sharing the same storage registry.
    let fresh = fresh_state_with_starter_graph_and_storage(state.storage.clone());
    fresh.load_graph_from_uri(&copy).unwrap();
    assert_eq!(fresh.graph.lock().unwrap().node_count(), 4);
}
