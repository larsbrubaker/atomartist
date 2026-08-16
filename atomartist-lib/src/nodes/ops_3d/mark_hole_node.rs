//! Mark Hole — stamps a solid/hole role onto every body that passes
//! through it, so a downstream Boolean **Combine** can subtract it.
//!
//! ## Why this node exists
//!
//! MatterCAD has no node like this: `OutputType` is a property of *every*
//! scene item (`PrintOutputTypes.Solid` / `.Hole`), set on the object
//! itself, and `BooleanMeshBuilder.CombineMeshes` (L104-192) reads it off
//! each child. Our equivalent of "every object" would be a Hole toggle on
//! every geometry-producing node, which is a lot of schema for a rarely
//! used flag and would still miss any node added later. One tiny
//! pass-through node marks a whole branch instead, composes with the rest
//! of the graph the way every other op does, and leaves the primitives
//! alone. Choosing between an enum on every producer and one node here is
//! the only design decision in plan step B-4; this is the smaller of the
//! two.
//!
//! ## What it does and does not touch
//!
//! Geometry is passed through untouched — same mesh, matrix, colour,
//! per-vertex colours — and, unlike the transform ops, the upstream
//! **origin claim** is preserved too: clicking the rendered part still
//! selects the node that made it, because this node changed nothing about
//! how the part looks. Only [`Body::role`] is rewritten.
//!
//! The role is runtime state that travels down the wire; it is not written
//! to the project file. What persists is this node and its `output_type`
//! property, which re-stamps the role on every evaluation.

use std::sync::Arc;

use crate::geometry::{Body, BodyRole, Geometry3d};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    enum_variant_for_index, EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, ParamSet, PropDef,
};
use crate::socket_types::SocketType;

pub struct MarkHoleNode;

/// Property key holding the role, serialized by name.
pub const OUTPUT_TYPE: &str = "output_type";

/// The two roles a body can carry, spelled as MatterCAD spells them.
/// `Support` is deliberately absent: nothing in AtomArtist consumes it.
pub const OUTPUT_TYPES: [&str; 2] = ["Hole", "Solid"];

/// `Hole` is the default — the node is called Mark Hole, and a user who
/// wires one up has already said what they want. `Solid` is there so the
/// marking can be switched off without rewiring.
fn params() -> ParamSet {
    ParamSet::new()
        .enum_(OUTPUT_TYPE, "Output Type", OUTPUT_TYPES[0], &OUTPUT_TYPES)
        .editor(EditorKind::EnumButtons {
            variants: OUTPUT_TYPES.iter().map(|v| Arc::from(*v)).collect(),
        })
        .description(
            "Hole marks the parts as negative space: a Boolean set to Combine unions \
             the holes separately and cuts them out of the solids.",
        )
}

/// Resolve the stored value, falling back to `Hole` for anything the
/// build does not recognise.
///
/// The numeric shape is accepted the same way [`super::boolean_node::operation_of`]
/// accepts it — through [`enum_variant_for_index`], the generic index →
/// variant migration. No released build ever wrote a number here, but the
/// subsystem's two enum readers behaving differently is the kind of
/// difference that only shows up in a file nobody can open.
pub fn role_of(props: &crate::registry::NodeProperties) -> BodyRole {
    let name = match props.get(OUTPUT_TYPE) {
        PortValue::StringVal(s) => s.as_str().to_string(),
        PortValue::Number(n) => {
            let variants: Vec<Arc<str>> = OUTPUT_TYPES.iter().map(|v| Arc::from(*v)).collect();
            match enum_variant_for_index(&variants, *n) {
                Some(v) => v.to_string(),
                None => return BodyRole::Hole,
            }
        }
        _ => return BodyRole::Hole,
    };
    match name.as_str() {
        "Solid" => BodyRole::Solid,
        _ => BodyRole::Hole,
    }
}

impl NodeDef for MarkHoleNode {
    fn type_id(&self) -> &'static str { "Mark Hole" }
    fn display_name(&self) -> &'static str { "Mark Hole" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        params()
            .mint_sockets(InstanceTemplate::builder(alloc).input("input", SocketType::Geometry3d))
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        params().prop_defs()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let input = match ctx.input_named("input") {
            PortValue::Geometry3d(g) => g.clone(),
            PortValue::None => return Ok(NodeOutputs::default()),
            other => {
                return Err(NodeError::msg(format!(
                    "Mark Hole: expected Geometry3d, got {:?}",
                    other.socket_type()
                )))
            }
        };
        let role = role_of(ctx.properties);
        let bodies: Vec<Body> = input.iter().map(|b| b.clone().with_role(role)).collect();
        let mut out = NodeOutputs::default();
        out.set(
            "out",
            PortValue::Geometry3d(Arc::new(Geometry3d::from_bodies(bodies))),
        );
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(MarkHoleNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::executor::evaluate_all;
    use crate::graph::graph::{Graph, Noodle};
    use crate::nodes;
    use crate::registry::NodeProperties;

    fn registry() -> NodeRegistry {
        let mut r = NodeRegistry::new();
        nodes::register_all(&mut r);
        r
    }

    #[test]
    fn mark_hole_defaults_to_hole_and_leaves_the_mesh_alone() {
        let reg = registry();
        let mut g = Graph::new();
        let bx = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let mh = g.add_new_node("Mark Hole", [200.0, 0.0], &reg).unwrap();
        let bx_out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let inp = g.get(mh).unwrap().input_by_name("input").unwrap().uid;
        g.connect(Noodle::new(bx, bx_out, mh, inp), &reg).unwrap();
        evaluate_all(&mut g, &reg).unwrap().expect_clean();

        let uid = g.get(mh).unwrap().output_by_name("out").unwrap().uid;
        match g.get(mh).unwrap().cached_outputs.get(&uid) {
            Some(PortValue::Geometry3d(geo)) => {
                let body = geo.first().expect("one body through");
                assert_eq!(body.role, BodyRole::Hole);
                assert_eq!(
                    body.origin,
                    Some(bx),
                    "Mark Hole changes no geometry, so the Box keeps the click claim"
                );
                assert!(crate::geometry::num_tris(&body.mesh) > 0);
            }
            other => panic!("expected Geometry3d, got {:?}", other),
        }
    }

    #[test]
    fn output_type_solid_clears_the_hole_role() {
        let mut props = NodeProperties::default();
        assert_eq!(role_of(&props), BodyRole::Hole, "default is Hole");
        props.insert(OUTPUT_TYPE, PortValue::StringVal(Arc::new("Solid".into())));
        assert_eq!(role_of(&props), BodyRole::Solid);
        props.insert(
            OUTPUT_TYPE,
            PortValue::StringVal(Arc::new("nonsense".into())),
        );
        assert_eq!(
            role_of(&props),
            BodyRole::Hole,
            "an unknown stored value falls back to the schema default"
        );
    }
}
