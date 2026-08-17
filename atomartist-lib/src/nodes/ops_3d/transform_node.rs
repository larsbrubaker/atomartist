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
    compose_with_upstream, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct TransformNode;

/// The Transform node's parameter schema. Uses the [`ParamSet::op`]
/// preseed for the shared `color` (INHERIT default, socket `Color`) +
/// `matrix` params, but marks `matrix` `no_socket`: Transform builds its
/// own matrix from the nine translate/rotate/scale parameters and would
/// discard any op `matrix` input, so wiring one would be a silent no-op.
/// The translation offsets are unbounded, and so are the rotations:
/// rotation is cyclic, so a wired multi-turn angle (720°, or an
/// animated angle that keeps winding) is a legal orientation rather
/// than an out-of-range value, and clamping it to ±360° would silently
/// change the result. Only the scales carry a declared range
/// (0.001..=1000.0), which keeps a wired negative scale from handing the
/// CSG kernel a mirrored body. Capitalized socket names are preserved.
fn params() -> ParamSet {
    ParamSet::op()
        .no_socket() // `matrix`: property-only; Transform builds its own.
        .number_unbounded("tx", "Translate X", 0.0)
        .socket_named("Translate X")
        .number_unbounded("ty", "Translate Y", 0.0)
        .socket_named("Translate Y")
        .number_unbounded("tz", "Translate Z", 0.0)
        .socket_named("Translate Z")
        .number_unbounded("rx", "Rotate X", 0.0)
        .socket_named("Rotate X")
        .number_unbounded("ry", "Rotate Y", 0.0)
        .socket_named("Rotate Y")
        .number_unbounded("rz", "Rotate Z", 0.0)
        .socket_named("Rotate Z")
        .number("sx", "Scale X", 1.0, 0.001..=1000.0)
        .socket_named("Scale X")
        .number("sy", "Scale Y", 1.0, 0.001..=1000.0)
        .socket_named("Scale Y")
        .number("sz", "Scale Z", 1.0, 0.001..=1000.0)
        .socket_named("Scale Z")
}

impl TransformNode {
    /// Build the transform matrix from the nine translate/rotate/scale
    /// parameters. Each reads its wired input socket first (so it can be
    /// driven by NumberConst / GraphInput / math nodes), falling back to
    /// the stored property, then the type default.
    fn build_matrix(ctx: &EvalCtx) -> [f32; 16] {
        let ps = params();
        let r = ps.reader(ctx);
        let tx = r.number("tx") as f32;
        let ty = r.number("ty") as f32;
        let tz = r.number("tz") as f32;
        let rx = (r.number("rx") as f32).to_radians();
        let ry = (r.number("ry") as f32).to_radians();
        let rz = (r.number("rz") as f32).to_radians();
        let sx = r.number("sx") as f32;
        let sy = r.number("sy") as f32;
        let sz = r.number("sz") as f32;

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
        // Geometry input stays first; the schema params (Color plus the
        // nine translate/rotate/scale sockets) follow. `matrix` mints no
        // socket (see `params`).
        params()
            .mint_sockets(
                InstanceTemplate::builder(alloc).input("input", SocketType::Geometry3d),
            )
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        params().prop_defs()
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
        let transform_matrix = Self::build_matrix(ctx);
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
    use crate::registry::{NodeInputs, NodeProperties};

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

    /// A wired `Translate Z` input overrides the stored `tz` property —
    /// the socket-or-property rule for the transform parameters.
    #[test]
    fn wired_translate_input_wins_over_property() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        let (inst, mut inputs) = setup_with_body(Body::from_mesh(mesh));
        // Property says tz=2, but the wired socket says tz=9 → 9 wins.
        let tz_uid = inst.input_by_name("Translate Z").unwrap().uid;
        inputs.insert(tz_uid, PortValue::Number(9.0));
        let props = props_with(&[("tz", 2.0)]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        let body = first_body(&outs);
        assert!((body.matrix[14] - 9.0).abs() < 1e-5,
                "wired Translate Z=9 should win over property tz=2; got {}", body.matrix[14]);
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

    /// A NumberConst emitting -1 wired into `Scale X` must not produce a
    /// mirrored (negative-determinant) transform: `sx` is declared
    /// 0.001..=1000.0, so the socket value clamps to the declared minimum
    /// before it reaches the matrix. Before the clamp landed, this graph
    /// handed the CSG kernel an inside-out body.
    #[test]
    fn negative_wired_scale_x_clamps_instead_of_mirroring() {
        use crate::graph::{executor::evaluate_all, Graph, Noodle};
        use crate::nodes::register_all;

        let mut reg = NodeRegistry::new();
        register_all(&mut reg);
        let mut g = Graph::new();
        let boxn = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let xf = g.add_new_node("Transform", [200.0, 0.0], &reg).unwrap();
        let bout = g.get(boxn).unwrap().output_by_name("out").unwrap().uid;
        let xin = g.get(xf).unwrap().input_by_name("input").unwrap().uid;
        g.connect(Noodle::new(boxn, bout, xf, xin), &reg).unwrap();

        let nc = g.add_new_node("NumberConst", [-200.0, 0.0], &reg).unwrap();
        g.set_property(nc, "value", PortValue::Number(-1.0)).unwrap();
        let nc_out = g.get(nc).unwrap().output_by_name("out").unwrap().uid;
        let sx_in = g.get(xf).unwrap().input_by_name("Scale X").unwrap().uid;
        g.connect(Noodle::new(nc, nc_out, xf, sx_in), &reg).unwrap();

        evaluate_all(&mut g, &reg).unwrap().expect_clean();
        let out_uid = g.get(xf).unwrap().output_by_name("out").unwrap().uid;
        let m = match g.get(xf).unwrap().cached_outputs.get(&out_uid) {
            Some(PortValue::Geometry3d(geo)) => geo.first().unwrap().matrix,
            other => panic!("expected Geometry3d output, got {other:?}"),
        };
        // The X-scale cell carries the clamped minimum, not -1.
        assert!(
            (m[0] - 0.001).abs() < 1e-6,
            "wired sx=-1 must clamp to the declared 0.001 minimum; got matrix[0]={}",
            m[0]
        );
        // And the upper-left 3×3 determinant stays positive (not mirrored).
        let det = m[0] * (m[5] * m[10] - m[9] * m[6])
            - m[4] * (m[1] * m[10] - m[9] * m[2])
            + m[8] * (m[1] * m[6] - m[5] * m[2]);
        assert!(det > 0.0, "transform must not be mirrored; determinant was {det}");
    }

    /// Rotation is cyclic, so a wired multi-turn angle must reach the
    /// matrix unclamped. 810° about Z is two full turns plus 90°; if the
    /// reader clamped it to a ±360° range it would come out as a plain
    /// 360° (identity) rotation instead.
    #[test]
    fn wired_multi_turn_rotation_is_not_clamped() {
        let n = TransformNode;
        let mesh = Arc::new(generate_box(1.0, 1.0, 1.0));
        let (inst, mut inputs) = setup_with_body(Body::from_mesh(mesh));
        let rz_uid = inst.input_by_name("Rotate Z").unwrap().uid;
        inputs.insert(rz_uid, PortValue::Number(810.0));
        let props = props_with(&[]);
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        let outs = n.evaluate(&ctx).unwrap();
        let body = first_body(&outs);
        // Column-major rot-Z: m[0] = cos, m[1] = sin. 810° ≡ 90°.
        assert!(
            body.matrix[0].abs() < 1e-5 && (body.matrix[1] - 1.0).abs() < 1e-5,
            "wired Rotate Z=810 must pass through as a 90° rotation \
             (cos≈0, sin≈1); got matrix[0]={}, matrix[1]={}",
            body.matrix[0],
            body.matrix[1]
        );
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
