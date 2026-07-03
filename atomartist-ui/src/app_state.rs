//! Shared application state owned by `demo-native` and `demo-wasm` and read
//! by every widget that needs to mutate the graph or display its current
//! evaluation result.
//!
//! The state is `Arc`-shared so the live evaluator can run on a background
//! thread on native (touching only the `Mutex<Graph>` and writing the
//! computed mesh into `last_mesh_output`). On WASM the evaluator is invoked
//! synchronously each frame, but the same shape works without modification.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use agg_gui::undo::UndoBuffer;
use atomartist_lib::geometry::Geometry3d;
use atomartist_lib::graph::executor::evaluate_dirty;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::undo_commands::{ChangePropertyCmd, ChangePropsCmd};
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::serialization::ChangeTracker;
use atomartist_lib::Graph;
use atomartist_renderer::{
    CameraPoseAnimation, OrbitCamera, ProjectionAnimation, RenderStyle, ViewportTool,
};

/// Top-level state passed by reference into every UI widget that mutates
/// the graph or reads evaluation results.
pub struct AppState {
    pub graph: Arc<Mutex<Graph>>,
    pub registry: Arc<NodeRegistry>,
    pub undo: Arc<Mutex<UndoBuffer>>,
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
    /// Path of the currently-open project file (`Save` writes here without
    /// re-prompting). `None` when the project has never been saved.
    pub current_file: Arc<Mutex<Option<PathBuf>>>,
    /// Latest known node-canvas zoom — written by `NodeCanvas` on each
    /// wheel event and read by `StatusBar` for the bottom-bar percentage.
    pub canvas_zoom: Arc<Mutex<f64>>,
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
    /// Render style picker beneath the tumble cube (Shaded / Wireframe).
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
    pub recent_projects: Arc<Mutex<Vec<PathBuf>>>,
}

impl AppState {
    pub fn new(graph: Graph, registry: NodeRegistry) -> Self {
        // Baseline the tracker on the graph we're handed so "unsaved
        // changes" means "diverged from launch state" until the first
        // save / load establishes a real on-disk baseline.
        let change_tracker = {
            let mut t = ChangeTracker::new();
            t.mark_saved(&graph);
            t
        };
        Self {
            graph: Arc::new(Mutex::new(graph)),
            registry: Arc::new(registry),
            undo: Arc::new(Mutex::new(UndoBuffer::new())),
            last_mesh_output: Arc::new(Mutex::new(None)),
            viewport_dirty: Arc::new(AtomicBool::new(false)),
            eval_ticket: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            eval_published: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            display_node: Arc::new(Mutex::new(None)),
            selection: Arc::new(Mutex::new(None)),
            current_file: Arc::new(Mutex::new(None)),
            canvas_zoom: Arc::new(Mutex::new(1.0)),
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
            theme: Arc::new(Mutex::new(agg_gui::theme::ThemePreference::Light)),
            accent_color: Arc::new(Mutex::new(agg_gui::theme::AccentColor::default())),
            change_tracker: Arc::new(Mutex::new(change_tracker)),
            recent_projects: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Re-baseline the unsaved-changes tracker on the current graph.
    /// Call after any operation that establishes a new "clean" state
    /// (seeding the starter graph, save, load).
    pub fn mark_saved_baseline(&self) {
        let graph = self.graph.lock().unwrap();
        self.change_tracker.lock().unwrap().mark_saved(&graph);
    }

    /// True when the live graph differs from the last clean baseline.
    /// Drives the "discard changes?" prompts.
    pub fn has_unsaved_changes(&self) -> bool {
        let graph = self.graph.lock().unwrap();
        self.change_tracker.lock().unwrap().has_unsaved_changes(&graph)
    }

    /// Record `path` as the most recent project, deduping and capping
    /// the list. Called on every successful load / save.
    pub fn note_recent_project(&self, path: &Path) {
        let mut recent = self.recent_projects.lock().unwrap();
        recent.retain(|p| p != path);
        recent.insert(0, path.to_path_buf());
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
            let cmd = ChangePropsCmd::new(self.graph.clone(), id, props);
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
            let cmd = ChangePropertyCmd::new(self.graph.clone(), id, name, value);
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
    }

    /// Pick the geometry bundle to display in the viewport. Returns
    /// the full [`Geometry3d`] (mesh + matrix + colour) so the
    /// renderer can drive its `base_color` uniform from the
    /// upstream node's colour property — without this the shader
    /// would always paint the new() default tint regardless of what
    /// the user set on the node.
    fn pick_display_mesh(&self, g: &Graph) -> Option<Arc<Geometry3d>> {
        // Look up any Geometry3d cached output on the node — socket
        // names vary across node types (`"out"` for primitives,
        // `"Geometry"` for Extrude). Picking by type is more robust
        // than picking by a hard-coded name.
        let first_geometry = |n: &atomartist_lib::graph::node::NodeInstance| {
            n.cached_outputs.values().find_map(|v| match v {
                PortValue::Geometry3d(g) => Some(g.clone()),
                _ => None,
            })
        };

        // Explicit user override: clicking a node sets `display_node`,
        // which pins the viewport to that node's first geometry
        // regardless of whether anything is wired to Output. Useful
        // for "preview just this node" while building.
        let display_id = *self.display_node.lock().unwrap();
        if let Some(id) = display_id {
            if let Some(n) = g.get(id) {
                if let Some(m) = first_geometry(n) {
                    return Some(m);
                }
            }
        }

        // Default: only render what's wired into the Output node. An
        // unconnected primitive sitting on the canvas is "not
        // outputting" and should NOT show in the viewport — matches
        // NodeDesigner / MatterCAD semantics. The Output node's
        // synthetic `__display__` socket carries the merged geometry
        // of everything wired into its input slots; an empty Output
        // (no connections, or zero-tri merged mesh) returns `None` so
        // the viewport renders nothing.
        let output_node = g.nodes().find(|n| n.type_id.as_ref() == "Output")?;
        let display_geom = output_node.cached_outputs.values().find_map(|v| match v {
            PortValue::Geometry3d(g) => Some(g.clone()),
            _ => None,
        })?;
        // Multi-body group: keep the geometry when at least one body
        // has triangles. An Output node wired to nothing produces an
        // empty bodies vec, which collapses to `None` here so the
        // viewport draws nothing.
        if display_geom.is_empty()
            || display_geom
                .iter()
                .all(|b| atomartist_lib::geometry::num_tris(&b.mesh) == 0)
        {
            return None;
        }
        Some(display_geom)
    }
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            registry: self.registry.clone(),
            undo: self.undo.clone(),
            last_mesh_output: self.last_mesh_output.clone(),
            viewport_dirty: self.viewport_dirty.clone(),
            eval_ticket: self.eval_ticket.clone(),
            eval_published: self.eval_published.clone(),
            display_node: self.display_node.clone(),
            selection: self.selection.clone(),
            current_file: self.current_file.clone(),
            canvas_zoom: self.canvas_zoom.clone(),
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
            theme: self.theme.clone(),
            accent_color: self.accent_color.clone(),
            change_tracker: self.change_tracker.clone(),
            recent_projects: self.recent_projects.clone(),
        }
    }
}

// File operations (load / save / import / export) live in
// `app_state_files.rs` to keep this file under the 800-line cap.
