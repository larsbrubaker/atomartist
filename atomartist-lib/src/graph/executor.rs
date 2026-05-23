//! Graph evaluation.
//!
//! Walks the DAG in topological order (Kahn's algorithm), calling each
//! node's `evaluate` with its upstream inputs and current properties, and
//! caching the resulting outputs back onto the node. The executor is
//! `Send` so native builds can run it on a background thread.
//!
//! Inputs are gathered keyed by the target socket's [`SocketUid`] — stable
//! identity across renames. Outputs are returned by name and resolved
//! against the producing node instance's output sockets to find the
//! corresponding uid for storage in `cached_outputs`. This keeps node
//! `evaluate` bodies name-keyed (ergonomic) while noodles remain uid-keyed
//! (robust).
//!
//! Two modes:
//!   - `evaluate_all`: walks every node. Used the first time a graph is
//!     loaded, or after structural changes that invalidate the cache.
//!   - `evaluate_dirty`: walks only nodes flagged `dirty` and propagates
//!     their newly-computed outputs to downstream nodes.

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::graph::{Graph, GraphError};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::registry::{EvalCtx, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry};

#[derive(Clone, Debug)]
pub enum ExecuteError {
    Graph(GraphError),
    Node { node: NodeId, type_id: Arc<str>, error: NodeError },
    UnknownNodeType { node: NodeId, type_id: Arc<str> },
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
            for e in graph.noodles().iter().filter(|e| e.from.node == *id) {
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
    // Look up the type_id without holding a long borrow.
    let type_id = {
        let node = graph
            .get(id)
            .ok_or(ExecuteError::Graph(GraphError::NodeNotFound(id)))?;
        node.type_id.clone()
    };
    let def = registry
        .get(&type_id)
        .ok_or_else(|| ExecuteError::UnknownNodeType {
            node: id,
            type_id: type_id.clone(),
        })?
        .clone();

    // Build inputs + property snapshot from immutable graph state.
    let (inputs, props_snapshot) = {
        let node = graph
            .get(id)
            .ok_or(ExecuteError::Graph(GraphError::NodeNotFound(id)))?;
        let mut inputs = NodeInputs::default();
        for e in graph.noodles() {
            if e.to.node != id {
                continue;
            }
            // Resolve the upstream cached value by source uid.
            let value = graph
                .get(e.from.node)
                .and_then(|src| src.cached_outputs.get(&e.from.socket).cloned())
                .unwrap_or(PortValue::None);
            inputs.insert(e.to.socket, value);
        }
        let mut props = NodeProperties::default();
        for (k, v) in &node.properties {
            props.insert(k.clone(), v.clone());
        }
        (inputs, props)
    };

    // Call evaluate against an EvalCtx that borrows the instance for
    // name-keyed accessors. Re-borrow the node read-only for this call.
    let outputs = {
        let node = graph
            .get(id)
            .ok_or(ExecuteError::Graph(GraphError::NodeNotFound(id)))?;
        let ctx = EvalCtx {
            instance: node,
            properties: &props_snapshot,
            inputs: &inputs,
        };
        def.evaluate(&ctx).map_err(|error| ExecuteError::Node {
            node: id,
            type_id: type_id.clone(),
            error,
        })?
    };

    // Resolve output names against the instance's output sockets to map
    // them to uids, then store under uid in cached_outputs.
    if let Some(node) = graph.get_mut(id) {
        store_outputs(node, outputs);
    }
    Ok(())
}

fn store_outputs(node: &mut NodeInstance, outputs: NodeOutputs) {
    // Build a name→uid map from the instance's outputs.
    let name_to_uid: HashMap<Arc<str>, crate::graph::socket::SocketUid> = node
        .outputs
        .iter()
        .map(|s| (s.name.clone(), s.uid))
        .collect();
    node.cached_outputs.clear();
    for (name, value) in outputs.by_name {
        if let Some(uid) = name_to_uid.get(&name) {
            node.cached_outputs.insert(*uid, value);
        }
        // Outputs the node wrote for a name that isn't on its socket list
        // are silently dropped. Catches stale node code referring to a
        // removed output without breaking eval; tests will surface it.
    }
}

/// Kahn's topological sort. Returns nodes in dependency order — every node
/// appears after all of its upstream producers.
fn topo_sort(graph: &Graph) -> Result<Vec<NodeId>, ExecuteError> {
    let mut in_degree: HashMap<NodeId, usize> = graph.nodes().map(|n| (n.id, 0)).collect();
    for e in graph.noodles() {
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
        for e in graph.noodles().iter().filter(|e| e.from.node == id) {
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
    use crate::graph::graph::Noodle;
    use crate::graph::socket::SocketUidAlloc;
    use crate::registry::{InstanceTemplate, NodeDef, NodeOutputs};
    use crate::socket_types::SocketType;

    struct AddNode;
    impl NodeDef for AddNode {
        fn type_id(&self) -> &'static str { "Add" }
        fn category(&self) -> &'static str { "Math" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .input("a", SocketType::Number)
                .input("b", SocketType::Number)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            let a = match ctx.input_named("a") { PortValue::Number(n) => *n, _ => 0.0 };
            let b = match ctx.input_named("b") { PortValue::Number(n) => *n, _ => 0.0 };
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(a + b));
            Ok(o)
        }
    }
    struct Const;
    impl NodeDef for Const {
        fn type_id(&self) -> &'static str { "Const" }
        fn category(&self) -> &'static str { "Math" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            let v = ctx.properties.number("value", 0.0);
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

    /// Builds: a=2, b=3, c = a + b.
    fn three_node_graph() -> (Graph, NodeId, NodeId, NodeId) {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let c = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        g.set_property(a, "value", PortValue::Number(2.0)).unwrap();
        g.set_property(b, "value", PortValue::Number(3.0)).unwrap();
        let out_a = g.get(a).unwrap().output_by_name("out").unwrap().uid;
        let out_b = g.get(b).unwrap().output_by_name("out").unwrap().uid;
        let in_a = g.get(c).unwrap().input_by_name("a").unwrap().uid;
        let in_b = g.get(c).unwrap().input_by_name("b").unwrap().uid;
        g.connect(Noodle::new(a, out_a, c, in_a), &reg).unwrap();
        g.connect(Noodle::new(b, out_b, c, in_b), &reg).unwrap();
        (g, a, b, c)
    }

    #[test]
    fn evaluate_all_three_node_chain() {
        let (mut g, _, _, c) = three_node_graph();
        let reg = registry();
        let order = evaluate_all(&mut g, &reg).unwrap();
        assert_eq!(order.len(), 3);
        let out_c_uid = g.get(c).unwrap().output_by_name("out").unwrap().uid;
        let result = g.get(c).unwrap().cached_outputs.get(&out_c_uid).cloned().unwrap();
        assert_eq!(result, PortValue::Number(5.0));
    }

    #[test]
    fn evaluate_dirty_only_recomputes_changed_subtree() {
        let (mut g, a, _, c) = three_node_graph();
        let reg = registry();
        evaluate_all(&mut g, &reg).unwrap();
        assert!(g.nodes().all(|n| !n.dirty));
        g.set_property(a, "value", PortValue::Number(10.0)).unwrap();
        let walked = evaluate_dirty(&mut g, &reg).unwrap();
        assert_eq!(walked.len(), 2, "only a and c should re-eval, not b");
        assert!(walked.contains(&a));
        assert!(walked.contains(&c));
        let out_c_uid = g.get(c).unwrap().output_by_name("out").unwrap().uid;
        let result = g.get(c).unwrap().cached_outputs.get(&out_c_uid).cloned().unwrap();
        assert_eq!(result, PortValue::Number(13.0));
    }

    #[test]
    fn no_cycle_means_topo_succeeds() {
        let (mut g, _, _, _) = three_node_graph();
        let order = topo_sort(&g).unwrap();
        assert_eq!(order.len(), 3);
        let reg = registry();
        evaluate_all(&mut g, &reg).unwrap();
    }
}
