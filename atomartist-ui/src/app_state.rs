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
use atomartist_lib::graph::node::NodeId;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;
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
        }
    }

    /// Set the dirty flag so the viewport repaints next frame.
    pub fn mark_viewport_dirty(&self) {
        self.viewport_dirty.store(true, Ordering::Relaxed);
    }

    /// Take + reset the dirty flag — used by the viewport widget.
    pub fn take_viewport_dirty(&self) -> bool {
        self.viewport_dirty.swap(false, Ordering::Relaxed)
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
        }
    }
}
