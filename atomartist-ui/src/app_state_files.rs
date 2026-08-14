//! File operations on [`AppState`] — new / load / save project,
//! mesh + scene import (`.stl` / `.obj` / `.3mf` / `.mcx` / `.atmr`),
//! and the File → Export formats. Split from `app_state.rs` to keep
//! both files under the 800-line cap; invoked from the menu action
//! handlers in `menu_actions`.
//!
//! Every operation addresses a project by [`StorageUri`] and moves its
//! bytes through the [`AppState::storage`] registry — this module never
//! touches `std::fs` (enforced by
//! `atomartist-lib/tests/no_fs_outside_provider.rs`). The provider
//! plumbing lives in [`crate::app_state_storage`].

use std::sync::Arc;

use atomartist_storage::StorageUri;

use atomartist_lib::graph::merge::merge_graph;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::undo_commands::AddNodeCmd;
use atomartist_lib::nodes::mesh::mesh_node;
use atomartist_lib::serialization::{
    export_3mf, export_obj, export_stl, import_mcx, read_project_from_bytes,
    write_project_to_bytes,
};
use atomartist_lib::Graph;

use crate::app_state::AppState;
use crate::app_state_storage::{display_uri, read_bytes, uri_extension, write_bytes};

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
        self.mark_saved_baseline();
        self.mark_viewport_dirty();
    }

    /// Load a graph from `uri`. Replaces the current graph wholesale,
    /// clears undo history, and runs an initial evaluation so the
    /// viewport repopulates. Returns `Err` with a user-readable message
    /// on parse / IO failure.
    ///
    /// The bytes come from the storage provider that owns the URI's
    /// scheme and are decoded by the format layer; `.atmr` (a zip
    /// archive) is the only project format, and the decoder ignores the
    /// name entirely — see `serialization::atmr`.
    pub fn load_graph_from_uri(&self, uri: &StorageUri) -> Result<(), String> {
        let bytes =
            read_bytes(&self.storage, uri).map_err(|e| format!("open {}: {}", display_uri(uri), e))?;
        let (result, assets) = read_project_from_bytes(&bytes, &self.registry)
            .map_err(|e| format!("open {}: {}", display_uri(uri), e))?;
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
    /// archive (graph + assets) regardless of the file name. Updates
    /// `current_file` on success so subsequent `Save` actions reuse the
    /// chosen location without re-prompting.
    pub fn save_graph_to_uri(&self, uri: &StorageUri) -> Result<(), String> {
        let bytes = {
            let graph = self.graph.lock().unwrap();
            let assets = self.assets.lock().unwrap();
            write_project_to_bytes(&graph, &assets)
                .map_err(|e| format!("write {}: {}", display_uri(uri), e))?
        };
        write_bytes(&self.storage, uri, bytes)
            .map_err(|e| format!("write {}: {}", display_uri(uri), e))?;
        *self.current_file.lock().unwrap() = Some(uri.clone());
        self.mark_saved_baseline();
        self.note_recent_project(uri);
        Ok(())
    }

    /// Import a mesh file (`.stl`, `.obj`, or `.3mf`) and spawn a
    /// `MeshNode` at the supplied canvas-space position.
    ///
    /// 1. Reads the bytes from the URI's storage provider.
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
        uri: &StorageUri,
        canvas_pos: [f64; 2],
    ) -> Result<NodeId, String> {
        let bytes =
            read_bytes(&self.storage, uri).map_err(|e| format!("read {}: {}", display_uri(uri), e))?;
        let original_filename = uri
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "mesh".to_string());
        let extension = uri_extension(uri);
        let mesh =
            mesh_node::decode_mesh(&bytes, &extension).map_err(|e| format!("import: {}", e))?;
        let new_id = self.spawn_mesh_node(mesh, original_filename, None, None, canvas_pos)?;
        self.evaluate_now();
        Ok(new_id)
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
        let three_mf_bytes = export_3mf(&mesh)
            .map_err(|e| format!("re-encode as 3MF: {}", e))?;
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
            let (node, _detached) = graph
                .remove_node(id)
                .map_err(|e| format!("snapshot for undo: {:?}", e))?;
            drop(graph);
            let cmd = AddNodeCmd::new(self.graph.clone(), node).with_label("Import Mesh");
            self.undo.lock().unwrap().add_and_do(Box::new(cmd));
            id
        };
        // Wire the import into the Output node so it shows up in the
        // viewport right away — the viewport renders only what's
        // connected to Output, and an invisible import reads as "the
        // drop did nothing".
        self.connect_to_output(new_id);
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
    /// Returns the number of nodes added.
    pub fn import_scene_file(&self, uri: &StorageUri) -> Result<usize, String> {
        let ext = uri_extension(uri);
        match ext.as_str() {
            "stl" | "obj" | "3mf" => {
                self.import_mesh_file(uri, self.next_import_position(0))?;
                Ok(1)
            }
            "mcx" => self.import_mcx_file(uri),
            "atmr" => self.import_project_file(uri),
            other => Err(format!(
                "unsupported import format: .{other} (expected .stl, .obj, .3mf, .mcx, or .atmr)"
            )),
        }
    }

    /// Import a MatterControl `.mcx` scene: one MeshNode per visible
    /// surface, transforms baked into each node's `matrix` property.
    fn import_mcx_file(&self, uri: &StorageUri) -> Result<usize, String> {
        let bytes =
            read_bytes(&self.storage, uri).map_err(|e| format!("read {}: {}", display_uri(uri), e))?;
        let mut warnings = Vec::new();
        let parts = import_mcx(&bytes, &mut warnings).map_err(|e| e.to_string())?;
        for w in &warnings {
            eprintln!("mcx import: {}", w);
        }
        let mut added = 0;
        for (i, part) in parts.into_iter().enumerate() {
            let pos = self.next_import_position(added);
            match self.spawn_mesh_node(
                part.mesh,
                part.name,
                Some(part.matrix),
                part.color,
                pos,
            ) {
                Ok(_) => added += 1,
                Err(e) => eprintln!("mcx import: part {} skipped: {}", i, e),
            }
        }
        if added == 0 {
            return Err("no meshes could be imported from the .mcx".to_string());
        }
        self.evaluate_now();
        Ok(added)
    }

    /// Merge another AtomArtist project into the current scene. The
    /// imported graph lands to the right of the existing nodes; its
    /// Output node is dropped and everything that fed it is rewired
    /// into this scene's Output so the imported geometry renders.
    fn import_project_file(&self, uri: &StorageUri) -> Result<usize, String> {
        let bytes =
            read_bytes(&self.storage, uri).map_err(|e| format!("open {}: {}", display_uri(uri), e))?;
        let (result, src_assets) = read_project_from_bytes(&bytes, &self.registry)
            .map_err(|e| format!("open {}: {}", display_uri(uri), e))?;
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
        Ok(merge.added_nodes.len())
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
        let Some(from_uid) = graph.get(id).and_then(|n| n.outputs.first().map(|s| s.uid))
        else {
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
    pub fn export_mesh_to_uri(
        &self,
        uri: &StorageUri,
        format: MeshExportFormat,
    ) -> Result<(), String> {
        let geom = self
            .last_mesh_output
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "no geometry to export — wire up a node with a 3D output".to_string())?;
        let meshes: Vec<_> = geom.iter().map(|b| b.mesh.clone()).collect();
        let merged = atomartist_lib::geometry::merge_meshes(&meshes);
        let bytes = match format {
            MeshExportFormat::Stl => export_stl(&merged),
            MeshExportFormat::Obj => export_obj(&merged),
            MeshExportFormat::ThreeMf => {
                export_3mf(&merged).map_err(|e| format!("encode 3MF: {}", e))?
            }
        };
        write_bytes(&self.storage, uri, bytes).map_err(|e| format!("write {}: {}", display_uri(uri), e))
    }

    /// File → Export → AtomArtist Project: write a copy of the whole
    /// project (graph + assets) to `uri` WITHOUT retargeting Save —
    /// `current_file`, the recent list, and the unsaved-changes
    /// baseline all stay put, unlike [`Self::save_graph_to_uri`].
    pub fn export_project_copy_to_uri(&self, uri: &StorageUri) -> Result<(), String> {
        let bytes = {
            let graph = self.graph.lock().unwrap();
            let assets = self.assets.lock().unwrap();
            write_project_to_bytes(&graph, &assets)
                .map_err(|e| format!("write {}: {}", display_uri(uri), e))?
        };
        write_bytes(&self.storage, uri, bytes).map_err(|e| format!("write {}: {}", display_uri(uri), e))
    }
}
