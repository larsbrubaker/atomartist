//! Menu action routing — translates the string action ids emitted by
//! the top menu bar (see `top_menu_bar::compose_menus`) into `AppState`
//! mutations, file dialogs, and debug-window toggles.
//!
//! Destructive file actions (`file.new`, `file.open`, recent-file
//! opens) funnel through [`confirm_discard_unsaved_then`] so unsaved
//! work always gets a Save / Discard / Cancel prompt first. The same
//! helper is exported for the platform shell's window-close path.
//!
//! # Why the gate takes a continuation
//!
//! Saving is asynchronous (Phase 4c): [`AppState::save_project_then`]
//! submits the write to the frame pump and returns before the bytes have
//! landed. A gate that returned `bool` could therefore only ever report
//! "the save was *started*", and the follow-up action would run against a
//! project that might yet fail to save. So the follow-up is handed *in*,
//! and runs inside the save's continuation — immediately for the local
//! providers (their jobs are already settled when `submit_op` sees them),
//! a frame or more later for a network one. A failed or cancelled save
//! simply never runs it; the error notice explains why.
//!
//! # Picking a file is asynchronous too
//!
//! Since step 6c-2 every [`FileDialogProvider`] picker answers with a
//! [`Job<Option<StorageUri>>`], because the in-app file browser is a
//! widget and cannot settle before the frame loop runs. Each call site
//! therefore submits the picker's job as a *loud* [`JobOp`] and does its
//! work from that operation's continuation ([`pick_then`]). Two
//! consequences worth knowing:
//!
//! - A blocking picker (`rfd` on native) hands back [`Job::ready`], which
//!   `submit_op` applies inline, so the desktop path — including the
//!   window-close save gate — stays synchronous in practice.
//! - While a picker is up it counts as storage activity, so [`storage_busy`]
//!   refuses a second File action. That is the intended reading: a dialog
//!   the user has not answered is exactly the state in which a second
//!   Open must not stack another one.
//!
//! # How failures are reported
//!
//! Save and open failures raise a modal (`FileDialogProvider::show_error`)
//! *as well as* the status-bar notice; export, import, and the recent-list
//! prune stay notice-only. See `docs/storage-architecture-plan.md` §7 for
//! the reasoning — in short, only the first two lose work the user cannot
//! recover by repeating the action. That is why every entry point here
//! takes the dialogs as an `Arc<dyn FileDialogProvider>`: the modal is
//! raised from the operation's continuation, which outlives the menu
//! callback whenever the provider is asynchronous.

use std::sync::Arc;

use agg_gui::theme::{AccentColor, ThemePreference};
use agg_gui_node_editor::NodeEditorCommand;
use atomartist_lib::graph::undo_commands::AddNodeCmd;
use atomartist_storage::{Job, StorageError, StorageUri};

use crate::app_state::AppState;
use crate::app_state_files::MeshExportFormat;
use crate::app_state_storage::{display_uri, stat_job, uri_file_stem, uri_label};
use crate::debug_windows::DebugWindowHandles;
use crate::storage_ops::{JobOp, NoticeLevel};
use crate::top_menu_bar::{FileDialogProvider, UnsavedChoice};

/// Gate for actions that would discard the current graph. `proceed` runs
/// when it is safe to continue:
/// - no unsaved changes → immediately,
/// - user picks **Save** → from the save's continuation, once the write
///   is confirmed (never, if the picker was cancelled or the write
///   failed),
/// - user picks **Discard** → immediately,
/// - user picks **Cancel** → never.
///
/// Callers must not hold an [`AppState`] lock across this call: with a
/// local provider `proceed` runs on the caller's stack.
///
/// Takes the dialogs as an `Arc` because a failed Save has to raise a
/// modal from inside the write's continuation, which outlives the caller
/// whenever the provider is asynchronous.
pub fn confirm_discard_unsaved_then(
    state: &AppState,
    dialogs: &Arc<dyn FileDialogProvider>,
    proceed: impl FnOnce(&AppState) + Send + 'static,
) {
    if !state.has_unsaved_changes() {
        proceed(state);
        return;
    }
    match dialogs.confirm_unsaved_changes() {
        UnsavedChoice::Save => save_current_then(state, dialogs, proceed),
        UnsavedChoice::Discard => proceed(state),
        UnsavedChoice::Cancel => {}
    }
}

/// Run `then` with the location the user picked.
///
/// The picker's job goes onto the frame pump as a **loud** operation: the
/// user asked for the dialog by name, it belongs in the status bar's
/// activity readout, and it must make [`storage_busy`] refuse a second
/// File action for as long as it is up.
///
/// Outcomes:
/// - `Ok(Some(uri))` → `then` runs, from the pump (or inline, for a
///   blocking picker whose job is already settled).
/// - `Ok(None)` → the user cancelled. Nothing happens and nothing is
///   said; a cancelled picker has never been a failure here.
/// - `Err(Cancelled)` → the picker was refused or torn down (the modal
///   handle refuses a stacked open this way). Also silent: the user is
///   already looking at the dialog that won.
/// - any other `Err` → a notice, because a picker that broke is not the
///   same as one the user dismissed.
fn pick_then(
    state: &AppState,
    label: impl Into<String>,
    job: Job<Option<StorageUri>>,
    then: impl FnOnce(&AppState, StorageUri) + Send + 'static,
) {
    state.submit_op(Box::new(JobOp::new(label, job, move |state, result| {
        match result {
            Ok(Some(uri)) => then(state, uri),
            Ok(None) | Err(StorageError::Cancelled) => {}
            Err(err) => state.notify(
                NoticeLevel::Error,
                format!("The file picker failed: {err}"),
            ),
        }
    })));
}

/// Save to the current location, prompting for one when the project has
/// never been saved.
pub fn save_current(state: &AppState, dialogs: &Arc<dyn FileDialogProvider>) {
    save_current_then(state, dialogs, |_state| {});
}

/// [`save_current`] plus a follow-up that runs only once the write is
/// confirmed — the sequencing primitive behind
/// [`confirm_discard_unsaved_then`].
pub fn save_current_then(
    state: &AppState,
    dialogs: &Arc<dyn FileDialogProvider>,
    on_success: impl FnOnce(&AppState) + Send + 'static,
) {
    let existing = state.current_file.lock().unwrap().clone();
    match existing {
        Some(uri) => save_project_reporting(state, dialogs, &uri, on_success),
        None => {
            // Never saved: ask where. A cancelled picker is not a failure
            // and gets no notice; it just means nothing (including the
            // follow-up) happens.
            let dialogs = dialogs.clone();
            let job = dialogs.pick_save_project("untitled.atmr");
            pick_then(state, "Choosing where to save", job, move |state, uri| {
                save_project_reporting(state, &dialogs, &uri, on_success)
            });
        }
    }
}

/// Save to `uri`, reporting a failure **both** ways: the status-bar
/// notice every storage continuation posts, and a modal.
///
/// Why a modal here and not on export (see
/// `docs/storage-architecture-plan.md` §7): losing a save costs the user
/// work they cannot get back by repeating the action, and the status
/// bar's single line is easy to miss — especially on the window-close
/// path, where the next thing that happens is the app going away.
fn save_project_reporting(
    state: &AppState,
    dialogs: &Arc<dyn FileDialogProvider>,
    uri: &StorageUri,
    on_success: impl FnOnce(&AppState) + Send + 'static,
) {
    let dialogs = dialogs.clone();
    state.save_project_with_outcome(uri, move |state, outcome| match outcome {
        Ok(()) => on_success(state),
        Err(message) => {
            state.notify(NoticeLevel::Error, message.clone());
            dialogs.show_error(&format!("Save failed: {message}"));
        }
    });
}

/// Open `uri`, reporting a failure both as a status-bar notice and a
/// modal — the same policy as [`save_project_reporting`], for the same
/// reason: an open the user explicitly asked for that silently does
/// nothing is indistinguishable from a hang.
fn open_project_reporting(
    state: &AppState,
    dialogs: &Arc<dyn FileDialogProvider>,
    uri: &StorageUri,
) {
    let dialogs = dialogs.clone();
    state.open_project_with_outcome(uri, move |state, outcome| {
        if let Err(message) = outcome {
            state.notify(NoticeLevel::Error, message.clone());
            dialogs.show_error(&format!("Open failed: {message}"));
        }
    });
}

/// Cheap re-entry guard on **every** File action — New, Open, recent-open,
/// Save, Save As, Import, and Export alike.
///
/// Double-clicking File → Open (or New / Save) while a slow provider
/// still has the previous chain in flight used to re-prompt and
/// double-submit. Refusing outright while any *user-initiated* operation
/// is in flight is blunt — a background export blocks a Save — but it is
/// honest, and it cannot produce the two-saves-racing outcome that
/// per-chain flags would still have to be careful about.
///
/// Quiet background work is deliberately not counted (see
/// [`crate::storage_ops`], "Loud and quiet operations"): the file
/// browser's thumbnail reads are in flight for as long as a directory is
/// on screen, and letting those refuse File → Open would make the menu
/// unusable exactly where it is most needed. Returns `true` when the
/// action should be dropped.
fn storage_busy(state: &AppState) -> bool {
    if state.pending_op_count() == 0 {
        return false;
    }
    state.notify(
        NoticeLevel::Info,
        "Storage is busy — try again in a moment.",
    );
    true
}

/// Suggested export filename: current project stem (or "export") plus
/// the format extension.
fn export_default_name(state: &AppState, ext: &str) -> String {
    let stem = state
        .current_file
        .lock()
        .unwrap()
        .as_ref()
        .and_then(uri_file_stem)
        .unwrap_or_else(|| "export".to_string());
    format!("{stem}.{ext}")
}

/// Open a project remembered in the recent list.
///
/// The entry may be stale (file deleted, or written by a provider this
/// build no longer registers), so the existence check is a real storage
/// operation: a `stat` job whose continuation either prunes the entry —
/// leaving a notice that says so — or runs the unsaved-changes gate and
/// opens the project.
///
/// Takes the dialogs as an `Arc` because the gate has to be reached from
/// inside that continuation, which outlives the menu callback whenever
/// the provider is asynchronous.
fn open_recent(state: &AppState, dialogs: &Arc<dyn FileDialogProvider>, uri: StorageUri) {
    let dialogs = dialogs.clone();
    let job = stat_job(&state.storage, &uri);
    state.submit_op(Box::new(JobOp::new(
        format!("Checking {}", uri_label(&uri)),
        job,
        move |state, result| {
            // An unknown scheme, a failed stat, and "nothing there" all
            // read the same way: the entry cannot be opened, so it
            // leaves the list.
            if !matches!(result, Ok(Some(_))) {
                state.notify(
                    NoticeLevel::Error,
                    format!(
                        "{} no longer exists — removing it from the recent list.",
                        display_uri(&uri)
                    ),
                );
                state.recent_projects.lock().unwrap().retain(|u| u != &uri);
                return;
            }
            let opener = dialogs.clone();
            confirm_discard_unsaved_then(state, &dialogs, move |state| {
                open_project_reporting(state, &opener, &uri)
            });
        },
    )));
}

/// Open `uri` as the project, through the same gate and the same
/// reporting the File menu uses.
///
/// The entry point for UI that already *has* a location and so needs no
/// picker: the favorites bar's project rows and its embedded browser
/// (step 6d-2). Routing through here rather than calling
/// [`AppState::open_project_with_outcome`] directly is what keeps the
/// unsaved-changes prompt in front of every destructive open — the whole
/// reason [`confirm_discard_unsaved_then`] exists.
///
/// Unlike [`open_recent`] there is no `stat` pre-check: these callers
/// name a location they have just listed or that the user just pinned, so
/// a missing file is reported by the read itself instead of being
/// silently pruned from a list it is not in.
pub fn open_project_gated(
    state: &AppState,
    dialogs: &Arc<dyn FileDialogProvider>,
    uri: StorageUri,
) {
    if storage_busy(state) {
        return;
    }
    let opener = dialogs.clone();
    confirm_discard_unsaved_then(state, dialogs, move |state| {
        open_project_reporting(state, &opener, &uri)
    });
}

/// Apply the current theme + accent combination to agg-gui's live
/// visuals. Called whenever either changes — same shape as the demo's
/// `apply_theme_visuals`.
fn apply_theme_visuals(theme: ThemePreference, accent: AccentColor) {
    use agg_gui::theme::{set_visuals, Visuals};
    let base = match theme {
        ThemePreference::Light => Visuals::light(),
        // System currently falls back to Dark; if agg-gui later grows
        // a `detect_system_theme()` AtomArtist can plug it in here.
        ThemePreference::Dark | ThemePreference::System => Visuals::dark(),
    };
    set_visuals(base.with_accent_color(accent));
}

/// Dispatch one menu action id — the entry point
/// [`crate::top_menu_bar`]'s callback calls, and the one an end-to-end
/// test drives to exercise a File-menu flow exactly as a click would
/// (the ids are the same strings `compose_menus` bakes into the items).
pub fn handle_action(
    state: &AppState,
    dialogs: &Arc<dyn FileDialogProvider>,
    debug: &DebugWindowHandles,
    action: &str,
) {
    if let Some(type_id) = action.strip_prefix("add.") {
        // Find the action's NodeDef by its dynamic type_id string and
        // intern it. Registry stores &'static str ids; we look up the
        // exact one rather than leaking new memory each call.
        let interned = state
            .registry
            .iter()
            .map(|d| d.type_id())
            .find(|s| *s == type_id);
        if let Some(static_id) = interned {
            // Add into the *active* graph (the component template when
            // drilled in, else the root) and push the undo command onto
            // that graph's stack — mirrors `AppStateModel::add_node`.
            // Build the node off-graph, then hand the full instance to
            // AddNodeCmd so redo can re-insert it.
            let ag = state.active_graph();
            let node = {
                let mut g = ag.lock().unwrap();
                // Menu-add has no cursor position; drop the node in open
                // space to the right of the current graph so it doesn't
                // stack on prior adds or hide behind existing nodes.
                let pos = crate::node_helpers::next_menu_add_position(&g);
                crate::node_helpers::add_node_with_defaults(
                    &mut g,
                    &state.registry,
                    static_id,
                    pos,
                )
                .and_then(|id| g.remove_node(id).ok().map(|(node, _detached)| node))
            };
            if let Some(node) = node {
                let cmd = AddNodeCmd::new(ag, node);
                state.active_undo().lock().unwrap().add_and_do(Box::new(cmd));
                state.schedule_evaluate_after_edit();
            }
        }
        return;
    }
    // Accent swatch picker — routes to the shared `Visuals` apply path
    // so the chosen colour flows through every widget on the next frame.
    if let Some(key) = action.strip_prefix("view.accent.") {
        if let Some(accent) = AccentColor::from_key(key) {
            *state.accent_color.lock().unwrap() = accent;
            let theme = *state.theme.lock().unwrap();
            apply_theme_visuals(theme, accent);
        }
        return;
    }
    if let Some(theme) = match action {
        "view.theme.light" => Some(ThemePreference::Light),
        "view.theme.dark" => Some(ThemePreference::Dark),
        "view.theme.system" => Some(ThemePreference::System),
        _ => None,
    } {
        *state.theme.lock().unwrap() = theme;
        let accent = *state.accent_color.lock().unwrap();
        apply_theme_visuals(theme, accent);
        return;
    }
    // Recent-file opens: the index refers into the live MRU list.
    if let Some(idx) = action.strip_prefix("file.recent.") {
        let Some(uri) = idx
            .parse::<usize>()
            .ok()
            .and_then(|i| state.recent_projects.lock().unwrap().get(i).cloned())
        else {
            return;
        };
        if storage_busy(state) {
            return;
        }
        open_recent(state, dialogs, uri);
        return;
    }
    if let Some(ext) = action.strip_prefix("file.export.") {
        if storage_busy(state) {
            return;
        }
        let format = match ext {
            "stl" => Some(MeshExportFormat::Stl),
            "3mf" => Some(MeshExportFormat::ThreeMf),
            "obj" => Some(MeshExportFormat::Obj),
            _ => None,
        };
        if let Some(format) = format {
            let name = export_default_name(state, format.extension());
            let job = dialogs.pick_save_export(format.extension(), &name);
            pick_then(state, "Choosing an export destination", job, move |state, uri| {
                state.export_mesh_to_uri(&uri, format)
            });
        } else if ext == "atmr" {
            let name = export_default_name(state, "atmr");
            let job = dialogs.pick_save_export("atmr", &name);
            pick_then(state, "Choosing an export destination", job, |state, uri| {
                state.export_project_copy_to_uri(&uri)
            });
        }
        return;
    }
    match action {
        "edit.undo" => {
            // Route to the active graph's undo stack — the component
            // template's stack when drilled in, else the root stack.
            state.active_undo().lock().unwrap().undo();
            state.schedule_evaluate_after_edit();
        }
        "edit.redo" => {
            state.active_undo().lock().unwrap().redo();
            state.schedule_evaluate_after_edit();
        }
        // Selection-scoped edits. The canvas widget owns the multi-
        // selection (this state mirrors only the primary one), and this
        // callback cannot reach the widget tree, so both actions are
        // queued on the shared `NodeEditorHandle` and applied by the
        // editor's next layout pass. Deletion runs through the model,
        // which pushes an undo command and schedules re-evaluation.
        "edit.delete" => {
            state.node_editor.push(NodeEditorCommand::DeleteSelection);
        }
        "edit.select_all" => {
            state.node_editor.push(NodeEditorCommand::SelectAll);
        }
        "file.new" => {
            if storage_busy(state) {
                return;
            }
            confirm_discard_unsaved_then(state, dialogs, |state| state.new_empty_project());
        }
        "file.open" => {
            if storage_busy(state) {
                return;
            }
            // The picker deliberately runs *after* the gate, inside its
            // continuation: with an asynchronous provider the Save the
            // user just asked for is still in flight here, and asking
            // "which file?" before knowing that save succeeded would
            // leave a picked file with nowhere to go.
            let picker = dialogs.clone();
            confirm_discard_unsaved_then(state, dialogs, move |state| {
                let job = picker.pick_open_project();
                pick_then(state, "Choosing a project to open", job, move |state, uri| {
                    open_project_reporting(state, &picker, &uri)
                });
            });
        }
        "file.save" => {
            if storage_busy(state) {
                return;
            }
            save_current(state, dialogs);
        }
        "file.save_as" => {
            if storage_busy(state) {
                return;
            }
            let suggested = state
                .current_file
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|uri| uri.file_name().map(|n| n.to_string()))
                .unwrap_or_else(|| "untitled.atmr".to_string());
            let dialogs = dialogs.clone();
            let job = dialogs.pick_save_project(&suggested);
            pick_then(state, "Choosing where to save", job, move |state, uri| {
                save_project_reporting(state, &dialogs, &uri, |_state| {})
            });
        }
        "file.import" => {
            // Import adds to the scene rather than replacing it, so no
            // unsaved-changes gate — but it does put a picker up and read
            // bytes, so it takes the same re-entry guard every other File
            // action does (a second Import while the first picker is up
            // would stack pickers).
            if storage_busy(state) {
                return;
            }
            let job = dialogs.pick_import_file();
            pick_then(state, "Choosing a file to import", job, |state, uri| {
                state.import_scene_file(&uri)
            });
        }
        "help.about" => {
            dialogs.show_info(
                "About AtomArtist",
                &format!(
                    "AtomArtist v{}\n\n\
                    A pure-Rust visual node-based 3D design tool.\n\
                    Built on agg-gui + manifold-rust + clipper2-rust + tess2-rust.\n\n\
                    https://github.com/larsbrubaker/atomartist",
                    env!("CARGO_PKG_VERSION"),
                ),
            );
        }
        "help.license" => {
            dialogs.show_info(
                "License",
                "AtomArtist is licensed under the MIT License.\n\
                See the LICENSE file in the project root for the full text.",
            );
        }
        "help.docs" => {
            dialogs.show_info(
                "Documentation",
                "Documentation lives in README.md and CLAUDE.md\n\
                in the project repository.\n\n\
                https://github.com/larsbrubaker/atomartist",
            );
        }
        "view.debug.inspector" => {
            debug.inspector_visible.set(!debug.inspector_visible.get());
        }
        "view.debug.performance" => {
            debug.perf_visible.set(!debug.perf_visible.get());
        }
        _ => {}
    }
}

// Tests live in `menu_actions_tests.rs` so this file stays under the
// 800-line cap.
#[cfg(test)]
#[path = "menu_actions_tests.rs"]
mod menu_actions_tests;
