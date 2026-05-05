//! Torus primitive node.

use std::sync::Arc;

use crate::geometry::generate_torus;
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct TorusNode;

impl NodeDef for TorusNode {
    fn type_id(&self) -> &'static str { "Torus" }
    fn display_name(&self) -> &'static str { "Torus" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("major_radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("minor_radius", PortValue::Number(3.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments_major", PortValue::Number(32.0)).with_range(3.0, 256.0),
            PropDef::new("segments_minor", PortValue::Number(16.0)).with_range(3.0, 256.0),
        ]
    }

    fn evaluate(&self, _i: &NodeInputs, p: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let major = p.number("major_radius", 10.0);
        let minor = p.number("minor_radius", 3.0);
        let su = p.number("segments_major", 32.0).round().clamp(3.0, 256.0) as u32;
        let sv = p.number("segments_minor", 16.0).round().clamp(3.0, 256.0) as u32;
        let mut o = NodeOutputs::default();
        o.set("out", PortValue::Geometry3d(Arc::new(generate_torus(major, minor, su, sv))));
        Ok(o)
    }
}

pub fn register(reg: &mut NodeRegistry) { reg.register(TorusNode); }
