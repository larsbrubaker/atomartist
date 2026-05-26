//! Align — translates a 3D mesh so its bounding box's chosen anchor
//! lands at the origin.
//!
//! Properties:
//!   - `align_x`, `align_y`, `align_z`: numeric -1..=1 — -1 = min edge,
//!     0 = center, +1 = max edge. Default (0, -1, 0): center XY, sit
//!     on the floor (Z min at 0). Matches MatterCAD's
//!     "Print Bed" alignment where users want a model to rest on the
//!     build plate.
//!
//! Matrix-composition contract (same as `TransformNode` / `FitToBounds`):
//! the alignment translation is composed into `Body.matrix`; vertices
//! stay in their upstream-local frame. World bounds are computed from
//! the union of every upstream body's transformed AABB so multi-body
//! groups align as a unit.

use std::sync::Arc;

use crate::geometry::{bounds, Body, Geometry3d};
use crate::graph::node::{matmul4x4, PortValue};
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    compose_with_upstream, op_props, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct AlignNode;

impl NodeDef for AlignNode {
    fn type_id(&self) -> &'static str { "Align" }
    fn display_name(&self) -> &'static str { "Align" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("input", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let tail = vec![
            PropDef::new("align_x", PortValue::Number(0.0)).with_range(-1.0, 1.0),
            PropDef::new("align_y", PortValue::Number(-1.0)).with_range(-1.0, 1.0),
            PropDef::new("align_z", PortValue::Number(0.0)).with_range(-1.0, 1.0),
        ];
        let mut p = op_props();
        p.extend(tail);
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Align: expected Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        // Union of every body's world AABB so multi-body groups align
        // as a single unit.
        let world_aabb = input.iter().fold(None, |acc: Option<([f32; 3], [f32; 3])>, body| {
            let local = bounds(&body.mesh)?;
            let world = transformed_aabb(local, &body.matrix);
            Some(match acc {
                None => world,
                Some((amn, amx)) => (
                    [amn[0].min(world.0[0]), amn[1].min(world.0[1]), amn[2].min(world.0[2])],
                    [amx[0].max(world.1[0]), amx[1].max(world.1[1]), amx[2].max(world.1[2])],
                ),
            })
        });
        let (mn, mx) = match world_aabb {
            Some(b) => b,
            None => {
                let mut o = NodeOutputs::default();
                o.set("out", PortValue::Geometry3d(input));
                return Ok(o);
            }
        };
        let ax = ctx.properties.number("align_x", 0.0) as f32;
        let ay = ctx.properties.number("align_y", -1.0) as f32;
        let az = ctx.properties.number("align_z", 0.0) as f32;
        let anchor_x = (mn[0] + mx[0]) * 0.5 + ax * (mx[0] - mn[0]) * 0.5;
        let anchor_y = (mn[1] + mx[1]) * 0.5 + ay * (mx[1] - mn[1]) * 0.5;
        let anchor_z = (mn[2] + mx[2]) * 0.5 + az * (mx[2] - mn[2]) * 0.5;
        let translate = column_major_translate(-anchor_x, -anchor_y, -anchor_z);

        let bodies: Vec<Body> = input
            .iter()
            .map(|upstream| {
                let composed_matrix = matmul4x4(&translate, &upstream.matrix);
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

/// Loose world AABB: transform the 8 local-AABB corners and take the
/// envelope. Shared shape with FitToBounds — same trade-off (cheap;
/// looser than transforming every vertex under rotation).
fn transformed_aabb(local: ([f32; 3], [f32; 3]), matrix: &[f32; 16]) -> ([f32; 3], [f32; 3]) {
    let (mn, mx) = local;
    let corners = [
        [mn[0], mn[1], mn[2]], [mx[0], mn[1], mn[2]],
        [mn[0], mx[1], mn[2]], [mx[0], mx[1], mn[2]],
        [mn[0], mn[1], mx[2]], [mx[0], mn[1], mx[2]],
        [mn[0], mx[1], mx[2]], [mx[0], mx[1], mx[2]],
    ];
    let mut wmn = [f32::INFINITY; 3];
    let mut wmx = [f32::NEG_INFINITY; 3];
    for c in &corners {
        let t = mat4_transform_point(matrix, *c);
        for k in 0..3 {
            if t[k] < wmn[k] { wmn[k] = t[k]; }
            if t[k] > wmx[k] { wmx[k] = t[k]; }
        }
    }
    (wmn, wmx)
}

fn mat4_transform_point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
    let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
    let z = m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14];
    let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
    if (w - 1.0).abs() < 1e-6 || w == 0.0 { [x, y, z] } else { [x / w, y / w, z / w] }
}

fn column_major_translate(tx: f32, ty: f32, tz: f32) -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        tx,  ty,  tz,  1.0,
    ]
}

pub fn register(reg: &mut NodeRegistry) { reg.register(AlignNode); }
