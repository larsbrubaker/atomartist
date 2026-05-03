//! Graph evaluation.
//!
//! Walks the DAG in topological order (Kahn's algorithm), calling each
//! node's `evaluate` with its upstream inputs and current properties, and
//! caching the resulting outputs back onto the node. The executor is
//! `Send` so native builds can run it on a background thread.
//!
//! Two modes:
//!   - `evaluate_all`: walks every node. Used the first time a graph is
//!     loaded, or after structural changes that invalidate the cache.
//!   - `evaluate_dirty`: walks only nodes flagged `dirty` and propagates
//!     their newly-computed outputs to downstream nodes.

use std::collections::HashMap;

use crate::graph::graph::{Graph, GraphError};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::registry::{NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry};

#[derive(Clone, Debug)]
pub enum ExecuteError {
    Graph(GraphError),
    Node { node: NodeId, type_id: &'static str, error: NodeError },
    UnknownNodeType { node: NodeId, type_id: &'static str },
    CycleDetected,
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecuteError::Graph(e) => write!(f, "{}", e),
            ExecuteError::Node { node, type_id, error } => {
                write!(f, "node {} ({}) failed: {}", node.0, type_id, error)
            }
            ExecuteError::UnknownNodeType { node, type_id } => {
                write!(f, "node {} has unknown type id '{}'", node.0, type_id)
            }
            ExecuteError::CycleDetected => write!(f, "graph contains a cycle"),
        }
    }
}

impl std::error::Error for ExecuteError {}

/// Walk every node in topological order. Returns the topo-sorted list of
/// `NodeId`s for callers (e.g. tests, or the UI to indicate progress).
pub fn evaluate_all(graph: &mut Graph, registry: &NodeRegistry) -> Result<Vec<NodeId>, ExecuteError> {
    let order = topo_sort(graph)?;
    for &id in &order {
        evaluate_one(graph, registry, id)?;
        if let Some(n) = graph.get_mut(id) {
            n.dirty = false;
        }
    }
    Ok(order)
}

/// Walk only dirty nodes (and their downstream dependents) in topological
/// order. Skips clean upstream nodes whose outputs are already cached.
pub fn evaluate_dirty(graph: &mut Graph, registry: &NodeRegistry) -> Result<Vec<NodeId>, ExecuteError> {
    let order = topo_sort(graph)?;
    let mut to_eval: std::collections::HashSet<NodeId> = graph
        .nodes()
        .filter(|n| n.dirty)
        .map(|n| n.id)
        .collect();
    // Propagate "dirty" forward through the topo order: any node downstream
    // of a dirty node is also stale.
    for id in &order {
        if to_eval.contains(id) {
            for e in graph.edges().iter().filter(|e| e.from.node == *id) {
                to_eval.insert(e.to.node);
            }
        }
    }
    let mut walked = Vec::new();
    for id in order {
        if to_eval.contains(&id) {
            evaluate_one(graph, registry, id)?;
            if let Some(n) = graph.get_mut(id) {
                n.dirty = false;
            }
            walked.push(id);
        }
    }
    Ok(walked)
}

/// Evaluate one node: gather its inputs from upstream `cached_outputs`,
/// snapshot its properties, call `evaluate`, store the result.
fn evaluate_one(
    graph: &mut Graph,
    registry: &NodeRegistry,
    id: NodeId,
) -> Result<(), ExecuteError> {
    // Snapshot the bits we need from immutable borrows so we can take a
    // mutable borrow later.
    let (type_id, props_snapshot, inputs) = {
        let node = graph
            .get(id)
            .ok_or(ExecuteError::Graph(GraphError::NodeNotFound(id)))?;
        let type_id = node.type_id;
        let mut inputs = NodeInputs::default();
        for e in graph.edges() {
            if e.to.node != id {
                continue;
            }
            // Look up the upstream value from the producer's cached_outputs.
            let value = graph
                .get(e.from.node)
                .and_then(|src| src.cached_outputs.get(e.from.name).cloned())
                .unwrap_or(PortValue::None);
            inputs.insert(e.to.name, value);
        }
        let mut props = NodeProperties::default();
        for (k, v) in &node.properties {
            props.insert(k, v.clone());
        }
        (type_id, props, inputs)
    };

    let def = registry
        .get(type_id)
        .ok_or(ExecuteError::UnknownNodeType { node: id, type_id })?;

    let outputs = def.evaluate(&inputs, &props_snapshot).map_err(|error| {
        ExecuteError::Node { node: id, type_id, error }
    })?;

    if let Some(node) = graph.get_mut(id) {
        store_outputs(node, outputs);
    }
    Ok(())
}

fn store_outputs(node: &mut NodeInstance, outputs: NodeOutputs) {
    node.cached_outputs.clear();
    for (k, v) in outputs.by_name {
        node.cached_outputs.insert(k, v);
    }
}

/// Kahn's topological sort. Returns nodes in dependency order — every node
/// appears after all of its upstream producers.
fn topo_sort(graph: &Graph) -> Result<Vec<NodeId>, ExecuteError> {
    let mut in_degree: HashMap<NodeId, usize> = graph.nodes().map(|n| (n.id, 0)).collect();
    for e in graph.edges() {
        if in_degree.contains_key(&e.to.node) && in_degree.contains_key(&e.from.node) {
            *in_degree.entry(e.to.node).or_insert(0) += 1;
        }
    }

    // Stable processing order: sort the initial roots by id so test output
    // is deterministic.
    let mut queue: std::collections::VecDeque<NodeId> = {
        let mut roots: Vec<NodeId> =
            in_degree.iter().filter(|(_, &d)| d == 0).map(|(id, _)| *id).collect();
        roots.sort();
        roots.into()
    };
    let mut out: Vec<NodeId> = Vec::with_capacity(graph.node_count());
    while let Some(id) = queue.pop_front() {
        out.push(id);
        for e in graph.edges().iter().filter(|e| e.from.node == id) {
            if let Some(d) = in_degree.get_mut(&e.to.node) {
                *d -= 1;
                if *d == 0 {
                    queue.push_back(e.to.node);
                }
            }
        }
    }
    if out.len() != graph.node_count() {
        return Err(ExecuteError::CycleDetected);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::graph::Edge;
    use crate::graph::node::SocketId;
    use crate::registry::{NodeDef, SocketDef};
    use crate::socket_types::SocketType;

    struct AddNode;
    impl NodeDef for AddNode {
        fn type_id(&self) -> &'static str { "Add" }
        fn category(&self) -> &'static str { "Math" }
        fn input_sockets(&self) -> Vec<SocketDef> {
            vec![
                SocketDef::required("a", SocketType::Number),
                SocketDef::required("b", SocketType::Number),
            ]
        }
        fn output_sockets(&self) -> Vec<SocketDef> {
            vec![SocketDef::required("out", SocketType::Number)]
        }
        fn evaluate(&self, inputs: &NodeInputs, _: &NodeProperties) -> Result<NodeOutputs, NodeError> {
            let a = match inputs.get("a") { PortValue::Number(n) => *n, _ => 0.0 };
            let b = match inputs.get("b") { PortValue::Number(n) => *n, _ => 0.0 };
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(a + b));
            Ok(o)
        }
    }
    struct Const;
    impl NodeDef for Const {
        fn type_id(&self) -> &'static str { "Const" }
        fn category(&self) -> &'static str { "Math" }
        fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
        fn output_sockets(&self) -> Vec<SocketDef> {
            vec![SocketDef::required("out", SocketType::Number)]
        }
        fn evaluate(&self, _: &NodeInputs, p: &NodeProperties) -> Result<NodeOutputs, NodeError> {
            let v = p.number("value", 0.0);
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(v));
            Ok(o)
        }
    }

    fn registry() -> NodeRegistry {
        let mut r = NodeRegistry::new();
        r.register(AddNode);
        r.register(Const);
        r
    }

    /// Builds: a=2, b=3, c = a + b. Returns (graph, NodeId of c).
    fn three_node_graph() -> (Graph, NodeId, NodeId, NodeId) {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.allocate_id();
        let b = g.allocate_id();
        let c = g.allocate_id();
        let mut na = NodeInstance::new(a, "Const", [0.0, 0.0]);
        na.properties.insert("value", PortValue::Number(2.0));
        let mut nb = NodeInstance::new(b, "Const", [0.0, 0.0]);
        nb.properties.insert("value", PortValue::Number(3.0));
        let nc = NodeInstance::new(c, "Add", [0.0, 0.0]);
        g.add_node(na).unwrap();
        g.add_node(nb).unwrap();
        g.add_node(nc).unwrap();
        g.connect(
            Edge { from: SocketId { node: a, name: "out" }, to: SocketId { node: c, name: "a" } },
            &reg,
        ).unwrap();
        g.connect(
            Edge { from: SocketId { node: b, name: "out" }, to: SocketId { node: c, name: "b" } },
            &reg,
        ).unwrap();
        (g, a, b, c)
    }

    #[test]
    fn evaluate_all_three_node_chain() {
        let (mut g, _, _, c) = three_node_graph();
        let reg = registry();
        let order = evaluate_all(&mut g, &reg).unwrap();
        assert_eq!(order.len(), 3);
        let result = g.get(c).unwrap().cached_outputs.get("out").cloned().unwrap();
        assert_eq!(result, PortValue::Number(5.0));
    }

    #[test]
    fn evaluate_dirty_only_recomputes_changed_subtree() {
        let (mut g, a, _, c) = three_node_graph();
        let reg = registry();
        evaluate_all(&mut g, &reg).unwrap();
        // All clean now.
        assert!(g.nodes().all(|n| !n.dirty));
        // Change a's value → a dirty, c dirty (b unchanged).
        g.set_property(a, "value", PortValue::Number(10.0)).unwrap();
        let walked = evaluate_dirty(&mut g, &reg).unwrap();
        assert_eq!(walked.len(), 2, "only a and c should re-eval, not b");
        assert!(walked.contains(&a));
        assert!(walked.contains(&c));
        let result = g.get(c).unwrap().cached_outputs.get("out").cloned().unwrap();
        assert_eq!(result, PortValue::Number(13.0));
    }

    #[test]
    fn no_cycle_means_topo_succeeds() {
        let (mut g, _, _, _) = three_node_graph();
        let order = topo_sort(&g).unwrap();
        assert_eq!(order.len(), 3);
        // Manually inserted edges form an acyclic DAG.
        // Mutating the graph here exercises the &mut path used by evaluate_all.
        let reg = registry();
        evaluate_all(&mut g, &reg).unwrap();
    }
}
