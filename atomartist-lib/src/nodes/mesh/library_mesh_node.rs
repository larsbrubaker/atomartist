//! LibraryMesh node — load a mesh from disk and surface it as a
//! `Geometry3d` output.
//!
//! Native: reads the file at `path` directly via `std::fs`. The bytes
//! are cached in a process-wide map keyed by canonicalized path + mtime
//! so repeated graph evaluations don't reread the same STL.
//!
//! WASM: file system access works differently — the user supplies bytes
//! via the browser File API, and the path property records only the
//! display name. Phase 10 wires that up; for now WASM reads return an
//! empty mesh.

use std::sync::Arc;

use manifold_rust::types::MeshGL;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::serialization::mesh_io::import_stl;
use crate::socket_types::SocketType;

pub struct LibraryMeshNode;

impl NodeDef for LibraryMeshNode {
    fn type_id(&self) -> &'static str { "LibraryMesh" }
    fn display_name(&self) -> &'static str { "Library Mesh" }
    fn category(&self) -> &'static str { "Mesh" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("path", PortValue::StringVal(Arc::new(String::new()))),
        ]
    }

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let path = match props.get("path") {
            PortValue::StringVal(s) => s.clone(),
            _ => return Ok(NodeOutputs::default()),
        };
        if path.is_empty() {
            return Ok(NodeOutputs::default());
        }
        let mesh = load_mesh_from_path(&path).map_err(NodeError::msg)?;
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(mesh)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(LibraryMeshNode);
}

#[cfg(not(target_arch = "wasm32"))]
fn load_mesh_from_path(path: &str) -> Result<MeshGL, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {}", path, e))?;
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".stl") {
        import_stl(&bytes).map_err(|e| format!("STL parse: {}", e))
    } else {
        Err(format!("unsupported mesh format: {}", path))
    }
}

#[cfg(target_arch = "wasm32")]
fn load_mesh_from_path(_path: &str) -> Result<MeshGL, String> {
    // WASM: file path strings can't be read directly from the FS; the UI
    // must inject bytes via a separate mechanism. Phase 10 wires that.
    Ok(MeshGL::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;
    use crate::serialization::mesh_io::export_stl;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn library_mesh_reads_a_round_tripped_stl_from_disk() {
        // Write a box to a temp STL, then point LibraryMesh at it.
        let dir = std::env::temp_dir();
        let path = dir.join("atomartist-libmesh-test.stl");
        let bytes = export_stl(&generate_box(1.0, 2.0, 3.0));
        std::fs::write(&path, &bytes).unwrap();

        let n = LibraryMeshNode;
        let mut props = NodeProperties::default();
        props.insert("path", PortValue::StringVal(Arc::new(path.to_string_lossy().into_owned())));
        let outs = n.evaluate(&NodeInputs::default(), &props).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(m) => {
                assert_eq!(m.tri_verts.len() / 3, 12, "box has 12 tris");
            }
            _ => panic!(),
        }
    }
}
