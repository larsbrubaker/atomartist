//! Output — the terminal display anchor. Whatever Geometry3d flows into
//! the Output node is what the 3D viewport renders. It has a single
//! `in` input (Geometry3d) and mirrors that value out so downstream
//! debugging tools can re-tap it. Display semantics live in
//! `AppState::pick_display_mesh`.
//!
//! NOTE: This single-input form is the temporary placeholder while Stage 2
//! of the engine refactor builds the unified Output node (Blender-style
//! dynamic multi-input). Replaced wholesale by the new file when Stage 2
//! lands.

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry,
};
use crate::socket_types::SocketType;

pub struct OutputNode;

impl NodeDef for OutputNode {
    fn type_id(&self) -> &'static str { "Output" }
    fn display_name(&self) -> &'static str { "Output" }
    fn category(&self) -> &'static str { "Output" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.input_named("in").clone();
        let mut out = NodeOutputs::default();
        out.set("out", v);
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(OutputNode);
}
