//! Align — translates a 3D mesh so its bounding box's chosen anchor
//! lands at the origin (or at a target reference mesh's anchor).
//!
//! Properties:
//!   - `align_x`, `align_y`, `align_z`: numeric -1..=1 — -1 = min edge,
//!     0 = center, +1 = max edge. Default (0, -1, 0): center XZ, sit
//!     on the floor (Y min at 0). Matches the most common use in
//!     NodeDesigner where users want a model to rest on a build plate.

use std::sync::Arc;

use crate::geometry::{apply_transform, bounds};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
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
        vec![
            PropDef::new("align_x", PortValue::Number(0.0)).with_range(-1.0, 1.0),
            PropDef::new("align_y", PortValue::Number(-1.0)).with_range(-1.0, 1.0),
            PropDef::new("align_z", PortValue::Number(0.0)).with_range(-1.0, 1.0),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Geometry3d(m) => m.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Align: expected Geometry3d, got {:?}", other.socket_type()
            ))),
        };
        let (mn, mx) = match bounds(&input) {
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
        let result = apply_transform(&input, &translate);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(result)));
        Ok(out)
    }
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
