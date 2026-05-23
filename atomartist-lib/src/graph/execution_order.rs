//! Topological-order queries on a [`Graph`].
//!
//! The executor needs a topo sort to walk nodes in dependency order; the
//! UI uses the same sort plus an upstream-reachability query (the
//! "ancestors" of a node) to highlight subgraphs and to drive selection
//! widgets like "select all upstream of this node."
//!
//! Both queries live on [`Graph`] rather than the executor so callers can
//! ask for them without invoking evaluation. The executor's `topo_sort`
//! delegates here.
//!
//! Stable order: roots (nodes with no incoming noodles) are processed in
//! ascending [`NodeId`] order; downstream nodes are enqueued in the order
//! their last in-edge resolves. This makes tests deterministic without
//! requiring callers to sort the result themselves.

use crate::graph::graph::{Graph, GraphError};
use crate::graph::node::NodeId;

impl Graph {
    /// Kahn's-algorithm topological sort over the live noodle list.
    /// Returns nodes in dependency order — every node appears after all
    /// of its upstream producers. Returns [`GraphError::CycleDetected`]
    /// when no valid order exists.
    ///
    /// JS analogue: `MatterGraph.computeExecutionOrder(false)`.
    /// JS-only flags (`only_onExecute`, `set_level`, node `priority`) are
    /// not modeled — every Rust `NodeDef` evaluates, levels are a UI
    /// concept handled by the canvas, and the priority knob never had
    /// a Rust counterpart.
    pub fn execution_order(&self) -> Result<Vec<NodeId>, GraphError> {
        use std::collections::{HashMap, VecDeque};

        let mut in_degree: HashMap<NodeId, usize> =
            self.nodes().map(|n| (n.id, 0)).collect();
        for e in self.noodles() {
            if in_degree.contains_key(&e.to.node) && in_degree.contains_key(&e.from.node) {
                *in_degree.entry(e.to.node).or_insert(0) += 1;
            }
        }

        let mut queue: VecDeque<NodeId> = {
            let mut roots: Vec<NodeId> = in_degree
                .iter()
                .filter(|(_, &d)| d == 0)
                .map(|(id, _)| *id)
                .collect();
            roots.sort();
            roots.into()
        };
        let mut out: Vec<NodeId> = Vec::with_capacity(self.node_count());
        while let Some(id) = queue.pop_front() {
            out.push(id);
            for e in self.noodles().iter().filter(|e| e.from.node == id) {
                if let Some(d) = in_degree.get_mut(&e.to.node) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(e.to.node);
                    }
                }
            }
        }
        if out.len() != self.node_count() {
            return Err(GraphError::CycleDetected);
        }
        Ok(out)
    }

    /// All upstream nodes that transitively feed into `node`, in
    /// execution order (so `result[0]` is a root, `result.last()` is the
    /// direct predecessor). Does not include `node` itself. Returns an
    /// empty vec if the node has no incoming noodles or doesn't exist.
    ///
    /// JS analogue: `MatterGraph.getAncestors(node)`.
    pub fn ancestors(&self, node: NodeId) -> Vec<NodeId> {
        use std::collections::HashSet;

        if !self.nodes.contains_key(&node) {
            return Vec::new();
        }
        // BFS upstream from `node`, collecting every reachable predecessor.
        let mut found: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<NodeId> = vec![node];
        while let Some(cur) = stack.pop() {
            for e in self.noodles().iter().filter(|e| e.to.node == cur) {
                if found.insert(e.from.node) {
                    stack.push(e.from.node);
                }
            }
        }
        // Sort the matches by topological order so callers get a stable,
        // dependency-respecting list. If the graph has a cycle, fall back
        // to insertion order — the upstream set is still well-defined,
        // only its ordering is.
        let mut ancestors: Vec<NodeId> = found.into_iter().collect();
        if let Ok(order) = self.execution_order() {
            let mut rank: std::collections::HashMap<NodeId, usize> =
                std::collections::HashMap::with_capacity(order.len());
            for (i, id) in order.into_iter().enumerate() {
                rank.insert(id, i);
            }
            ancestors.sort_by_key(|id| rank.get(id).copied().unwrap_or(usize::MAX));
        } else {
            ancestors.sort();
        }
        ancestors
    }
}
