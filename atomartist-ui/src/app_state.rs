//! Shared application state owned by `demo-native` and `demo-wasm` and read
//! by every widget that needs to mutate the graph or display its current
//! evaluation result.
//!
//! The state is `Arc`-shared so the live evaluator can run on a background
//! thread on native (touching only the `Mutex<Graph>` and writing the
//! computed mesh into `last_mesh_output`). On WASM the evaluator is invoked
//! synchronously each frame, but the same shape works without modification.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use agg_gui::undo::UndoBuffer;
use atomartist_storage::{StorageRegistry, StorageUri};
use atomartist_lib::geometry::Geometry3d;
use atomartist_lib::graph::executor::evaluate_dirty;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::undo_commands::{ChangePropertyCmd, ChangePropsCmd};
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::serialization::{ChangeTracker, SavedBaseline};
use atomartist_lib::Graph;
use atomartist_renderer::{
    CameraPoseAnimation, OrbitCamera, ProjectionAnimation, RenderStyle, ViewportTool,
};

/// One drilled-in editing level on the component-navigation stack.
///
/// When the user double-clicks a component node the UI pushes an
/// `EditLevel` naming the template being edited and holding a **fresh**
/// undo stack — component-edit history is scoped to the level and dropped
/// when the user exits (a v1 simplification; see
/// [`AppState::exit_to`]). The root graph keeps the long-lived undo stack
/// on `AppState::undo`.
#[derive(Clone)]
pub struct EditLevel {
    /// Human-readable component name (the def's `display_name`) — the
    /// breadcrumb label for the next step.
    pub label: String,
    /// The component's registered `type_id`, for breadcrumb / diagnostics.
    pub type_id: String,
    /// The live template graph being edited in place (shared with every
    /// instance's `SubgraphNodeDef`).
    pub graph: Arc<Mutex<Graph>>,
    /// Undo stack scoped to this level's edits — discarded on exit.
    pub undo: Arc<Mutex<UndoBuffer>>,
}

/// Top-level state passed by reference into every UI widget that mutates
/// the graph or reads evaluation results.
pub struct AppState {
    pub graph: Arc<Mutex<Graph>>,
    pub registry: Arc<NodeRegistry>,
    pub undo: Arc<Mutex<UndoBuffer>>,
    /// Component drill-in stack. Empty = editing the root graph; each
    /// pushed [`EditLevel`] redirects the node-canvas model + its undo
    /// stack to the component template on top. The 3-D viewport and the
    /// evaluator always stay on the root graph. Shared via `Arc` so the
    /// `AppStateModel` clone and the breadcrumb widget observe the same
    /// stack.
    pub edit_stack: Arc<Mutex<Vec<EditLevel>>>,
    /// Most recently computed output geometry (for the 3D viewport).
    /// Carries the mesh **plus** the per-node `matrix` and `color`
    /// pulled forward from upstream (see
    /// [`atomartist_lib::geometry::Geometry3d`]), so the renderer
    /// can read both the triangle data and the material tint /
    /// alpha that drive the shader's `base_color`. Written by
    /// `schedule_evaluate`, read by
    /// `Viewport3dWidget::needs_draw` / `current_geometry`.
    pub last_mesh_output: Arc<Mutex<Option<Arc<Geometry3d>>>>,
    /// Set whenever the graph or its outputs change so the viewport knows
    /// to repaint.
    pub viewport_dirty: Arc<AtomicBool>,
    /// Monotonic ticket counter for evaluation requests, paired with
    /// `eval_published` so out-of-order evaluation threads can't
    /// publish an older mesh over a newer one (each `schedule_evaluate`
    /// spawns a thread; during a drag several are in flight at once
    /// and finish in arbitrary order — without the guard the viewport
    /// snaps backward a frame, visible as bounce).
    pub eval_ticket: Arc<std::sync::atomic::AtomicU64>,
    /// Highest ticket whose result has been stored into
    /// `last_mesh_output`.
    pub eval_published: Arc<std::sync::atomic::AtomicU64>,
    /// The node id whose output should be displayed in the viewport. When
    /// `None`, the viewport shows nothing (empty grid). Phase 4+ wires this
    /// up to user selection.
    pub display_node: Arc<Mutex<Option<NodeId>>>,
    /// The node id currently highlighted as "selected" — drives the
    /// outline silhouette in the 3-D viewport and the canvas-side highlight
    /// of the source node.  Synchronised between the canvas (left-click on
    /// a node) and the viewport (left-click on a mesh).  `None` when nothing
    /// is selected.
    pub selection: Arc<Mutex<Option<NodeId>>>,
    /// Location of the currently-open project (`Save` writes here without
    /// re-prompting). `None` when the project has never been saved. A
    /// [`StorageUri`], not a path — the project may live on disk, in
    /// browser storage, or behind a remote provider.
    pub current_file: Arc<Mutex<Option<StorageUri>>>,
    /// Latest known node-canvas zoom — written by `NodeCanvas` on each
    /// wheel event and read by `StatusBar` for the bottom-bar percentage.
    pub canvas_zoom: Arc<Mutex<f64>>,
    /// Latest known node-canvas pan offset (editor-local translation of
    /// canvas space) — written by the node editor on each pan / zoom and
    /// read by [`crate::drag_insert`] to map a pointer *outside* the
    /// canvas (over the favorites bar) into canvas coordinates.
    pub canvas_pan: Arc<Mutex<[f64; 2]>>,
    /// Shared 3-D viewport orbit camera.  The viewport widget and the
    /// tumble cube widget both read / write this through the
    /// `Arc<Mutex<>>` so click-to-orient on the cube takes effect on
    /// the very next viewport paint.
    pub camera: Arc<Mutex<OrbitCamera>>,
    /// Active default-left-mouse tool, picked by the radio cluster of
    /// buttons around the tumble cube.
    pub viewport_tool: Arc<Mutex<ViewportTool>>,
    /// Turntable vs. trackball orbit mode toggle. Mirrors MatterCAD's
    /// `UserSettingsKey.TurntableMode`. Default `true` (turntable).
    pub turntable: Arc<Mutex<bool>>,
    /// Perspective vs. orthographic projection toggle. Mirrors
    /// MatterCAD's `UserSettingsKey.PerspectiveMode`. Default `true`
    /// (perspective).
    pub perspective: Arc<Mutex<bool>>,
    /// Render style picker beneath the tumble cube (Shaded / Outlines /
    /// Non-Manifold / Polygons / Overhang).
    pub render_style: Arc<Mutex<RenderStyle>>,
    /// Bed-toggle button beneath the cube.  Drives the floor-grid pass
    /// in `WgpuSceneRenderer` so the user can hide the grid when it
    /// distracts from the model.  Default `true` — grid on.
    pub show_bed: Arc<Mutex<bool>>,
    /// Snap-amount picker beneath the cube.  Stub for now (AtomArtist
    /// has no node-snap behaviour yet); selection is recorded so
    /// future grid-snap features can read it. Default `1.0`.
    pub snap_amount: Arc<Mutex<f64>>,
    /// In-flight camera pose animation started by viewport chrome
    /// buttons (Home / Fit). Ticked by `Viewport3dWidget::paint`.
    pub camera_animation: Arc<Mutex<Option<CameraPoseAnimation>>>,
    /// In-flight perspective <-> orthographic projection tween
    /// started by the perspective HUD button. Ticked alongside
    /// `camera_animation` so the camera's `fov_y` / `radius` /
    /// `projection` ease over ~0.25 s instead of snapping. Mirrors
    /// MatterCAD's `TrackballTumbleWidgetExtended.DoSwitchToProjectionMode`.
    pub projection_animation: Arc<Mutex<Option<ProjectionAnimation>>>,
    /// Bytes for every asset embedded in the project (`MeshNode` assets,
    /// future images, etc.). Saved alongside `graph.json` inside the
    /// `.atmr` zip. Cloned via `Arc` so background threads can read
    /// without locking the main app, but writes go through the
    /// `Mutex` to keep insert-and-spawn-node atomic.
    pub assets: Arc<Mutex<atomartist_lib::serialization::AssetStore>>,
    /// Most recent viewport preview, already encoded as a 256×192 PNG
    /// (see [`crate::thumbnail`]). Refreshed opportunistically by the
    /// platform shell — reading pixels back off the GPU is the shell's
    /// business, and it cannot happen synchronously inside a save — and
    /// embedded as `Metadata/thumbnail.png` by the next project write.
    ///
    /// A shell that never fills this (the test harness, WASM today)
    /// simply writes projects without a preview; the entry is optional
    /// forever. The preview may therefore be a few seconds older than
    /// the graph it ships with, which is the trade the design accepts.
    pub latest_thumbnail: Arc<Mutex<Option<Vec<u8>>>>,
    /// User-selected theme + accent color. The View menu's Color and
    /// Theme submenus mutate these; `set_visuals` is re-applied from
    /// the combination whenever either changes. Mirrors the demo-ui
    /// pattern (theme + accent picked independently, combined into one
    /// `Visuals` snapshot).
    pub theme: Arc<Mutex<agg_gui::theme::ThemePreference>>,
    pub accent_color: Arc<Mutex<agg_gui::theme::AccentColor>>,
    /// Snapshot-based unsaved-changes detector. Re-baselined on
    /// new / load / save; consulted by the "discard changes?"
    /// prompts before destructive file actions and app close.
    pub change_tracker: Arc<Mutex<ChangeTracker>>,
    /// Most-recently-used project files, newest first, deduped and
    /// capped at [`crate::settings::MAX_RECENT_PROJECTS`]. Fed from
    /// persisted settings at startup; updated on every successful
    /// load / save; rendered as the File → Open Recent submenu.
    pub recent_projects: Arc<Mutex<Vec<StorageUri>>>,
    /// Pinned entries for the left favorites rail (design §2), plus the
    /// "have we ever seeded?" flag. Owned here — not by the bar widget —
    /// for the same reason `recent_projects` is: the shells persist the
    /// row through [`Self::ui_settings`](crate::AppState::ui_settings),
    /// which runs on a frame tick with no access to the widget tree. The
    /// bar mutates this slot and the next settings write picks it up.
    pub favorites: Arc<Mutex<crate::file_browser::Favorites>>,
    /// Whether the favorites bar is expanded into its full panel (`true`)
    /// or collapsed to the icon rail (`false`). Persisted in
    /// [`UiSettings`](crate::settings::UiSettings).
    pub favorites_bar_expanded: Arc<Mutex<bool>>,
    /// Width the bar opens to, in logical pixels. Kept across a
    /// snap-closed collapse so re-opening restores the user's size —
    /// the parts-bar behaviour the design's §2 pins down.
    pub favorites_bar_width: Arc<Mutex<f32>>,
    /// Command channel to the node-canvas widget (`NodeEditor`).
    ///
    /// The canvas owns the *full* multi-selection; this state mirrors
    /// only the primary one (`selection`). Menu items that act on the
    /// whole selection — Edit → Delete Selected / Select All — run in a
    /// callback with no access to the widget tree, so they queue a
    /// [`NodeEditorCommand`](agg_gui_node_editor::NodeEditorCommand)
    /// here and the editor drains it at the start of its next layout.
    pub node_editor: agg_gui_node_editor::NodeEditorHandle,
    /// Scheme -> storage-provider lookup used by every project IO
    /// operation (`app_state_files`). The shell decides what is in it:
    /// `demo-native` registers `LocalFsProvider`, the test harness a
    /// `MemoryProvider`, `demo-wasm` (for now) nothing at all. This
    /// crate deliberately registers no provider of its own — hard-coding
    /// a platform backend here is exactly what the storage seam exists
    /// to prevent.
    pub storage: Arc<StorageRegistry>,
    /// Storage operations whose [`atomartist_storage::Job`] has not
    /// settled yet, drained once per frame by
    /// [`AppState::pump_storage`](crate::storage_ops). Synchronous
    /// providers never put anything here — `submit_op` applies those
    /// inline. The whole pump lives in `storage_ops.rs`.
    pub(crate) pending_ops: crate::storage_ops::PendingOps,
    /// User-facing messages posted by storage continuations, which run
    /// outside the widget tree and so have no dialog provider to talk
    /// to. Drained by the UI on its next paint.
    pub(crate) notices: crate::storage_ops::Notices,
    /// The most recent drained [`Notice`](crate::storage_ops::Notice),
    /// kept so the status bar has something to paint after the queue is
    /// emptied. Written by
    /// [`AppState::pump_notices`](crate::storage_ops), cleared by
    /// [`AppState::dismiss_notice`](crate::storage_ops) when the user
    /// clicks the message.
    pub(crate) last_notice: crate::storage_ops::LastNotice,
}

impl AppState {
    /// State with an **empty** storage registry: every project IO
    /// operation will fail with "no storage provider for scheme …".
    /// Shells and tests use [`Self::with_storage`] to supply backends.
    pub fn new(graph: Graph, registry: NodeRegistry) -> Self {
        Self::with_storage(graph, registry, Arc::new(StorageRegistry::new()))
    }

    pub fn with_storage(
        graph: Graph,
        registry: NodeRegistry,
        storage: Arc<StorageRegistry>,
    ) -> Self {
        // Baseline the tracker on the graph we're handed so "unsaved
        // changes" means "diverged from launch state" until the first
        // save / load establishes a real on-disk baseline.
        let change_tracker = {
            let mut t = ChangeTracker::new();
            t.mark_saved(&graph);
            t
        };
        let registry = Arc::new(registry);
        // Seed the primitive palette here rather than in the shells: the
        // registry is in hand, and every entry point that builds an
        // `AppState` (both shells, the UI-test harness, a bare unit test)
        // then starts with the same rail. A shell that later applies a
        // settings file replaces this row wholesale — including with the
        // deliberately empty one of a user who cleared it — because
        // `apply_ui_settings` assigns the persisted value and only *then*
        // runs the seed-once, which the persisted flag disarms.
        //
        // A registry with no seed types in it (plenty of unit tests build
        // one) leaves the flag alone — see `Favorites::seed_defaults_once`.
        let favorites = {
            let mut favorites = crate::file_browser::Favorites::default();
            favorites.seed_defaults_once(&registry);
            favorites
        };
        Self {
            graph: Arc::new(Mutex::new(graph)),
            registry,
            undo: Arc::new(Mutex::new(UndoBuffer::new())),
            edit_stack: Arc::new(Mutex::new(Vec::new())),
            last_mesh_output: Arc::new(Mutex::new(None)),
            viewport_dirty: Arc::new(AtomicBool::new(false)),
            eval_ticket: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            eval_published: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            display_node: Arc::new(Mutex::new(None)),
            selection: Arc::new(Mutex::new(None)),
            current_file: Arc::new(Mutex::new(None)),
            canvas_zoom: Arc::new(Mutex::new(1.0)),
            canvas_pan: Arc::new(Mutex::new([0.0, 0.0])),
            camera: Arc::new(Mutex::new(OrbitCamera::default())),
            viewport_tool: Arc::new(Mutex::new(ViewportTool::default())),
            turntable: Arc::new(Mutex::new(true)),
            perspective: Arc::new(Mutex::new(true)),
            render_style: Arc::new(Mutex::new(RenderStyle::default())),
            show_bed: Arc::new(Mutex::new(true)),
            snap_amount: Arc::new(Mutex::new(1.0)),
            camera_animation: Arc::new(Mutex::new(None)),
            projection_animation: Arc::new(Mutex::new(None)),
            assets: Arc::new(Mutex::new(
                atomartist_lib::serialization::AssetStore::new(),
            )),
            latest_thumbnail: Arc::new(Mutex::new(None)),
            theme: Arc::new(Mutex::new(agg_gui::theme::ThemePreference::Light)),
            accent_color: Arc::new(Mutex::new(agg_gui::theme::AccentColor::default())),
            change_tracker: Arc::new(Mutex::new(change_tracker)),
            recent_projects: Arc::new(Mutex::new(Vec::new())),
            favorites: Arc::new(Mutex::new(favorites)),
            favorites_bar_expanded: Arc::new(Mutex::new(false)),
            favorites_bar_width: Arc::new(Mutex::new(crate::favorites_bar::DEFAULT_EXPANDED_W)),
            node_editor: agg_gui_node_editor::NodeEditorHandle::new(),
            storage,
            pending_ops: Arc::new(Mutex::new(Vec::new())),
            notices: Arc::new(Mutex::new(Vec::new())),
            last_notice: Arc::new(Mutex::new(None)),
        }
    }

    /// Re-baseline the unsaved-changes tracker on the current graph.
    /// Call after any operation that establishes a new "clean" state
    /// (seeding the starter graph, save, load).
    pub fn mark_saved_baseline(&self) {
        let graph = self.graph.lock().unwrap();
        self.change_tracker.lock().unwrap().mark_saved(&graph);
    }

    /// Capture the current graph as a baseline to install *later*, once
    /// some asynchronous write of those same bytes is confirmed. See
    /// [`Self::apply_saved_baseline`].
    pub fn saved_baseline_now(&self) -> SavedBaseline {
        let graph = self.graph.lock().unwrap();
        ChangeTracker::baseline_of(&graph)
    }

    /// Install a baseline captured by [`Self::saved_baseline_now`].
    ///
    /// The asynchronous counterpart of [`Self::mark_saved_baseline`]: a
    /// save's continuation runs after the graph may have moved on, and
    /// only the bytes that were actually written are "saved". Edits made
    /// while the write was in flight stay unsaved.
    pub fn apply_saved_baseline(&self, baseline: SavedBaseline) {
        self.change_tracker.lock().unwrap().mark_saved_from(baseline);
    }

    /// True when the live graph differs from the last clean baseline.
    /// Drives the "discard changes?" prompts.
    ///
    /// The change tracker only compares the *root* graph against its
    /// baseline, so template edits made while drilled into a component
    /// (`edit_depth() > 0`) are invisible to it — File > New/Open would
    /// skip the confirm prompt and `edit_stack.clear()` would silently
    /// discard those component edits. Until per-template tracking exists,
    /// we take the coarse-but-safe route: any active drill-in reports
    /// unsaved changes, so a user mid-edit always gets the prompt.
    pub fn has_unsaved_changes(&self) -> bool {
        if self.edit_depth() > 0 {
            return true;
        }
        let graph = self.graph.lock().unwrap();
        self.change_tracker.lock().unwrap().has_unsaved_changes(&graph)
    }

    /// Record `uri` as the most recent project, deduping and capping
    /// the list. Called on every successful load / save.
    pub fn note_recent_project(&self, uri: &StorageUri) {
        let mut recent = self.recent_projects.lock().unwrap();
        recent.retain(|u| u != uri);
        recent.insert(0, uri.clone());
        recent.truncate(crate::settings::MAX_RECENT_PROJECTS);
    }

    /// Update the visual selection — the canvas highlights the source
    /// node, and the viewport draws an outline around its mesh. Bumps
    /// the viewport dirty flag AND requests a global redraw so the
    /// canvas-side widget (which reads `primary_selection()` at paint
    /// time) picks up the change when the viewport drives the write.
    pub fn set_selection(&self, id: Option<NodeId>) {
        *self.selection.lock().unwrap() = id;
        self.mark_viewport_dirty();
        // agg-gui's reactive paint loop only repaints on explicit
        // requests — a mutex write inside an `Arc<Mutex<…>>` doesn't
        // count. Without this the canvas would only refresh its
        // node-highlight on the next unrelated event (mouse-move,
        // hover, key-press), so a pure click in the 3-D viewport
        // would visibly fail to "select" the matching node.
        agg_gui::animation::request_draw();
    }

    /// Publish a freshly captured viewport preview (PNG bytes). Called
    /// by the platform shell once its asynchronous frame readback
    /// completes; the next project write embeds it.
    pub fn set_thumbnail_png(&self, png: Vec<u8>) {
        *self.latest_thumbnail.lock().unwrap() = Some(png);
    }

    /// Drop the parked preview because the model it shows is no longer
    /// the open document (File → New, File → Open). Without this a save
    /// issued before the shell's next capture would embed the *previous*
    /// project's picture — a mislabel that then lives in the file.
    pub fn clear_thumbnail_png(&self) {
        *self.latest_thumbnail.lock().unwrap() = None;
    }

    /// The preview to embed in the project being written right now, if
    /// the shell has produced one yet.
    pub fn thumbnail_png(&self) -> Option<Vec<u8>> {
        self.latest_thumbnail.lock().unwrap().clone()
    }

    /// Set the dirty flag so the viewport repaints next frame.
    pub fn mark_viewport_dirty(&self) {
        self.viewport_dirty.store(true, Ordering::Relaxed);
    }

    /// Take + reset the dirty flag — used by the viewport widget.
    pub fn take_viewport_dirty(&self) -> bool {
        self.viewport_dirty.swap(false, Ordering::Relaxed)
    }

    /// Kick off an evaluation pass.
    ///
    /// On native, spawns a background thread that locks the graph, runs
    /// `evaluate_dirty`, picks the display node's mesh output, and stores
    /// it in `last_mesh_output`. On WASM, runs synchronously in the same
    /// frame.
    ///
    /// The dirty flag is set on completion so the viewport repaints.
    pub fn schedule_evaluate(&self) {
        let task = self.make_eval_task();
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::thread::spawn(move || {
                task.run();
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            task.run();
        }
    }

    /// Synchronous alternative — used by tests and tight code paths that
    /// need the result immediately.
    pub fn evaluate_now(&self) {
        self.make_eval_task().run();
    }

    /// Only the Send parts of AppState — UndoBuffer is !Send because
    /// its `Box<dyn UndoRedoCommand>` trait objects don't carry Send.
    /// Each task takes a fresh monotonic ticket so stale evaluation
    /// threads can't publish over a newer result.
    fn make_eval_task(&self) -> EvalTask {
        EvalTask {
            graph: self.graph.clone(),
            registry: self.registry.clone(),
            last_mesh_output: self.last_mesh_output.clone(),
            viewport_dirty: self.viewport_dirty.clone(),
            display_node: self.display_node.clone(),
            ticket: 1 + self.eval_ticket.fetch_add(1, Ordering::Relaxed),
            published: self.eval_published.clone(),
        }
    }

    /// Set the display target — the canvas calls this when the user
    /// selects a node with a Geometry3d output.
    pub fn set_display_node(&self, id: Option<NodeId>) {
        *self.display_node.lock().unwrap() = id;
        self.mark_viewport_dirty();
    }

    /// Write a fresh `matrix` value onto a node and push the change
    /// onto the undo stack — coalescing mid-stroke samples into a
    /// single undo step for in-progress drags.
    ///
    /// Used by the 3-D viewport's body-drag handlers (XY bed plane,
    /// Z control). Bypasses the `NodeGraphModel::set_property` bridge
    /// because that bridge filters out `Matrix4x4` writes (the
    /// canvas-side property panel only edits scalars). Caller
    /// guarantees the matrix is a valid 4×4 column-major transform.
    pub fn set_node_matrix_with_undo(&self, id: NodeId, matrix: [f32; 16]) {
        self.set_node_property_with_undo(id, "matrix", PortValue::Matrix4x4(matrix));
    }

    /// Write a fresh numeric `name` property (e.g. `"height"`) onto a
    /// node, coalesced + undoable exactly like
    /// [`Self::set_node_matrix_with_undo`]. Used by the 3-D scale
    /// controls when the selected node exposes an editable dimension
    /// parameter (the field-editing path, vs. matrix scaling).
    pub fn set_node_number_with_undo(&self, id: NodeId, name: &str, value: f64) {
        self.set_node_property_with_undo(id, name, PortValue::Number(value));
    }

    /// Write a numeric property **and** the matrix in one atomic update
    /// with a single evaluation. The height control's field path needs
    /// the pair to land together: the height edit rebuilds the mesh and
    /// the matrix carries the base-lock compensation — written
    /// separately, an evaluation can catch the gap between them and
    /// paint a frame where the body has scaled but not yet re-anchored
    /// (visible bounce). One `ChangePropsCmd` per stroke, so a single
    /// Ctrl+Z restores both values (MatterCAD's one "Scale" entry).
    pub fn set_node_number_and_matrix_with_undo(
        &self,
        id: NodeId,
        name: &str,
        value: f64,
        matrix: [f32; 16],
    ) {
        let values = [PortValue::Matrix4x4(matrix), PortValue::Number(value)];
        let names = ["matrix", name];
        let coalesced = self.undo.lock().unwrap().try_coalesce_last(|top| {
            if let Some(cmd) = top.as_any_mut().downcast_mut::<ChangePropsCmd>() {
                if cmd.matches(id, &names) {
                    cmd.extend_into(&values);
                    return true;
                }
            }
            false
        });
        if !coalesced {
            let props = names
                .iter()
                .zip(values.iter())
                .map(|(n, v)| (std::sync::Arc::<str>::from(*n), v.clone()))
                .collect();
            let cmd = ChangePropsCmd::new(self.graph.clone(), id, props)
                .with_registry(self.registry.clone());
            self.undo.lock().unwrap().add_and_do(Box::new(cmd));
        }
        self.schedule_evaluate();
    }

    /// Shared coalesced-undo write for a single node property. Merges
    /// mid-stroke samples into one undo step (matching `AppStateModel`'s
    /// property-panel writes, so a 3-D drag and a slider edit of the
    /// same property collapse together), then re-evaluates so the
    /// viewport reflects the change each frame.
    fn set_node_property_with_undo(&self, id: NodeId, name: &str, value: PortValue) {
        self.apply_property_cmd(id, name, value);
        self.schedule_evaluate();
    }

    /// Apply one coalesced `ChangePropertyCmd` to the graph + undo
    /// stack WITHOUT scheduling an evaluation — callers batch several
    /// writes and evaluate once.
    fn apply_property_cmd(&self, id: NodeId, name: &str, value: PortValue) {
        let name: std::sync::Arc<str> = std::sync::Arc::<str>::from(name);
        let coalesced = {
            let name_clone = name.clone();
            self.undo.lock().unwrap().try_coalesce_last(|top| {
                if let Some(cmd) = top.as_any_mut().downcast_mut::<ChangePropertyCmd>() {
                    if cmd.id == id && cmd.name == name_clone {
                        cmd.extend_into(value.clone());
                        return true;
                    }
                }
                false
            })
        };
        if !coalesced {
            let cmd = ChangePropertyCmd::new(self.graph.clone(), id, name, value)
                .with_registry(self.registry.clone());
            self.undo.lock().unwrap().add_and_do(Box::new(cmd));
        }
    }
}

/// Send-only subset of `AppState` used by the background evaluator.
struct EvalTask {
    graph: Arc<Mutex<Graph>>,
    registry: Arc<NodeRegistry>,
    last_mesh_output: Arc<Mutex<Option<Arc<Geometry3d>>>>,
    viewport_dirty: Arc<AtomicBool>,
    display_node: Arc<Mutex<Option<NodeId>>>,
    /// Monotonic schedule ticket (see `AppState::eval_ticket`).
    ticket: u64,
    /// Highest ticket already published — guards the result store.
    published: Arc<std::sync::atomic::AtomicU64>,
}

impl EvalTask {
    fn run(self) {
        let mesh = {
            let mut g = self.graph.lock().unwrap();
            let _ = evaluate_dirty(&mut g, &self.registry);
            self.pick_display_mesh(&g)
        };
        {
            // Publish only if no newer evaluation already has —
            // spawned threads finish in arbitrary order, and an older
            // result landing after a newer one snaps the viewport
            // backward a frame (visible bounce during drags). The
            // check + store happen under the output lock so two
            // threads can't interleave between them.
            let mut out = self.last_mesh_output.lock().unwrap();
            if self.published.load(Ordering::Acquire) < self.ticket {
                *out = mesh;
                self.published.store(self.ticket, Ordering::Release);
            }
        }
        self.viewport_dirty.store(true, Ordering::Relaxed);
        // Same class of problem the storage completion hook solves: on
        // native this runs on a spawned thread, and a reactive host parked
        // in `ControlFlow::Wait` never reads the dirty flag it just set.
        // `signal_async_state_change` publishes a cross-thread bump *and*
        // fires the shell's host waker, so the new mesh reaches the
        // viewport without waiting for the user to jiggle the mouse.
        // Any-thread safe by construction (an atomic plus the waker), and
        // a no-op beyond the atomic when no waker is installed.
        agg_gui::animation::signal_async_state_change();
    }

    /// Pick the geometry bundle to display in the viewport. Returns
    /// the full [`Geometry3d`] (mesh + matrix + colour) so the
    /// renderer can drive its `base_color` uniform from the
    /// upstream node's colour property — without this the shader
    /// would always paint the new() default tint regardless of what
    /// the user set on the node.
    fn pick_display_mesh(&self, g: &Graph) -> Option<Arc<Geometry3d>> {
        // Resolve the geometry a node contributes to the viewport. The
        // Output node exposes its merged group on a socket explicitly
        // named `__display__` (the concatenation of every connected
        // body) *alongside* per-input pass-through Geometry3d outputs, so
        // we prefer `__display__` by name and only fall back to "first
        // Geometry3d output" for single-output primitives used as an
        // explicit preview target. The old code picked "first Geometry3d
        // cached output" unconditionally; because `cached_outputs` is a
        // HashMap, that could return one single-body pass-through instead
        // of the full merge — so a second body wired into Output never
        // showed. Selecting by name makes multi-body Output deterministic.
        let display_geometry = |n: &atomartist_lib::graph::node::NodeInstance| {
            n.outputs
                .iter()
                .find(|s| s.name.as_ref() == "__display__")
                .and_then(|s| n.cached_outputs.get(&s.uid))
                .or_else(|| {
                    n.cached_outputs
                        .values()
                        .find(|v| matches!(v, PortValue::Geometry3d(_)))
                })
                .and_then(|v| match v {
                    PortValue::Geometry3d(g) => Some(g.clone()),
                    _ => None,
                })
        };
        let non_empty = |m: &Arc<Geometry3d>| {
            !(m.is_empty()
                || m.iter()
                    .all(|b| atomartist_lib::geometry::num_tris(&b.mesh) == 0))
        };

        // Explicit programmatic override: an app / test may pin a specific
        // node's geometry via `set_display_node` (the starter graph pins
        // the Output node). Canvas selection deliberately does NOT drive
        // this — see `AppStateModel::on_primary_selection_changed` — so an
        // unconnected primitive never renders just because it is selected.
        let display_id = *self.display_node.lock().unwrap();
        if let Some(id) = display_id {
            if let Some(m) = g.get(id).and_then(display_geometry) {
                if non_empty(&m) {
                    return Some(m);
                }
            }
        }

        // Default: only render what's wired into the Output node. An
        // unconnected primitive sitting on the canvas is "not
        // outputting" and should NOT show in the viewport — matches
        // NodeDesigner / MatterCAD semantics. An empty Output (no
        // connections, or a zero-tri merged mesh) returns `None` so the
        // viewport renders nothing.
        let output_node = g.nodes().find(|n| n.type_id.as_ref() == "Output")?;
        let display_geom = display_geometry(output_node)?;
        if non_empty(&display_geom) {
            Some(display_geom)
        } else {
            None
        }
    }
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            registry: self.registry.clone(),
            undo: self.undo.clone(),
            edit_stack: self.edit_stack.clone(),
            last_mesh_output: self.last_mesh_output.clone(),
            viewport_dirty: self.viewport_dirty.clone(),
            eval_ticket: self.eval_ticket.clone(),
            eval_published: self.eval_published.clone(),
            display_node: self.display_node.clone(),
            selection: self.selection.clone(),
            current_file: self.current_file.clone(),
            canvas_zoom: self.canvas_zoom.clone(),
            canvas_pan: self.canvas_pan.clone(),
            camera: self.camera.clone(),
            viewport_tool: self.viewport_tool.clone(),
            turntable: self.turntable.clone(),
            perspective: self.perspective.clone(),
            render_style: self.render_style.clone(),
            show_bed: self.show_bed.clone(),
            snap_amount: self.snap_amount.clone(),
            camera_animation: self.camera_animation.clone(),
            projection_animation: self.projection_animation.clone(),
            assets: self.assets.clone(),
            latest_thumbnail: self.latest_thumbnail.clone(),
            theme: self.theme.clone(),
            accent_color: self.accent_color.clone(),
            change_tracker: self.change_tracker.clone(),
            recent_projects: self.recent_projects.clone(),
            favorites: self.favorites.clone(),
            favorites_bar_expanded: self.favorites_bar_expanded.clone(),
            favorites_bar_width: self.favorites_bar_width.clone(),
            // Shared, not copied: the clone a menu callback holds must
            // reach the same canvas widget the tree's clone installed.
            node_editor: self.node_editor.clone(),
            storage: self.storage.clone(),
            // Shared, not copied: a clone handed to a widget must see the
            // same in-flight operations the shell's pump drains.
            pending_ops: self.pending_ops.clone(),
            notices: self.notices.clone(),
            last_notice: self.last_notice.clone(),
        }
    }
}

// File operations (load / save / import / export) live in
// `app_state_files.rs`, and the frame-loop storage job pump
// (`submit_op` / `pump_storage` / notices) in `storage_ops.rs`, to keep
// this file under the 800-line cap.
