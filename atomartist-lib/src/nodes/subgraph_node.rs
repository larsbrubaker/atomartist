//! `SubgraphNodeDef` — a runtime-constructed `NodeDef` that wraps a saved
//! template `Graph` and surfaces it as a single reusable node in another
//! graph.
//!
//! How it works:
//!   1. Caller scans a template graph for `GraphInput` / `GraphOutput`
//!      nodes and reads each one's `name` property to enumerate the
//!      subgraph's input + output sockets.
//!   2. `SubgraphNodeDef::new` snapshots the template + the I/O port
//!      mapping into a struct ready to register.
//!   3. On every parent-graph evaluation, `evaluate`:
//!        a. clones the template graph (so concurrent / repeated calls
//!           don't see stale state)
//!        b. injects each parent-supplied input value into the matching
//!           `GraphInput` node's `_injected` property
//!        c. marks every node in the clone dirty + runs `evaluate_all`
//!        d. reads each `GraphOutput`'s `cached_outputs.in` (passthrough
//!           result) and surfaces it as the matching parent output
//!
//! Owned strings: `NodeDef::type_id()` returns `&'static str`. We
//! satisfy that by leaking the user-supplied subgraph names + socket
//! names once at registration via `Box::leak`. One leak per subgraph
//! type registration, bounded by user actions — acceptable for an
//! interactive tool.

use std::collections::HashMap;

use crate::graph::executor::evaluate_all;
use crate::graph::node::{NodeId, PortValue};
use crate::graph::Graph;
use crate::registry::{
    NodeDef, NodeError, NodeInputs, NodeOutputs, NodeProperties, NodeRegistry, PropDef, SocketDef,
};
use crate::socket_types::SocketType;

/// One mapping between a parent-graph socket name and a `GraphInput` /
/// `GraphOutput` node inside the template.
#[derive(Clone, Debug)]
struct PortBinding {
    /// Static reference the registry stores. Leaked from the
    /// user-supplied name string at construction time.
    socket_name: &'static str,
    /// Id of the node inside the template that hosts this port.
    template_node_id: NodeId,
    /// Logical type carried over the port — copied from the
    /// GraphInput / GraphOutput's declared output socket type.
    socket_type: SocketType,
}

pub struct SubgraphNodeDef {
    type_id: &'static str,
    display_name: &'static str,
    category: &'static str,
    template: Graph,
    inputs: Vec<PortBinding>,
    outputs: Vec<PortBinding>,
}

impl SubgraphNodeDef {
    /// Build from a template graph plus the desired type id / display
    /// name. Scans the template for GraphInput / GraphOutput nodes and
    /// reads each one's `name` property to determine port mapping. If
    /// two GraphInputs share the same name the later one wins.
    pub fn build(
        type_id: impl Into<String>,
        display_name: impl Into<String>,
        category: impl Into<String>,
        template: Graph,
    ) -> Self {
        let mut inputs: HashMap<String, PortBinding> = HashMap::new();
        let mut outputs: HashMap<String, PortBinding> = HashMap::new();

        // Helper: infer the socket type of a GraphInput by looking at
        // the first downstream edge from its `out` socket and reading
        // the target socket's declared type. Returns Geometry3d as a
        // fallback when there's no wiring (subgraph builder hasn't
        // connected the input yet).
        let infer_input_type = |node_id: NodeId| -> SocketType {
            for edge in template.edges() {
                if edge.from.node == node_id && edge.from.name == "out" {
                    let target = match template.get(edge.to.node) {
                        Some(n) => n, None => continue,
                    };
                    // Look up the target's NodeDef inline — we can't ask
                    // the registry here because we may be in the middle
                    // of building it. Use a fresh local registry.
                    let local = build_template_registry();
                    if let Some(def) = local.get(target.type_id) {
                        if let Some(sock) = def.input_sockets()
                            .into_iter()
                            .find(|s| s.name == edge.to.name)
                        {
                            return sock.socket_type;
                        }
                    }
                }
            }
            SocketType::Geometry3d
        };

        // Symmetric helper for GraphOutput — type comes from whatever
        // is upstream feeding its `in` socket.
        let infer_output_type = |node_id: NodeId| -> SocketType {
            for edge in template.edges() {
                if edge.to.node == node_id && edge.to.name == "in" {
                    let source = match template.get(edge.from.node) {
                        Some(n) => n, None => continue,
                    };
                    let local = build_template_registry();
                    if let Some(def) = local.get(source.type_id) {
                        if let Some(sock) = def.output_sockets()
                            .into_iter()
                            .find(|s| s.name == edge.from.name)
                        {
                            return sock.socket_type;
                        }
                    }
                }
            }
            SocketType::Geometry3d
        };

        for node in template.nodes() {
            let port_name = match node.properties.get("name") {
                Some(PortValue::StringVal(s)) => s.as_str().to_string(),
                _ => continue,
            };
            let leaked_socket: &'static str = Box::leak(port_name.clone().into_boxed_str());
            let socket_type = match node.type_id {
                "GraphInput" => infer_input_type(node.id),
                "GraphOutput" => infer_output_type(node.id),
                _ => continue,
            };
            let binding = PortBinding {
                socket_name: leaked_socket,
                template_node_id: node.id,
                socket_type,
            };
            match node.type_id {
                "GraphInput" => { inputs.insert(port_name, binding); }
                "GraphOutput" => { outputs.insert(port_name, binding); }
                _ => {}
            }
        }

        // Deterministic order: sort by socket name so port lists are
        // stable across runs.
        let mut input_list: Vec<PortBinding> = inputs.into_values().collect();
        input_list.sort_by(|a, b| a.socket_name.cmp(b.socket_name));
        let mut output_list: Vec<PortBinding> = outputs.into_values().collect();
        output_list.sort_by(|a, b| a.socket_name.cmp(b.socket_name));

        let type_id_static: &'static str = Box::leak(type_id.into().into_boxed_str());
        let display_name_static: &'static str = Box::leak(display_name.into().into_boxed_str());
        let category_static: &'static str = Box::leak(category.into().into_boxed_str());

        Self {
            type_id: type_id_static,
            display_name: display_name_static,
            category: category_static,
            template,
            inputs: input_list,
            outputs: output_list,
        }
    }
}

impl NodeDef for SubgraphNodeDef {
    fn type_id(&self) -> &'static str { self.type_id }
    fn display_name(&self) -> &'static str { self.display_name }
    fn category(&self) -> &'static str { self.category }

    fn input_sockets(&self) -> Vec<SocketDef> {
        self.inputs
            .iter()
            .map(|b| SocketDef::optional(b.socket_name, b.socket_type))
            .collect()
    }

    fn output_sockets(&self) -> Vec<SocketDef> {
        self.outputs
            .iter()
            .map(|b| SocketDef::required(b.socket_name, b.socket_type))
            .collect()
    }

    fn properties(&self) -> Vec<PropDef> { Vec::new() }

    fn evaluate(&self, inputs: &NodeInputs, _props: &NodeProperties) -> Result<NodeOutputs, NodeError> {
        // Defensive clone — the template itself stays untouched so
        // multiple instances of this subgraph type evaluate independently.
        // For very large templates this clone is the dominant cost; a
        // future optimization could keep a per-instance scratch graph.
        let mut scratch = clone_graph(&self.template);

        // Build a registry containing only the node types the template
        // mentions. We can't use the full parent registry here because
        // SubgraphNodeDef itself is in it — recursively instantiating
        // would risk an infinite loop. The local registry includes the
        // built-ins (excluding subgraphs).
        let local_reg = build_template_registry();

        // Inject parent inputs into matching GraphInput nodes via
        // their `_injected` property.
        for binding in &self.inputs {
            let value = inputs.get(binding.socket_name).clone();
            if let Some(node) = scratch.get_mut(binding.template_node_id) {
                node.properties.insert("_injected", value);
                node.dirty = true;
            }
        }
        // Mark every node dirty so evaluate_all walks the whole DAG.
        for n in scratch.nodes_mut() {
            n.dirty = true;
        }

        evaluate_all(&mut scratch, &local_reg)
            .map_err(|e| NodeError::msg(format!("subgraph eval: {}", e)))?;

        // Pull each GraphOutput's `out` (passthrough of its `in`) into
        // the parent-facing NodeOutputs.
        let mut out = NodeOutputs::default();
        for binding in &self.outputs {
            let v = scratch
                .get(binding.template_node_id)
                .and_then(|n| n.cached_outputs.get("out").cloned())
                .unwrap_or(PortValue::None);
            out.set(binding.socket_name, v);
        }
        Ok(out)
    }
}

/// Register a `SubgraphNodeDef` built from a template `Graph` into the
/// caller's registry. Returns the leaked `&'static str` type id so the
/// caller can refer to the new node type later (e.g. when
/// instantiating it in a parent graph).
pub fn register_subgraph(
    reg: &mut NodeRegistry,
    type_id: impl Into<String>,
    display_name: impl Into<String>,
    template: Graph,
) -> &'static str {
    let def = SubgraphNodeDef::build(type_id, display_name, "Components", template);
    let id = def.type_id();
    reg.register(def);
    id
}

/// Clone a graph — duplicate nodes (including their property maps) and
/// edges. NodeIds are preserved (since we drive evaluation by id, not
/// position).
fn clone_graph(src: &Graph) -> Graph {
    let mut out = Graph::new();
    for n in src.nodes() {
        let _ = out.add_node(n.clone());
    }
    for e in src.edges() {
        out.edges_mut().push(e.clone());
    }
    out
}

/// Build a registry containing only the built-in node types used inside
/// subgraph templates. Crucially does NOT include `SubgraphNodeDef`s
/// themselves — recursive subgraphs are deferred (they need a per-call
/// recursion-depth guard to avoid infinite loops in malformed inputs).
fn build_template_registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    super::register_all(&mut reg);
    reg
}

// Add a `nodes_mut` accessor to Graph if one doesn't exist yet; the
// registration code above needs it. The accessor is kept module-private
// here for clarity but exposed as an inherent method on `Graph` so other
// callers (the executor) don't have to walk a HashMap directly.
//
// (No code here — the accessor lives on Graph itself; see
// `crate::graph::graph::Graph::nodes_mut`.)
