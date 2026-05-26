//! Transform node — translation + rotation + scale applied as a
//! **matrix composition** on top of the upstream body, not by baking
//! into vertices.
//!
//! Property layout matches NodeDesigner: nine separate `Number` properties
//! (tx/ty/tz, rx/ry/rz in degrees, sx/sy/sz). Rotation order is XYZ
//! (apply X first, then Y, then Z) which matches what most 3D modelers
//! mean when they say "Euler XYZ".
//!
//! Each output body's matrix is `transform_matrix · upstream.matrix`,
//! preserving the upstream's transform. The upstream's mesh is reused
//! by-Arc — dragging a gizmo writes only properties, never the mesh,
//! so re-evaluation is `O(bodies)` not `O(vertices)`. Matches
//! MatterCAD's `TransformWrapperObject3D` (composes via `*=`, no
//! mesh bake).
//!
//! Colour follows the same pass-through rule: if the user hasn't set
//! the Transform's `color` (it's still the `INHERIT_COLOR` sentinel),
//! each output body keeps the upstream body's colour. Setting an
//! opaque colour overrides every output body.

use std::sync::Arc;

use crate::geometry::{Body, Geometry3d};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    compose_with_upstream, op_props, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
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

        let m1 = mat_mul(&rxm, &s);
        let m2 = mat_mul(&rym, &m1);
        let m3 = mat_mul(&rzm, &m2);
        mat_mul(&tm, &m3)
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
        let tail = vec![
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
        // Op-variant: color default is INHERIT_COLOR so upstream colour
        // flows through until the user picks an override.
        let mut p = op_props();
        p.extend(tail);
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
        // Apply the composed transform to *every* body in the upstream
        // group, not just the first. Each output body's matrix is
        // `transform_matrix · upstream.matrix`; colour pulls from
        // upstream unless this node has an explicit override. Mesh
        // bytes are shared via Arc — no per-vertex transformation.
        let transform_matrix = Self::build_matrix(ctx.properties);
        let bodies: Vec<Body> = input
            .iter()
            .map(|upstream| {
                // Compose the upstream's matrix with this op's
                // transform, then apply the op's own colour override on
                // top (compose_with_upstream uses the op's matrix prop
                // by default — we override that with our built matrix).
                let composed_matrix = crate::graph::node::matmul4x4(
                    &transform_matrix,
                    &upstream.matrix,
                );
                let mut b = compose_with_upstream(ctx, upstream);
                b.matrix = composed_matrix;
                b
            })
            .collect();
        let mut out = NodeOutputs::default();
        out.set(
            "out",
            PortValue::Geometry3d(Arc::new(Geometry3d::from_bodies(bodies))),
        );
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(TransformNode);
}

// --- column-major 4x4 matrix helpers --------------------------------------
// (Generic matmul4x4 lives in graph::node so other ops can share it;
// the per-axis builders below are Transform-specific.)

use crate::graph::node::matmul4x4 as mat_mul;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{generate_box, Body, Geometry3d, INHERIT_COLOR};
    use crate::graph::node::{identity_matrix, NodeId, NodeInstance};
    use crate::registry::NodeInputs;

    fn props_with(values: &[(&'static str, f64)]) -> NodeProperties {
        let mut p = NodeProperties::default();
        for (k, v) in values {
            p.insert(*k, PortValue::Number(*v));
        }
        // Default color + matrix props so resolution doesn't panic.
        p.insert("color", PortValue::Color(INHERIT_COLOR));
        p.insert("matrix", PortValue::Matrix4x4(identity_matrix()));
        p
    }

    fn setup_with_body(body: Body) -> (NodeInstance, NodeInputs) {
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
            PortValue::Geometry3d(Arc::new(Geometry3d::from_body(body))),
        );
        (inst, inputs)
    }

    fn first_body(outs: &NodeOutputs) -> Body {
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(g) => g.first().unwrap().clone(),
            _ => panic!("expected Geometry3d output"),
        }
    }

    /// Translation composes into the body's matrix; vertices are NOT
    /// modified. (Pre-rewrite this test asserted the opposite — verts
    /// were baked. The new contract: mesh is shared by Arc, transforms
    /// stack as matrices.)
    #[test]
    fn translate_composes_into_matrix_no_vertex_bake() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        let upstream = Body::from_mesh(mesh.clone());
        let (inst, inputs) = setup_with_body(upstream);
        let props = props_with(&[("ty", 5.0)]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        let body = first_body(&outs);
        // Mesh is the same Arc — no per-vertex transformation happened.
        assert!(Arc::ptr_eq(&body.mesh, &mesh),
                "Transform must not re-bake mesh; output mesh should reuse upstream Arc");
        // Translation lives in the matrix's bottom row (column-major:
        // m[12]=tx, m[13]=ty, m[14]=tz).
        assert!((body.matrix[13] - 5.0).abs() < 1e-5,
                "ty=5 should land at matrix[13]; got matrix {:?}", body.matrix);
    }

    /// Upstream's matrix is preserved — Transform stacks on top.
    /// Verifies the §9 matrix-composition contract that MatterCAD's
    /// `TransformWrapperObject3D` implements via `item.Matrix *= ...`.
    #[test]
    fn transform_composes_with_upstream_matrix() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        // Upstream has a 2× scale on X already baked into its matrix
        // (e.g. an earlier Transform in the chain).
        let mut upstream_matrix = identity_matrix();
        upstream_matrix[0] = 2.0;
        let upstream = Body::from_mesh(mesh).with_matrix(upstream_matrix);
        let (inst, inputs) = setup_with_body(upstream);
        // This Transform adds a +3 on tx.
        let props = props_with(&[("tx", 3.0)]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        let body = first_body(&outs);
        // Composed: translate(3) · scale(2,1,1). Apply to (1, 0, 0) →
        // (2*1+3, 0, 0) = (5, 0, 0). The translation cell is matrix[12];
        // the X-scale cell is matrix[0].
        assert!((body.matrix[0] - 2.0).abs() < 1e-5, "X scale should survive: matrix[0]={}", body.matrix[0]);
        assert!((body.matrix[12] - 3.0).abs() < 1e-5, "tx=3 in composed matrix[12]; got {}", body.matrix[12]);
    }

    /// Upstream colour passes through when the Transform has no
    /// explicit colour set (INHERIT_COLOR sentinel).
    #[test]
    fn upstream_color_passes_through_when_op_color_is_inherit() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        let red = [1.0, 0.0, 0.0, 1.0];
        let upstream = Body::from_mesh(mesh).with_color(red);
        let (inst, inputs) = setup_with_body(upstream);
        let props = props_with(&[]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        let body = first_body(&outs);
        assert_eq!(body.color, red,
                   "Transform with INHERIT_COLOR must pass upstream red through");
    }

    /// Explicit (opaque) colour on the Transform overrides upstream.
    #[test]
    fn explicit_op_color_overrides_upstream() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        let red = [1.0, 0.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let upstream = Body::from_mesh(mesh).with_color(red);
        let (inst, inputs) = setup_with_body(upstream);
        let mut props = props_with(&[]);
        props.insert("color", PortValue::Color(blue));
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        let body = first_body(&outs);
        assert_eq!(body.color, blue,
                   "Transform with explicit blue must override upstream red");
    }

    /// Transform overwrites the upstream Body's `origin` claim with its
    /// own `NodeId` so a viewport click on the rendered (transformed)
    /// box selects the Transform node, not the upstream Box. Matches
    /// NodeDesigner's "click the displayed result → select the most-
    /// downstream op" UX.
    #[test]
    fn transform_claims_origin_for_itself() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        // Tag upstream with a deliberate (different) NodeId so we can
        // see whether Transform overwrites it.
        let upstream_node_id = NodeId(42);
        let upstream = Body::from_mesh(mesh).with_origin(upstream_node_id);
        let (inst, inputs) = setup_with_body(upstream);
        let props = props_with(&[]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        let body = first_body(&outs);
        // The Transform's own NodeId is NodeId(1) (set in setup_with_body).
        assert_eq!(body.origin, Some(NodeId(1)),
                   "Transform should claim origin = its own NodeId; got {:?}", body.origin);
        assert_ne!(body.origin, Some(upstream_node_id),
                   "upstream Box's origin must be overwritten");
    }

    /// Multi-body inputs: every body gets composed, not just the first.
    /// Per-body colours preserved.
    #[test]
    fn every_body_in_multi_body_input_is_composed() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        let red = [1.0, 0.0, 0.0, 1.0];
        let green = [0.0, 1.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let bodies = vec![
            Body::from_mesh(mesh.clone()).with_color(red),
            Body::from_mesh(mesh.clone()).with_color(green),
            Body::from_mesh(mesh).with_color(blue),
        ];
        let mut alloc = SocketUidAlloc::new();
        let tpl = n.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), "Transform", [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let mut inputs = NodeInputs::default();
        let uid = inst.input_by_name("input").unwrap().uid;
        inputs.insert(
            uid,
            PortValue::Geometry3d(Arc::new(Geometry3d::from_bodies(bodies))),
        );
        let props = props_with(&[("tz", 7.0)]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        match outs.by_name.get("out").unwrap() {
            PortValue::Geometry3d(g) => {
                assert_eq!(g.len(), 3, "multi-body input must produce multi-body output");
                let colors: Vec<_> = g.iter().map(|b| b.color).collect();
                assert_eq!(colors, vec![red, green, blue],
                           "every upstream body's colour must propagate");
                for body in g.iter() {
                    assert!((body.matrix[14] - 7.0).abs() < 1e-5,
                            "tz=7 should land at matrix[14] for every body; got {}", body.matrix[14]);
                }
            }
            _ => panic!("expected Geometry3d output"),
        }
    }
}
