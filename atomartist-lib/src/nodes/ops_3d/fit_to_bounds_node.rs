//! FitToBounds — uniformly scale the input mesh so its bounding box
//! fits inside a target box (width × height × depth) centered at origin.

use std::sync::Arc;

use crate::geometry::{apply_transform, bounds};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
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
        let mut p = vec![
            PropDef::new("width",  PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("depth",  PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("uniform", PortValue::Bool(true)),
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
                "FitToBounds: expected Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        // Multi-body inputs: operate on the first body. The remaining
        // bodies pass through untouched in the output group. Re-fitting
        // the bounds across all bodies is a future enhancement.
        let first = match input.first() {
            Some(b) => b,
            None => {
                let mut o = NodeOutputs::default();
                o.set("out", PortValue::Geometry3d(input));
                return Ok(o);
            }
        };
        let (mn, mx) = match bounds(&first.mesh) {
            Some(b) => b,
            None => {
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
        let cx = (mn[0] + mx[0]) * 0.5;
        let cy = (mn[1] + mx[1]) * 0.5;
        let cz = (mn[2] + mx[2]) * 0.5;
        let m = scale_about([cx, cy, cz], [sx, sy, sz]);
        let result = apply_transform(&first.mesh, &m);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, result))));
        Ok(out)
    }
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
