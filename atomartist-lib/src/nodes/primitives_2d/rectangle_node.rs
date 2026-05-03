//! 2D Rectangle node — outputs a `CrossSection` quad in XY.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;
use manifold_rust::linalg::Vec2;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct RectangleNode;

impl NodeDef for RectangleNode {
    fn type_id(&self) -> &'static str { "Rectangle" }
    fn display_name(&self) -> &'static str { "Rectangle" }
    fn category(&self) -> &'static str { "Primitives 2D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Path2d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("width", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
        ]
    }

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let w = props.number("width", 20.0);
        let h = props.number("height", 20.0);
        let half_w = w * 0.5;
        let half_h = h * 0.5;
        // CCW from outside (looking down -Z toward XY plane).
        let contour = vec![
            Vec2::new(-half_w, -half_h),
            Vec2::new( half_w, -half_h),
            Vec2::new( half_w,  half_h),
            Vec2::new(-half_w,  half_h),
        ];
        let cs = CrossSection::from_polygons_fill(vec![contour]);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(RectangleNode);
}
