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
use atomartist_lib::graph::executor::evaluate_dirty;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::serialization::{
    export_stl, graph_from_json_str, graph_to_json_string,
};
use atomartist_lib::Graph;
use atomartist_renderer::{CameraPoseAnimation, OrbitCamera, RenderStyle, ViewportTool};
use manifold_rust::types::MeshGL;

/// Top-level state passed by reference into every UI widget that mutates
/// the graph or reads evaluation results.
pub struct AppState {
    pub graph: Arc<Mutex<Graph>>,
    pub registry: Arc<NodeRegistry>,
    pub undo: Arc<Mutex<UndoBuffer>>,
    /// Most recently computed output mesh (for the 3D viewport). Written
    /// by `schedule_evaluate` and read by `Viewport3dWidget::needs_draw`.
    pub last_mesh_output: Arc<Mutex<Option<Arc<MeshGL>>>>,
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
    /// Render style picker beneath the tumble cube (Shaded / Wireframe /
    /// OutlineOnly).
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
    last_mesh_output: Arc<Mutex<Option<Arc<MeshGL>>>>,
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

    fn pick_display_mesh(&self, g: &Graph) -> Option<Arc<MeshGL>> {
        let display_id = *self.display_node.lock().unwrap();
        if let Some(id) = display_id {
            if let Some(n) = g.get(id) {
                if let Some(PortValue::Geometry3d(m)) = n.cached_outputs.get("out") {
                    return Some(m.clone());
                }
            }
        }
        let mut best: Option<(NodeId, Arc<MeshGL>)> = None;
        for n in g.nodes() {
            if let Some(PortValue::Geometry3d(m)) = n.cached_outputs.get("out") {
                if best.as_ref().map(|(id, _)| n.id > *id).unwrap_or(true) {
                    best = Some((n.id, m.clone()));
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
    pub fn load_graph_from_path(&self, path: &Path) -> Result<(), String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
        let result = graph_from_json_str(&s, &self.registry).map_err(|e| e.to_string())?;
        *self.graph.lock().unwrap() = result.graph;
        self.undo.lock().unwrap().clear_history();
        *self.current_file.lock().unwrap() = Some(path.to_path_buf());
        // Pick a default display node — the highest-id node with a
        // Geometry3d output, matching what evaluate_now does.
        *self.display_node.lock().unwrap() = None;
        *self.selection.lock().unwrap() = None;
        self.evaluate_now();
        Ok(())
    }

    /// Save the current graph to `path` (JSON). Updates `current_file`.
    pub fn save_graph_to_path(&self, path: &Path) -> Result<(), String> {
        let json = graph_to_json_string(&self.graph.lock().unwrap());
        std::fs::write(path, json).map_err(|e| format!("write {}: {}", path.display(), e))?;
        *self.current_file.lock().unwrap() = Some(path.to_path_buf());
        Ok(())
    }

    /// Save the current displayed mesh as a binary STL.
    pub fn export_stl_to_path(&self, path: &Path) -> Result<(), String> {
        let mesh = self
            .last_mesh_output
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "no geometry to export — wire up a node with a 3D output".to_string())?;
        let bytes = export_stl(&mesh);
        std::fs::write(path, bytes).map_err(|e| format!("write {}: {}", path.display(), e))?;
        Ok(())
    }
}
