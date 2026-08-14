//! File workflows over a *genuinely asynchronous* storage provider.
//!
//! No NodeDesigner counterpart — this covers the Phase 4c conversion
//! (`docs/storage-architecture-plan.md` §3.3): every `AppState` file
//! operation submits its IO to the frame pump and applies the result in a
//! continuation. The other file suites run against `MemoryProvider`,
//! which settles inline and therefore cannot tell a real submit-and-
//! continue implementation from a synchronous one. These tests wrap that
//! provider in `FlakyProvider` so results only arrive when the test
//! advances *both* clocks — the provider's (`FlakyProvider::pump`) and
//! the app's (`TestHarness::pump`) — which is exactly what Phase 5's OPFS
//! backend and Phase 8's HTTP backend will do.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use atomartist_lib::serialization::write_project_to_bytes;
use atomartist_storage::{
    FlakyConfig, FlakyProvider, MemoryProvider, Precondition, StorageProvider, StorageRegistry,
    StorageUri,
};
use atomartist_ui::menu_actions::{confirm_discard_unsaved_then, save_current_then};
use atomartist_ui::top_menu_bar::{FileDialogProvider, UnsavedChoice};
use atomartist_ui::{fresh_state_with_starter_graph_and_storage, AppState, NoticeLevel};
use atomartist_ui_test::TestHarness;

/// Harness whose only storage provider is a memory store behind the
/// given fault-injection config.
fn harness_with(config: FlakyConfig) -> (TestHarness, Arc<FlakyProvider>) {
    let provider = Arc::new(FlakyProvider::new(
        Arc::new(MemoryProvider::new("mem", "Memory")),
        config,
    ));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the flaky provider");
    let state = fresh_state_with_starter_graph_and_storage(Arc::new(registry));
    (TestHarness::with_app_state(state), provider)
}

fn uri(name: &str) -> StorageUri {
    StorageUri::new("mem", &format!("/{name}"))
}

/// Serialize the harness's current project the way `save_project` does,
/// so a test can seed storage with a genuinely loadable `.atmr`.
fn project_bytes(state: &AppState) -> Vec<u8> {
    let graph = state.graph.lock().unwrap();
    let assets = state.assets.lock().unwrap();
    write_project_to_bytes(&graph, &assets).expect("starter project serializes")
}

/// Plant bytes in the provider, driving its simulated clock to delivery.
fn put(provider: &Arc<FlakyProvider>, at: &StorageUri, bytes: Vec<u8>) {
    let job = provider.write(at, bytes, Precondition::None);
    provider.pump_until_idle();
    job.take().expect("settled").expect("seed write succeeds");
}

/// Whether the provider holds anything at `at`.
fn stored(provider: &Arc<FlakyProvider>, at: &StorageUri) -> bool {
    let job = provider.stat(at);
    provider.pump_until_idle();
    matches!(job.take(), Some(Ok(Some(_))))
}

/// Advance both clocks `rounds` times — one simulated frame each.
fn frames(h: &TestHarness, provider: &Arc<FlakyProvider>, rounds: usize) {
    for _ in 0..rounds {
        provider.pump();
        h.pump();
    }
}

/// Make the graph differ from its saved baseline.
fn dirty(state: &AppState) {
    let mut g = state.graph.lock().unwrap();
    g.add_new_node("Box", [400.0, 400.0], &state.registry)
        .unwrap();
}

/// The error message on display, if any.
fn shown_error(state: &AppState) -> Option<String> {
    state
        .last_notice()
        .filter(|n| n.level == NoticeLevel::Error)
        .map(|n| n.text)
}

/// Scripted dialogs: fixed answer to the unsaved prompt, fixed save
/// target, prompt counter, and a log of every modal error raised.
struct ScriptedDialogs {
    answer: UnsavedChoice,
    save_path: Option<StorageUri>,
    prompts: AtomicUsize,
    errors: Mutex<Vec<String>>,
}

impl ScriptedDialogs {
    fn saving_to(uri: StorageUri) -> Self {
        ScriptedDialogs {
            answer: UnsavedChoice::Save,
            save_path: Some(uri),
            prompts: AtomicUsize::new(0),
            errors: Mutex::new(Vec::new()),
        }
    }

    /// Modal error messages raised so far.
    fn modals(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
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
        self.answer
    }
    fn show_error(&self, message: &str) {
        self.errors.lock().unwrap().push(message.to_string());
    }
    fn show_info(&self, _title: &str, _message: &str) {}
}

/// The user-visible contract of an asynchronous open: the old project
/// stays on screen — unchanged and intact — until the bytes actually
/// arrive, and the new one swaps in on the frame the pump applies it.
#[test]
fn open_swaps_the_graph_in_only_once_the_read_lands() {
    let (h, provider) = harness_with(FlakyConfig::default().with_latency(2));
    let at = uri("bracket.atmr");
    let expected_nodes = h.state().graph.lock().unwrap().node_count();
    put(&provider, &at, project_bytes(h.state()));

    // Start from an empty document so the swap is unmistakable.
    h.state().new_empty_project();
    h.state().open_project(&at);

    assert_eq!(h.state().pending_op_count(), 1, "the read is in flight");
    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        0,
        "the graph must not change before the bytes arrive"
    );
    assert_eq!(h.state().current_file.lock().unwrap().clone(), None);

    // Frame 1: the provider has not delivered yet.
    frames(&h, &provider, 1);
    assert_eq!(h.state().graph.lock().unwrap().node_count(), 0);

    // Frame 2: the read lands and the continuation swaps the project in.
    frames(&h, &provider, 1);
    assert_eq!(h.state().pending_op_count(), 0);
    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        expected_nodes,
        "the loaded project must be live after the pump applies it"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        Some(at.clone())
    );
    assert_eq!(
        h.state().recent_projects.lock().unwrap().first(),
        Some(&at),
        "opening records the project as most-recent"
    );
    assert!(!h.state().has_unsaved_changes(), "a fresh open is clean");
}

/// A failed open must leave the current document exactly as it was and
/// tell the user why — the worst outcome here would be a half-swapped
/// project.
#[test]
fn a_failed_open_keeps_the_current_project_and_notices() {
    let (h, provider) = harness_with(FlakyConfig::failing_every(1));
    let before = h.state().graph.lock().unwrap().node_count();

    h.state().open_project(&uri("missing.atmr"));
    frames(&h, &provider, 2);

    assert_eq!(h.state().graph.lock().unwrap().node_count(), before);
    assert_eq!(h.state().current_file.lock().unwrap().clone(), None);
    let error = shown_error(h.state()).expect("the user must be told");
    assert!(
        error.contains("missing.atmr"),
        "notice should name the project: {error}"
    );
}

/// The correctness improvement Phase 4c buys: the saved baseline,
/// `current_file`, and the recent list all move *after* a confirmed
/// write. A write that fails leaves the document dirty, still pointing
/// at wherever it pointed before, with the failure on the status bar.
#[test]
fn a_failed_save_leaves_the_document_dirty_and_untargeted() {
    let (h, provider) = harness_with(FlakyConfig::failing_every(1));
    dirty(h.state());
    assert!(h.state().has_unsaved_changes());

    let at = uri("nope.atmr");
    h.state().save_project(&at);
    frames(&h, &provider, 2);

    assert!(
        h.state().has_unsaved_changes(),
        "a failed save must not re-baseline the change tracker"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        None,
        "a failed save must not retarget Save"
    );
    assert!(
        h.state().recent_projects.lock().unwrap().is_empty(),
        "a failed save must not enter the recent list"
    );
    let error = shown_error(h.state()).expect("the user must be told");
    assert!(
        error.contains("nope.atmr"),
        "notice should name the project: {error}"
    );
}

/// Save → then-open, the sequenced flow behind File → Open with unsaved
/// changes. The follow-up must wait for the save to be confirmed, and
/// then run — across as many frames as the provider needs.
#[test]
fn the_save_then_open_chain_completes_across_frames() {
    let (h, provider) = harness_with(FlakyConfig::default().with_latency(2));
    let other = uri("other.atmr");
    put(&provider, &other, project_bytes(h.state()));

    dirty(h.state());
    let target = uri("current.atmr");
    let scripted = Arc::new(ScriptedDialogs::saving_to(target.clone()));
    let dialogs: Arc<dyn FileDialogProvider> = scripted.clone();

    let to_open = other.clone();
    confirm_discard_unsaved_then(h.state(), &dialogs, move |state| state.open_project(&to_open));

    // The prompt was answered immediately (it is a modal), but nothing
    // downstream of the save has happened yet.
    assert_eq!(scripted.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(h.state().pending_op_count(), 1, "the save is in flight");
    assert!(h.state().has_unsaved_changes());
    assert_eq!(h.state().current_file.lock().unwrap().clone(), None);

    // Two frames deliver the write; its continuation re-baselines and
    // submits the open, which needs two more.
    frames(&h, &provider, 2);
    assert!(
        stored(&provider, &target),
        "the save must have been written before the open starts"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        Some(target.clone()),
        "the save retargets Save before the follow-up runs"
    );

    frames(&h, &provider, 2);
    assert_eq!(h.state().pending_op_count(), 0);
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        Some(other),
        "the queued Open runs once the save is confirmed"
    );
    assert!(!h.state().has_unsaved_changes());
}

/// Startup auto-reopen of a last project that has since been deleted is
/// not a failure the user asked for, so it must not post an
/// [`NoticeLevel::Error`]: error notices are deliberately sticky (an Info
/// never displaces an undismissed one), so one posted before the user has
/// even touched the app would swallow every later "Saved …" confirmation.
#[test]
fn a_failed_startup_reopen_is_not_a_sticky_error() {
    let (h, provider) = harness_with(FlakyConfig::failing_every(1));
    let gone = uri("gone.atmr");
    h.state().recent_projects.lock().unwrap().push(gone.clone());

    h.state().reopen_last_project(&gone);
    frames(&h, &provider, 2);

    assert_eq!(
        shown_error(h.state()),
        None,
        "a stale last-project must not raise an error notice"
    );
    assert!(
        h.state().recent_projects.lock().unwrap().is_empty(),
        "an unopenable last project should leave the recent list"
    );

    // The real damage of a sticky error: it suppresses the next
    // confirmation. Prove a later Info still reaches the display slot.
    h.state().notify(NoticeLevel::Info, "Saved thing.atmr");
    h.pump();
    assert_eq!(
        h.state().last_notice().map(|n| n.text),
        Some("Saved thing.atmr".to_string()),
        "later confirmations must still be visible"
    );
}

/// Mechanics of `demo-native`'s deferred window close.
///
/// The shell answers `CloseRequested` by handing the gate a continuation
/// that sets an `AtomicBool`, then closes only if that flag is already
/// set — otherwise it stays open and finishes the close from the pump.
/// This test pins the two properties the shell depends on. Only the
/// winit half (re-checking the flag in `AboutToWait` and calling
/// `elwt.exit()`) is left to manual testing; it has no headless entry
/// point.
#[test]
fn the_close_permission_flag_is_immediate_when_storage_is_and_deferred_when_it_is_not() {
    // A settled provider must set the flag *before* the call returns, or
    // closing the window would take an extra frame for every user.
    let (h, _provider) = harness_with(FlakyConfig::default());
    dirty(h.state());
    let dialogs: Arc<dyn FileDialogProvider> = Arc::new(ScriptedDialogs::saving_to(uri("sync.atmr")));
    let close_now = Arc::new(AtomicBool::new(false));
    let flag = close_now.clone();
    confirm_discard_unsaved_then(h.state(), &dialogs, move |_state| {
        flag.store(true, Ordering::SeqCst)
    });
    assert!(
        close_now.load(Ordering::SeqCst),
        "a synchronous provider must let the window close on this event"
    );

    // A slow provider must NOT: the window stays open, and permission
    // arrives from the pump once the save is confirmed.
    let (h, provider) = harness_with(FlakyConfig::default().with_latency(2));
    dirty(h.state());
    let dialogs: Arc<dyn FileDialogProvider> = Arc::new(ScriptedDialogs::saving_to(uri("slow.atmr")));
    let close_later = Arc::new(AtomicBool::new(false));
    let flag = close_later.clone();
    confirm_discard_unsaved_then(h.state(), &dialogs, move |_state| {
        flag.store(true, Ordering::SeqCst)
    });
    assert!(
        !close_later.load(Ordering::SeqCst),
        "the close must wait for the save to land"
    );
    frames(&h, &provider, 2);
    assert!(
        close_later.load(Ordering::SeqCst),
        "the pump must deliver the permission to close"
    );
}

/// The other half of the chain: when the save fails, the action that was
/// waiting on it must not happen. Losing the unsaved work here is the
/// exact failure the gate exists to prevent.
#[test]
fn a_failed_save_never_runs_the_action_that_was_waiting_on_it() {
    let (h, provider) = harness_with(FlakyConfig::failing_every(1));
    dirty(h.state());
    let nodes_before = h.state().graph.lock().unwrap().node_count();

    let dialogs: Arc<dyn FileDialogProvider> = Arc::new(ScriptedDialogs::saving_to(uri("current.atmr")));
    let ran = Arc::new(AtomicBool::new(false));
    let flag = ran.clone();
    confirm_discard_unsaved_then(h.state(), &dialogs, move |state| {
        flag.store(true, Ordering::SeqCst);
        state.new_empty_project();
    });
    frames(&h, &provider, 2);

    assert!(
        !ran.load(Ordering::SeqCst),
        "the follow-up must not run after a failed save"
    );
    assert_eq!(
        h.state().graph.lock().unwrap().node_count(),
        nodes_before,
        "the unsaved work must still be there"
    );
    assert!(shown_error(h.state()).is_some(), "the user must be told");
}

/// Save is a "losing the result costs work" operation, so per the policy
/// in `docs/storage-architecture-plan.md` §7 a failed write raises a
/// **modal** as well as the status-bar notice. A status-bar line alone is
/// too easy to miss, and the user walks away believing the file is on
/// disk.
#[test]
fn a_failed_save_raises_a_modal_as_well_as_a_notice() {
    let (h, provider) = harness_with(FlakyConfig::failing_every(1));
    dirty(h.state());

    let scripted = Arc::new(ScriptedDialogs::saving_to(uri("nope.atmr")));
    let dialogs: Arc<dyn FileDialogProvider> = scripted.clone();
    save_current_then(h.state(), &dialogs, |_state| {});
    frames(&h, &provider, 2);

    assert!(
        shown_error(h.state()).is_some(),
        "the status bar must carry the failure"
    );
    let modals = scripted.modals();
    assert_eq!(
        modals.len(),
        1,
        "a failed save must also raise a modal: {modals:?}"
    );
    assert!(
        modals[0].contains("nope.atmr"),
        "the modal should name the project: {}",
        modals[0]
    );
}
