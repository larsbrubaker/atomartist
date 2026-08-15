//! Import operations on [`AppState`] — bringing outside geometry into
//! the *current* scene, as opposed to the open / save / export project
//! operations in [`crate::app_state_files`] (which this module was split
//! from to stay under the 800-line cap).
//!
//! Three entry points, all reachable from File → Import and from a file
//! drop on the canvas ([`crate::top_level`]):
//!
//! - `.stl` / `.obj` / `.3mf` — one MeshNode ([`AppState::import_mesh_file`]).
//! - `.mcx` — a MatterControl scene, one MeshNode per visible surface.
//! - `.atmr` — another AtomArtist project, merged in beside the current
//!   nodes.
//!
//! Same submit-and-continue rule as the project operations: the provider
//! read is a job on the frame pump and everything downstream of the bytes
//! (decode, spawn, merge, evaluate) runs in its continuation, so failures
//! arrive as [`NoticeLevel::Error`] notices rather than return values.
//! Callers must not hold an [`AppState`] lock across these calls — a
//! local provider settles inline and runs the continuation on their stack.

use std::sync::Arc;

use atomartist_storage::StorageUri;

use atomartist_lib::graph::merge::merge_graph;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::undo_commands::{AddNodeCmd, BatchCmd, ConnectToFreeInputCmd};
use atomartist_lib::nodes::mesh::mesh_node;
use atomartist_lib::serialization::{export_3mf, import_mcx, read_project_from_bytes};

use crate::app_state::AppState;
use crate::app_state_storage::{display_uri, read_job, uri_extension, uri_label};
use crate::storage_ops::{JobOp, NoticeLevel};

/// Mesh formats a drop can bring in as a single `MeshNode`.
pub const MESH_IMPORT_EXTENSIONS: &[&str] = &["stl", "obj", "3mf"];
/// Scene formats a drop merges into the current graph. These place
/// themselves, so a drop position does not apply to them.
pub const SCENE_IMPORT_EXTENSIONS: &[&str] = &["mcx", "atmr"];

/// Whether [`AppState::import_dropped_file`] can do anything with this
/// extension (lower-case, no dot).
///
/// The single source of truth for "is this draggable / droppable": the
/// browser's is-this-entry-draggable check and the import dispatch read
/// the same lists, so a new format can never be draggable-but-not-
/// importable (or the reverse).
pub fn is_importable_extension(ext: &str) -> bool {
    MESH_IMPORT_EXTENSIONS.contains(&ext) || SCENE_IMPORT_EXTENSIONS.contains(&ext)
}

/// [`is_importable_extension`] applied to a URI's extension.
pub fn is_importable_uri(uri: &StorageUri) -> bool {
    is_importable_extension(&uri_extension(uri))
}

impl AppState {
    /// Import a mesh file (`.stl`, `.obj`, or `.3mf`) and spawn a
    /// `MeshNode` at the supplied canvas-space position.
    ///
    /// 1. Submits a read of the URI's storage provider.
    /// 2. The continuation decodes the bytes into a `MeshGL` via the
    ///    format-detecting [`mesh_node::decode_mesh`].
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
    pub fn import_mesh_file(&self, uri: &StorageUri, canvas_pos: [f64; 2]) {
        let uri = uri.clone();
        let job = read_job(&self.storage, &uri);
        self.submit_op(Box::new(JobOp::new(
            format!("Importing {}", uri_label(&uri)),
            job,
            move |state, result| match result {
                Ok(bytes) => state.spawn_imported_mesh(&uri, &bytes, canvas_pos),
                Err(err) => state.notify(
                    NoticeLevel::Error,
                    format!("read {}: {}", display_uri(&uri), err),
                ),
            },
        )));
    }

    /// Decode-and-spawn half of [`Self::import_mesh_file`], run from the
    /// read job's continuation.
    fn spawn_imported_mesh(&self, uri: &StorageUri, bytes: &[u8], canvas_pos: [f64; 2]) {
        let original_filename = uri
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "mesh".to_string());
        let extension = uri_extension(uri);
        let mesh = match mesh_node::decode_mesh(bytes, &extension) {
            Ok(mesh) => mesh,
            Err(err) => {
                self.notify(NoticeLevel::Error, format!("import: {}", err));
                return;
            }
        };
        match self.spawn_mesh_node(mesh, original_filename, None, None, canvas_pos) {
            Ok(_id) => self.evaluate_now(),
            Err(err) => self.notify(NoticeLevel::Error, format!("import: {}", err)),
        }
    }

    /// Shared MeshNode-spawning tail of every mesh import path (file
    /// drop, File → Import, `.mcx` parts). Re-encodes the mesh as
    /// `.3mf` (the canonical embedded format), inserts it into the
    /// asset store, creates the node with optional `matrix` / `color`
    /// property overrides, records an undoable AddNode command, and
    /// wires the node into the Output node so it renders immediately.
    /// Does NOT evaluate — callers batch several spawns and evaluate
    /// once.
    fn spawn_mesh_node(
        &self,
        mesh: manifold_rust::types::MeshGL,
        label: String,
        matrix: Option<[f32; 16]>,
        color: Option<[f32; 4]>,
        canvas_pos: [f64; 2],
    ) -> Result<NodeId, String> {
        // Always persist as .3mf — the project rule.
        let three_mf_bytes = export_3mf(&mesh).map_err(|e| format!("re-encode as 3MF: {}", e))?;
        let asset_ref = {
            let mut assets = self.assets.lock().unwrap();
            assets.insert(three_mf_bytes, label.clone(), None, Some("3mf".into()))
        };

        let new_id = {
            let mut graph = self.graph.lock().unwrap();
            // Create + populate the node in one pass; then pull the
            // fully-configured instance out so AddNodeCmd owns it (so
            // Ctrl+Z removes the import + Ctrl+Y restores it with the
            // same asset_ref + mesh cache).
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
                    Arc::<str>::from("label"),
                    PortValue::StringVal(Arc::new(label)),
                )
                .ok();
            if let Some(m) = matrix {
                graph
                    .set_property(id, Arc::<str>::from("matrix"), PortValue::Matrix4x4(m))
                    .ok();
            }
            if let Some(c) = color {
                graph
                    .set_property(id, Arc::<str>::from("color"), PortValue::Color(c))
                    .ok();
            }
            graph
                .set_property(
                    id,
                    Arc::<str>::from("mesh"),
                    PortValue::Geometry3d(Arc::new(
                        atomartist_lib::geometry::Geometry3d::from_mesh(Arc::new(mesh)),
                    )),
                )
                .ok();
            // Wire the import into the Output node so it shows up in the
            // viewport right away — the viewport renders only what's
            // connected to Output, and an invisible import reads as "the
            // drop did nothing".
            //
            // Same policy as every other insertion path since step
            // 6f-4: first `Geometry3d` output → the Output's first
            // *free* input (`crate::node_insertion`), rather than the
            // old "first output socket → last input". Planned here,
            // applied by the command below, so the wire lands inside
            // the import's single undo step instead of dangling outside
            // it.
            let plan = crate::node_insertion::plan_auto_connect(&graph, id);
            let (node, _detached) = graph
                .remove_node(id)
                .map_err(|e| format!("snapshot for undo: {:?}", e))?;
            drop(graph);
            let add = AddNodeCmd::new(self.graph.clone(), node).with_label("Import Mesh");
            let cmd: Box<dyn agg_gui::undo::UndoRedoCommand> = match plan {
                Some(plan) => Box::new(BatchCmd::new(
                    "Import Mesh",
                    vec![
                        Box::new(add),
                        Box::new(ConnectToFreeInputCmd::new(
                            self.graph.clone(),
                            self.registry.clone(),
                            plan.from,
                            plan.from_socket,
                            plan.output,
                        )),
                    ],
                )),
                None => Box::new(add),
            };
            self.undo.lock().unwrap().add_and_do(cmd);
            id
        };
        Ok(new_id)
    }

    /// File → Import entry point: bring `uri` into the *current*
    /// scene (unlike Open, which replaces it). Dispatches on
    /// extension:
    ///
    /// - `.stl` / `.obj` / `.3mf` — one MeshNode via
    ///   [`Self::import_mesh_file`].
    /// - `.mcx` — MatterControl scene: every visible surface becomes a
    ///   MeshNode with its world transform + color preserved.
    /// - `.atmr` — AtomArtist project: the graph is merged in beside
    ///   the existing nodes and its Output feeders rewired into this
    ///   scene's Output.
    ///
    /// An unrecognised extension is rejected at submit time (no read is
    /// issued) with an error notice naming the formats we accept.
    pub fn import_scene_file(&self, uri: &StorageUri) {
        let ext = uri_extension(uri);
        match ext.as_str() {
            "stl" | "obj" | "3mf" => self.import_mesh_file(uri, self.next_import_position(0)),
            "mcx" => self.import_mcx_file(uri),
            "atmr" => self.import_project_file(uri),
            other => self.notify(
                NoticeLevel::Error,
                format!(
                    "unsupported import format: .{other} \
                     (expected .stl, .obj, .3mf, .mcx, or .atmr)"
                ),
            ),
        }
    }

    /// The **drop** entry point: bring `uri` into the current scene at
    /// `canvas_pos` (canvas-space, Y-up), dispatching on extension the
    /// way a drop has to — silently ignoring anything we have no
    /// importer for, because a drop can carry any file at all.
    ///
    /// Shared by the two surfaces that drop files into the graph so
    /// they cannot drift apart: the OS file-drop handler wired onto the
    /// node canvas in [`crate::top_level`], and the favorites-bar /
    /// browser drag-insert gesture in [`crate::drag_insert`].
    ///
    /// Returns whether the extension was one we import.
    pub fn import_dropped_file(&self, uri: &StorageUri, canvas_pos: [f64; 2]) -> bool {
        let ext = uri_extension(uri);
        if MESH_IMPORT_EXTENSIONS.contains(&ext.as_str()) {
            self.import_mesh_file(uri, canvas_pos);
            return true;
        }
        if SCENE_IMPORT_EXTENSIONS.contains(&ext.as_str()) {
            self.import_scene_file(uri);
            return true;
        }
        false
    }

    /// Import a MatterControl `.mcx` scene: one MeshNode per visible
    /// surface, transforms baked into each node's `matrix` property.
    fn import_mcx_file(&self, uri: &StorageUri) {
        let uri = uri.clone();
        let job = read_job(&self.storage, &uri);
        self.submit_op(Box::new(JobOp::new(
            format!("Importing {}", uri_label(&uri)),
            job,
            move |state, result| match result {
                Ok(bytes) => state.merge_mcx_bytes(&bytes),
                Err(err) => state.notify(
                    NoticeLevel::Error,
                    format!("read {}: {}", display_uri(&uri), err),
                ),
            },
        )));
    }

    /// Decode-and-spawn half of [`Self::import_mcx_file`].
    fn merge_mcx_bytes(&self, bytes: &[u8]) {
        let mut warnings = Vec::new();
        let parts = match import_mcx(bytes, &mut warnings) {
            Ok(parts) => parts,
            Err(err) => {
                self.notify(NoticeLevel::Error, err.to_string());
                return;
            }
        };
        for w in &warnings {
            eprintln!("mcx import: {}", w);
        }
        let mut added = 0;
        for (i, part) in parts.into_iter().enumerate() {
            let pos = self.next_import_position(added);
            match self.spawn_mesh_node(part.mesh, part.name, Some(part.matrix), part.color, pos) {
                Ok(_) => added += 1,
                Err(e) => eprintln!("mcx import: part {} skipped: {}", i, e),
            }
        }
        if added == 0 {
            self.notify(
                NoticeLevel::Error,
                "no meshes could be imported from the .mcx",
            );
            return;
        }
        self.evaluate_now();
    }

    /// Merge another AtomArtist project into the current scene. The
    /// imported graph lands to the right of the existing nodes; its
    /// Output node is dropped and everything that fed it is rewired
    /// into this scene's Output so the imported geometry renders.
    fn import_project_file(&self, uri: &StorageUri) {
        let uri = uri.clone();
        let job = read_job(&self.storage, &uri);
        self.submit_op(Box::new(JobOp::new(
            format!("Importing {}", uri_label(&uri)),
            job,
            move |state, result| match result {
                Ok(bytes) => state.merge_project_bytes(&uri, &bytes),
                Err(err) => state.notify(
                    NoticeLevel::Error,
                    format!("open {}: {}", display_uri(&uri), err),
                ),
            },
        )));
    }

    /// Decode-and-merge half of [`Self::import_project_file`].
    fn merge_project_bytes(&self, uri: &StorageUri, bytes: &[u8]) {
        let (result, src_assets) = match read_project_from_bytes(bytes, &self.registry) {
            Ok(decoded) => decoded,
            Err(err) => {
                self.notify(
                    NoticeLevel::Error,
                    format!("open {}: {}", display_uri(uri), err),
                );
                return;
            }
        };
        for w in &result.warnings {
            eprintln!("project import: {}", w);
        }
        let mut src_graph = result.graph;
        let warnings = mesh_node::resolve_mesh_assets(&mut src_graph, &src_assets);
        for w in &warnings {
            eprintln!("project import: {}", w);
        }
        // Merge the asset payloads first. Asset refs are content
        // hashes, so re-inserting the bytes preserves every reference
        // the imported nodes carry.
        {
            let mut assets = self.assets.lock().unwrap();
            for r in src_assets.refs_sorted() {
                if let Some(e) = src_assets.get(&r) {
                    assets.insert(
                        e.bytes.clone(),
                        e.original_filename.clone(),
                        e.label.clone(),
                        Some(e.extension.clone()),
                    );
                }
            }
        }
        let merge = {
            let mut graph = self.graph.lock().unwrap();
            // Land the import to the right of the current nodes.
            let dst_max_x = graph
                .nodes()
                .map(|n| n.position[0])
                .fold(f64::NEG_INFINITY, f64::max);
            let src_min_x = src_graph
                .nodes()
                .map(|n| n.position[0])
                .fold(f64::INFINITY, f64::min);
            let offset_x = if dst_max_x.is_finite() && src_min_x.is_finite() {
                dst_max_x + 260.0 - src_min_x
            } else {
                0.0
            };
            merge_graph(&mut graph, src_graph, &self.registry, [offset_x, 0.0])
        };
        for w in &merge.warnings {
            eprintln!("project import: {}", w);
        }
        // Rewire what fed the imported Output into ours, one at a
        // time — the Output node regrows its trailing placeholder
        // input after every adopt, so we re-resolve it per feeder.
        for (node, _socket) in &merge.output_feeders {
            self.connect_to_output(*node);
        }
        // Project-level import is not undoable as a single step; drop
        // stale history rather than leave Ctrl+Z half-applying across
        // the merge boundary.
        self.undo.lock().unwrap().clear_history();
        self.evaluate_now();
    }

    /// Pick a canvas position for the `i`-th node of an import batch —
    /// a simple downward stack under a fixed left margin, matching
    /// where the Add Node menu drops new nodes.
    fn next_import_position(&self, i: usize) -> [f64; 2] {
        [80.0, 220.0 - (i as f64) * 90.0]
    }

    /// Connect `id`'s first output socket into the scene's Output
    /// node (its trailing placeholder input adopts the connection).
    /// No-op when the graph has no Output node or the connection is
    /// rejected — callers treat visibility wiring as best-effort.
    pub fn connect_to_output(&self, id: NodeId) -> bool {
        use atomartist_lib::graph::graph::Noodle;
        let mut graph = self.graph.lock().unwrap();
        let Some(output) = graph
            .nodes()
            .find(|n| n.type_id.as_ref() == "Output")
            .map(|n| n.id)
        else {
            return false;
        };
        let Some(from_uid) = graph.get(id).and_then(|n| n.outputs.first().map(|s| s.uid)) else {
            return false;
        };
        let Some(to_uid) = graph
            .get(output)
            .and_then(|n| n.inputs.last().map(|s| s.uid))
        else {
            return false;
        };
        graph
            .connect(Noodle::new(id, from_uid, output, to_uid), &self.registry)
            .is_ok()
    }
}
