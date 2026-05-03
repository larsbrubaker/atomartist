//! Node-type registry.
//!
//! Each node type (Box, Cylinder, Extrude, Transform, ...) is described by
//! an implementation of the `NodeDef` trait and registered into a
//! `NodeRegistry` at startup. The registry powers two callers:
//!
//!   1. The graph executor — given a `NodeInstance.type_id`, it looks up the
//!      `NodeDef` and calls `evaluate` to produce output values.
//!   2. The UI canvas — it iterates the registry to populate the "add node"
//!      menu, grouped by `category`.
//!
//! No proc-macro magic; registration is a plain function call. Each node
//! module exposes a `register(reg: &mut NodeRegistry)` function that the
//! library's top-level `register_all_nodes()` invokes.

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::socket_types::SocketType;

/// Description of one named input or output socket on a node type.
#[derive(Clone, Debug)]
pub struct SocketDef {
    pub name: &'static str,
    pub socket_type: SocketType,
    /// True when the socket is allowed to be unconnected (the executor will
    /// pass `PortValue::None` if no edge is wired).
    pub optional: bool,
}

impl SocketDef {
    pub fn required(name: &'static str, socket_type: SocketType) -> Self {
        Self { name, socket_type, optional: false }
    }
    pub fn optional(name: &'static str, socket_type: SocketType) -> Self {
        Self { name, socket_type, optional: true }
    }
}

/// Description of one settable property on a node type.
///
/// Properties are values stored on the node itself (as opposed to flowing in
/// over a socket connection); they appear as widgets on the node box on the
/// canvas, and as rows in the right-side property panel when the node is
/// selected. Numeric properties may carry hints about their range and step
/// for sliders / drag-values; the UI layer uses these but the executor
/// ignores them.
#[derive(Clone, Debug)]
pub struct PropDef {
    pub name: &'static str,
    pub default: PortValue,
    /// Inclusive minimum for numeric properties. `None` for non-numeric or
    /// unbounded values.
    pub min: Option<f64>,
    /// Inclusive maximum.
    pub max: Option<f64>,
}

impl PropDef {
    pub fn new(name: &'static str, default: PortValue) -> Self {
        Self { name, default, min: None, max: None }
    }
    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }
}

/// Inputs handed to `NodeDef::evaluate`: for each declared input socket the
/// executor inserts the upstream value (or `PortValue::None` if unconnected
/// and the socket is optional).
#[derive(Default)]
pub struct NodeInputs {
    pub by_name: HashMap<&'static str, PortValue>,
}

impl NodeInputs {
    pub fn get(&self, name: &str) -> &PortValue {
        self.by_name.get(name).unwrap_or(&PortValue::None)
    }
    pub fn insert(&mut self, name: &'static str, value: PortValue) {
        self.by_name.insert(name, value);
    }
}

/// Property snapshot handed to `NodeDef::evaluate`. The executor copies a
/// node's `properties` map into here at evaluation time so node code never
/// touches mutable state.
#[derive(Default)]
pub struct NodeProperties {
    pub by_name: HashMap<&'static str, PortValue>,
}

impl NodeProperties {
    pub fn get(&self, name: &str) -> &PortValue {
        self.by_name.get(name).unwrap_or(&PortValue::None)
    }

    /// Convenience accessor that unwraps `PortValue::Number`, returning the
    /// `default` if the property is missing or wrong-typed.
    pub fn number(&self, name: &str, default: f64) -> f64 {
        match self.get(name) {
            PortValue::Number(n) => *n,
            _ => default,
        }
    }

    pub fn bool_(&self, name: &str, default: bool) -> bool {
        match self.get(name) {
            PortValue::Bool(b) => *b,
            _ => default,
        }
    }

    pub fn insert(&mut self, name: &'static str, value: PortValue) {
        self.by_name.insert(name, value);
    }
}

/// Outputs returned by `NodeDef::evaluate`, one entry per declared output
/// socket. The executor stores these on the node's `cached_outputs` map and
/// uses them as inputs to downstream nodes.
#[derive(Default)]
pub struct NodeOutputs {
    pub by_name: HashMap<&'static str, PortValue>,
}

impl NodeOutputs {
    pub fn set(&mut self, name: &'static str, value: PortValue) {
        self.by_name.insert(name, value);
    }
}

/// Errors a node may raise during evaluation. Currently a single string
/// variant; more structured variants can be added as nodes need them.
#[derive(Clone, Debug)]
pub enum NodeError {
    Message(String),
}

impl NodeError {
    pub fn msg(s: impl Into<String>) -> Self {
        NodeError::Message(s.into())
    }
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::Message(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for NodeError {}

/// One registered node type — describes its sockets, properties, category,
/// and how to evaluate it.
pub trait NodeDef: Send + Sync {
    /// Stable identifier used for serialization and registry lookup.
    fn type_id(&self) -> &'static str;

    /// Human-readable label used in the "add node" menu.
    fn display_name(&self) -> &'static str {
        self.type_id()
    }

    /// Menu category, e.g. "Primitives 3D", "Operations 3D", "Math".
    fn category(&self) -> &'static str;

    fn input_sockets(&self) -> Vec<SocketDef>;
    fn output_sockets(&self) -> Vec<SocketDef>;

    fn properties(&self) -> Vec<PropDef> {
        Vec::new()
    }

    /// Compute outputs from inputs and properties. Pure function — must not
    /// stash mutable state on `&self` and must be safe to call from a
    /// background thread.
    fn evaluate(
        &self,
        inputs: &NodeInputs,
        props: &NodeProperties,
    ) -> Result<NodeOutputs, NodeError>;
}

/// Map of `type_id` to `Arc<dyn NodeDef>`. Constructed once at startup and
/// shared across the executor and UI via `Arc<NodeRegistry>`.
#[derive(Default)]
pub struct NodeRegistry {
    by_type_id: HashMap<&'static str, Arc<dyn NodeDef>>,
    /// Insertion order of type ids, preserved so the "add node" menu shows
    /// node types in a predictable order.
    order: Vec<&'static str>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<N: NodeDef + 'static>(&mut self, def: N) {
        let id = def.type_id();
        debug_assert!(
            !self.by_type_id.contains_key(id),
            "node type '{}' already registered",
            id
        );
        self.by_type_id.insert(id, Arc::new(def));
        self.order.push(id);
    }

    pub fn get(&self, type_id: &str) -> Option<&Arc<dyn NodeDef>> {
        self.by_type_id.get(type_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn NodeDef>> {
        self.order
            .iter()
            .filter_map(move |id| self.by_type_id.get(id))
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Group registered node types by their `category()` for menu display.
    pub fn by_category(&self) -> Vec<(&'static str, Vec<Arc<dyn NodeDef>>)> {
        let mut buckets: HashMap<&'static str, Vec<Arc<dyn NodeDef>>> = HashMap::new();
        let mut order: Vec<&'static str> = Vec::new();
        for id in &self.order {
            if let Some(def) = self.by_type_id.get(id) {
                let cat = def.category();
                if !buckets.contains_key(cat) {
                    order.push(cat);
                }
                buckets.entry(cat).or_default().push(def.clone());
            }
        }
        order.into_iter().map(|c| (c, buckets.remove(c).unwrap_or_default())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyNode;
    impl NodeDef for DummyNode {
        fn type_id(&self) -> &'static str { "Dummy" }
        fn category(&self) -> &'static str { "Test" }
        fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
        fn output_sockets(&self) -> Vec<SocketDef> {
            vec![SocketDef::required("out", SocketType::Number)]
        }
        fn evaluate(&self, _i: &NodeInputs, _p: &NodeProperties) -> Result<NodeOutputs, NodeError> {
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(42.0));
            Ok(o)
        }
    }

    struct OtherNode;
    impl NodeDef for OtherNode {
        fn type_id(&self) -> &'static str { "Other" }
        fn category(&self) -> &'static str { "Test" }
        fn input_sockets(&self) -> Vec<SocketDef> { vec![] }
        fn output_sockets(&self) -> Vec<SocketDef> { vec![] }
        fn evaluate(&self, _i: &NodeInputs, _p: &NodeProperties) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
    }

    #[test]
    fn registry_round_trip() {
        let mut reg = NodeRegistry::new();
        reg.register(DummyNode);
        reg.register(OtherNode);
        assert_eq!(reg.len(), 2);
        assert!(reg.get("Dummy").is_some());
        assert!(reg.get("Missing").is_none());
        let cats = reg.by_category();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].0, "Test");
        assert_eq!(cats[0].1.len(), 2);
    }

    #[test]
    fn properties_default_to_none_when_missing() {
        let p = NodeProperties::default();
        assert_eq!(p.number("missing", 7.5), 7.5);
        assert!(!p.bool_("missing", false));
    }
}
