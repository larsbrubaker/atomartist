//! Project operations on [`AppState`] — new / open / save, the File →
//! Export formats, and the UI-settings snapshot. Mesh + scene import
//! (`.stl` / `.obj` / `.3mf` / `.mcx` / `.atmr`) lives next door in
//! [`crate::app_state_files_import`]; both were split from `app_state.rs`
//! to keep every file under the 800-line cap. Invoked from the menu
//! action handlers in [`crate::menu_actions`].
//!
//! Every operation addresses a project by [`StorageUri`] and moves its
//! bytes through the [`AppState::storage`] registry — this module never
//! touches `std::fs` (enforced by
//! `atomartist-lib/tests/no_fs_outside_provider.rs`). The provider
//! plumbing lives in [`crate::app_state_storage`].
//!
//! # Everything here is submit-and-continue
//!
//! An operation splits at the IO boundary: the provider call becomes a
//! [`JobOp`] handed to the frame pump, and everything downstream of the
//! bytes runs in its continuation. Nothing returns a `Result` for the IO,
//! because the IO has not happened yet when the call returns — failures
//! reach the user as an [`NoticeLevel::Error`] notice on the status bar
//! (see [`crate::storage_ops`]).
//!
//! Two consequences worth remembering:
//!
//! - **No caller may hold an [`AppState`] lock across one of these
//!   calls.** A local provider settles inline, so the continuation runs on
//!   the caller's stack and will deadlock against a held lock.
//! - Anything that must observe the *graph as it is now* (project
//!   serialization, and the saved baseline that describes those same
//!   bytes) happens at submit time, before the job is created. Anything
//!   that records the *outcome* (current file, recents, and *installing*
//!   the captured baseline) happens in the continuation, so a failed
//!   write never reports success.

use atomartist_storage::StorageUri;

use atomartist_lib::nodes::mesh::mesh_node;
use atomartist_lib::serialization::{
    export_3mf, export_obj, export_stl, read_project_from_bytes,
    write_project_to_bytes_with_thumbnail, ChangeTracker,
};
use atomartist_lib::Graph;

use crate::app_state::AppState;
use crate::app_state_storage::{display_uri, read_job, uri_label, write_job};
use crate::storage_ops::{JobOp, NoticeLevel};

/// Mesh formats offered by File → Export. `.atmr` export is separate —
/// it saves the whole project (graph + assets), not baked geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshExportFormat {
    Stl,
    ThreeMf,
    Obj,
}

impl MeshExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            MeshExportFormat::Stl => "stl",
            MeshExportFormat::ThreeMf => "3mf",
            MeshExportFormat::Obj => "obj",
        }
    }
}

impl AppState {
    /// Replace the current graph with an empty one. Clears undo history
    /// and the current-file slot.
    pub fn new_empty_project(&self) {
        *self.graph.lock().unwrap() = Graph::new();
        self.undo.lock().unwrap().clear_history();
        // Discard any drill-in navigation: the old root (and its
        // component templates) is being thrown away, so there's nothing
        // to sync back — clear the stack directly rather than exiting it.
        self.edit_stack.lock().unwrap().clear();
        *self.current_file.lock().unwrap() = None;
        *self.display_node.lock().unwrap() = None;
        *self.selection.lock().unwrap() = None;
        *self.last_mesh_output.lock().unwrap() = None;
        // The parked preview shows the model we just discarded; a save
        // before the shell's next capture would bake it into the new
        // project's file.
        self.clear_thumbnail_png();
        self.mark_saved_baseline();
        self.mark_viewport_dirty();
    }

    /// Open the project stored at `uri`, replacing the current graph.
    ///
    /// Submits the provider read to the frame pump; when the bytes land,
    /// the continuation decodes them, resolves mesh assets, swaps the
    /// graph in, clears undo history + drill-in state, records the file
    /// and the recent entry, and evaluates so the viewport repopulates.
    /// A read or decode failure leaves the current project untouched and
    /// posts an error notice.
    ///
    /// The bytes come from the storage provider that owns the URI's
    /// scheme and are decoded by the format layer; `.atmr` (a zip
    /// archive) is the only project format, and the decoder ignores the
    /// name entirely — see `serialization::atmr`.
    pub fn open_project(&self, uri: &StorageUri) {
        self.open_project_then(uri, |_state| {});
    }

    /// [`Self::open_project`] plus a follow-up that runs only once the
    /// project is actually open — the shape a sequenced flow ("save the
    /// current project, *then* open this one") needs. A failure posts an
    /// [`NoticeLevel::Error`].
    pub fn open_project_then(
        &self,
        uri: &StorageUri,
        on_success: impl FnOnce(&AppState) + Send + 'static,
    ) {
        self.open_project_with_outcome(uri, move |state, outcome| match outcome {
            Ok(()) => on_success(state),
            Err(message) => state.notify(NoticeLevel::Error, message),
        })
    }

    /// The open primitive with **no opinion about how failure is
    /// reported**: `on_done` receives `Ok(())` once the project is live,
    /// or `Err(message)` describing a failed read or a failed decode.
    ///
    /// Exists because not every open is equally serious. A File → Open
    /// the user just asked for is an error ([`Self::open_project_then`]);
    /// the startup auto-reopen of a project that has since been deleted
    /// is not ([`Self::reopen_last_project`]), and posting an error there
    /// would sit in the status bar's single slot suppressing every later
    /// confirmation.
    pub fn open_project_with_outcome(
        &self,
        uri: &StorageUri,
        on_done: impl FnOnce(&AppState, Result<(), String>) + Send + 'static,
    ) {
        let uri = uri.clone();
        let job = read_job(&self.storage, &uri);
        self.submit_op(Box::new(JobOp::new(
            format!("Opening {}", uri_label(&uri)),
            job,
            move |state, result| {
                let outcome = match result {
                    Ok(bytes) => state.apply_opened_project(&uri, &bytes),
                    Err(err) => Err(format!("open {}: {}", display_uri(&uri), err)),
                };
                on_done(state, outcome);
            },
        )));
    }

    /// Startup auto-reopen of the last project the user had open.
    ///
    /// Deliberately *not* [`Self::open_project`]: nobody asked for this
    /// open, so a project that has been deleted or moved since the last
    /// session is ordinary news, not a failure. It reports at
    /// [`NoticeLevel::Info`] (errors are sticky — an Info never displaces
    /// an undismissed one — so an error here would swallow the user's
    /// first "Saved …") and drops the entry from the recent list, with the
    /// underlying reason on stderr for anyone debugging.
    pub fn reopen_last_project(&self, uri: &StorageUri) {
        let entry = uri.clone();
        self.open_project_with_outcome(uri, move |state, outcome| {
            if let Err(message) = outcome {
                eprintln!("startup: last project not reopened: {message}");
                state.recent_projects.lock().unwrap().retain(|u| u != &entry);
                state.notify(
                    NoticeLevel::Info,
                    "Last project no longer available — starting fresh.",
                );
            }
        })
    }

    /// Decode-and-swap half of [`Self::open_project`], run from the read
    /// job's continuation. Returns `Err(message)` — un-notified, so the
    /// caller decides how loud the failure is — when the bytes are not a
    /// project this build can read.
    fn apply_opened_project(&self, uri: &StorageUri, bytes: &[u8]) -> Result<(), String> {
        let (result, assets) = match read_project_from_bytes(bytes, &self.registry) {
            Ok(decoded) => decoded,
            Err(err) => {
                return Err(format!("open {}: {}", display_uri(uri), err));
            }
        };
        // Decode warnings first (schema-version mismatch, unknown node
        // types that were skipped) — they explain missing nodes, so
        // swallowing them turns a partial load into a silent one.
        for w in &result.warnings {
            eprintln!("project load: {}", w);
        }
        let mut graph = result.graph;
        // Resolve every MeshNode's asset reference into a live MeshGL
        // before swapping the graph in — once the executor sees the new
        // graph it'll be eligible for evaluation.
        let warnings = mesh_node::resolve_mesh_assets(&mut graph, &assets);
        for w in &warnings {
            eprintln!("project load: {}", w);
        }
        *self.graph.lock().unwrap() = graph;
        *self.assets.lock().unwrap() = assets;
        self.undo.lock().unwrap().clear_history();
        // Exit any drilled-in component from the previous project — the
        // old root and its templates are being replaced wholesale, so
        // clear the stack directly (no exit-sync against a discarded
        // graph).
        self.edit_stack.lock().unwrap().clear();
        *self.current_file.lock().unwrap() = Some(uri.clone());
        // Same reason as File → New: the slot still holds a picture of
        // the project we just replaced. Better no preview than the
        // wrong one until the shell captures this model.
        self.clear_thumbnail_png();
        self.mark_saved_baseline();
        self.note_recent_project(uri);
        // Pick a default display node — the highest-id node with a
        // Geometry3d output, matching what evaluate_now does.
        *self.display_node.lock().unwrap() = None;
        *self.selection.lock().unwrap() = None;
        self.evaluate_now();
        Ok(())
    }

    /// Save the current graph to `uri`. Always writes the `.atmr` zip
    /// archive (graph + assets) regardless of the file name.
    ///
    /// The project is serialized *synchronously*, before the write job is
    /// created: it needs the graph lock and must capture the document the
    /// user asked to save, not whatever it has become by the time an
    /// asynchronous provider gets around to the write. Only the write
    /// itself is deferred; `current_file`, the saved baseline, and the
    /// recent list all move in the continuation, so a failed write leaves
    /// the project dirty and still pointing at its old location.
    pub fn save_project(&self, uri: &StorageUri) {
        self.save_project_then(uri, |_state| {});
    }

    /// [`Self::save_project`] plus a follow-up that runs only after the
    /// write is confirmed.
    ///
    /// This is how the unsaved-changes gate sequences "Save, then open
    /// the other project" without blocking: the follow-up is the action
    /// that was waiting on the save. It never runs when the write fails —
    /// the error notice explains why nothing else happened.
    pub fn save_project_then(
        &self,
        uri: &StorageUri,
        on_success: impl FnOnce(&AppState) + Send + 'static,
    ) {
        self.save_project_with_outcome(uri, move |state, outcome| match outcome {
            Ok(()) => on_success(state),
            Err(message) => state.notify(NoticeLevel::Error, message),
        })
    }

    /// The save primitive with **no opinion about how failure is
    /// reported**: `on_done` receives `Ok(())` once the write is
    /// confirmed, or `Err(message)` for a failed serialization or a
    /// failed write.
    ///
    /// [`crate::menu_actions`] uses this to put a *modal* in front of a
    /// failed save on top of the status-bar notice — losing a save costs
    /// the user real work, so it must not be dismissible by not looking
    /// (see `docs/storage-architecture-plan.md` §7).
    pub fn save_project_with_outcome(
        &self,
        uri: &StorageUri,
        on_done: impl FnOnce(&AppState, Result<(), String>) + Send + 'static,
    ) {
        let uri = uri.clone();
        // Bytes and baseline are captured under the same lock, so the
        // baseline installed when the write confirms describes exactly the
        // document that was written — not whatever the graph has become
        // while an asynchronous provider was busy.
        // The preview is read *before* the graph lock: it is an
        // independent slot the shell fills from its paint loop, and
        // taking it first keeps the graph lock's scope to serialization.
        let thumbnail = self.thumbnail_png();
        let (bytes, baseline) = {
            let graph = self.graph.lock().unwrap();
            let assets = self.assets.lock().unwrap();
            (
                write_project_to_bytes_with_thumbnail(&graph, &assets, thumbnail.as_deref()),
                ChangeTracker::baseline_of(&graph),
            )
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(err) => {
                // Serialization failed, so there is nothing to submit —
                // report the outcome inline rather than silently doing
                // nothing.
                on_done(self, Err(format!("write {}: {}", display_uri(&uri), err)));
                return;
            }
        };
        let job = write_job(&self.storage, &uri, bytes);
        self.submit_op(Box::new(JobOp::new(
            format!("Saving {}", uri_label(&uri)),
            job,
            move |state, result| match result {
                Ok(_stamp) => {
                    *state.current_file.lock().unwrap() = Some(uri.clone());
                    state.apply_saved_baseline(baseline);
                    state.note_recent_project(&uri);
                    state.notify(NoticeLevel::Info, format!("Saved {}", uri_label(&uri)));
                    on_done(state, Ok(()));
                }
                Err(err) => {
                    on_done(state, Err(format!("write {}: {}", display_uri(&uri), err)))
                }
            },
        )));
    }

    /// Snapshot the HUD-button state into a [`crate::UiSettings`]
    /// for persistence. Callers serialise this to disk via
    /// `UiSettings::write_to_file`.
    ///
    /// `debug_windows` and `main_window` are filled in with
    /// defaults — those live outside `AppState` (the widget tree
    /// and the platform shell respectively), so the shell is
    /// responsible for splicing the current values in before
    /// writing the settings blob (see `demo-native::main`).
    pub fn ui_settings(&self) -> crate::UiSettings {
        crate::UiSettings {
            perspective: *self.perspective.lock().unwrap(),
            turntable: *self.turntable.lock().unwrap(),
            show_bed: *self.show_bed.lock().unwrap(),
            render_style: *self.render_style.lock().unwrap(),
            snap_amount: *self.snap_amount.lock().unwrap(),
            main_window: crate::MainWindowState::default(),
            debug_windows: crate::DebugWindowsState::default(),
            // Forward the URI of the currently-open project so the
            // shell's AutoSave loop persists it on every paint where
            // it changed. The native shell uses this on next launch
            // to auto-reopen the same file.
            last_project_path: self.current_file.lock().unwrap().clone(),
            theme: *self.theme.lock().unwrap(),
            accent_color: *self.accent_color.lock().unwrap(),
            recent_projects: self.recent_projects.lock().unwrap().clone(),
            // Same "not owned by AppState" caveat as `main_window` /
            // `debug_windows`: the favorites row lives with the
            // favorites bar (step 6d-2), so whoever owns it must
            // splice the live value in before writing the blob —
            // otherwise a save clears the user's row.
            favorites: crate::file_browser::Favorites::default(),
        }
    }

    /// Push a saved [`crate::UiSettings`] snapshot back into the
    /// live `AppState` AND propagate the perspective / turntable
    /// flags into the shared camera so the very first frame after
    /// startup matches what the user left things as. Used by the
    /// demo-native shell on load.
    ///
    /// Takes the settings by reference so the caller can keep them
    /// around for the auto-reopen path (which needs `last_project_path`)
    /// and for `build_app` (which reads `debug_windows`).
    pub fn apply_ui_settings(&self, s: &crate::UiSettings) {
        use atomartist_renderer::{OrbitMode, Projection};
        *self.perspective.lock().unwrap() = s.perspective;
        *self.turntable.lock().unwrap() = s.turntable;
        *self.show_bed.lock().unwrap() = s.show_bed;
        *self.render_style.lock().unwrap() = s.render_style;
        *self.snap_amount.lock().unwrap() = s.snap_amount;
        // Mirror into the camera so the very first paint sees the
        // restored projection / orbit mode (the HUD buttons read
        // from the same `Arc<Mutex<bool>>` slots above, so they're
        // already correct).
        let mut c = self.camera.lock().unwrap();
        c.projection = if s.perspective {
            Projection::Perspective
        } else {
            Projection::Orthographic
        };
        c.orbit_mode = if s.turntable {
            OrbitMode::Turntable
        } else {
            OrbitMode::Trackball
        };
        drop(c);
        *self.recent_projects.lock().unwrap() = s.recent_projects.clone();
        *self.theme.lock().unwrap() = s.theme;
        *self.accent_color.lock().unwrap() = s.accent_color;
        // Push the restored theme + accent into agg-gui's live
        // visuals so the very first paint matches the user's saved
        // selection — same call the View menu uses.
        let base = match s.theme {
            agg_gui::theme::ThemePreference::Light => agg_gui::theme::Visuals::light(),
            agg_gui::theme::ThemePreference::Dark | agg_gui::theme::ThemePreference::System => {
                agg_gui::theme::Visuals::dark()
            }
        };
        agg_gui::theme::set_visuals(base.with_accent_color(s.accent_color));
    }

    /// Save the current displayed geometry in the chosen mesh format.
    /// Reads the triangle data out of [`Geometry3d::mesh`] — mesh
    /// export disregards the per-node matrix + colour bundle the
    /// renderer uses. Multi-body groups are concatenated into a single
    /// mesh before encoding (none of the three formats carries the
    /// per-body split we'd need to keep them apart).
    ///
    /// Encoding happens at submit time (it reads the live geometry);
    /// only the write is deferred. Nothing to export, a failed encode,
    /// and a failed write all surface as error notices.
    pub fn export_mesh_to_uri(&self, uri: &StorageUri, format: MeshExportFormat) {
        let geom = self.last_mesh_output.lock().unwrap().clone();
        let Some(geom) = geom else {
            self.notify(
                NoticeLevel::Error,
                "no geometry to export — wire up a node with a 3D output",
            );
            return;
        };
        let meshes: Vec<_> = geom.iter().map(|b| b.mesh.clone()).collect();
        let merged = atomartist_lib::geometry::merge_meshes(&meshes);
        let bytes = match format {
            MeshExportFormat::Stl => Ok(export_stl(&merged)),
            MeshExportFormat::Obj => Ok(export_obj(&merged)),
            MeshExportFormat::ThreeMf => {
                export_3mf(&merged).map_err(|e| format!("encode 3MF: {}", e))
            }
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(err) => {
                self.notify(NoticeLevel::Error, err);
                return;
            }
        };
        self.submit_write(uri, bytes, "Exporting");
    }

    /// File → Export → AtomArtist Project: write a copy of the whole
    /// project (graph + assets) to `uri` WITHOUT retargeting Save —
    /// `current_file`, the recent list, and the unsaved-changes
    /// baseline all stay put, unlike [`Self::save_project`].
    pub fn export_project_copy_to_uri(&self, uri: &StorageUri) {
        let thumbnail = self.thumbnail_png();
        let bytes = {
            let graph = self.graph.lock().unwrap();
            let assets = self.assets.lock().unwrap();
            write_project_to_bytes_with_thumbnail(&graph, &assets, thumbnail.as_deref())
        };
        match bytes {
            Ok(bytes) => self.submit_write(uri, bytes, "Exporting"),
            Err(err) => self.notify(
                NoticeLevel::Error,
                format!("write {}: {}", display_uri(uri), err),
            ),
        }
    }

    /// Submit a plain "write these bytes" job whose only follow-up is an
    /// error notice — the shape both export paths need. `verb` is the
    /// status-bar label's leading word ("Exporting bracket.stl").
    fn submit_write(&self, uri: &StorageUri, bytes: Vec<u8>, verb: &str) {
        let uri = uri.clone();
        let job = write_job(&self.storage, &uri, bytes);
        self.submit_op(Box::new(JobOp::new(
            format!("{verb} {}", uri_label(&uri)),
            job,
            move |state, result| {
                if let Err(err) = result {
                    state.notify(
                        NoticeLevel::Error,
                        format!("write {}: {}", display_uri(&uri), err),
                    );
                }
            },
        )));
    }
}
