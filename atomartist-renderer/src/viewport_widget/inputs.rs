//! `ViewportInputs` + `ViewportTool` — the bundle of shared state +
//! callbacks the host wires into [`super::Viewport3dWidget`].
//!
//! Split out of `viewport_widget.rs` to keep that file under the
//! 800-line guardrail as the input set grows (mesh, camera, render
//! style, render tool, bed toggle, animations, matrix read/write
//! callbacks for body drag, …).

use std::sync::{Arc, Mutex};

use atomartist_lib::geometry::Geometry3d;
use atomartist_lib::graph::node::NodeId;

use crate::camera::OrbitCamera;
use crate::camera_animations::{CameraPoseAnimation, ProjectionAnimation};
use crate::scene_renderer::RenderStyle;

/// Default left-mouse-drag behaviour, picked by the radio cluster of
/// buttons around the tumble cube.  Mirrors MatterCAD's
/// `ViewControls3DButtons` enum minus the printer-specific entries.
///
/// `Select` is the historical AtomArtist behaviour: plain left-drag
/// becomes a click-or-drag selection.  The other variants change what
/// plain left-drag does — useful on trackpads without a right or
/// middle mouse button, exactly the case MatterCAD targets these
/// buttons at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewportTool {
    Select,
    Rotate,
    Pan,
    Zoom,
}

impl Default for ViewportTool {
    fn default() -> Self {
        Self::Select
    }
}

/// External hooks the widget needs from the app: where to read the
/// latest mesh from, the live `display_node` so a left-click selection
/// mirrors into the canvas, and a writable selection slot. Also the
/// matrix read / write callbacks the body-drag handlers fire through.
pub struct ViewportInputs {
    /// Latest displayed geometry — bundle of mesh + per-node matrix
    /// + per-node colour pulled forward from upstream.
    pub last_mesh_output: Arc<Mutex<Option<Arc<Geometry3d>>>>,
    /// The display node whose mesh is currently rendered. Read-only
    /// from the viewport's perspective — used to know which node id
    /// to write into `selection` when the user left-clicks the
    /// displayed mesh.
    pub display_node: Arc<Mutex<Option<NodeId>>>,
    /// The currently-selected node id (mirrored to / from the
    /// canvas).
    pub selection: Arc<Mutex<Option<NodeId>>>,
    /// Shared orbit camera — held in an `Arc<Mutex<>>` so the tumble
    /// cube widget can read the current orientation each paint and
    /// write back animated orientations on click-to-orient.
    pub camera: Arc<Mutex<OrbitCamera>>,
    /// Active mouse-button-1 tool (Select / Rotate / Pan / Zoom).
    pub tool: Arc<Mutex<ViewportTool>>,
    /// Render style picker beneath the tumble cube.
    pub render_style: Arc<Mutex<RenderStyle>>,
    /// Bed-toggle state.
    pub show_bed: Arc<Mutex<bool>>,
    /// Optional camera pose tween started by external HUD controls.
    pub camera_animation: Arc<Mutex<Option<CameraPoseAnimation>>>,
    /// Optional perspective <-> orthographic tween.
    pub projection_animation: Arc<Mutex<Option<ProjectionAnimation>>>,
    /// Callback the viewport calls during body / gizmo drags to push
    /// a fresh `matrix` value onto a node — coalesced + undoable.
    /// `atomartist-ui` wires this to
    /// `AppState::set_node_matrix_with_undo`; tests that boot the
    /// viewport without an AppState can leave it `None` to disable
    /// the write-back path.
    ///
    /// Not `Send + Sync` — the underlying `UndoBuffer` holds
    /// `Box<dyn UndoRedoCommand>` trait objects that are
    /// intentionally `!Send` (some commands carry `Rc`-shared state
    /// for text editing). The closure runs on the main thread along
    /// with every other viewport event handler, so single-threaded
    /// access is fine.
    pub write_node_matrix: Option<Arc<dyn Fn(NodeId, [f32; 16])>>,
    /// Snapshot reader the viewport uses at drag-start to capture
    /// the picked body's `matrix` property — the unmodified per-node
    /// matrix, NOT the composed-with-upstream Body.matrix the
    /// renderer paints with. Returning `None` aborts the drag (the
    /// node doesn't expose a `matrix` property).
    pub read_node_matrix: Option<Arc<dyn Fn(NodeId) -> Option<[f32; 16]>>>,
    /// Reader for a named numeric property (e.g. `"height"`) on a node.
    /// Returns `None` when the node has no such `Number` property — the
    /// scale controls use this both to *detect* an editable dimension
    /// (the "has a Height field" test) and to read its current value at
    /// drag-start.
    pub read_node_number: Option<Arc<dyn Fn(NodeId, &str) -> Option<f64>>>,
    /// Writer for a named numeric property, coalesced + undoable
    /// (wired to `AppState::set_node_number_with_undo`). Used by the
    /// height / width / depth scale controls' field-editing path.
    pub write_node_number: Option<Arc<dyn Fn(NodeId, &str, f64)>>,
    /// Atomic writer for a numeric property **plus** the matrix in one
    /// graph update + one evaluation (wired to
    /// `AppState::set_node_number_and_matrix_with_undo`). The height
    /// control's field path needs the pair together: the matrix
    /// carries the base-lock compensation for the height change, and a
    /// gap between them paints a one-frame bounce.
    pub write_node_number_and_matrix: Option<Arc<dyn Fn(NodeId, &str, f64, [f32; 16])>>,
    /// Snap-grid distance in world units; `0` = snapping off. Shared
    /// with the toolbar's snap dropdown (`AppState::snap_amount`).
    /// MatterCAD's `SnapGridDistance`: XY drags snap the grabbed AABB
    /// edge, Z drags snap the body's bottom position, height drags
    /// snap the size.
    pub snap_amount: Arc<Mutex<f64>>,
}

impl ViewportInputs {
    /// Build a default-populated input bundle with empty
    /// `Arc<Mutex<>>`s for every slot — used by tests and the
    /// unit-of-work paint code to avoid replicating every default
    /// in every call site.
    pub fn empty() -> Self {
        Self {
            last_mesh_output: Arc::new(Mutex::new(None)),
            display_node: Arc::new(Mutex::new(None)),
            selection: Arc::new(Mutex::new(None)),
            camera: Arc::new(Mutex::new(OrbitCamera::default())),
            tool: Arc::new(Mutex::new(ViewportTool::default())),
            render_style: Arc::new(Mutex::new(RenderStyle::default())),
            show_bed: Arc::new(Mutex::new(true)),
            camera_animation: Arc::new(Mutex::new(None)),
            projection_animation: Arc::new(Mutex::new(None)),
            write_node_matrix: None,
            read_node_matrix: None,
            read_node_number: None,
            write_node_number: None,
            write_node_number_and_matrix: None,
            snap_amount: Arc::new(Mutex::new(0.0)),
        }
    }

    /// Invoke the registered matrix-writer callback. No-op when none
    /// is wired (e.g. unit tests booting the viewport without an
    /// `AppState`).
    pub(crate) fn push_node_matrix(&self, id: NodeId, matrix: [f32; 16]) {
        if let Some(f) = &self.write_node_matrix {
            f(id, matrix);
        }
    }

    /// Read a node's `matrix` property via the registered reader.
    /// Returns `None` when no reader is wired or the node has no
    /// matrix property.
    pub(crate) fn read_node_matrix(&self, id: NodeId) -> Option<[f32; 16]> {
        self.read_node_matrix.as_ref()?(id)
    }

    /// Read a node's numeric `name` property. `None` when no reader is
    /// wired or the node has no such property — the scale controls
    /// treat `None` as "no editable field, fall back to matrix scale".
    pub(crate) fn read_node_number(&self, id: NodeId, name: &str) -> Option<f64> {
        self.read_node_number.as_ref()?(id, name)
    }

    /// Push a numeric `name` property value through the registered
    /// writer. No-op when none is wired.
    pub(crate) fn push_node_number(&self, id: NodeId, name: &str, value: f64) {
        if let Some(f) = &self.write_node_number {
            f(id, name, value);
        }
    }

    /// Current snap-grid distance (`0` = off).
    pub(crate) fn snap(&self) -> f32 {
        *self.snap_amount.lock().unwrap() as f32
    }

    /// Push a numeric property and the matrix as ONE atomic graph
    /// update (single evaluation). Falls back to two separate writes
    /// when the combined writer isn't wired, so headless tests that
    /// only register the simple writers still observe both values.
    pub(crate) fn push_node_number_and_matrix(
        &self,
        id: NodeId,
        name: &str,
        value: f64,
        matrix: [f32; 16],
    ) {
        if let Some(f) = &self.write_node_number_and_matrix {
            f(id, name, value, matrix);
        } else {
            self.push_node_matrix(id, matrix);
            self.push_node_number(id, name, value);
        }
    }
}
