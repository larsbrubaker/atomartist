//! Merge one graph into another — the engine behind "File → Import
//! a project into the current scene".
//!
//! The source graph's node ids and socket uids are only unique within
//! that graph, so every node is re-identified against the destination's
//! allocators before insertion; noodles are remapped through the same
//! tables. The source's `Output` node is deliberately **not** copied —
//! a scene has exactly one Output — and the connections that fed it are
//! returned as [`MergeResult::output_feeders`] so the caller can rewire
//! them into the destination's Output (see `AppState::import_project_file`
//! in `atomartist-ui`).

use std::collections::HashMap;

use crate::registry::NodeRegistry;

use super::graph::{Graph, Noodle};
use super::node::NodeId;
use super::socket::SocketUid;

/// What [`merge_graph`] did, in destination-graph ids.
pub struct MergeResult {
    /// Every node added, in source-iteration order.
    pub added_nodes: Vec<NodeId>,
    /// Output sockets (node + socket uid, remapped) that were wired
    /// into the source graph's Output node. The caller typically
    /// reconnects these to the destination's Output so the imported
    /// scene renders.
    pub output_feeders: Vec<(NodeId, SocketUid)>,
    /// Human-readable notes about anything skipped (rejected
    /// connections, dropped Output nodes).
    pub warnings: Vec<String>,
}

/// Move every node of `src` (except its `Output`) into `dst`, shifting
/// canvas positions by `offset` so the import lands beside — not on top
/// of — the existing nodes. Connections are re-established through
/// [`Graph::connect`] so dynamic-input hooks (Output-style adopting
/// sockets) behave exactly as if the user had wired them by hand.
pub fn merge_graph(
    dst: &mut Graph,
    src: Graph,
    registry: &NodeRegistry,
    offset: [f64; 2],
) -> MergeResult {
    let mut result = MergeResult {
        added_nodes: Vec::new(),
        output_feeders: Vec::new(),
        warnings: Vec::new(),
    };

    // Source Output nodes are dropped: the destination already has (at
    // most) one, and two would fight over the viewport.
    let excluded: Vec<NodeId> = src
        .nodes()
        .filter(|n| n.type_id.as_ref() == "Output")
        .map(|n| n.id)
        .collect();
    if !excluded.is_empty() {
        result
            .warnings
            .push("imported project's Output node was merged into this scene's Output".into());
    }

    let mut node_map: HashMap<NodeId, NodeId> = HashMap::new();
    let mut socket_map: HashMap<SocketUid, SocketUid> = HashMap::new();

    // Snapshot connections up front: `remove_node` below detaches every
    // noodle touching the node it removes, so reading `src.noodles()`
    // after pass 1 would see an empty list.
    let src_noodles: Vec<Noodle> = src.noodles().to_vec();

    // Pass 1: re-identify and insert every kept node.
    let src_ids: Vec<NodeId> = src.nodes().map(|n| n.id).collect();
    let mut src = src;
    for old_id in src_ids {
        if excluded.contains(&old_id) {
            continue;
        }
        let Ok((mut node, _)) = src.remove_node(old_id) else {
            continue;
        };
        let new_id = dst.allocate_id();
        node.id = new_id;
        node.position[0] += offset[0];
        node.position[1] += offset[1];
        for s in node.inputs.iter_mut().chain(node.outputs.iter_mut()) {
            let new_uid = dst.socket_alloc().allocate();
            socket_map.insert(s.uid, new_uid);
            s.uid = new_uid;
        }
        node.cached_outputs = node
            .cached_outputs
            .into_iter()
            .filter_map(|(uid, v)| socket_map.get(&uid).map(|nu| (*nu, v)))
            .collect();
        node_map.insert(old_id, new_id);
        if let Err(e) = dst.add_node(node) {
            result
                .warnings
                .push(format!("import: node {:?} skipped: {:?}", old_id, e));
            continue;
        }
        result.added_nodes.push(new_id);
    }

    // Pass 2: re-establish connections through the public connect path.
    for noodle in src_noodles {
        let feeds_output = excluded.contains(&noodle.to.node);
        let (Some(&from_node), Some(&from_socket)) = (
            node_map.get(&noodle.from.node),
            socket_map.get(&noodle.from.socket),
        ) else {
            continue;
        };
        if feeds_output {
            result.output_feeders.push((from_node, from_socket));
            continue;
        }
        let (Some(&to_node), Some(&to_socket)) = (
            node_map.get(&noodle.to.node),
            socket_map.get(&noodle.to.socket),
        ) else {
            continue;
        };
        if let Err(e) = dst.connect(
            Noodle::new(from_node, from_socket, to_node, to_socket),
            registry,
        ) {
            result
                .warnings
                .push(format!("import: connection skipped: {:?}", e));
        }
    }

    // Everything just added must re-evaluate in its new home.
    for id in &result.added_nodes {
        dst.mark_dirty_subtree(*id);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::graph::Noodle;
    use crate::nodes;
    use crate::registry::NodeRegistry;

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        nodes::register_all(&mut reg);
        reg
    }

    /// Box → Output, the smallest complete scene.
    fn small_scene(reg: &NodeRegistry) -> Graph {
        let mut g = Graph::new();
        let b = g.add_new_node("Box", [10.0, 10.0], reg).unwrap();
        let o = g.add_new_node("Output", [200.0, 10.0], reg).unwrap();
        let from = g.get(b).unwrap().outputs[0].uid;
        let to = g.get(o).unwrap().inputs[0].uid;
        g.connect(Noodle::new(b, from, o, to), reg).unwrap();
        g
    }

    #[test]
    fn merge_remaps_ids_and_reports_output_feeders() {
        let reg = registry();
        let mut dst = small_scene(&reg);
        let dst_nodes_before = dst.node_count();
        let src = small_scene(&reg);

        let result = merge_graph(&mut dst, src, &reg, [50.0, 50.0]);

        // Output excluded → exactly one node (the Box) added.
        assert_eq!(result.added_nodes.len(), 1);
        assert_eq!(dst.node_count(), dst_nodes_before + 1);
        // Its feeder connection is reported for rewiring.
        assert_eq!(result.output_feeders.len(), 1);
        // Position shifted by the offset.
        let new_box = dst.get(result.added_nodes[0]).unwrap();
        assert_eq!(new_box.position, [60.0, 60.0]);
        // No id/uid collisions: every node id unique.
        let mut ids: Vec<u64> = dst.nodes().map(|n| n.id.0).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), dst.node_count());
        // Socket uids unique across the whole graph.
        let mut uids: Vec<u64> = dst
            .nodes()
            .flat_map(|n| n.inputs.iter().chain(n.outputs.iter()).map(|s| s.uid.0))
            .collect();
        let total = uids.len();
        uids.sort();
        uids.dedup();
        assert_eq!(uids.len(), total, "socket uids must not collide");
    }

    #[test]
    fn merged_interior_connections_survive() {
        let reg = registry();
        let mut dst = Graph::new();
        // Source: Rectangle → Inflate (an interior, non-Output
        // connection — same pair the starter graph wires).
        let mut src = Graph::new();
        let b = src.add_new_node("Rectangle", [0.0, 0.0], &reg).unwrap();
        let s = src.add_new_node("Inflate", [100.0, 0.0], &reg).unwrap();
        let from = src.get(b).unwrap().outputs[0].uid;
        let to = src.get(s).unwrap().inputs[0].uid;
        src.connect(Noodle::new(b, from, s, to), &reg).unwrap();

        let result = merge_graph(&mut dst, src, &reg, [0.0, 0.0]);
        assert_eq!(result.added_nodes.len(), 2);
        assert_eq!(
            dst.noodle_count(),
            1,
            "interior connection must be re-established"
        );
    }
}
