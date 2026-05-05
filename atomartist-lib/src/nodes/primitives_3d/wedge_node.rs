//! Wedge primitive node — triangular prism (right-triangle cross-section).

use std::sync::Arc;

use crate::geometry::generate_wedge;
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct WedgeNode;

impl NodeDef for WedgeNode {
    fn type_id(&self) -> &'static str { "Wedge" }
    fn display_name(&self) -> &'static str { "Wedge" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("width", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("depth", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
        ]
    }

    fn evaluate(&self, _i: &NodeInputs, p: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let w = p.number("width", 20.0);
        let h = p.number("height", 20.0);
        let d = p.number("depth", 20.0);
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(generate_wedge(w, h, d))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(WedgeNode); }
