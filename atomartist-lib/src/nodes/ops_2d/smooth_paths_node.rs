//! SmoothPaths — applies the CrossSection `simplify` pass to remove
//! micro-segments while preserving the overall shape. Useful as a
//! cleanup step after Boolean ops.

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct SmoothPathsNode;

/// The SmoothPaths node's `epsilon` parameter — the single source from
/// which its optional socket, property row, and `evaluate` read all
/// derive. The required `input` Path2d socket is minted separately.
fn params() -> ParamSet {
    ParamSet::new().number("epsilon", "Epsilon", 0.05, 0.0001..=10.0)
}

impl NodeDef for SmoothPathsNode {
    fn type_id(&self) -> &'static str { "SmoothPaths" }
    fn display_name(&self) -> &'static str { "Smooth Paths" }
    fn category(&self) -> &'static str { "Operations 2D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        params()
            .mint_sockets(
                InstanceTemplate::builder(alloc).input("input", SocketType::Path2d),
            )
            .output("out", SocketType::Path2d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        params().prop_defs()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Path2d(p) => p.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => return Err(NodeError::msg(format!(
                "SmoothPaths: expected Path2d, got {:?}", other.socket_type()
            ))),
        };
        let eps = params().reader(ctx).number("epsilon").max(0.0);
        let cleaned = input.simplify(eps);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cleaned)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(SmoothPathsNode); }
