//! Graph data structure — nodes, edges, mutation API.
//!
//! `Graph` owns a `HashMap<NodeId, NodeInstance>`, a `Vec<Edge>`, and the
//! shared [`SocketUidAlloc`] that hands out stable socket identifiers.
//! It is the source-of-truth for a project; the executor reads it, undo
//! commands mutate it, and the UI displays it.
//!
//! Mutations are deliberately small and granular (add_node, remove_node,
//! connect, disconnect, rename_socket, …) so undo commands can wrap each
//! one as a discrete reversible operation, and so node behavior hooks
//! (`on_input_connected`, …) can invoke socket-level mutations on the
//! same Graph without needing a richer API.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::graph::socket::{SocketUid, SocketUidAlloc};
use crate::registry::{ConnectCtx, DisconnectCtx, NodeRegistry, ValidateCtx};
use crate::socket_types::SocketType;

/// Identifier for one socket endpoint on an edge.
///
/// `(node, socket)` — where `socket` is the stable [`SocketUid`] allocated
/// when the socket was created. Names and types may change; this pair does
/// not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EdgeEndpoint {
    pub node: NodeId,
    pub socket: SocketUid,
}

impl EdgeEndpoint {
    pub fn new(node: NodeId, socket: SocketUid) -> Self {
        Self { node, socket }
    }
}

/// One directed connection from an output socket to an input socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    pub from: EdgeEndpoint,
    pub to: EdgeEndpoint,
}

impl Edge {
    pub fn new(
        from_node: NodeId,
        from_socket: SocketUid,
        to_node: NodeId,
        to_socket: SocketUid,
    ) -> Self {
        Self {
            from: EdgeEndpoint::new(from_node, from_socket),
            to: EdgeEndpoint::new(to_node, to_socket),
        }
    }
}

/// Errors raised by `Graph` mutations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    NodeNotFound(NodeId),
    SocketNotFound { node: NodeId, socket: SocketUid },
    UnknownNodeType { type_id: String },
    TypeMismatch { expected: SocketType, actual: SocketType },
    /// The edge would create a cycle in the DAG.
    CycleDetected,
    /// Connecting would leave the input with two incoming edges; the caller
    /// must explicitly disconnect the existing one first.
    InputAlreadyConnected,
    /// The target node's `validate_input_connection` hook rejected the
    /// edge. The wrapped string is the human-readable reason from the hook.
    ConnectionRejected(String),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::NodeNotFound(NodeId(id)) => write!(f, "node not found: {}", id),
            GraphError::SocketNotFound { node, socket } => {
                write!(f, "socket {} not found on node {}", socket.0, node.0)
            }
            GraphError::UnknownNodeType { type_id } => {
                write!(f, "unknown node type '{}'", type_id)
            }
            GraphError::TypeMismatch { expected, actual } => {
                write!(f, "socket type mismatch: expected {:?}, got {:?}", expected, actual)
            }
            GraphError::CycleDetected => write!(f, "connection would create a cycle"),
            GraphError::InputAlreadyConnected => write!(f, "input socket already connected"),
            GraphError::ConnectionRejected(why) => write!(f, "connection rejected: {}", why),
        }
    }
}

impl std::error::Error for GraphError {}

/// Source-of-truth document for a node graph.
#[derive(Default)]
pub struct Graph {
    pub(crate) nodes: HashMap<NodeId, NodeInstance>,
    edges: Vec<Edge>,
    next_id: AtomicU64,
    socket_alloc: SocketUidAlloc,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a fresh `NodeId`. Strictly monotonic; never reused.
    pub fn allocate_id(&self) -> NodeId {
        NodeId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Allocate a fresh `SocketUid`. Used by code that mints sockets
    /// outside of `add_new_node` (e.g. dynamic-input nodes appending a
    /// trailing empty slot in `on_input_connected`).
    pub fn allocate_socket_uid(&mut self) -> SocketUid {
        self.socket_alloc.allocate()
    }

    /// Borrow the uid allocator — primarily used by the serialization
    /// loader to bump the allocator past values it has just resurrected.
    pub fn socket_alloc(&mut self) -> &mut SocketUidAlloc {
        &mut self.socket_alloc
    }

    /// Snapshot of the next uid value, for serialization.
    pub fn peek_next_socket_uid(&self) -> u64 {
        self.socket_alloc.peek_next()
    }

    pub fn nodes(&self) -> impl Iterator<Item = &NodeInstance> {
        self.nodes.values()
    }

    /// Mutable iteration over every node — used by `SubgraphNodeDef` to
    /// flag every node dirty before re-evaluating a freshly-cloned
    /// template.
    pub fn nodes_mut(&mut self) -> impl Iterator<Item = &mut NodeInstance> {
        self.nodes.values_mut()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Direct mutable access to the edge list. Used by undo commands that
    /// need to restore exact edges without re-validating (the edge was
    /// known-valid when the command was originally created).
    pub fn edges_mut(&mut self) -> &mut Vec<Edge> {
        &mut self.edges
    }

    pub fn get(&self, id: NodeId) -> Option<&NodeInstance> {
        self.nodes.get(&id)
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut NodeInstance> {
        self.nodes.get_mut(&id)
    }

    /// Create a new node of the given type, using its `NodeDef` to mint
    /// the initial socket layout and seed properties. Returns the new
    /// node's id.
    ///
    /// Most callers — UI menu actions, tests, file loaders for new nodes
    /// — should use this rather than building a [`NodeInstance`] by hand.
    /// `add_node` is reserved for the loader and undo-restore paths
    /// (which need exact uid preservation).
    pub fn add_new_node(
        &mut self,
        type_id: &str,
        position: [f64; 2],
        registry: &NodeRegistry,
    ) -> Result<NodeId, GraphError> {
        let def = registry
            .get(type_id)
            .ok_or_else(|| GraphError::UnknownNodeType { type_id: type_id.into() })?
            .clone();
        let id = self.allocate_id();
        let template = def.instantiate(&mut self.socket_alloc);
        let mut instance = NodeInstance::new(id, type_id.to_string(), position);
        instance.inputs = template.inputs;
        instance.outputs = template.outputs;
        // Seed defaults from the property schema, then layer per-template
        // overrides on top — this means callers can change just the ones
        // they care about without re-listing every property.
        for prop in def.properties() {
            instance
                .properties
                .insert(prop.name.clone(), prop.default.clone());
        }
        for (name, value) in template.initial_properties {
            instance.properties.insert(name, value);
        }
        self.nodes.insert(id, instance);
        Ok(id)
    }

    /// Insert a pre-built node. Returns `Err(NodeNotFound)` if a node with
    /// that id already exists (id collision is a programmer error). The
    /// caller is responsible for the node's socket layout — typically the
    /// loader and undo-restore paths, where exact uids must be preserved.
    pub fn add_node(&mut self, node: NodeInstance) -> Result<(), GraphError> {
        // Bump the id allocator past any externally-supplied id so subsequent
        // `allocate_id` calls don't collide.
        let next = node.id.0 + 1;
        let cur = self.next_id.load(Ordering::Relaxed);
        if next > cur {
            self.next_id.store(next, Ordering::Relaxed);
        }
        // Bump the socket-uid allocator past every uid on the new node.
        for s in node.inputs.iter().chain(node.outputs.iter()) {
            self.socket_alloc.observe(s.uid);
        }
        if self.nodes.contains_key(&node.id) {
            return Err(GraphError::NodeNotFound(node.id));
        }
        self.nodes.insert(node.id, node);
        Ok(())
    }

    /// Remove a node and any edges referencing it. Returns the removed node
    /// instance plus the removed edges so undo can restore them.
    pub fn remove_node(&mut self, id: NodeId) -> Result<(NodeInstance, Vec<Edge>), GraphError> {
        let node = self.nodes.remove(&id).ok_or(GraphError::NodeNotFound(id))?;
        let mut detached = Vec::new();
        self.edges.retain(|e| {
            if e.from.node == id || e.to.node == id {
                detached.push(*e);
                false
            } else {
                true
            }
        });
        // Mark direct downstream nodes dirty so the next eval recomputes them.
        for e in &detached {
            if e.from.node == id {
                if let Some(n) = self.nodes.get_mut(&e.to.node) {
                    n.dirty = true;
                }
            }
        }
        Ok((node, detached))
    }

    /// Connect an output socket to an input socket. Validates:
    ///   - both nodes exist
    ///   - both sockets are present on their node instances (by uid)
    ///   - socket types are compatible
    ///   - the target node's `validate_input_connection` hook allows it
    ///   - the input has no existing incoming edge
    ///   - the resulting graph is acyclic
    ///
    /// On success, inserts the edge, marks the destination subtree dirty,
    /// and invokes the target type's `on_input_connected` hook (which may
    /// further mutate sockets via the granular helpers below).
    pub fn connect(&mut self, edge: Edge, registry: &NodeRegistry) -> Result<(), GraphError> {
        // Existence + socket validation via uid lookup.
        let from_type = self.lookup_output_socket_type(&edge.from)?;
        let to_type = self.lookup_input_socket_type(&edge.to)?;
        if !from_type.is_compatible_with(to_type) {
            return Err(GraphError::TypeMismatch {
                expected: to_type,
                actual: from_type,
            });
        }

        // Single-incoming-edge invariant on the destination input.
        if self.edges.iter().any(|e| e.to == edge.to) {
            return Err(GraphError::InputAlreadyConnected);
        }

        // Cycle check — a path from edge.to.node back to edge.from.node would
        // close a loop once this edge is added.
        if self.has_path(edge.to.node, edge.from.node) {
            return Err(GraphError::CycleDetected);
        }

        // Pre-connect veto hook on the target type.
        let target_type_id = self
            .get(edge.to.node)
            .ok_or(GraphError::NodeNotFound(edge.to.node))?
            .type_id
            .clone();
        let target_def = registry
            .get(&target_type_id)
            .ok_or_else(|| GraphError::UnknownNodeType { type_id: target_type_id.to_string() })?
            .clone();
        {
            let validate = ValidateCtx {
                graph: self,
                this_node: edge.to.node,
                target_socket: edge.to.socket,
                source_node: edge.from.node,
                source_socket: edge.from.socket,
            };
            if let Err(why) = target_def.validate_input_connection(&validate) {
                return Err(GraphError::ConnectionRejected(why));
            }
        }

        self.mark_dirty_subtree(edge.to.node);
        self.edges.push(edge);

        // Post-connect behavior hook. Runs after the edge is in place so
        // the hook can inspect the live graph (`graph.edges()` already
        // includes the new edge).
        let mut ctx = ConnectCtx {
            graph: self,
            this_node: edge.to.node,
            target_socket: edge.to.socket,
            source_node: edge.from.node,
            source_socket: edge.from.socket,
        };
        target_def.on_input_connected(&mut ctx);

        Ok(())
    }

    /// Remove an existing edge by exact match. No-op + `Ok(false)` if not
    /// present. Invokes `on_input_disconnected` on the target type after
    /// the edge is gone, so the hook can collapse the now-orphan slot.
    pub fn disconnect(
        &mut self,
        edge: &Edge,
        registry: &NodeRegistry,
    ) -> Result<bool, GraphError> {
        let len_before = self.edges.len();
        self.edges.retain(|e| e != edge);
        let removed = self.edges.len() < len_before;
        if !removed {
            return Ok(false);
        }
        self.mark_dirty_subtree(edge.to.node);

        let target_type_id = match self.get(edge.to.node) {
            Some(n) => n.type_id.clone(),
            // Node already gone — nothing to hook into.
            None => return Ok(true),
        };
        let target_def = match registry.get(&target_type_id) {
            Some(d) => d.clone(),
            None => return Ok(true),
        };
        let mut ctx = DisconnectCtx {
            graph: self,
            this_node: edge.to.node,
            target_socket: edge.to.socket,
        };
        target_def.on_input_disconnected(&mut ctx);
        Ok(true)
    }

    /// Returns true if there is a directed path of edges from `start` to
    /// `target` (used for cycle detection in `connect`).
    pub fn has_path(&self, start: NodeId, target: NodeId) -> bool {
        if start == target {
            return true;
        }
        let mut stack = vec![start];
        let mut visited = std::collections::HashSet::new();
        visited.insert(start);
        while let Some(cur) = stack.pop() {
            for e in self.edges.iter().filter(|e| e.from.node == cur) {
                if e.to.node == target {
                    return true;
                }
                if visited.insert(e.to.node) {
                    stack.push(e.to.node);
                }
            }
        }
        false
    }

    /// Mark `start` and every transitive downstream node dirty.
    pub fn mark_dirty_subtree(&mut self, start: NodeId) {
        let mut stack = vec![start];
        let mut visited = std::collections::HashSet::new();
        while let Some(cur) = stack.pop() {
            if !visited.insert(cur) {
                continue;
            }
            if let Some(n) = self.nodes.get_mut(&cur) {
                n.dirty = true;
            }
            let downstream: Vec<NodeId> = self
                .edges
                .iter()
                .filter(|e| e.from.node == cur)
                .map(|e| e.to.node)
                .collect();
            stack.extend(downstream);
        }
    }

    /// Direct property mutation. Marks the node + subtree dirty.
    pub fn set_property(
        &mut self,
        id: NodeId,
        name: impl Into<Arc<str>>,
        value: PortValue,
    ) -> Result<(), GraphError> {
        if !self.nodes.contains_key(&id) {
            return Err(GraphError::NodeNotFound(id));
        }
        if let Some(n) = self.nodes.get_mut(&id) {
            n.properties.insert(name.into(), value);
        }
        self.mark_dirty_subtree(id);
        Ok(())
    }

    /// Move a node to a new canvas position. Does not mark dirty (position
    /// has no effect on evaluation).
    pub fn set_position(&mut self, id: NodeId, position: [f64; 2]) -> Result<(), GraphError> {
        let n = self.nodes.get_mut(&id).ok_or(GraphError::NodeNotFound(id))?;
        n.position = position;
        Ok(())
    }

    // Granular socket-level mutations (rename_socket, relabel_socket,
    // retype_socket, append/remove/reorder, edges_touching) live in
    // sibling module `socket_mutations` to keep this file under the
    // project-wide 800-line cap.

    fn lookup_output_socket_type(
        &self,
        endpoint: &EdgeEndpoint,
    ) -> Result<SocketType, GraphError> {
        let node = self
            .nodes
            .get(&endpoint.node)
            .ok_or(GraphError::NodeNotFound(endpoint.node))?;
        node.output_by_uid(endpoint.socket)
            .map(|s| s.socket_type)
            .ok_or(GraphError::SocketNotFound {
                node: endpoint.node,
                socket: endpoint.socket,
            })
    }

    fn lookup_input_socket_type(
        &self,
        endpoint: &EdgeEndpoint,
    ) -> Result<SocketType, GraphError> {
        let node = self
            .nodes
            .get(&endpoint.node)
            .ok_or(GraphError::NodeNotFound(endpoint.node))?;
        node.input_by_uid(endpoint.socket)
            .map(|s| s.socket_type)
            .ok_or(GraphError::SocketNotFound {
                node: endpoint.node,
                socket: endpoint.socket,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::NodeInstance;
    use crate::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeProperties,
    };

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
    struct ConstNumber;
    impl NodeDef for ConstNumber {
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
        r.register(ConstNumber);
        r
    }

    fn uid_of_input(g: &Graph, node: NodeId, name: &str) -> SocketUid {
        g.get(node).unwrap().input_by_name(name).unwrap().uid
    }

    fn uid_of_output(g: &Graph, node: NodeId, name: &str) -> SocketUid {
        g.get(node).unwrap().output_by_name(name).unwrap().uid
    }

    #[test]
    fn add_remove_round_trip() {
        let mut g = Graph::new();
        let id = g.allocate_id();
        g.add_node(NodeInstance::new(id, "Const", [0.0, 0.0])).unwrap();
        assert_eq!(g.node_count(), 1);
        let (removed, edges) = g.remove_node(id).unwrap();
        assert_eq!(removed.id, id);
        assert!(edges.is_empty());
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn connect_validates_socket_uid_existence() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let out_a = uid_of_output(&g, a, "out");
        let in_a = uid_of_input(&g, b, "a");

        // Wrong uid → SocketNotFound
        let e = Edge::new(a, SocketUid(999), b, in_a);
        assert!(matches!(g.connect(e, &reg), Err(GraphError::SocketNotFound { .. })));

        // Right wiring works
        let ok = Edge::new(a, out_a, b, in_a);
        g.connect(ok, &reg).unwrap();
        assert_eq!(g.edge_count(), 1);

        // Duplicate input connection rejected
        assert_eq!(
            g.connect(ok, &reg),
            Err(GraphError::InputAlreadyConnected)
        );
    }

    #[test]
    fn cycle_detection() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let out_a = uid_of_output(&g, a, "out");
        let out_b = uid_of_output(&g, b, "out");
        let in_a_b = uid_of_input(&g, b, "a");
        let in_a_a = uid_of_input(&g, a, "a");
        g.connect(Edge::new(a, out_a, b, in_a_b), &reg).unwrap();
        // b → a would close a cycle
        assert_eq!(
            g.connect(Edge::new(b, out_b, a, in_a_a), &reg),
            Err(GraphError::CycleDetected),
        );
    }

    #[test]
    fn remove_node_detaches_edges() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let out_a = uid_of_output(&g, a, "out");
        let in_a_b = uid_of_input(&g, b, "a");
        g.connect(Edge::new(a, out_a, b, in_a_b), &reg).unwrap();
        let (_, edges) = g.remove_node(a).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn dirty_propagates_downstream_on_connect() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let c = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        for n in g.nodes.values_mut() { n.dirty = false; }

        let out_a = uid_of_output(&g, a, "out");
        let in_a_b = uid_of_input(&g, b, "a");
        g.connect(Edge::new(a, out_a, b, in_a_b), &reg).unwrap();
        assert!(g.get(b).unwrap().dirty, "destination of new edge must be dirty");
        assert!(!g.get(c).unwrap().dirty);

        let out_b = uid_of_output(&g, b, "out");
        let in_a_c = uid_of_input(&g, c, "a");
        g.connect(Edge::new(b, out_b, c, in_a_c), &reg).unwrap();
        assert!(g.get(c).unwrap().dirty);
    }

    #[test]
    fn rename_socket_preserves_edges() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let out_a = uid_of_output(&g, a, "out");
        let in_a_b = uid_of_input(&g, b, "a");
        g.connect(Edge::new(a, out_a, b, in_a_b), &reg).unwrap();
        g.rename_socket(b, in_a_b, "renamed_a").unwrap();
        // Edge still resolves — uid hasn't changed.
        let resolved = g.get(b).unwrap().input_by_name("renamed_a").unwrap();
        assert_eq!(resolved.uid, in_a_b);
        assert_eq!(g.edges().len(), 1);
    }

    #[test]
    fn remove_input_socket_gcs_edges() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let out_a = uid_of_output(&g, a, "out");
        let in_a_b = uid_of_input(&g, b, "a");
        g.connect(Edge::new(a, out_a, b, in_a_b), &reg).unwrap();
        let (removed, detached) = g.remove_input_socket(b, in_a_b).unwrap();
        assert_eq!(removed.uid, in_a_b);
        assert_eq!(detached.len(), 1);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn reorder_input_sockets_preserves_uids_and_edges() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
        let b = g.add_new_node("Add", [0.0, 0.0], &reg).unwrap();
        let out_a = uid_of_output(&g, a, "out");
        let in_a_b = uid_of_input(&g, b, "a");
        let in_b_b = uid_of_input(&g, b, "b");
        g.connect(Edge::new(a, out_a, b, in_a_b), &reg).unwrap();

        // Swap inputs 0 and 1 on b.
        g.reorder_input_sockets(b, &[1, 0]).unwrap();
        let nb = g.get(b).unwrap();
        assert_eq!(nb.inputs[0].uid, in_b_b);
        assert_eq!(nb.inputs[1].uid, in_a_b);
        // Edge still references in_a_b — unaffected by reorder.
        assert_eq!(g.edges()[0].to.socket, in_a_b);
    }
}
