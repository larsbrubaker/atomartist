//! Unit tests for [`crate::menu_actions`] — menu-add placement, the
//! recent-list prune, the modal-vs-notice failure policy for open /
//! export, and the storage-busy re-entry guard.
//!
//! Split out of `menu_actions.rs` so that file stays under the project's
//! 800-line cap (same arrangement as `storage_ops_tests.rs`).

use super::*;
use crate::debug_windows::DebugWindowHandles;
use crate::settings::DebugWindowsState;
use crate::top_level::fresh_state_with_starter_graph;
use crate::top_menu_bar::NoFileDialogs;
use atomartist_lib::graph::node::NodeId;

fn debug_handles() -> DebugWindowHandles {
    DebugWindowHandles::new(DebugWindowsState::default())
}

/// Scripted dialogs for the `handle_action` flows: canned picker
/// answers plus a log of every modal error raised, so a test can
/// assert on *which* failures get a modal and which stay notice-only.
struct ScriptedDialogs {
    unsaved: UnsavedChoice,
    open_path: Option<StorageUri>,
    save_path: Option<StorageUri>,
    export_path: Option<StorageUri>,
    prompts: std::sync::atomic::AtomicUsize,
    errors: std::sync::Mutex<Vec<String>>,
}

impl ScriptedDialogs {
    fn new() -> Self {
        ScriptedDialogs {
            unsaved: UnsavedChoice::Discard,
            open_path: None,
            save_path: None,
            export_path: None,
            prompts: std::sync::atomic::AtomicUsize::new(0),
            errors: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn opening(mut self, uri: StorageUri) -> Self {
        self.open_path = Some(uri);
        self
    }
    fn exporting_to(mut self, uri: StorageUri) -> Self {
        self.export_path = Some(uri);
        self
    }
    fn saving_to(mut self, uri: StorageUri) -> Self {
        self.unsaved = UnsavedChoice::Save;
        self.save_path = Some(uri);
        self
    }
    fn modals(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }
    fn prompt_count(&self) -> usize {
        self.prompts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl crate::top_menu_bar::FileDialogProvider for ScriptedDialogs {
    // Canned answers settle immediately, the way a blocking native
    // picker's do — `submit_op` then runs the continuation inline, so
    // these flows stay as step-by-step assertable as they were before the
    // pickers became asynchronous.
    fn pick_open_project(&self) -> Job<Option<StorageUri>> {
        Job::ready(self.open_path.clone())
    }
    fn pick_save_project(&self, _name: &str) -> Job<Option<StorageUri>> {
        Job::ready(self.save_path.clone())
    }
    fn pick_save_export(&self, _ext: &str, _name: &str) -> Job<Option<StorageUri>> {
        Job::ready(self.export_path.clone())
    }
    fn pick_import_file(&self) -> Job<Option<StorageUri>> {
        Job::ready(None)
    }
    fn confirm_unsaved_changes(&self) -> UnsavedChoice {
        self.prompts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.unsaved
    }
    fn show_error(&self, message: &str) {
        self.errors.lock().unwrap().push(message.to_string());
    }
    fn show_info(&self, _title: &str, _message: &str) {}
}

/// Dialogs whose pickers *break* rather than answering — the case a
/// cancelled pick must not be confused with. The modal handle produces it
/// by refusing a stacked open; a future provider-backed picker could
/// produce it for any reason at all.
struct BrokenPickers(StorageError);

impl crate::top_menu_bar::FileDialogProvider for BrokenPickers {
    fn pick_open_project(&self) -> Job<Option<StorageUri>> {
        Job::failed(self.0.clone())
    }
    fn pick_save_project(&self, _name: &str) -> Job<Option<StorageUri>> {
        Job::failed(self.0.clone())
    }
    fn pick_save_export(&self, _ext: &str, _name: &str) -> Job<Option<StorageUri>> {
        Job::failed(self.0.clone())
    }
    fn pick_import_file(&self) -> Job<Option<StorageUri>> {
        Job::failed(self.0.clone())
    }
    fn confirm_unsaved_changes(&self) -> UnsavedChoice {
        UnsavedChoice::Discard
    }
    fn show_error(&self, _message: &str) {}
    fn show_info(&self, _title: &str, _message: &str) {}
}

/// Starter-graph state whose only provider is a memory store behind
/// the given fault-injection config.
fn flaky_state(
    config: atomartist_storage::FlakyConfig,
) -> (AppState, Arc<atomartist_storage::FlakyProvider>) {
    use atomartist_storage::{
        FlakyProvider, MemoryProvider, StorageProvider, StorageRegistry,
    };
    let provider = Arc::new(FlakyProvider::new(
        Arc::new(MemoryProvider::new("mem", "Memory")),
        config,
    ));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let state =
        crate::top_level::fresh_state_with_starter_graph_and_storage(Arc::new(registry));
    (state, provider)
}

fn mem_uri(name: &str) -> StorageUri {
    StorageUri::new("mem", &format!("/{name}"))
}

/// Make the graph differ from its saved baseline.
fn dirty(state: &AppState) {
    let mut g = state.graph.lock().unwrap();
    g.add_new_node("Box", [400.0, 400.0], &state.registry)
        .unwrap();
}

fn shown_error(state: &AppState) -> Option<String> {
    state
        .last_notice()
        .filter(|n| n.level == NoticeLevel::Error)
        .map(|n| n.text)
}

/// Failing to open a project the user explicitly asked for costs them
/// the action, so per the policy in
/// `docs/storage-architecture-plan.md` §7 it raises a modal as well as
/// the status-bar notice.
#[test]
fn a_failed_open_from_the_file_menu_raises_a_modal_and_a_notice() {
    use atomartist_storage::FlakyConfig;
    let (state, _provider) = flaky_state(FlakyConfig::failing_every(1));
    let scripted = Arc::new(ScriptedDialogs::new().opening(mem_uri("missing.atmr")));
    let dialogs: Arc<dyn crate::top_menu_bar::FileDialogProvider> = scripted.clone();

    handle_action(&state, &dialogs, &debug_handles(), "file.open");
    state.pump_storage();

    assert!(
        shown_error(&state).is_some(),
        "the status bar must carry the failure"
    );
    let modals = scripted.modals();
    assert_eq!(
        modals.len(),
        1,
        "a failed open must also raise a modal: {modals:?}"
    );
    assert!(
        modals[0].contains("missing.atmr"),
        "the modal should name the project: {}",
        modals[0]
    );
}

/// The other half of the same policy: an export is non-destructive and
/// happens immediately after an explicit user action, so it stays
/// notice-only. A modal here would just be a second click to make.
#[test]
fn a_failed_export_notices_without_raising_a_modal() {
    use atomartist_storage::FlakyConfig;
    let (state, _provider) = flaky_state(FlakyConfig::failing_every(1));
    state.evaluate_now();
    let scripted = Arc::new(ScriptedDialogs::new().exporting_to(mem_uri("out.stl")));
    let dialogs: Arc<dyn crate::top_menu_bar::FileDialogProvider> = scripted.clone();

    handle_action(&state, &dialogs, &debug_handles(), "file.export.stl");
    state.pump_storage();

    assert!(
        shown_error(&state).is_some(),
        "a failed export must still reach the status bar"
    );
    assert!(
        scripted.modals().is_empty(),
        "a failed export must not raise a modal: {:?}",
        scripted.modals()
    );
}

/// Double-clicking File → New while a slow provider still has the
/// gate's Save in flight used to re-prompt and submit a second save.
/// The second invocation must be refused with an informational notice
/// instead.
#[test]
fn a_destructive_file_action_is_refused_while_storage_is_busy() {
    use atomartist_storage::FlakyConfig;
    let (state, _provider) = flaky_state(FlakyConfig::default().with_latency(2));
    dirty(&state);
    let scripted = Arc::new(ScriptedDialogs::new().saving_to(mem_uri("busy.atmr")));
    let dialogs: Arc<dyn crate::top_menu_bar::FileDialogProvider> = scripted.clone();

    handle_action(&state, &dialogs, &debug_handles(), "file.new");
    assert_eq!(state.pending_op_count(), 1, "the gate's save is in flight");
    assert_eq!(scripted.prompt_count(), 1);

    handle_action(&state, &dialogs, &debug_handles(), "file.new");
    assert_eq!(
        state.pending_op_count(),
        1,
        "the second New must not submit another save"
    );
    assert_eq!(
        scripted.prompt_count(),
        1,
        "the second New must not re-prompt"
    );
    state.pump_storage();
    let notice = state.last_notice().expect("the user must be told why");
    assert_eq!(notice.level, NoticeLevel::Info);
    assert!(
        notice.text.contains("busy"),
        "notice should explain the refusal: {}",
        notice.text
    );
}

/// A picker that *fails* is not a picker the user dismissed: the first
/// says nothing (an abandoned action needs no explanation), the second
/// leaves an error the user can act on.
#[test]
fn a_broken_picker_notices_while_a_cancelled_one_stays_silent() {
    let state = fresh_state_with_starter_graph();
    let broken: Arc<dyn crate::top_menu_bar::FileDialogProvider> =
        Arc::new(BrokenPickers(StorageError::Io("no picker".into())));

    handle_action(&state, &broken, &debug_handles(), "file.open");
    state.pump_storage();

    let notice = shown_error(&state).expect("a broken picker must be reported");
    assert!(
        notice.contains("picker"),
        "the notice should name what broke: {notice}"
    );

    // The refusal a stacked open produces reads as "someone else is being
    // asked", which is not an error the user can do anything about.
    let refused: Arc<dyn crate::top_menu_bar::FileDialogProvider> =
        Arc::new(BrokenPickers(StorageError::Cancelled));
    let quiet = fresh_state_with_starter_graph();
    handle_action(&quiet, &refused, &debug_handles(), "file.open");
    quiet.pump_storage();
    assert_eq!(shown_error(&quiet), None);

    // As does an ordinary cancel.
    let none: Arc<dyn crate::top_menu_bar::FileDialogProvider> = Arc::new(NoFileDialogs);
    let quiet = fresh_state_with_starter_graph();
    handle_action(&quiet, &none, &debug_handles(), "file.import");
    quiet.pump_storage();
    assert_eq!(shown_error(&quiet), None);
}

/// Import and Export put a picker up and touch storage like every other
/// File action, so they take the same re-entry guard: while something the
/// user asked for is in flight, they are refused with the "busy" notice
/// rather than stacking a second picker.
#[test]
fn import_and_export_are_refused_while_storage_is_busy() {
    use atomartist_storage::FlakyConfig;
    for action in ["file.import", "file.export.stl"] {
        let (state, _provider) = flaky_state(FlakyConfig::default().with_latency(2));
        state.evaluate_now();
        dirty(&state);
        let scripted = Arc::new(ScriptedDialogs::new().saving_to(mem_uri("busy.atmr")));
        let dialogs: Arc<dyn crate::top_menu_bar::FileDialogProvider> = scripted.clone();

        // Put a save in flight through the unsaved-changes gate.
        handle_action(&state, &dialogs, &debug_handles(), "file.new");
        assert_eq!(state.pending_op_count(), 1, "the gate's save is in flight");

        handle_action(&state, &dialogs, &debug_handles(), action);
        assert_eq!(
            state.pending_op_count(),
            1,
            "{action} must not submit anything while storage is busy"
        );
        state.pump_storage();
        let notice = state.last_notice().expect("the user must be told why");
        assert!(
            notice.text.contains("busy"),
            "{action}: notice should explain the refusal: {}",
            notice.text
        );
    }
}

/// A stale entry in the Open Recent list — the project was deleted,
/// or lives on a backend this build no longer registers — must be
/// pruned with a message rather than silently doing nothing. The
/// existence check is a storage job now, so the pruning happens when
/// the `stat` lands: here, two clocks later.
#[test]
fn open_recent_prunes_a_missing_entry_once_the_stat_lands() {
    use atomartist_storage::{
        FlakyConfig, FlakyProvider, MemoryProvider, StorageProvider, StorageRegistry,
        StorageUri,
    };
    use std::sync::Arc;

    let provider = Arc::new(FlakyProvider::new(
        Arc::new(MemoryProvider::new("mem", "Memory")),
        FlakyConfig::default().with_latency(1),
    ));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    let state = crate::top_level::fresh_state_with_starter_graph_and_storage(Arc::new(registry));

    let missing = StorageUri::new("mem", "/gone.atmr");
    state.recent_projects.lock().unwrap().push(missing.clone());

    let dialogs: Arc<dyn crate::top_menu_bar::FileDialogProvider> = Arc::new(NoFileDialogs);
    handle_action(&state, &dialogs, &debug_handles(), "file.recent.0");

    assert_eq!(state.pending_op_count(), 1, "the stat is in flight");
    assert_eq!(
        state.recent_projects.lock().unwrap().len(),
        1,
        "nothing is pruned before the answer arrives"
    );

    provider.pump();
    state.pump_storage();
    // A notice posted *by* a continuation reaches the display slot
    // on the next frame — `pump_storage` drains the queue before it
    // applies anything (see `storage_ops`).
    state.pump_storage();

    assert!(
        state.recent_projects.lock().unwrap().is_empty(),
        "a missing project must leave the recent list"
    );
    let notice = state.last_notice().expect("the user must be told");
    assert_eq!(notice.level, NoticeLevel::Error);
    assert!(
        notice.text.contains("no longer exists"),
        "notice should explain the pruning: {}",
        notice.text
    );
}

/// (position, id) for every node of `type_id` in the active graph.
fn nodes_of_type(state: &AppState, type_id: &str) -> Vec<([f64; 2], NodeId)> {
    let ag = state.active_graph();
    let g = ag.lock().unwrap();
    g.nodes()
        .filter(|n| n.type_id.as_ref() == type_id)
        .map(|n| (n.position, n.id))
        .collect()
}

/// Reproduces the user-reported "menu-added node can't be selected /
/// moved / connected, and the next add doesn't appear" cluster. Root
/// cause: menu-add dropped every node at a single fixed canvas point
/// that overlapped a starter node, so successive adds stacked on top
/// of each other and behind existing nodes — invisible to hit-testing.
#[test]
fn menu_add_places_nodes_without_overlap_or_stacking() {
    let state = fresh_state_with_starter_graph();
    let dialogs: std::sync::Arc<dyn crate::top_menu_bar::FileDialogProvider> =
        std::sync::Arc::new(NoFileDialogs);
    let debug = debug_handles();

    handle_action(&state, &dialogs, &debug, "add.Cylinder");
    handle_action(&state, &dialogs, &debug, "add.Sphere");

    // Symptom 4: both nodes actually get added, with distinct ids.
    let cyl = nodes_of_type(&state, "Cylinder");
    let sph = nodes_of_type(&state, "Sphere");
    assert_eq!(cyl.len(), 1, "Cylinder should be added exactly once");
    assert_eq!(sph.len(), 1, "Sphere should be added exactly once");
    assert_ne!(cyl[0].1, sph[0].1, "added nodes must have distinct ids");

    // Symptoms 1/4: successive menu-adds must not stack on the same
    // canvas position.
    assert_ne!(
        cyl[0].0, sph[0].0,
        "successive menu-added nodes must not stack at one position",
    );

    // Symptoms 1/2: a new node must not land on top of an existing
    // node, or its title bar / sockets are unreachable by hit-testing.
    let existing: Vec<[f64; 2]> = {
        let ag = state.active_graph();
        let g = ag.lock().unwrap();
        g.nodes()
            .filter(|n| {
                n.type_id.as_ref() != "Cylinder" && n.type_id.as_ref() != "Sphere"
            })
            .map(|n| n.position)
            .collect()
    };
    // A node header is roughly 170 wide × 120 tall in canvas units;
    // anything closer than that overlaps enough to steal hit-testing.
    let overlaps = |a: [f64; 2], b: [f64; 2]| {
        (a[0] - b[0]).abs() < 170.0 && (a[1] - b[1]).abs() < 120.0
    };
    for new_pos in [cyl[0].0, sph[0].0] {
        for e in &existing {
            assert!(
                !overlaps(new_pos, *e),
                "new node at {:?} overlaps existing node at {:?}",
                new_pos,
                e,
            );
        }
    }
}

/// Follow-up to the placement fix: the rightward cascade must not run
/// off toward +X forever. Once a row fills, menu-add wraps to a new row
/// below (Y-up: smaller Y). Guarantees X stays bounded and no two nodes
/// ever collide, however many are added.
#[test]
fn menu_add_wraps_row_keeping_x_bounded_without_overlap() {
    let state = fresh_state_with_starter_graph();
    let dialogs: std::sync::Arc<dyn crate::top_menu_bar::FileDialogProvider> =
        std::sync::Arc::new(NoFileDialogs);
    let debug = debug_handles();

    // Left-most column across the starter graph anchors the wrap bound.
    let leftmost_x = {
        let ag = state.active_graph();
        let g = ag.lock().unwrap();
        g.nodes().map(|n| n.position[0]).fold(f64::INFINITY, f64::min)
    };

    // Add well past one row's worth (~6 columns) to force several wraps.
    for _ in 0..18 {
        handle_action(&state, &dialogs, &debug, "add.Cylinder");
    }

    let positions: Vec<[f64; 2]> = {
        let ag = state.active_graph();
        let g = ag.lock().unwrap();
        g.nodes().map(|n| n.position).collect()
    };

    // X stays bounded: nothing cascades past the wrap extent from the
    // left-most column (must match ROW_MAX_EXTENT in node_helpers).
    const ROW_MAX_EXTENT: f64 = 1400.0;
    for p in &positions {
        assert!(
            p[0] <= leftmost_x + ROW_MAX_EXTENT + 1.0,
            "node X {} exceeded the wrap bound {}",
            p[0],
            leftmost_x + ROW_MAX_EXTENT,
        );
    }

    // No two nodes (added or starter) collide.
    let overlaps = |a: [f64; 2], b: [f64; 2]| {
        (a[0] - b[0]).abs() < 170.0 && (a[1] - b[1]).abs() < 120.0
    };
    for i in 0..positions.len() {
        for j in (i + 1)..positions.len() {
            assert!(
                !overlaps(positions[i], positions[j]),
                "nodes at {:?} and {:?} overlap",
                positions[i],
                positions[j],
            );
        }
    }
}
