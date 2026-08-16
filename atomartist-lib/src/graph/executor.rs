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
//!
//! # A refused node does not stop the pass
//!
//! Node failures are *collected*, not propagated: a node whose
//! `evaluate` returns `Err` is recorded in [`EvalReport::failures`] and
//! the walk continues with every node that does not depend on it. Only
//! the failed node's transitive dependents are skipped (recorded in
//! [`EvalReport::skipped`]), and they keep their previously cached
//! outputs — stale-but-visible beats a viewport that empties because one
//! operand of one Boolean was refused. Graph-level problems (a cycle, a
//! missing node) still abort the pass as `Err`, since there is no
//! meaningful partial answer for those.
//!
//! # A node can speak without failing
//!
//! [`NodeOutputs::warnings`](crate::registry::NodeOutputs::warnings)
//! reaches [`EvalReport::warnings`] as [`NodeWarning`]s. A warning is not
//! a failure in any respect: the node is walked, its outputs are stored,
//! its dependents evaluate, and it is not flagged `failed`. It is how a
//! node that produced a *degraded but correct* result (the Boolean node's
//! partial union) tells the user what it had to do.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::graph::graph::{Graph, GraphError};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::registry::{EvalCtx, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry};

#[derive(Clone, Debug)]
pub enum ExecuteError {
    Graph(GraphError),
    Node {
        node: NodeId,
        type_id: Arc<str>,
        error: NodeError,
    },
    UnknownNodeType {
        node: NodeId,
        type_id: Arc<str>,
    },
    CycleDetected,
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecuteError::Graph(e) => write!(f, "{}", e),
            ExecuteError::Node {
                node,
                type_id,
                error,
            } => {
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

/// One node that refused to evaluate during a pass.
///
/// Carries enough for a host to both *find* the node (badge it on a
/// canvas) and *describe* the failure (a status-bar message).
#[derive(Clone, Debug)]
pub struct NodeFailure {
    pub node: NodeId,
    pub type_id: Arc<str>,
    /// The node's own message — already operand-naming for nodes that
    /// bother (e.g. "input 'b' is not a closed solid").
    pub message: String,
}

impl std::fmt::Display for NodeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.type_id, self.message)
    }
}

/// One node that evaluated *successfully* but has something to say.
///
/// A warning never blocks anything: the node produced its outputs, its
/// dependents run normally, and it is not marked `failed`. It exists for
/// results that are correct but degraded — the Boolean node's partial
/// union carries the parts the kernel refused into the output rather than
/// losing them, and the only thing left to do is tell the user which ones
/// (`Object3DBooleanOperations` + `BooleanObject3D.ReportSkippedOperands`).
#[derive(Clone, Debug)]
pub struct NodeWarning {
    pub node: NodeId,
    pub type_id: Arc<str>,
    pub message: String,
}

impl std::fmt::Display for NodeWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.type_id, self.message)
    }
}

/// Outcome of one evaluation pass.
#[derive(Clone, Debug, Default)]
pub struct EvalReport {
    /// Nodes that evaluated successfully, in topological order.
    pub walked: Vec<NodeId>,
    /// Non-fatal messages from nodes that *did* evaluate, in topological
    /// order. Independent of [`failures`](Self::failures): a node appears
    /// in at most one of the two, and a warning leaves it walked, clean
    /// and unblocking.
    pub warnings: Vec<NodeWarning>,
    /// Nodes whose `evaluate` returned an error (or whose type is not
    /// registered), in topological order.
    pub failures: Vec<NodeFailure>,
    /// Nodes not evaluated because something upstream failed. Their
    /// `cached_outputs` are whatever the last good pass left there.
    pub skipped: Vec<NodeId>,
}

impl EvalReport {
    /// True when every node the pass touched produced a value.
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }

    /// The recorded failure for `node`, if it failed this pass.
    pub fn failure_for(&self, node: NodeId) -> Option<&NodeFailure> {
        self.failures.iter().find(|f| f.node == node)
    }

    /// Panic unless every walked node produced a value, naming what
    /// failed.
    ///
    /// Evaluation no longer returns `Err` for a node failure, so a bare
    /// `evaluate_all(..).unwrap()` — which *was* the assertion in a lot
    /// of tests — now succeeds on a broken graph. Chain this where the
    /// point of the call is "and it all worked". Also useful to a host
    /// that treats any node failure as fatal.
    #[track_caller]
    pub fn expect_clean(self) -> Self {
        assert!(
            self.failures.is_empty(),
            "evaluation failed: {}",
            self.failures
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        );
        self
    }
}

/// Walk every node in topological order.
pub fn evaluate_all(
    graph: &mut Graph,
    registry: &NodeRegistry,
) -> Result<EvalReport, ExecuteError> {
    let order = topo_sort(graph)?;
    let all: HashSet<NodeId> = order.iter().copied().collect();
    run_pass(graph, registry, order, &all)
}

/// Walk only dirty nodes (and their downstream dependents) in topological
/// order. Skips clean upstream nodes whose outputs are already cached.
pub fn evaluate_dirty(
    graph: &mut Graph,
    registry: &NodeRegistry,
) -> Result<EvalReport, ExecuteError> {
    let order = topo_sort(graph)?;
    let mut to_eval: HashSet<NodeId> = graph.nodes().filter(|n| n.dirty).map(|n| n.id).collect();
    // Propagate "dirty" forward through the topo order: any node downstream
    // of a dirty node is also stale.
    for id in &order {
        if to_eval.contains(id) {
            for e in graph.noodles().iter().filter(|e| e.from.node == *id) {
                to_eval.insert(e.to.node);
            }
        }
    }
    run_pass(graph, registry, order, &to_eval)
}

/// Shared walk for both modes: evaluate each selected node in topological
/// order, collecting per-node failures and skipping their dependents.
///
/// Every selected node ends the pass `dirty == false`, failed and skipped
/// ones included. That is deliberate: leaving a broken node dirty would
/// make it re-evaluate (and re-report) on every single tick while the
/// graph stays broken. Because dirty propagates forward from whatever the
/// user edits next, fixing the failed node re-evaluates its dependents
/// anyway.
///
/// The blocked cone is seeded from [`NodeInstance::failed`], not just
/// from failures observed in *this* pass: a node whose upstream failed
/// two passes ago must still not evaluate against that upstream's stale
/// outputs when the user edits it directly.
fn run_pass(
    graph: &mut Graph,
    registry: &NodeRegistry,
    order: Vec<NodeId>,
    selected: &HashSet<NodeId>,
) -> Result<EvalReport, ExecuteError> {
    let mut report = EvalReport::default();
    // Nodes with a failed ancestor. Because we walk in topological order,
    // marking direct dependents as we pass each stalled node is enough to
    // reach the whole downstream cone.
    let mut blocked: HashSet<NodeId> = HashSet::new();
    for id in order {
        // A node still carrying a failure from an earlier pass poisons
        // its dependents even when it isn't being re-walked now.
        let already_failed = graph.get(id).is_some_and(|n| n.failed);
        if already_failed && !selected.contains(&id) {
            for e in graph.noodles().iter().filter(|e| e.from.node == id) {
                blocked.insert(e.to.node);
            }
            continue;
        }
        if !selected.contains(&id) {
            continue;
        }
        let stalled = if blocked.contains(&id) {
            report.skipped.push(id);
            true
        } else {
            match evaluate_one(graph, registry, id) {
                Ok(warnings) => {
                    report.walked.push(id);
                    let type_id = graph
                        .get(id)
                        .map(|n| n.type_id.clone())
                        .unwrap_or_else(|| Arc::from(""));
                    for message in warnings {
                        report.warnings.push(NodeWarning {
                            node: id,
                            type_id: type_id.clone(),
                            message,
                        });
                    }
                    false
                }
                Err(ExecuteError::Node {
                    node,
                    type_id,
                    error,
                }) => {
                    report.failures.push(NodeFailure {
                        node,
                        type_id,
                        message: error.to_string(),
                    });
                    true
                }
                Err(ExecuteError::UnknownNodeType { node, type_id }) => {
                    report.failures.push(NodeFailure {
                        node,
                        message: format!("unknown node type '{}'", type_id),
                        type_id,
                    });
                    true
                }
                // Graph-level problems have no partial answer.
                Err(other) => return Err(other),
            }
        };
        if stalled {
            for e in graph.noodles().iter().filter(|e| e.from.node == id) {
                blocked.insert(e.to.node);
            }
        }
        // A skipped node is not itself broken — only its ancestor is —
        // so `failed` records the node's *own* verdict.
        let own_failure = report.failure_for(id).is_some();
        if let Some(n) = graph.get_mut(id) {
            n.dirty = false;
            n.failed = own_failure;
        }
    }
    Ok(report)
}

/// Evaluate one node: gather its inputs from upstream `cached_outputs`,
/// snapshot its properties, call `evaluate`, store the result. Returns the
/// node's non-fatal messages ([`NodeOutputs::warnings`]).
fn evaluate_one(
    graph: &mut Graph,
    registry: &NodeRegistry,
    id: NodeId,
) -> Result<Vec<String>, ExecuteError> {
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
            inputs.insert_with_source(e.to.socket, value, e.from.node);
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
    let mut outputs = outputs;
    let warnings = std::mem::take(&mut outputs.warnings);
    if let Some(node) = graph.get_mut(id) {
        store_outputs(node, outputs);
    }
    Ok(warnings)
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

/// Topological sort. Delegates to [`Graph::execution_order`] and adapts
/// its [`GraphError`] into [`ExecuteError`] so the executor's surface
/// stays self-contained.
fn topo_sort(graph: &Graph) -> Result<Vec<NodeId>, ExecuteError> {
    graph.execution_order().map_err(|e| match e {
        GraphError::CycleDetected => ExecuteError::CycleDetected,
        other => ExecuteError::Graph(other),
    })
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
        fn type_id(&self) -> &'static str {
            "Add"
        }
        fn category(&self) -> &'static str {
            "Math"
        }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .input("a", SocketType::Number)
                .input("b", SocketType::Number)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            let a = match ctx.input_named("a") {
                PortValue::Number(n) => *n,
                _ => 0.0,
            };
            let b = match ctx.input_named("b") {
                PortValue::Number(n) => *n,
                _ => 0.0,
            };
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(a + b));
            Ok(o)
        }
    }
    struct Const;
    impl NodeDef for Const {
        fn type_id(&self) -> &'static str {
            "Const"
        }
        fn category(&self) -> &'static str {
            "Math"
        }
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

    /// Fails whenever its `boom` property is true — stands in for a
    /// Boolean node refusing a non-solid operand.
    struct Fussy;
    impl NodeDef for Fussy {
        fn type_id(&self) -> &'static str {
            "Fussy"
        }
        fn category(&self) -> &'static str {
            "Math"
        }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .input("a", SocketType::Number)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            if ctx.properties.bool_("boom", false) {
                return Err(NodeError::msg("input 'a' is not a closed solid"));
            }
            let a = match ctx.input_named("a") {
                PortValue::Number(n) => *n,
                _ => 0.0,
            };
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(a + 1.0));
            Ok(o)
        }
    }

    /// Succeeds, but has something to say about how — stands in for a
    /// Boolean node that unioned everything it could and carried the rest
    /// through.
    struct Chatty;
    impl NodeDef for Chatty {
        fn type_id(&self) -> &'static str {
            "Chatty"
        }
        fn category(&self) -> &'static str {
            "Math"
        }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .input("a", SocketType::Number)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            let a = match ctx.input_named("a") {
                PortValue::Number(n) => *n,
                _ => 0.0,
            };
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(a + 1.0));
            o.warn("1 of 3 parts are not watertight solids");
            Ok(o)
        }
    }

    fn registry() -> NodeRegistry {
        let mut r = NodeRegistry::new();
        r.register(AddNode);
        r.register(Const);
        r.register(Fussy);
        r.register(Chatty);
        r
    }

    /// A warning is reported without costing the node — or anything
    /// downstream of it — its result.
    #[test]
    fn a_warning_is_reported_and_nothing_is_blocked() {
        let reg = registry();
        let mut g = Graph::new();
        let k = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let w = g.add_new_node("Chatty", [0.0, 0.0], &reg).unwrap();
        let sink = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        g.set_property(k, "value", PortValue::Number(2.0)).unwrap();
        let k_out = g.get(k).unwrap().output_by_name("out").unwrap().uid;
        let w_in = g.get(w).unwrap().input_by_name("a").unwrap().uid;
        let w_out = g.get(w).unwrap().output_by_name("out").unwrap().uid;
        let s_in = g.get(sink).unwrap().input_by_name("a").unwrap().uid;
        g.connect(Noodle::new(k, k_out, w, w_in), &reg).unwrap();
        g.connect(Noodle::new(w, w_out, sink, s_in), &reg).unwrap();

        let report = evaluate_all(&mut g, &reg).unwrap();

        assert!(report.is_clean(), "a warning is not a failure");
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].node, w);
        assert_eq!(report.warnings[0].type_id.as_ref(), "Chatty");
        assert!(report.warnings[0].message.contains("watertight"));
        assert!(report.walked.contains(&w) && report.walked.contains(&sink));
        assert!(report.skipped.is_empty(), "nothing downstream is blocked");
        assert!(!g.get(w).unwrap().failed, "a warning does not fail the node");
        assert_eq!(cached_number(&g, sink), Some(3.0));
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
        let report = evaluate_all(&mut g, &reg).unwrap();
        assert_eq!(report.walked.len(), 3);
        assert!(report.is_clean());
        let out_c_uid = g.get(c).unwrap().output_by_name("out").unwrap().uid;
        let result = g
            .get(c)
            .unwrap()
            .cached_outputs
            .get(&out_c_uid)
            .cloned()
            .unwrap();
        assert_eq!(result, PortValue::Number(5.0));
    }

    #[test]
    fn evaluate_dirty_only_recomputes_changed_subtree() {
        let (mut g, a, _, c) = three_node_graph();
        let reg = registry();
        evaluate_all(&mut g, &reg).unwrap();
        assert!(g.nodes().all(|n| !n.dirty));
        g.set_property(a, "value", PortValue::Number(10.0)).unwrap();
        let walked = evaluate_dirty(&mut g, &reg).unwrap().walked;
        assert_eq!(walked.len(), 2, "only a and c should re-eval, not b");
        assert!(walked.contains(&a));
        assert!(walked.contains(&c));
        let out_c_uid = g.get(c).unwrap().output_by_name("out").unwrap().uid;
        let result = g
            .get(c)
            .unwrap()
            .cached_outputs
            .get(&out_c_uid)
            .cloned()
            .unwrap();
        assert_eq!(result, PortValue::Number(13.0));
    }

    /// `k(2) -> fussy -> sink(Add)`, plus a `lone` Const that depends on
    /// nothing. Mirrors a Boolean refusing an operand while the rest of
    /// the graph is perfectly healthy.
    fn fussy_graph() -> (Graph, NodeId, NodeId, NodeId) {
        let reg = registry();
        let mut g = Graph::new();
        let k = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let f = g.add_new_node("Fussy", [0.0, 0.0], &reg).unwrap();
        let sink = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let lone = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        g.set_property(k, "value", PortValue::Number(2.0)).unwrap();
        g.set_property(lone, "value", PortValue::Number(7.0))
            .unwrap();
        let k_out = g.get(k).unwrap().output_by_name("out").unwrap().uid;
        let f_in = g.get(f).unwrap().input_by_name("a").unwrap().uid;
        let f_out = g.get(f).unwrap().output_by_name("out").unwrap().uid;
        let s_in = g.get(sink).unwrap().input_by_name("a").unwrap().uid;
        g.connect(Noodle::new(k, k_out, f, f_in), &reg).unwrap();
        g.connect(Noodle::new(f, f_out, sink, s_in), &reg).unwrap();
        (g, f, sink, lone)
    }

    fn cached_number(g: &Graph, id: NodeId) -> Option<f64> {
        let uid = g.get(id)?.output_by_name("out")?.uid;
        match g.get(id)?.cached_outputs.get(&uid)? {
            PortValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// A refused node must not take the whole pass down with it: it is
    /// recorded by id, and nodes that don't depend on it still compute.
    #[test]
    fn a_failed_node_is_recorded_and_independent_nodes_still_evaluate() {
        let (mut g, f, sink, lone) = fussy_graph();
        let reg = registry();
        g.set_property(f, "boom", PortValue::Bool(true)).unwrap();

        let report = evaluate_all(&mut g, &reg).unwrap();

        assert!(!report.is_clean());
        assert_eq!(report.failures.len(), 1);
        let failure = report.failure_for(f).expect("the fussy node is named");
        assert_eq!(failure.type_id.as_ref(), "Fussy");
        assert!(failure.message.contains("input 'a'"), "{}", failure.message);
        assert!(report.skipped.contains(&sink), "the dependent is skipped");
        assert!(
            report.walked.contains(&lone),
            "an unrelated node still runs"
        );
        assert_eq!(cached_number(&g, lone), Some(7.0));
    }

    /// The dependent keeps the value from the last good pass rather than
    /// evaluating against a vanished input — stale beats empty.
    #[test]
    fn dependents_of_a_failed_node_keep_their_last_good_output() {
        let (mut g, f, sink, _) = fussy_graph();
        let reg = registry();
        evaluate_all(&mut g, &reg).unwrap();
        assert_eq!(cached_number(&g, sink), Some(3.0));

        g.set_property(f, "boom", PortValue::Bool(true)).unwrap();
        let report = evaluate_dirty(&mut g, &reg).unwrap();

        assert_eq!(report.failures.len(), 1);
        assert_eq!(cached_number(&g, sink), Some(3.0), "stale, not gone");
        assert!(
            g.nodes().all(|n| !n.dirty),
            "a broken pass still settles the dirty flags, so a parked graph \
             does not re-report the same failure every tick"
        );
    }

    /// Editing a node *downstream* of a node that failed on an earlier
    /// pass must not evaluate it against the failed node's stale
    /// outputs: the failed node is clean-flagged by then, so only the
    /// persisted `failed` flag can stop it.
    #[test]
    fn a_downstream_edit_does_not_evaluate_against_a_failed_nodes_stale_output() {
        let (mut g, f, sink, _) = fussy_graph();
        let reg = registry();
        evaluate_all(&mut g, &reg).unwrap();
        assert_eq!(cached_number(&g, sink), Some(3.0));
        g.set_property(f, "boom", PortValue::Bool(true)).unwrap();
        evaluate_dirty(&mut g, &reg).unwrap();
        assert!(g.get(f).unwrap().failed, "the failure is remembered");

        // The user now edits the *sink* (its own property, nothing to do
        // with the broken upstream).
        g.set_property(sink, "unrelated", PortValue::Number(1.0))
            .unwrap();
        let report = evaluate_dirty(&mut g, &reg).unwrap();

        assert!(report.skipped.contains(&sink), "the dependent is skipped");
        assert!(!report.walked.contains(&sink));
        assert_eq!(cached_number(&g, sink), Some(3.0), "last good value kept");

        // Fixing the upstream re-runs both.
        g.set_property(f, "boom", PortValue::Bool(false)).unwrap();
        let report = evaluate_dirty(&mut g, &reg).unwrap();
        assert!(report.is_clean());
        assert!(report.walked.contains(&f) && report.walked.contains(&sink));
        assert!(!g.get(f).unwrap().failed, "the flag clears on success");
    }

    /// Fixing the node clears the failure and re-runs its dependents.
    #[test]
    fn fixing_a_failed_node_clears_the_failure() {
        let (mut g, f, sink, _) = fussy_graph();
        let reg = registry();
        g.set_property(f, "boom", PortValue::Bool(true)).unwrap();
        evaluate_all(&mut g, &reg).unwrap();
        assert!(cached_number(&g, sink).is_none());

        g.set_property(f, "boom", PortValue::Bool(false)).unwrap();
        let report = evaluate_dirty(&mut g, &reg).unwrap();

        assert!(report.is_clean());
        assert!(report.walked.contains(&sink));
        assert_eq!(cached_number(&g, sink), Some(3.0));
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
