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
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
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

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("asset", PortValue::StringVal(Arc::new(String::new()))),
            PropDef::new("label", PortValue::StringVal(Arc::new(String::new()))),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let mut out = NodeOutputs::default();
        if let PortValue::Geometry3d(mesh) = ctx.properties.get(MESH_CACHE_KEY) {
            out.set("out", PortValue::Geometry3d(Arc::clone(mesh)));
        }
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(MeshNode);
}

/// Walk `graph` and populate every `MeshNode`'s runtime mesh cache from
/// `assets`. Call this after loading a project.
pub fn resolve_mesh_assets(graph: &mut Graph, assets: &AssetStore) -> Vec<String> {
    let mut warnings = Vec::new();
    let ids: Vec<_> = graph
        .nodes()
        .filter(|n| n.type_id.as_ref() == TYPE_ID)
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
                    n.properties.insert(
                        Arc::from(MESH_CACHE_KEY),
                        PortValue::Geometry3d(Arc::new(
                            crate::geometry::Geometry3d::from_mesh(Arc::new(mesh)),
                        )),
                    );
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

/// Import a mesh from any supported file extension.
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
    use crate::registry::{NodeInputs, NodeProperties};
    use crate::serialization::mesh_3mf::export_3mf;

    fn make_node(id: u64, asset_ref: &str) -> NodeInstance {
        let mut n = NodeInstance::new(
            crate::graph::node::NodeId(id),
            TYPE_ID,
            [0.0, 0.0],
        );
        n.properties.insert(
            Arc::from("asset"),
            PortValue::StringVal(Arc::new(asset_ref.to_string())),
        );
        n
    }

    fn make_ctx(props: NodeProperties) -> (NodeInstance, NodeInputs, NodeProperties) {
        let mut alloc = SocketUidAlloc::new();
        let tpl = MeshNode.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(crate::graph::node::NodeId(1), TYPE_ID, [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let inputs = NodeInputs::default();
        (inst, inputs, props)
    }

    #[test]
    fn evaluate_emits_empty_when_no_cache() {
        let (inst, inputs, props) = make_ctx(NodeProperties::default());
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let out = MeshNode.evaluate(&ctx).unwrap();
        assert!(out.by_name.get("out").is_none());
    }

    #[test]
    fn evaluate_returns_cached_mesh() {
        let mesh = generate_box(1.0, 2.0, 3.0);
        let mut props = NodeProperties::default();
        props.insert(
            MESH_CACHE_KEY,
            PortValue::Geometry3d(Arc::new(
                crate::geometry::Geometry3d::from_mesh(Arc::new(mesh)),
            )),
        );
        let (inst, inputs, props) = make_ctx(props);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let out = MeshNode.evaluate(&ctx).unwrap();
        match out.by_name.get("out").unwrap() {
            PortValue::Geometry3d(g) => assert_eq!(g.mesh.tri_verts.len() / 3, 12),
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
            Some(PortValue::Geometry3d(g)) => assert_eq!(g.mesh.tri_verts.len() / 3, 12),
            other => panic!("expected resolved geometry, got {:?}", other),
        }
        assert!(node.dirty, "resolution should flag the node dirty");
    }

    #[test]
    fn resolve_emits_warning_for_missing_asset() {
        let mut graph = Graph::new();
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
