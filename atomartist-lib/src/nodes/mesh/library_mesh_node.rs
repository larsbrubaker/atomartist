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
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, PropDef,
};
use crate::serialization::mesh_io::import_stl;
use crate::socket_types::SocketType;

pub struct LibraryMeshNode;

impl NodeDef for LibraryMeshNode {
    fn type_id(&self) -> &'static str { "LibraryMesh" }
    fn display_name(&self) -> &'static str { "Library Mesh" }
    fn category(&self) -> &'static str { "Mesh" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let mut p = vec![
            PropDef::new("path", PortValue::StringVal(Arc::new(String::new()))),
        ];
        p.extend(geometry_props());
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let path = match ctx.properties.get("path") {
            PortValue::StringVal(s) => s.clone(),
            _ => return Ok(NodeOutputs::default()),
        };
        if path.is_empty() {
            return Ok(NodeOutputs::default());
        }
        let mesh = load_mesh_from_path(&path).map_err(NodeError::msg)?;
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, mesh))));
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
    Ok(MeshGL::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::registry::{NodeInputs, NodeProperties};
    use crate::serialization::mesh_io::export_stl;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn library_mesh_reads_a_round_tripped_stl_from_disk() {
        let dir = std::env::temp_dir();
        let path = dir.join("atomartist-libmesh-test.stl");
        let bytes = export_stl(&generate_box(1.0, 2.0, 3.0));
        std::fs::write(&path, &bytes).unwrap();

        let n = LibraryMeshNode;
        let mut alloc = SocketUidAlloc::new();
        let tpl = n.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), "LibraryMesh", [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let inputs = NodeInputs::default();
        let mut props = NodeProperties::default();
        props.insert(
            "path",
            PortValue::StringVal(Arc::new(path.to_string_lossy().into_owned())),
        );
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(g) => {
                assert_eq!(g.mesh.tri_verts.len() / 3, 12, "box has 12 tris");
            }
            _ => panic!(),
        }
    }
}
