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
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::nodes::mesh::mesh_node;
use atomartist_lib::serialization::{
    export_3mf, export_stl, load_project_with_assets_from_path,
    save_project_with_assets_to_path,
};
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
}

impl AppState {
    pub fn new(graph: Graph, registry: NodeRegistry) -> Self {
        Self {
            graph: Arc::new(Mutex::new(graph)),
            registry: Arc::new(registry),
            undo: Arc::new(Mutex::new(UndoBuffer::new())),
            last_mesh_output: Arc::new(Mutex::new(None)),
            viewport_dirty: Arc::new(AtomicBool::new(false)),
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
        }
    }

    /// Update the visual selection — the canvas highlights the source
    /// node, and the viewport draws an outline around its mesh. Bumps the
    /// viewport dirty flag so the outline pass re-runs.
    pub fn set_selection(&self, id: Option<NodeId>) {
        *self.selection.lock().unwrap() = id;
        self.mark_viewport_dirty();
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
        // Only the Send parts of AppState — UndoBuffer is !Send because
        // its `Box<dyn UndoRedoCommand>` trait objects don't carry Send.
        let task = EvalTask {
            graph: self.graph.clone(),
            registry: self.registry.clone(),
            last_mesh_output: self.last_mesh_output.clone(),
            viewport_dirty: self.viewport_dirty.clone(),
            display_node: self.display_node.clone(),
        };
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
        let task = EvalTask {
            graph: self.graph.clone(),
            registry: self.registry.clone(),
            last_mesh_output: self.last_mesh_output.clone(),
            viewport_dirty: self.viewport_dirty.clone(),
            display_node: self.display_node.clone(),
        };
        task.run();
    }

    /// Set the display target — the canvas calls this when the user
    /// selects a node with a Geometry3d output.
    pub fn set_display_node(&self, id: Option<NodeId>) {
        *self.display_node.lock().unwrap() = id;
        self.mark_viewport_dirty();
    }
}

/// Send-only subset of `AppState` used by the background evaluator.
struct EvalTask {
    graph: Arc<Mutex<Graph>>,
    registry: Arc<NodeRegistry>,
    last_mesh_output: Arc<Mutex<Option<Arc<Geometry3d>>>>,
    viewport_dirty: Arc<AtomicBool>,
    display_node: Arc<Mutex<Option<NodeId>>>,
}

impl EvalTask {
    fn run(self) {
        let mesh = {
            let mut g = self.graph.lock().unwrap();
            let _ = evaluate_dirty(&mut g, &self.registry);
            self.pick_display_mesh(&g)
        };
        *self.last_mesh_output.lock().unwrap() = mesh;
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
        let display_id = *self.display_node.lock().unwrap();
        if let Some(id) = display_id {
            if let Some(n) = g.get(id) {
                if let Some(m) = first_geometry(n) {
                    return Some(m);
                }
            }
        }
        let mut best: Option<(NodeId, Arc<Geometry3d>)> = None;
        for n in g.nodes() {
            if let Some(m) = first_geometry(n) {
                if best.as_ref().map(|(id, _)| n.id > *id).unwrap_or(true) {
                    best = Some((n.id, m));
                }
            }
        }
        best.map(|(_, m)| m)
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
        }
    }
}

// ---------------------------------------------------------------------------
// File operations — invoked from menu actions in `top_menu_bar`.
// ---------------------------------------------------------------------------

impl AppState {
    /// Replace the current graph with an empty one. Clears undo history
    /// and the current-file slot.
    pub fn new_empty_project(&self) {
        *self.graph.lock().unwrap() = Graph::new();
        self.undo.lock().unwrap().clear_history();
        *self.current_file.lock().unwrap() = None;
        *self.display_node.lock().unwrap() = None;
        *self.selection.lock().unwrap() = None;
        *self.last_mesh_output.lock().unwrap() = None;
        self.mark_viewport_dirty();
    }

    /// Load a graph from `path`. Replaces the current graph wholesale,
    /// clears undo history, and runs an initial evaluation so the
    /// viewport repopulates. Returns `Err` with a user-readable message
    /// on parse / IO failure.
    ///
    /// Dispatches on file extension: `.atmr` loads through the zip
    /// container, `.json` reads the raw JSON file. Unknown / missing
    /// extensions try ATMR first — see `serialization::atmr` for
    /// the full dispatch rules.
    pub fn load_graph_from_path(&self, path: &Path) -> Result<(), String> {
        let (result, assets) = load_project_with_assets_from_path(path, &self.registry)
            .map_err(|e| format!("open {}: {}", path.display(), e))?;
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
        *self.current_file.lock().unwrap() = Some(path.to_path_buf());
        // Pick a default display node — the highest-id node with a
        // Geometry3d output, matching what evaluate_now does.
        *self.display_node.lock().unwrap() = None;
        *self.selection.lock().unwrap() = None;
        self.evaluate_now();
        Ok(())
    }

    /// Save the current graph to `path`. Picks the on-disk format
    /// from the file extension — `.atmr` writes a zip archive
    /// containing `graph.json`, `.json` writes plain JSON, anything
    /// else (including no extension at all) defaults to `.atmr`.
    /// Updates `current_file` on success so subsequent `Save` actions
    /// reuse the chosen path without re-prompting.
    pub fn save_graph_to_path(&self, path: &Path) -> Result<(), String> {
        let graph = self.graph.lock().unwrap();
        let assets = self.assets.lock().unwrap();
        save_project_with_assets_to_path(path, &graph, &assets)
            .map_err(|e| format!("write {}: {}", path.display(), e))?;
        drop(graph);
        drop(assets);
        *self.current_file.lock().unwrap() = Some(path.to_path_buf());
        Ok(())
    }

    /// Import a mesh file (`.stl`, `.obj`, or `.3mf`) and spawn a
    /// `MeshNode` at the supplied canvas-space position.
    ///
    /// 1. Reads the file bytes off disk.
    /// 2. Decodes into a `MeshGL` via the format-detecting
    ///    [`mesh_node::decode_mesh`].
    /// 3. Re-encodes the mesh as `.3mf` so the project always persists
    ///    in one canonical format (matches the project rule "meshes
    ///    are stored as .3mf").
    /// 4. Inserts the bytes into [`AppState::assets`] (deduplicating
    ///    on content hash).
    /// 5. Creates a fresh `MeshNode` instance with the asset reference
    ///    set and the runtime mesh cache pre-populated, so the
    ///    viewport sees geometry immediately without waiting for a
    ///    re-resolve pass.
    /// 6. Triggers `evaluate_now` to push the new mesh into the
    ///    `last_mesh_output` channel the viewport reads.
    ///
    /// Returns the new `NodeId` on success.
    pub fn import_mesh_file(
        &self,
        path: &Path,
        canvas_pos: [f64; 2],
    ) -> Result<NodeId, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("read {}: {}", path.display(), e))?;
        let original_filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "mesh".to_string());
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mesh =
            mesh_node::decode_mesh(&bytes, &extension).map_err(|e| format!("import: {}", e))?;
        // Always persist as .3mf — the project rule.
        let three_mf_bytes = export_3mf(&mesh)
            .map_err(|e| format!("re-encode as 3MF: {}", e))?;

        let asset_ref = {
            let mut assets = self.assets.lock().unwrap();
            assets.insert(three_mf_bytes, original_filename, None, Some("3mf".into()))
        };

        let new_id = {
            let mut graph = self.graph.lock().unwrap();
            // add_new_node calls `NodeDef::instantiate`, which mints the
            // input/output sockets and seeds default properties. We then
            // overwrite the `asset` and `mesh` properties so the runtime
            // cache is populated before the first eval.
            let id = graph
                .add_new_node(mesh_node::TYPE_ID, canvas_pos, &self.registry)
                .map_err(|e| format!("add MeshNode: {}", e))?;
            graph
                .set_property(
                    id,
                    Arc::<str>::from("asset"),
                    PortValue::StringVal(Arc::new(asset_ref.as_str().to_string())),
                )
                .ok();
            graph
                .set_property(
                    id,
                    Arc::<str>::from("mesh"),
                    PortValue::Geometry3d(Arc::new(
                        atomartist_lib::geometry::Geometry3d::from_mesh(Arc::new(mesh)),
                    )),
                )
                .ok();
            id
        };
        self.evaluate_now();
        Ok(new_id)
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
            // Forward the path of the currently-open project so the
            // shell's AutoSave loop persists it on every paint where
            // it changed. The native shell uses this on next launch
            // to auto-reopen the same file.
            last_project_path: self.current_file.lock().unwrap().clone(),
            theme: *self.theme.lock().unwrap(),
            accent_color: *self.accent_color.lock().unwrap(),
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

    /// Save the current displayed mesh as a binary STL. Reads the
    /// triangle data out of [`Geometry3d::mesh`] — STL export
    /// disregards the per-node matrix + colour bundle the renderer
    /// uses.
    pub fn export_stl_to_path(&self, path: &Path) -> Result<(), String> {
        let geom = self
            .last_mesh_output
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "no geometry to export — wire up a node with a 3D output".to_string())?;
        let bytes = export_stl(&geom.mesh);
        std::fs::write(path, bytes).map_err(|e| format!("write {}: {}", path.display(), e))?;
        Ok(())
    }
}
