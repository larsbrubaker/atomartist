//! Combine node — merges multiple geometries into one mesh.
//!
//! Dynamic-input model (second consumer after [`super::super::output_node`]):
//! the node starts with one empty trailing input slot, typed
//! [`SocketType::Geometry3d`]. On connect the slot adopts the source's
//! socket name + display label, and a fresh trailing empty is appended.
//! Disconnect collapses the slot. The single static output `"out"`
//! carries the straight concatenation of every connected input mesh.
//!
//! For Boolean union with surface healing, use the dedicated `Boolean`
//! node.
//!
//! Where the Output node validates the design's "any-type adoption"
//! path, Combine validates the "constrained-type adoption" path: the
//! placeholder slot is pre-typed [`SocketType::Geometry3d`] so the
//! canvas refuses any non-geometry source up front — no `validate_input_connection`
//! veto needed.

use std::sync::Arc;

use crate::geometry::{merge_meshes, num_tris, num_verts};
use crate::graph::node::PortValue;
use crate::graph::socket::{Socket, SocketUidAlloc};
use crate::registry::{
    geometry_props, wrap_mesh, ConnectCtx, DisconnectCtx, EvalCtx, InstanceTemplate, NodeDef,
    NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

pub struct CombineNode;

impl NodeDef for CombineNode {
    fn type_id(&self) -> &'static str { "Combine" }
    fn display_name(&self) -> &'static str { "Combine" }
    fn category(&self) -> &'static str { "Operations 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        // One typed-empty placeholder input; one static output.
        InstanceTemplate::builder(alloc)
            .input_opt("", SocketType::Geometry3d)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn on_input_connected(&self, ctx: &mut ConnectCtx) {
        // Adopt the source-socket name as the slot's label so the user
        // can see what feeds each input. Combine never publishes mirror
        // outputs — its merged result lives entirely on the static
        // "out" socket.
        let (source_name, source_title) = {
            let src = match ctx.graph.get(ctx.source_node) {
                Some(n) => n,
                None => return,
            };
            let name = src
                .output_by_uid(ctx.source_socket)
                .map(|s| s.name.to_string())
                .unwrap_or_default();
            let title = src.type_id.to_string();
            (name, title)
        };

        // Unique internal name within Combine's input list. Two Box
        // sources both emit "out" → suffix `_1`, `_2`, …
        let unique_name = {
            let me = match ctx.graph.get(ctx.this_node) {
                Some(n) => n,
                None => return,
            };
            let mut candidate = source_name.clone();
            let mut suffix = 1;
            while me
                .inputs
                .iter()
                .any(|s| s.uid != ctx.target_socket && s.name.as_ref() == candidate.as_str())
            {
                candidate = format!("{}_{}", source_name, suffix);
                suffix += 1;
            }
            candidate
        };

        let display_label = format!("{} - {}", source_title, source_name);
        let _ = ctx.graph.rename_socket(
            ctx.this_node,
            ctx.target_socket,
            Arc::<str>::from(unique_name.as_str()),
        );
        let _ = ctx.graph.relabel_socket(
            ctx.this_node,
            ctx.target_socket,
            Some(Arc::<str>::from(display_label.as_str())),
        );
        // Type stays Geometry3d — no retype needed.

        ensure_trailing_empty_input(ctx.graph, ctx.this_node);
    }

    fn on_input_disconnected(&self, ctx: &mut DisconnectCtx) {
        // Drop the slot unless it's already empty (defensive).
        let was_configured = match ctx.graph.get(ctx.this_node) {
            Some(n) => n
                .input_by_uid(ctx.target_socket)
                .map(|s| !s.name.as_ref().is_empty())
                .unwrap_or(false),
            None => return,
        };
        if was_configured {
            let _ = ctx.graph.remove_input_socket(ctx.this_node, ctx.target_socket);
        }
        ensure_trailing_empty_input(ctx.graph, ctx.this_node);
    }

    fn properties(&self) -> Vec<PropDef> {
        geometry_props()
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        // Iterate the instance's input slots, skipping the trailing
        // empty placeholder. Concatenate every non-empty Geometry3d
        // value into the merged mesh.
        let mut parts = Vec::new();
        for slot in &ctx.instance.inputs {
            if slot.name.as_ref().is_empty() {
                continue;
            }
            if let PortValue::Geometry3d(g) = ctx.input(slot.uid) {
                if num_verts(&g.mesh) > 0 && num_tris(&g.mesh) > 0 {
                    parts.push(g.mesh.clone());
                }
            }
        }
        let merged = merge_meshes(&parts);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, merged))));
        Ok(out)
    }
}

/// Append a trailing empty Geometry3d input if the node doesn't already
/// end in one. Combine's empty slot is typed `Geometry3d` (not `None`)
/// so the canvas's compatibility check refuses non-geometry drops.
fn ensure_trailing_empty_input(
    graph: &mut crate::graph::graph::Graph,
    node: crate::graph::node::NodeId,
) {
    let needs_one = match graph.get(node) {
        Some(n) => n
            .inputs
            .last()
            .map(|s| !s.name.as_ref().is_empty())
            .unwrap_or(true),
        None => return,
    };
    if needs_one {
        let uid = graph.allocate_socket_uid();
        let _ = graph.append_input_socket(
            node,
            Socket::new(uid, "", SocketType::Geometry3d, true),
        );
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CombineNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;
    use crate::graph::executor::evaluate_all;
    use crate::graph::graph::{Graph, Noodle};
    use crate::nodes;

    fn registry() -> NodeRegistry {
        let mut r = NodeRegistry::new();
        nodes::register_all(&mut r);
        r
    }

    #[test]
    fn fresh_combine_has_one_typed_empty_slot_and_one_output() {
        let reg = registry();
        let mut g = Graph::new();
        let c = g.add_new_node("Combine", [0.0, 0.0], &reg).unwrap();
        let n = g.get(c).unwrap();
        assert_eq!(n.inputs.len(), 1);
        assert_eq!(n.inputs[0].name.as_ref(), "");
        assert_eq!(
            n.inputs[0].socket_type,
            SocketType::Geometry3d,
            "placeholder slot is typed Geometry3d so the canvas rejects non-geometry drops",
        );
        assert!(n.inputs[0].optional);
        assert_eq!(n.outputs.len(), 1);
        assert_eq!(n.outputs[0].name.as_ref(), "out");
    }

    #[test]
    fn connecting_a_box_configures_the_slot_and_appends_an_empty_one() {
        let reg = registry();
        let mut g = Graph::new();
        let bx = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let c = g.add_new_node("Combine", [200.0, 0.0], &reg).unwrap();
        let bx_out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let empty = g.get(c).unwrap().inputs[0].uid;
        g.connect(Noodle::new(bx, bx_out, c, empty), &reg).unwrap();

        let n = g.get(c).unwrap();
        assert_eq!(n.inputs.len(), 2);
        assert_eq!(n.inputs[0].name.as_ref(), "out");
        assert_eq!(
            n.inputs[0].display_label.as_ref().map(|s| s.as_ref()),
            Some("Box - out"),
        );
        assert_eq!(n.inputs[1].name.as_ref(), "");
        assert_eq!(n.outputs.len(), 1, "no mirror outputs — Combine has a single static output");
    }

    #[test]
    fn two_boxes_with_same_source_name_get_suffixed() {
        let reg = registry();
        let mut g = Graph::new();
        let bx1 = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let bx2 = g.add_new_node("Box", [0.0, 100.0], &reg).unwrap();
        let c = g.add_new_node("Combine", [200.0, 0.0], &reg).unwrap();

        let bx1_out = g.get(bx1).unwrap().output_by_name("out").unwrap().uid;
        let bx2_out = g.get(bx2).unwrap().output_by_name("out").unwrap().uid;
        let e1 = g.get(c).unwrap().inputs[0].uid;
        g.connect(Noodle::new(bx1, bx1_out, c, e1), &reg).unwrap();
        let e2 = g.get(c).unwrap().inputs.last().unwrap().uid;
        g.connect(Noodle::new(bx2, bx2_out, c, e2), &reg).unwrap();

        let n = g.get(c).unwrap();
        assert_eq!(n.inputs.len(), 3);
        assert_eq!(n.inputs[0].name.as_ref(), "out");
        assert_eq!(n.inputs[1].name.as_ref(), "out_1");
        assert_eq!(n.inputs[2].name.as_ref(), "");
    }

    #[test]
    fn disconnect_collapses_slot_and_preserves_trailing_empty() {
        let reg = registry();
        let mut g = Graph::new();
        let bx = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let c = g.add_new_node("Combine", [200.0, 0.0], &reg).unwrap();
        let bx_out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let e1 = g.get(c).unwrap().inputs[0].uid;
        let noodle = Noodle::new(bx, bx_out, c, e1);
        g.connect(noodle, &reg).unwrap();
        // Slot adopted name "out"; find it via name lookup since the
        // uid is preserved across adoption.
        let configured = g.get(c).unwrap().input_by_name("out").unwrap().uid;
        let noodle = Noodle::new(bx, bx_out, c, configured);
        g.disconnect(&noodle, &reg).unwrap();

        let n = g.get(c).unwrap();
        assert_eq!(n.inputs.len(), 1);
        assert_eq!(n.inputs[0].name.as_ref(), "");
    }

    #[test]
    fn evaluate_merges_two_connected_boxes() {
        let reg = registry();
        let mut g = Graph::new();
        let bx1 = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let bx2 = g.add_new_node("Box", [100.0, 0.0], &reg).unwrap();
        g.set_property(bx1, "width", PortValue::Number(1.0)).unwrap();
        g.set_property(bx1, "height", PortValue::Number(1.0)).unwrap();
        g.set_property(bx1, "depth", PortValue::Number(1.0)).unwrap();
        g.set_property(bx2, "width", PortValue::Number(1.0)).unwrap();
        g.set_property(bx2, "height", PortValue::Number(1.0)).unwrap();
        g.set_property(bx2, "depth", PortValue::Number(1.0)).unwrap();
        let c = g.add_new_node("Combine", [200.0, 0.0], &reg).unwrap();
        let bx1_out = g.get(bx1).unwrap().output_by_name("out").unwrap().uid;
        let bx2_out = g.get(bx2).unwrap().output_by_name("out").unwrap().uid;
        let e1 = g.get(c).unwrap().inputs[0].uid;
        g.connect(Noodle::new(bx1, bx1_out, c, e1), &reg).unwrap();
        let e2 = g.get(c).unwrap().inputs.last().unwrap().uid;
        g.connect(Noodle::new(bx2, bx2_out, c, e2), &reg).unwrap();

        evaluate_all(&mut g, &reg).unwrap();
        let out_uid = g.get(c).unwrap().output_by_name("out").unwrap().uid;
        match g.get(c).unwrap().cached_outputs.get(&out_uid) {
            Some(PortValue::Geometry3d(geo)) => {
                assert_eq!(num_verts(&geo.mesh), 48, "two unit boxes → 24+24 vertices");
                assert_eq!(num_tris(&geo.mesh), 24, "two unit boxes → 12+12 triangles");
            }
            other => panic!("expected merged Geometry3d, got {:?}", other),
        }
        let _ = generate_box(1.0, 1.0, 1.0); // silence unused-import lint
    }

    #[test]
    fn evaluate_skips_disconnected_slots() {
        // Just the placeholder slot → empty merged mesh.
        let reg = registry();
        let mut g = Graph::new();
        let c = g.add_new_node("Combine", [0.0, 0.0], &reg).unwrap();
        evaluate_all(&mut g, &reg).unwrap();
        let out_uid = g.get(c).unwrap().output_by_name("out").unwrap().uid;
        match g.get(c).unwrap().cached_outputs.get(&out_uid) {
            Some(PortValue::Geometry3d(geo)) => {
                assert_eq!(num_verts(&geo.mesh), 0);
                assert_eq!(num_tris(&geo.mesh), 0);
            }
            other => panic!("expected empty Geometry3d, got {:?}", other),
        }
    }
}
