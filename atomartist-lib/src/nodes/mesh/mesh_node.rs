//! `Mesh` node — emits a triangle mesh embedded in the project's
//! `.atmr` bundle.
//!
//! Where [`LibraryMeshNode`](super::library_mesh_node) reads a mesh
//! from a disk path (and therefore breaks if the project moves), this
//! node references the mesh via a content-addressed
//! [`AssetRef`](crate::serialization::AssetRef) into the project's
//! [`AssetStore`](crate::serialization::AssetStore). The bytes travel
//! with the project file — moving / emailing / version-controlling the
//! `.atmr` keeps every mesh intact.
//!
//! ## Property layout
//!
//! - `asset` (string): the asset reference. Empty until a file is
//!   imported (the inspector then shows "no mesh assigned").
//! - `label` (string, optional): UI display name. Falls back to the
//!   asset's original filename when blank.
//!
//! ## Runtime cache
//!
//! `properties["mesh"]` holds the resolved `PortValue::Geometry3d`
//! after [`resolve_mesh_assets`] has run. This key is *not* a declared
//! [`PropDef`] — it's an internal slot the loader fills in. The graph
//! JSON serializer drops heap geometry from properties, so the cache
//! is invisible to the on-disk format.

use std::sync::Arc;

use crate::graph::graph::Graph;
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::serialization::asset_store::{AssetRef, AssetStore};
use crate::serialization::mesh_3mf::import_3mf;
use crate::serialization::mesh_io::import_stl;
use crate::serialization::mesh_obj::import_obj;
use crate::socket_types::SocketType;

/// Type id under which the registry stores `MeshNode`. Also used by
/// [`resolve_mesh_assets`] to scan a graph.
pub const TYPE_ID: &str = "Mesh";

/// Internal property slot where the resolved `MeshGL` lives at
/// runtime. Not a declared `PropDef` — the loader sets it and the
/// inspector ignores it.
const MESH_CACHE_KEY: &str = "mesh";

pub struct MeshNode;

impl NodeDef for MeshNode {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }
    fn display_name(&self) -> &'static str {
        "Mesh"
    }
    fn category(&self) -> &'static str {
        "Mesh"
    }

    fn input_sockets(&self) -> Vec<SocketDef> {
        vec![]
    }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("asset", PortValue::StringVal(Arc::new(String::new()))),
            PropDef::new("label", PortValue::StringVal(Arc::new(String::new()))),
        ]
    }

    fn evaluate(
        &self,
        _inputs: &NodeInputs,
        props: &NodeProperties,
    ) -> Result<NodeOutputs, NodeError> {
        let mut out = NodeOutputs::default();
        if let PortValue::Geometry3d(mesh) = props.get(MESH_CACHE_KEY) {
            out.set("out", PortValue::Geometry3d(Arc::clone(mesh)));
        }
        // Empty output is the right behaviour when the asset hasn't
        // been resolved yet (just-loaded project before
        // `resolve_mesh_assets` ran, or a dangling reference).
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(MeshNode);
}

/// Walk `graph` and populate every `MeshNode`'s runtime mesh cache from
/// `assets`. Call this after loading a project. Nodes whose `asset`
/// reference is missing from the store are left untouched — their
/// `evaluate` will return an empty output until the user re-imports.
///
/// Returns a list of warnings for asset references that didn't
/// resolve, so callers can surface them in the UI without aborting
/// the whole load.
pub fn resolve_mesh_assets(graph: &mut Graph, assets: &AssetStore) -> Vec<String> {
    let mut warnings = Vec::new();
    // Collect node ids first to avoid borrow conflict with the mutating loop.
    let ids: Vec<_> = graph
        .nodes()
        .filter(|n| n.type_id == TYPE_ID)
        .map(|n| n.id)
        .collect();
    for id in ids {
        let asset_ref_str = match graph.get(id).and_then(|n| n.properties.get("asset")) {
            Some(PortValue::StringVal(s)) if !s.is_empty() => s.as_str().to_string(),
            _ => continue,
        };
        let asset_ref = match AssetRef::parse(&asset_ref_str) {
            Some(r) => r,
            None => {
                warnings.push(format!(
                    "mesh node {} has malformed asset ref `{}`",
                    id.0, asset_ref_str
                ));
                continue;
            }
        };
        let entry = match assets.get(&asset_ref) {
            Some(e) => e,
            None => {
                warnings.push(format!(
                    "mesh node {} references missing asset `{}`",
                    id.0, asset_ref_str
                ));
                continue;
            }
        };
        match decode_mesh(&entry.bytes, &entry.extension) {
            Ok(mesh) => {
                if let Some(n) = graph.get_mut(id) {
                    n.properties
                        .insert(MESH_CACHE_KEY, PortValue::Geometry3d(Arc::new(mesh)));
                    n.dirty = true;
                }
            }
            Err(e) => warnings.push(format!(
                "mesh node {} failed to decode `{}`: {}",
                id.0, entry.original_filename, e
            )),
        }
    }
    warnings
}

/// Import a mesh from any supported file extension. Used both by the
/// drag-drop handler (when the user drops a `.stl`/`.obj`/`.3mf` onto
/// the app) and by [`resolve_mesh_assets`] when materialising a node
/// from its in-bundle bytes.
///
/// AtomArtist always persists meshes as `.3mf`, so on the load side
/// the extension is normally `3mf`; the other branches are kept for
/// import and for legacy projects that may have stored other formats.
pub fn decode_mesh(bytes: &[u8], extension: &str) -> Result<manifold_rust::types::MeshGL, String> {
    match extension.to_ascii_lowercase().as_str() {
        "stl" => import_stl(bytes).map_err(|e| format!("STL parse: {}", e)),
        "obj" => import_obj(bytes).map_err(|e| format!("OBJ parse: {}", e)),
        "3mf" => import_3mf(bytes).map_err(|e| format!("3MF parse: {}", e)),
        other => Err(format!("unsupported mesh extension: .{}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;
    use crate::graph::node::NodeInstance;
    use crate::serialization::mesh_3mf::export_3mf;

    fn make_node(id: u64, asset_ref: &str) -> NodeInstance {
        let mut n = NodeInstance::new(
            crate::graph::node::NodeId(id),
            TYPE_ID,
            [0.0, 0.0],
        );
        n.properties.insert(
            "asset",
            PortValue::StringVal(Arc::new(asset_ref.to_string())),
        );
        n
    }

    #[test]
    fn evaluate_emits_empty_when_no_cache() {
        let n = MeshNode;
        let props = NodeProperties::default();
        let out = n.evaluate(&NodeInputs::default(), &props).unwrap();
        assert!(out.by_name.get("out").is_none());
    }

    #[test]
    fn evaluate_returns_cached_mesh() {
        let mesh = generate_box(1.0, 2.0, 3.0);
        let n = MeshNode;
        let mut props = NodeProperties::default();
        props.insert(MESH_CACHE_KEY, PortValue::Geometry3d(Arc::new(mesh)));
        let out = n.evaluate(&NodeInputs::default(), &props).unwrap();
        match out.by_name.get("out").unwrap() {
            PortValue::Geometry3d(m) => assert_eq!(m.tri_verts.len() / 3, 12),
            _ => panic!("expected Geometry3d output"),
        }
    }

    #[test]
    fn resolve_populates_cache_from_asset_store() {
        let bytes = export_3mf(&generate_box(2.0, 2.0, 2.0)).unwrap();
        let mut assets = AssetStore::new();
        let r = assets.insert(bytes, "cube.3mf".into(), None, None);

        let mut graph = Graph::new();
        graph.add_node(make_node(1, r.as_str())).unwrap();

        let warnings = resolve_mesh_assets(&mut graph, &assets);
        assert!(warnings.is_empty(), "no warnings expected: {:?}", warnings);

        let node = graph.get(crate::graph::node::NodeId(1)).unwrap();
        match node.properties.get(MESH_CACHE_KEY) {
            Some(PortValue::Geometry3d(m)) => assert_eq!(m.tri_verts.len() / 3, 12),
            other => panic!("expected resolved geometry, got {:?}", other),
        }
        assert!(node.dirty, "resolution should flag the node dirty");
    }

    #[test]
    fn resolve_emits_warning_for_missing_asset() {
        let mut graph = Graph::new();
        // Reference a valid-looking but non-existent asset.
        let dangling = AssetRef::from_bytes(b"phantom");
        graph
            .add_node(make_node(1, dangling.as_str()))
            .unwrap();

        let warnings = resolve_mesh_assets(&mut graph, &AssetStore::new());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing asset"));
    }

    #[test]
    fn decode_mesh_routes_by_extension() {
        let mesh = generate_box(1.0, 1.0, 1.0);
        let bytes = export_3mf(&mesh).unwrap();
        let imported = decode_mesh(&bytes, "3mf").unwrap();
        assert_eq!(imported.tri_verts.len(), mesh.tri_verts.len());
        assert!(decode_mesh(&[], "abc").is_err());
    }
}
