//! Graph data structure — nodes, edges, mutation API.
//!
//! `Graph` owns a `HashMap<NodeId, NodeInstance>` and a `Vec<Edge>`. It is
//! the source-of-truth for a project; the executor reads it, undo commands
//! mutate it, and the UI displays it. Mutations are deliberately small and
//! granular (add_node, remove_node, connect, disconnect) so undo commands
//! can wrap each one as a discrete reversible operation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::graph::node::{NodeId, NodeInstance, PortValue, SocketId};
use crate::registry::NodeRegistry;
use crate::socket_types::SocketType;

/// One directed connection from an output socket to an input socket.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    pub from: SocketId,
    pub to: SocketId,
}

/// Errors raised by `Graph` mutations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    NodeNotFound(NodeId),
    SocketNotFound { node: NodeId, name: String },
    TypeMismatch { expected: SocketType, actual: SocketType },
    /// The edge would create a cycle in the DAG.
    CycleDetected,
    /// Connecting would leave the input with two incoming edges; the caller
    /// must explicitly disconnect the existing one first.
    InputAlreadyConnected,
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::NodeNotFound(NodeId(id)) => write!(f, "node not found: {}", id),
            GraphError::SocketNotFound { node, name } => {
                write!(f, "socket '{}' not found on node {}", name, node.0)
            }
            GraphError::TypeMismatch { expected, actual } => {
                write!(f, "socket type mismatch: expected {:?}, got {:?}", expected, actual)
            }
            GraphError::CycleDetected => write!(f, "connection would create a cycle"),
            GraphError::InputAlreadyConnected => write!(f, "input socket already connected"),
        }
    }
}

impl std::error::Error for GraphError {}

/// Source-of-truth document for a node graph.
#[derive(Default)]
pub struct Graph {
    nodes: HashMap<NodeId, NodeInstance>,
    edges: Vec<Edge>,
    next_id: AtomicU64,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a fresh `NodeId`. Strictly monotonic; never reused.
    pub fn allocate_id(&self) -> NodeId {
        NodeId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    pub fn nodes(&self) -> impl Iterator<Item = &NodeInstance> {
        self.nodes.values()
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

    /// Insert a node. Returns `Err(GraphError::NodeNotFound)` if a node with
    /// that id already exists (id collision is a programmer error).
    pub fn add_node(&mut self, node: NodeInstance) -> Result<(), GraphError> {
        // Bump the id allocator past any externally-supplied id so subsequent
        // `allocate_id` calls don't collide.
        let next = node.id.0 + 1;
        let cur = self.next_id.load(Ordering::Relaxed);
        if next > cur {
            self.next_id.store(next, Ordering::Relaxed);
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
                detached.push(e.clone());
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
    ///   - both sockets are declared on their node-types
    ///   - socket types are compatible
    ///   - the input has no existing incoming edge
    ///   - the resulting graph is acyclic
    /// Marks the destination node and its downstream subtree dirty.
    pub fn connect(&mut self, edge: Edge, registry: &NodeRegistry) -> Result<(), GraphError> {
        // Existence + socket validation.
        let from_type = self.lookup_output_socket_type(&edge.from, registry)?;
        let to_type = self.lookup_input_socket_type(&edge.to, registry)?;
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

        self.mark_dirty_subtree(edge.to.node);
        self.edges.push(edge);
        Ok(())
    }

    /// Remove an existing edge by exact match. No-op + Ok(false) if not present.
    pub fn disconnect(&mut self, edge: &Edge) -> Result<bool, GraphError> {
        let len_before = self.edges.len();
        self.edges.retain(|e| e != edge);
        let removed = self.edges.len() < len_before;
        if removed {
            self.mark_dirty_subtree(edge.to.node);
        }
        Ok(removed)
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
        name: &'static str,
        value: PortValue,
    ) -> Result<(), GraphError> {
        if !self.nodes.contains_key(&id) {
            return Err(GraphError::NodeNotFound(id));
        }
        if let Some(n) = self.nodes.get_mut(&id) {
            n.properties.insert(name, value);
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

    fn lookup_output_socket_type(
        &self,
        sid: &SocketId,
        registry: &NodeRegistry,
    ) -> Result<SocketType, GraphError> {
        let node = self.nodes.get(&sid.node).ok_or(GraphError::NodeNotFound(sid.node))?;
        let def = registry
            .get(node.type_id)
            .ok_or_else(|| GraphError::SocketNotFound {
                node: sid.node,
                name: node.type_id.into(),
            })?;
        def.output_sockets()
            .into_iter()
            .find(|s| s.name == sid.name)
            .map(|s| s.socket_type)
            .ok_or_else(|| GraphError::SocketNotFound {
                node: sid.node,
                name: sid.name.into(),
            })
    }

    fn lookup_input_socket_type(
        &self,
        sid: &SocketId,
        registry: &NodeRegistry,
    ) -> Result<SocketType, GraphError> {
        let node = self.nodes.get(&sid.node).ok_or(GraphError::NodeNotFound(sid.node))?;
        let def = registry
            .get(node.type_id)
            .ok_or_else(|| GraphError::SocketNotFound {
                node: sid.node,
                name: node.type_id.into(),
            })?;
        def.input_sockets()
            .into_iter()
            .find(|s| s.name == sid.name)
            .map(|s| s.socket_type)
            .ok_or_else(|| GraphError::SocketNotFound {
                node: sid.node,
                name: sid.name.into(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, SocketDef};

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
    struct ConstNumber;
    impl NodeDef for ConstNumber {
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
        r.register(ConstNumber);
        r
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
    fn connect_validates_types() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.allocate_id();
        let b = g.allocate_id();
        g.add_node(NodeInstance::new(a, "Const", [0.0, 0.0])).unwrap();
        g.add_node(NodeInstance::new(b, "Add", [0.0, 0.0])).unwrap();

        // Wrong socket name → SocketNotFound
        let e = Edge {
            from: SocketId { node: a, name: "wrong" },
            to: SocketId { node: b, name: "a" },
        };
        assert!(matches!(g.connect(e, &reg), Err(GraphError::SocketNotFound { .. })));

        // Right wiring works
        let ok = Edge {
            from: SocketId { node: a, name: "out" },
            to: SocketId { node: b, name: "a" },
        };
        g.connect(ok.clone(), &reg).unwrap();
        assert_eq!(g.edge_count(), 1);

        // Duplicate input connection rejected
        let dup = Edge {
            from: SocketId { node: a, name: "out" },
            to: SocketId { node: b, name: "a" },
        };
        assert_eq!(g.connect(dup, &reg), Err(GraphError::InputAlreadyConnected));
    }

    #[test]
    fn cycle_detection() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.allocate_id();
        let b = g.allocate_id();
        g.add_node(NodeInstance::new(a, "Add", [0.0, 0.0])).unwrap();
        g.add_node(NodeInstance::new(b, "Add", [0.0, 0.0])).unwrap();
        g.connect(
            Edge { from: SocketId { node: a, name: "out" }, to: SocketId { node: b, name: "a" } },
            &reg,
        )
        .unwrap();
        // b → a would close a cycle
        let bad = Edge {
            from: SocketId { node: b, name: "out" },
            to: SocketId { node: a, name: "a" },
        };
        assert_eq!(g.connect(bad, &reg), Err(GraphError::CycleDetected));
    }

    #[test]
    fn remove_node_detaches_edges() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.allocate_id();
        let b = g.allocate_id();
        g.add_node(NodeInstance::new(a, "Const", [0.0, 0.0])).unwrap();
        g.add_node(NodeInstance::new(b, "Add", [0.0, 0.0])).unwrap();
        g.connect(
            Edge { from: SocketId { node: a, name: "out" }, to: SocketId { node: b, name: "a" } },
            &reg,
        )
        .unwrap();
        let (_, edges) = g.remove_node(a).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn dirty_propagates_downstream_on_connect() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.allocate_id();
        let b = g.allocate_id();
        let c = g.allocate_id();
        g.add_node(NodeInstance::new(a, "Const", [0.0, 0.0])).unwrap();
        g.add_node(NodeInstance::new(b, "Add", [0.0, 0.0])).unwrap();
        g.add_node(NodeInstance::new(c, "Add", [0.0, 0.0])).unwrap();
        // Clear initial dirty flags
        for n in g.nodes.values_mut() { n.dirty = false; }

        g.connect(
            Edge { from: SocketId { node: a, name: "out" }, to: SocketId { node: b, name: "a" } },
            &reg,
        )
        .unwrap();
        assert!(g.get(b).unwrap().dirty, "destination of new edge must be dirty");
        // c is not downstream of b yet, so still clean.
        assert!(!g.get(c).unwrap().dirty);

        g.connect(
            Edge { from: SocketId { node: b, name: "out" }, to: SocketId { node: c, name: "a" } },
            &reg,
        )
        .unwrap();
        assert!(g.get(c).unwrap().dirty);
    }
}
