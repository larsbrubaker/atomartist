//! FitToBounds — scale the input geometry so its world-space bounding
//! box fits inside a target box (width × height × depth) centered at
//! origin.
//!
//! Matrix-composition contract (matches `TransformNode`): the scale is
//! stored on `Body.matrix` rather than baked into vertices. World
//! bounds are computed by transforming the local AABB's 8 corners by
//! each upstream body's matrix and taking the union — loose but
//! adequate for typical inputs (a tight world AABB would require
//! transforming every vertex). Multi-body inputs use a single shared
//! scale so the whole group fits together.

use std::sync::Arc;

use crate::geometry::{bounds, Body, Geometry3d};
use crate::graph::node::{matmul4x4, PortValue};
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    compose_with_upstream, op_props, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct FitToBoundsNode;

impl NodeDef for FitToBoundsNode {
    fn type_id(&self) -> &'static str { "FitToBounds" }
    fn display_name(&self) -> &'static str { "Fit to Bounds" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("input", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let tail = vec![
            PropDef::new("width",  PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("depth",  PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("uniform", PortValue::Bool(true)),
        ];
        // Op-variant geometry_props: color defaults to INHERIT_COLOR so
        // upstream tints flow through.
        let mut p = op_props();
        p.extend(tail);
        p
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "FitToBounds: expected Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        // Union of every body's world AABB so multi-body groups share
        // one scale and fit together as a unit.
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
                // Empty / no-vertex input — passthrough.
                let mut o = NodeOutputs::default();
                o.set("out", PortValue::Geometry3d(input));
                return Ok(o);
            }
        };
        let cur = [
            (mx[0] - mn[0]).max(1e-6),
            (mx[1] - mn[1]).max(1e-6),
            (mx[2] - mn[2]).max(1e-6),
        ];
        let target = [
            ctx.properties.number("width", 20.0) as f32,
            ctx.properties.number("height", 20.0) as f32,
            ctx.properties.number("depth", 20.0) as f32,
        ];
        let factor = [
            target[0] / cur[0],
            target[1] / cur[1],
            target[2] / cur[2],
        ];
        let uniform = ctx.properties.bool_("uniform", true);
        let (sx, sy, sz) = if uniform {
            let s = factor[0].min(factor[1]).min(factor[2]);
            (s, s, s)
        } else {
            (factor[0], factor[1], factor[2])
        };
        let cw = [
            (mn[0] + mx[0]) * 0.5,
            (mn[1] + mx[1]) * 0.5,
            (mn[2] + mx[2]) * 0.5,
        ];
        let scale_matrix = scale_about(cw, [sx, sy, sz]);
        // Compose the shared scale on top of each body's existing
        // matrix; mesh stays untouched. Then run colour resolution.
        let bodies: Vec<Body> = input
            .iter()
            .map(|upstream| {
                let composed_matrix = matmul4x4(&scale_matrix, &upstream.matrix);
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

/// Transform the 8 corners of a local AABB by `matrix` and return the
/// world-space AABB enclosing the transformed corners. Loose under
/// rotation (the transformed AABB envelope may be larger than the true
/// AABB of every transformed vertex) but cheap and adequate for
/// FitToBounds where the user just needs the geometry to fit.
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

fn scale_about(c: [f32; 3], s: [f32; 3]) -> [f32; 16] {
    let tx = c[0] - s[0] * c[0];
    let ty = c[1] - s[1] * c[1];
    let tz = c[2] - s[2] * c[2];
    [
        s[0], 0.0,  0.0,  0.0,
        0.0,  s[1], 0.0,  0.0,
        0.0,  0.0,  s[2], 0.0,
        tx,   ty,   tz,   1.0,
    ]
}

pub fn register(reg: &mut NodeRegistry) { reg.register(FitToBoundsNode); }
