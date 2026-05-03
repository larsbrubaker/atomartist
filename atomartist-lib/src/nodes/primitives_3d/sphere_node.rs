//! Sphere primitive node — UV sphere centered at origin.

use std::sync::Arc;

use crate::geometry::generate_sphere;
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct SphereNode;

impl NodeDef for SphereNode {
    fn type_id(&self) -> &'static str { "Sphere" }
    fn display_name(&self) -> &'static str { "Sphere" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }

    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Geometry3d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("segments_u", PortValue::Number(32.0)).with_range(3.0, 256.0),
            PropDef::new("segments_v", PortValue::Number(16.0)).with_range(2.0, 256.0),
        ]
    }

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let r = props.number("radius", 10.0);
        let su = props.number("segments_u", 32.0).round().clamp(3.0, 256.0) as u32;
        let sv = props.number("segments_v", 16.0).round().clamp(2.0, 256.0) as u32;
        let mesh = generate_sphere(r, su, sv);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(mesh)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(SphereNode);
}
