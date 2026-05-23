//! Stroke — produces a closed outline of the input path at a specified width.
//!
//! Built on `CrossSection::offset` — offset outward by `+w/2` and inward
//! by `-w/2`, then take the difference of the two so the stroke is the
//! ring between them. For an open path this is approximate; for closed
//! shapes it produces a proper picture-frame.

use std::sync::Arc;

use crate::geometry::path2d::CrossSection;
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct StrokeNode;

impl NodeDef for StrokeNode {
    fn type_id(&self) -> &'static str { "Stroke" }
    fn display_name(&self) -> &'static str { "Stroke" }
    fn category(&self) -> &'static str { "Operations 2D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("input", SocketType::Path2d)
            .output("out", SocketType::Path2d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("width", PortValue::Number(1.0)).with_range(0.001, 1000.0),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Path2d(p) => p.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "Stroke: expected Path2d, got {:?}", other.socket_type()
            ))),
        };
        let w = ctx.properties.number("width", 1.0).max(1e-6);
        let outer: CrossSection = input.offset(w * 0.5);
        let inner: CrossSection = input.offset(-w * 0.5);
        let ring = outer.difference(&inner);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(ring)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(StrokeNode); }
