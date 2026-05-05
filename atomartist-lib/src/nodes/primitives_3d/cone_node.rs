//! Cone primitive node.

use std::sync::Arc;

use crate::geometry::generate_cone;
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct ConeNode;

impl NodeDef for ConeNode {
    fn type_id(&self) -> &'static str { "Cone" }
    fn display_name(&self) -> &'static str { "Cone" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments", PortValue::Number(32.0)).with_range(3.0, 256.0),
        ]
    }

    fn evaluate(&self, _i: &NodeInputs, p: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let r = p.number("radius", 10.0);
        let h = p.number("height", 20.0);
        let s = p.number("segments", 32.0).round().clamp(3.0, 256.0) as u32;
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(generate_cone(r, h, s))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(ConeNode); }
