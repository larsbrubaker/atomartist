//! 2D Star node — N-pointed star with alternating outer / inner radii.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;
use manifold_rust::linalg::Vec2;

use crate::graph::node::PortValue;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

pub struct StarNode;

impl NodeDef for StarNode {
    fn type_id(&self) -> &'static str { "Star" }
    fn display_name(&self) -> &'static str { "Star" }
    fn category(&self) -> &'static str { "Primitives 2D" }

    fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
    fn output_sockets(&self) -> Vec<SocketDef> {
        vec![SocketDef::required("out", SocketType::Path2d)]
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("points", PortValue::Number(5.0)).with_range(3.0, 64.0),
            PropDef::new("outer_radius", PortValue::Number(10.0)).with_range(0.001, 10_000.0),
            PropDef::new("inner_radius", PortValue::Number(4.0)).with_range(0.001, 10_000.0),
        ]
    }

    fn evaluate(&self, _inputs: &NodeInputs, props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        let n = props.number("points", 5.0).round().clamp(3.0, 64.0) as usize;
        let r_out = props.number("outer_radius", 10.0);
        let r_in = props.number("inner_radius", 4.0).min(r_out);
        let total = n * 2;
        let mut contour = Vec::with_capacity(total);
        for i in 0..total {
            let angle = (i as f64) * std::f64::consts::TAU / (total as f64) - std::f64::consts::FRAC_PI_2;
            let r = if i % 2 == 0 { r_out } else { r_in };
            contour.push(Vec2::new(r * angle.cos(), r * angle.sin()));
        }
        let cs = CrossSection::from_polygons_fill(vec![contour]);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Path2d(Arc::new(cs)));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(StarNode);
}
