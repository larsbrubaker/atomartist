//! GraphOutput — declarative output port for a subgraph.
//!
//! NOTE: This node is slated for removal in Stage 1i of the engine refactor;
//! the unified `Output` node (Stage 2) absorbs both this and the legacy
//! display-anchor `OutputNode`. Kept here in its single-input form purely
//! so the workspace compiles during the refactor — no behavior changes.

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct GraphOutputNode;

impl NodeDef for GraphOutputNode {
    fn type_id(&self) -> &'static str { "GraphOutput" }
    fn display_name(&self) -> &'static str { "Graph Output" }
    fn category(&self) -> &'static str { "I/O" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![PropDef::new("name", PortValue::StringVal(Arc::new("output".into())))]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.input_named("in").clone();
        let mut out = NodeOutputs::default();
        out.set("out", v);
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(GraphOutputNode);
}
