//! Cylinder primitive node — Y-axis cylinder centered at origin.

use std::sync::Arc;

use crate::geometry::generate_cylinder;
use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct CylinderNode;

impl NodeDef for CylinderNode {
    fn type_id(&self) -> &'static str { "Cylinder" }
    fn display_name(&self) -> &'static str { "Cylinder" }
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

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let r = props.number("radius", 10.0);
        let h = props.number("height", 20.0);
        let segments = props.number("segments", 32.0).round().clamp(3.0, 256.0) as u32;
        let mesh = generate_cylinder(r, h, segments);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(mesh)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CylinderNode);
}
