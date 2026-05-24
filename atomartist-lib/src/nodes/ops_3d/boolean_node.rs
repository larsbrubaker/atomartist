//! Boolean operation node — Union / Difference / Intersection on two
//! `MeshGL` solids via `manifold-rust`.
//!
//! Inputs are converted to `Manifold`, the requested op is performed, and
//! the result is exported back to `MeshGL`. We strip normals before
//! handing meshes to manifold (so its property-interpolation across new
//! cut vertices doesn't yield mid-face-averaged normals), then run
//! `compute_flat_normals` on the result so the output is render-ready.

use std::sync::Arc;

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{MeshGL, OpType};

use crate::geometry::mesh3d::{compute_flat_normals, NUM_PROP};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct BooleanNode;

impl NodeDef for BooleanNode {
    fn type_id(&self) -> &'static str { "Boolean" }
    fn display_name(&self) -> &'static str { "Boolean" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("a", SocketType::Geometry3d)
            .input("b", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let mut p = vec![
            // Operation: 0 = Union, 1 = Difference, 2 = Intersection.
            PropDef::new("operation", PortValue::Number(0.0)).with_range(0.0, 2.0),
        ];
        p.extend(geometry_props());
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let geom_a = match ctx.input_named("a") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Boolean: input 'a' must be Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        let geom_b = match ctx.input_named("b") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Boolean: input 'b' must be Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        let op_idx = ctx.properties.number("operation", 0.0).round() as i32;
        let op = match op_idx {
            0 => OpType::Add,         // Union
            1 => OpType::Subtract,    // Difference (a - b)
            2 => OpType::Intersect,
            _ => OpType::Add,
        };

        let stripped_a = strip_normals(&geom_a.mesh);
        let stripped_b = strip_normals(&geom_b.mesh);
        let ma = Manifold::from_mesh_gl(&stripped_a);
        let mb = Manifold::from_mesh_gl(&stripped_b);
        let result = ma.boolean(&mb, op);
        let mut out_mesh = result.get_mesh_gl(-1);
        promote_to_num_prop6(&mut out_mesh);
        compute_flat_normals(&mut out_mesh);

        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, out_mesh))));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(BooleanNode);
}

/// Build a num_prop=3 MeshGL clone (positions only), merging coincident
/// vertices so the result is manifold (shared edges share vertex indices).
/// Without this Manifold treats each per-face-flat duplicate as a separate
/// disconnected triangle and produces empty output.
fn strip_normals(mesh: &MeshGL) -> MeshGL {
    let stride = mesh.num_prop.max(3) as usize;
    let n = mesh.vert_properties.len() / stride;
    if n == 0 {
        return MeshGL {
            num_prop: 3,
            ..Default::default()
        };
    }

    let scale = 1e5;
    let mut bucket: std::collections::HashMap<(i64, i64, i64), u32> =
        std::collections::HashMap::new();
    let mut out_pos: Vec<f32> = Vec::new();
    let mut remap: Vec<u32> = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * stride;
        let x = mesh.vert_properties[off];
        let y = mesh.vert_properties[off + 1];
        let z = mesh.vert_properties[off + 2];
        let key = (
            (x as f64 * scale).round() as i64,
            (y as f64 * scale).round() as i64,
            (z as f64 * scale).round() as i64,
        );
        let new_id = *bucket.entry(key).or_insert_with(|| {
            let id = (out_pos.len() / 3) as u32;
            out_pos.extend_from_slice(&[x, y, z]);
            id
        });
        remap.push(new_id);
    }
    let new_tris: Vec<u32> = mesh.tri_verts.iter().map(|i| remap[*i as usize]).collect();
    let mut filtered: Vec<u32> = Vec::with_capacity(new_tris.len());
    for tri in new_tris.chunks_exact(3) {
        if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
            filtered.extend_from_slice(tri);
        }
    }
    MeshGL {
        num_prop: 3,
        vert_properties: out_pos,
        tri_verts: filtered,
        ..Default::default()
    }
}

fn promote_to_num_prop6(mesh: &mut MeshGL) {
    if mesh.num_prop == NUM_PROP {
        return;
    }
    let n = mesh.vert_properties.len() / mesh.num_prop as usize;
    let mut out = Vec::with_capacity(n * NUM_PROP as usize);
    for i in 0..n {
        let off = i * mesh.num_prop as usize;
        out.push(mesh.vert_properties[off]);
        out.push(mesh.vert_properties[off + 1]);
        out.push(mesh.vert_properties[off + 2]);
        out.push(0.0);
        out.push(0.0);
        out.push(0.0);
    }
    mesh.vert_properties = out;
    mesh.num_prop = NUM_PROP;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::registry::{NodeInputs, NodeProperties};

    fn wrap(m: Arc<MeshGL>) -> PortValue {
        PortValue::Geometry3d(Arc::new(crate::geometry::Geometry3d::from_mesh(m)))
    }

    #[test]
    fn union_of_overlapping_boxes_yields_single_solid() {
        let n = BooleanNode;
        let a = Arc::new(generate_box(2.0, 2.0, 2.0));
        let b = Arc::new(generate_box(2.0, 2.0, 2.0));
        let mut alloc = SocketUidAlloc::new();
        let tpl = n.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), "Boolean", [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let mut inputs = NodeInputs::default();
        inputs.insert(inst.input_by_name("a").unwrap().uid, wrap(a));
        inputs.insert(inst.input_by_name("b").unwrap().uid, wrap(b));
        let mut props = NodeProperties::default();
        props.insert("operation", PortValue::Number(0.0));
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(g) => {
                assert!(!g.mesh.vert_properties.is_empty());
                assert!(g.mesh.tri_verts.len() / 3 >= 12);
            }
            _ => panic!(),
        }
    }
}
