//! Transform node — applies translation, rotation, and scale to a 3D mesh.
//!
//! Property layout matches NodeDesigner: nine separate `Number` properties
//! (tx/ty/tz, rx/ry/rz in degrees, sx/sy/sz). Rotation order is XYZ
//! (apply X first, then Y, then Z) which matches what most 3D modelers
//! mean when they say "Euler XYZ".

use std::sync::Arc;

use crate::geometry::apply_transform;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeProperties, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct TransformNode;

impl TransformNode {
    fn build_matrix(props: &NodeProperties) -> [f32; 16] {
        let tx = props.number("tx", 0.0) as f32;
        let ty = props.number("ty", 0.0) as f32;
        let tz = props.number("tz", 0.0) as f32;
        let rx = (props.number("rx", 0.0) as f32).to_radians();
        let ry = (props.number("ry", 0.0) as f32).to_radians();
        let rz = (props.number("rz", 0.0) as f32).to_radians();
        let sx = props.number("sx", 1.0) as f32;
        let sy = props.number("sy", 1.0) as f32;
        let sz = props.number("sz", 1.0) as f32;

        let s = mat_scale(sx, sy, sz);
        let rxm = mat_rot_x(rx);
        let rym = mat_rot_y(ry);
        let rzm = mat_rot_z(rz);
        let tm = mat_translate(tx, ty, tz);

        let m1 = mat_mul(rxm, s);
        let m2 = mat_mul(rym, m1);
        let m3 = mat_mul(rzm, m2);
        mat_mul(tm, m3)
    }
}

impl NodeDef for TransformNode {
    fn type_id(&self) -> &'static str { "Transform" }
    fn display_name(&self) -> &'static str { "Transform" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("input", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let mut p = vec![
            PropDef::new("tx", PortValue::Number(0.0)),
            PropDef::new("ty", PortValue::Number(0.0)),
            PropDef::new("tz", PortValue::Number(0.0)),
            PropDef::new("rx", PortValue::Number(0.0)).with_range(-360.0, 360.0),
            PropDef::new("ry", PortValue::Number(0.0)).with_range(-360.0, 360.0),
            PropDef::new("rz", PortValue::Number(0.0)).with_range(-360.0, 360.0),
            PropDef::new("sx", PortValue::Number(1.0)).with_range(0.001, 1000.0),
            PropDef::new("sy", PortValue::Number(1.0)).with_range(0.001, 1000.0),
            PropDef::new("sz", PortValue::Number(1.0)).with_range(0.001, 1000.0),
        ];
        // Prepend color + matrix so they render as the first two rows.
        let mut p = { let mut g = geometry_props(); g.extend(p); g };
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Transform: expected Geometry3d input, got {:?}", other.socket_type()
            ))),
        };
        let matrix = Self::build_matrix(ctx.properties);
        // Multi-body inputs: transform the first body. Rest pass
        // through. (Future: apply to every body and emit a
        // multi-body group.)
        let first = match input.first() {
            Some(b) => b,
            None => return Ok(NodeOutputs::default()),
        };
        let out_mesh = apply_transform(&first.mesh, &matrix);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, out_mesh))));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(TransformNode);
}

// --- column-major 4x4 matrix helpers --------------------------------------

fn mat_translate(tx: f32, ty: f32, tz: f32) -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        tx,  ty,  tz,  1.0,
    ]
}

fn mat_scale(sx: f32, sy: f32, sz: f32) -> [f32; 16] {
    [
        sx,  0.0, 0.0, 0.0,
        0.0, sy,  0.0, 0.0,
        0.0, 0.0, sz,  0.0,
        0.0, 0.0, 0.0, 1.0,
    ]
}

fn mat_rot_x(a: f32) -> [f32; 16] {
    let c = a.cos();
    let s = a.sin();
    [
        1.0, 0.0,  0.0, 0.0,
        0.0,   c,    s, 0.0,
        0.0,  -s,    c, 0.0,
        0.0, 0.0,  0.0, 1.0,
    ]
}

fn mat_rot_y(a: f32) -> [f32; 16] {
    let c = a.cos();
    let s = a.sin();
    [
          c, 0.0,  -s, 0.0,
        0.0, 1.0, 0.0, 0.0,
          s, 0.0,   c, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]
}

fn mat_rot_z(a: f32) -> [f32; 16] {
    let c = a.cos();
    let s = a.sin();
    [
          c,    s, 0.0, 0.0,
         -s,    c, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]
}

/// Column-major 4x4 multiply: returns A · B.
fn mat_mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut r = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[col * 4 + k];
            }
            r[col * 4 + row] = sum;
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{generate_box, get_pos, num_verts};
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::registry::NodeInputs;

    fn props_with(values: &[(&'static str, f64)]) -> NodeProperties {
        let mut p = NodeProperties::default();
        for (k, v) in values {
            p.insert(*k, PortValue::Number(*v));
        }
        p
    }

    fn setup(mesh: Arc<manifold_rust::types::MeshGL>) -> (NodeInstance, NodeInputs) {
        let n = TransformNode;
        let mut alloc = SocketUidAlloc::new();
        let tpl = n.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), "Transform", [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let mut inputs = NodeInputs::default();
        let uid = inst.input_by_name("input").unwrap().uid;
        inputs.insert(
            uid,
            PortValue::Geometry3d(Arc::new(
                crate::geometry::Geometry3d::from_mesh(mesh),
            )),
        );
        (inst, inputs)
    }

    #[test]
    fn translate_shifts_all_y_by_5() {
        let n = TransformNode;
        let m = Arc::new(generate_box(1.0, 1.0, 1.0));
        let (inst, inputs) = setup(m.clone());
        let props = props_with(&[("ty", 5.0), ("sx", 1.0), ("sy", 1.0), ("sz", 1.0)]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(t) => {
                for i in 0..num_verts(&t.first().unwrap().mesh) {
                    let p = get_pos(&t.first().unwrap().mesh, i);
                    let p0 = get_pos(&m, i);
                    assert!((p[1] - (p0[1] + 5.0)).abs() < 1e-5,
                            "vert {} y did not shift by 5: was {}, now {}",
                            i, p0[1], p[1]);
                }
            }
            _ => panic!("expected Geometry3d output"),
        }
    }

    #[test]
    fn scale_doubles_x_dimension() {
        let n = TransformNode;
        let m = Arc::new(generate_box(2.0, 2.0, 2.0));
        let (inst, inputs) = setup(m.clone());
        let props = props_with(&[("sx", 2.0), ("sy", 1.0), ("sz", 1.0)]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(t) => {
                for i in 0..num_verts(&t.first().unwrap().mesh) {
                    let p = get_pos(&t.first().unwrap().mesh, i);
                    let p0 = get_pos(&m, i);
                    assert!((p[0] - p0[0] * 2.0).abs() < 1e-5);
                    assert!((p[1] - p0[1]).abs() < 1e-5);
                    assert!((p[2] - p0[2]).abs() < 1e-5);
                }
            }
            _ => panic!(),
        }
    }
}
