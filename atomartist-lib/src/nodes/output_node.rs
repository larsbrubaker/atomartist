//! Output — the terminal of a graph.
//!
//! Blender-style dynamic multi-input node: always shows a trailing empty
//! socket; connecting to it adopts the source's name and type, mints a
//! mirror output socket so downstream tools can re-tap, and appends a new
//! empty slot. Disconnect collapses the slot.
//!
//! Behavioral parity with NodeDesigner's `graph-output.js`:
//!   - Empty trailing input slot, always present.
//!   - On connect: rename slot to source-socket name, label as
//!     "{SourceTitle} - {source-socket-name}", retype to source's type,
//!     append matching output, ensure trailing empty exists.
//!   - On disconnect: remove the slot and its output mirror, ensure
//!     trailing empty exists.
//!   - Duplicate source rejected (`validate_input_connection`).
//!   - `evaluate` mirrors each connected input to its named output, and
//!     additionally merges every `Geometry3d` input into a synthetic
//!     `__display__` output so the 3D viewport's `pick_display_mesh`
//!     finds it via the "first Geometry3d cached output" heuristic.

use std::sync::Arc;

use crate::geometry::{merge_meshes, num_tris, num_verts};
use crate::graph::node::PortValue;
use crate::graph::socket::{Socket, SocketUidAlloc};
use crate::registry::{
    ConnectCtx, DisconnectCtx, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, ValidateCtx,
};
use crate::socket_types::SocketType;

/// Internal output-socket name carrying the merged display mesh. Never
/// shown in the UI; the viewport picks it up via `pick_display_mesh`'s
/// "first Geometry3d cached output" heuristic.
const DISPLAY_OUTPUT_NAME: &str = "__display__";

pub struct OutputNode;

impl NodeDef for OutputNode {
    fn type_id(&self) -> &'static str { "Output" }
    fn display_name(&self) -> &'static str { "Output" }
    fn category(&self) -> &'static str { "Output" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        // One empty placeholder input (type=None makes the canvas accept
        // any drop) plus the synthetic display output for the viewport.
        InstanceTemplate::builder(alloc)
            .input_opt("", SocketType::None)
            .output(DISPLAY_OUTPUT_NAME, SocketType::Geometry3d)
            .build()
    }

    /// Reject a connection from a source socket that already lands on
    /// this Output node — the JS code's duplicate-source rule. Without
    /// it the same source would create two slots, both registering as
    /// graph outputs, and the second would silently clobber the first.
    fn validate_input_connection(&self, ctx: &ValidateCtx) -> Result<(), String> {
        for noodle in ctx.graph.noodles() {
            if noodle.to.node == ctx.this_node
                && noodle.from.node == ctx.source_node
                && noodle.from.socket == ctx.source_socket
            {
                return Err("source already connected to this Output".into());
            }
        }
        Ok(())
    }

    fn on_input_connected(&self, ctx: &mut ConnectCtx) {
        // Read the source's name + type + node-title.
        let (source_name, source_type, source_title) = {
            let src = match ctx.graph.get(ctx.source_node) {
                Some(n) => n,
                None => return,
            };
            let src_socket = match src.output_by_uid(ctx.source_socket) {
                Some(s) => s,
                None => return,
            };
            let name = src_socket.name.to_string();
            let ty = src_socket.socket_type;
            let title = src.type_id.to_string();
            (name, ty, title)
        };

        // Compute a graph-unique internal name. Two sources may share
        // the same socket name (e.g. two Box nodes both output "out");
        // suffix `_1`, `_2`, … to disambiguate within this Output's
        // input list. The target socket's current name is `""` so it
        // doesn't collide with itself in this scan.
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

        // Configure the input slot.
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
        let _ = ctx.graph.retype_socket(ctx.this_node, ctx.target_socket, source_type);

        // Mirror as an output so downstream graphs (and subgraph
        // encapsulation) can read the value back out. Mint a fresh uid
        // — distinct from the input's uid by design.
        let mirror_uid = ctx.graph.allocate_socket_uid();
        let _ = ctx.graph.append_output_socket(
            ctx.this_node,
            Socket::new(mirror_uid, unique_name.as_str(), source_type, false),
        );

        // Maintain the trailing-empty invariant.
        ensure_trailing_empty_input(ctx.graph, ctx.this_node);
    }

    fn on_input_disconnected(&self, ctx: &mut DisconnectCtx) {
        // Find the slot's current name so we can remove the matching
        // output mirror, then drop the input slot itself.
        let slot_name = match ctx.graph.get(ctx.this_node) {
            Some(n) => n.input_by_uid(ctx.target_socket).map(|s| s.name.clone()),
            None => return,
        };

        // Skip if the slot is already empty (defensive — shouldn't
        // happen via normal disconnect flow, but the graph's hook fires
        // before any consistency checks here).
        if let Some(name) = slot_name {
            if name.as_ref() != "" {
                // Remove the matching output mirror by name.
                let mirror_uid = ctx
                    .graph
                    .get(ctx.this_node)
                    .and_then(|n| n.output_by_name(&name).map(|s| s.uid));
                if let Some(uid) = mirror_uid {
                    let _ = ctx.graph.remove_output_socket(ctx.this_node, uid);
                }
                // Remove the input slot.
                let _ = ctx.graph.remove_input_socket(ctx.this_node, ctx.target_socket);
            }
        }

        ensure_trailing_empty_input(ctx.graph, ctx.this_node);
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let mut out = NodeOutputs::default();

        // 1) Pass each configured input through to its same-named output.
        //    The empty placeholder slot has name "" — skip it.
        for slot in &ctx.instance.inputs {
            if slot.name.as_ref().is_empty() {
                continue;
            }
            let value = ctx.input(slot.uid).clone();
            out.set(slot.name.clone(), value);
        }

        // 2) Merge every connected Geometry3d input into __display__ so
        //    the viewport renders the union. Each upstream geometry
        //    carries its own matrix + colour; the merge baked the
        //    matrices via `apply_transform` so the merged result is
        //    world-space and uses the first input's colour as the
        //    representative tint.
        let mut meshes = Vec::new();
        let mut first_geom: Option<Arc<crate::geometry::Geometry3d>> = None;
        for slot in &ctx.instance.inputs {
            if slot.name.as_ref().is_empty() {
                continue;
            }
            if let PortValue::Geometry3d(g) = ctx.input(slot.uid) {
                if num_verts(&g.mesh) > 0 && num_tris(&g.mesh) > 0 {
                    if first_geom.is_none() {
                        first_geom = Some(g.clone());
                    }
                    if g.matrix == crate::graph::node::identity_matrix() {
                        meshes.push(g.mesh.clone());
                    } else {
                        let baked = crate::geometry::apply_transform(&g.mesh, &g.matrix);
                        meshes.push(Arc::new(baked));
                    }
                }
            }
        }
        let merged = merge_meshes(&meshes);
        let merged_geom = match first_geom {
            Some(g) => crate::geometry::Geometry3d {
                mesh: Arc::new(merged),
                matrix: crate::graph::node::identity_matrix(),
                color: g.color,
            },
            None => crate::geometry::Geometry3d::from_mesh(Arc::new(merged)),
        };
        out.set(
            DISPLAY_OUTPUT_NAME,
            PortValue::Geometry3d(Arc::new(merged_geom)),
        );

        Ok(out)
    }
}

/// Append a trailing empty input socket if the node doesn't already end
/// in one. The Output node uses an empty (name=="", type=None) slot to
/// invite the next drop.
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
            Socket::new(uid, "", SocketType::None, true),
        );
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(OutputNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;
    use crate::graph::graph::{Graph, GraphError, Noodle};
    use crate::graph::executor::evaluate_all;
    use crate::nodes;

    fn registry() -> NodeRegistry {
        let mut r = NodeRegistry::new();
        nodes::register_all(&mut r);
        r
    }

    /// Helper: build (Box, Output) pair and wire Box.out → Output's
    /// first input slot. Returns ids + the target uid that got
    /// configured.
    fn wire_box_to_output(
        g: &mut Graph,
        reg: &NodeRegistry,
    ) -> (
        crate::graph::node::NodeId,
        crate::graph::node::NodeId,
    ) {
        let bx = g.add_new_node("Box", [0.0, 0.0], reg).unwrap();
        let out = g.add_new_node("Output", [200.0, 0.0], reg).unwrap();
        let out_box = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        // The Output's first (only) input is the trailing empty placeholder.
        let in_target = g.get(out).unwrap().inputs[0].uid;
        g.connect(Noodle::new(bx, out_box, out, in_target), reg).unwrap();
        (bx, out)
    }

    #[test]
    fn fresh_output_node_has_empty_trailing_slot_and_display_output() {
        let reg = registry();
        let mut g = Graph::new();
        let out = g.add_new_node("Output", [0.0, 0.0], &reg).unwrap();
        let n = g.get(out).unwrap();
        assert_eq!(n.inputs.len(), 1, "starts with one empty input");
        assert_eq!(n.inputs[0].name.as_ref(), "");
        assert_eq!(n.inputs[0].socket_type, SocketType::None);
        assert!(n.inputs[0].optional);
        assert_eq!(n.outputs.len(), 1, "carries the synthetic display output");
        assert_eq!(n.outputs[0].name.as_ref(), DISPLAY_OUTPUT_NAME);
    }

    #[test]
    fn connecting_a_box_configures_the_slot_and_appends_an_empty_one() {
        let reg = registry();
        let mut g = Graph::new();
        let (_bx, out) = wire_box_to_output(&mut g, &reg);
        let n = g.get(out).unwrap();
        // Two input slots now: the configured one, and a fresh trailing empty.
        assert_eq!(n.inputs.len(), 2);
        assert_eq!(n.inputs[0].name.as_ref(), "out", "adopts source name");
        assert_eq!(n.inputs[0].socket_type, SocketType::Geometry3d);
        assert_eq!(
            n.inputs[0].display_label.as_ref().map(|s| s.as_ref()),
            Some("Box - out"),
        );
        assert_eq!(n.inputs[1].name.as_ref(), "", "trailing empty re-appended");
        // Mirror output appeared.
        assert!(
            n.outputs.iter().any(|s| s.name.as_ref() == "out"),
            "should mirror an 'out' output socket",
        );
    }

    #[test]
    fn second_source_with_same_socket_name_gets_suffixed() {
        let reg = registry();
        let mut g = Graph::new();
        let (_bx1, out) = wire_box_to_output(&mut g, &reg);
        // Connect a second Box to the (now-trailing-empty) slot.
        let bx2 = g.add_new_node("Box", [0.0, 100.0], &reg).unwrap();
        let bx2_out = g.get(bx2).unwrap().output_by_name("out").unwrap().uid;
        let trailing = g.get(out).unwrap().inputs.last().unwrap().uid;
        g.connect(Noodle::new(bx2, bx2_out, out, trailing), &reg).unwrap();

        let n = g.get(out).unwrap();
        // Two configured slots + one trailing empty.
        assert_eq!(n.inputs.len(), 3);
        assert_eq!(n.inputs[0].name.as_ref(), "out");
        assert_eq!(n.inputs[1].name.as_ref(), "out_1", "second slot is suffixed");
        assert_eq!(n.inputs[2].name.as_ref(), "");
        // Both mirror outputs exist with distinct names.
        assert!(n.outputs.iter().any(|s| s.name.as_ref() == "out"));
        assert!(n.outputs.iter().any(|s| s.name.as_ref() == "out_1"));
    }

    #[test]
    fn duplicate_source_socket_is_rejected() {
        let reg = registry();
        let mut g = Graph::new();
        let (bx, out) = wire_box_to_output(&mut g, &reg);
        // Try to wire the same Box.out into the trailing empty — should fail.
        let bx_out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let trailing = g.get(out).unwrap().inputs.last().unwrap().uid;
        let result = g.connect(Noodle::new(bx, bx_out, out, trailing), &reg);
        assert!(matches!(result, Err(GraphError::ConnectionRejected(_))));
    }

    #[test]
    fn disconnect_collapses_slot_and_preserves_trailing_empty() {
        let reg = registry();
        let mut g = Graph::new();
        let (bx, out) = wire_box_to_output(&mut g, &reg);
        let bx_out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let in_target = g.get(out).unwrap().input_by_name("out").unwrap().uid;
        let noodle = Noodle::new(bx, bx_out, out, in_target);
        g.disconnect(&noodle, &reg).unwrap();
        let n = g.get(out).unwrap();
        assert_eq!(n.inputs.len(), 1, "back to one empty slot");
        assert_eq!(n.inputs[0].name.as_ref(), "");
        // Mirror output should be gone (only display remains).
        assert!(
            !n.outputs.iter().any(|s| s.name.as_ref() == "out"),
            "mirror output should be removed",
        );
        assert!(
            n.outputs.iter().any(|s| s.name.as_ref() == DISPLAY_OUTPUT_NAME),
            "display output should remain",
        );
    }

    #[test]
    fn evaluate_merges_two_boxes_into_display_output() {
        let reg = registry();
        let mut g = Graph::new();
        let bx1 = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let bx2 = g.add_new_node("Box", [100.0, 0.0], &reg).unwrap();
        let out = g.add_new_node("Output", [200.0, 0.0], &reg).unwrap();
        let bx1_out = g.get(bx1).unwrap().output_by_name("out").unwrap().uid;
        let bx2_out = g.get(bx2).unwrap().output_by_name("out").unwrap().uid;
        // First wire — lands on the initial empty slot.
        let s1 = g.get(out).unwrap().inputs[0].uid;
        g.connect(Noodle::new(bx1, bx1_out, out, s1), &reg).unwrap();
        // Second wire — lands on the newly-appended trailing empty.
        let s2 = g.get(out).unwrap().inputs.last().unwrap().uid;
        g.connect(Noodle::new(bx2, bx2_out, out, s2), &reg).unwrap();

        evaluate_all(&mut g, &reg).unwrap();
        let n = g.get(out).unwrap();
        let display_uid = n.output_by_name(DISPLAY_OUTPUT_NAME).unwrap().uid;
        match n.cached_outputs.get(&display_uid) {
            Some(PortValue::Geometry3d(g)) => {
                // Two default boxes: 24 verts each, 12 tris each.
                assert_eq!(num_verts(&g.mesh), 48);
                assert_eq!(num_tris(&g.mesh), 24);
            }
            other => panic!("expected merged Geometry3d, got {:?}", other),
        }
    }

    #[test]
    fn evaluate_passes_each_named_input_through_to_matching_output() {
        let reg = registry();
        let mut g = Graph::new();
        let bx = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
        let out = g.add_new_node("Output", [200.0, 0.0], &reg).unwrap();
        let bx_out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
        let in_target = g.get(out).unwrap().inputs[0].uid;
        g.connect(Noodle::new(bx, bx_out, out, in_target), &reg).unwrap();

        evaluate_all(&mut g, &reg).unwrap();
        let n = g.get(out).unwrap();
        // Mirror output named "out" should carry the same Geometry3d the
        // Box emitted (pointer-equal via the Arc clone path).
        let mirror_uid = n.output_by_name("out").unwrap().uid;
        let bx_cached = g
            .get(bx)
            .unwrap()
            .cached_outputs
            .get(&bx_out)
            .cloned()
            .unwrap();
        let out_cached = g
            .get(out)
            .unwrap()
            .cached_outputs
            .get(&mirror_uid)
            .cloned()
            .unwrap();
        assert_eq!(
            bx_cached, out_cached,
            "mirror output should carry the same Arc as the source",
        );
        let _ = generate_box(1.0, 1.0, 1.0); // silence unused-import in fixtures
    }
}
