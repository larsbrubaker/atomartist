//! 2D Rectangle node — outputs a `CrossSection` quad in XY.

use std::sync::Arc;

use manifold_rust::cross_section::CrossSection;
use manifold_rust::linalg::Vec2;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct RectangleNode;

impl NodeDef for RectangleNode {
    fn type_id(&self) -> &'static str { "Rectangle" }
    fn display_name(&self) -> &'static str { "Rectangle" }
    fn category(&self) -> &'static str { "Primitives 2D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Path2d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            PropDef::new("width", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
            PropDef::new("height", PortValue::Number(20.0)).with_range(0.001, 10_000.0),
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let w = ctx.properties.number("width", 20.0);
        let h = ctx.properties.number("height", 20.0);
        let half_w = w * 0.5;
        let half_h = h * 0.5;
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
