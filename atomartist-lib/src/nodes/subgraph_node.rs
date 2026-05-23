//! `SubgraphNodeDef` — a runtime-constructed `NodeDef` that wraps a saved
//! template `Graph` and surfaces it as a single reusable node in another
//! graph.
//!
//! How it works:
//!   1. Caller scans a template graph for `GraphInput` / `GraphOutput`
//!      nodes and reads each one's `name` property to enumerate the
//!      subgraph's input + output sockets.
//!   2. `SubgraphNodeDef::build` snapshots the template + the I/O port
//!      mapping into a struct ready to register.
//!   3. On every parent-graph evaluation, `evaluate`:
//!        a. clones the template graph (so concurrent / repeated calls
//!           don't see stale state)
//!        b. injects each parent-supplied input value into the matching
//!           `GraphInput` node's `_injected` property
//!        c. marks every node in the clone dirty + runs `evaluate_all`
//!        d. reads each `GraphOutput`'s cached `out` value (passthrough
//!           of its `in`) and surfaces it as the matching parent output
//!
//! Owned strings: `NodeDef::type_id()` returns `&'static str`. We
//! satisfy that by leaking the user-supplied subgraph names + socket
//! names once at registration via `Box::leak`. One leak per subgraph
//! type registration, bounded by user actions — acceptable for an
//! interactive tool.

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::executor::evaluate_all;
use crate::graph::node::{NodeId, PortValue};
use crate::graph::socket::SocketUidAlloc;
use crate::graph::Graph;
use crate::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

/// One mapping between a parent-graph socket name and a `GraphInput` /
/// `GraphOutput` node inside the template.
#[derive(Clone, Debug)]
struct PortBinding {
    /// Static reference the registry stores. Leaked from the
    /// user-supplied name string at construction time.
    socket_name: Arc<str>,
    /// Id of the node inside the template that hosts this port.
    template_node_id: NodeId,
    /// Logical type carried over the port.
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
    /// reads each one's `name` property to determine port mapping.
    ///
    /// Socket types are read straight from the template instance's
    /// sockets — these were resolved by the template node's
    /// `instantiate` when the user added it, so they're already correct.
    pub fn build(
        type_id: impl Into<String>,
        display_name: impl Into<String>,
        category: impl Into<String>,
        template: Graph,
    ) -> Self {
        let mut inputs: HashMap<String, PortBinding> = HashMap::new();
        let mut outputs: HashMap<String, PortBinding> = HashMap::new();

        for node in template.nodes() {
            let port_name = match node.properties.get("name") {
                Some(PortValue::StringVal(s)) => s.as_str().to_string(),
                _ => continue,
            };
            let socket_type = match node.type_id.as_ref() {
                // GraphInput exposes data via its `out` socket; that's the
                // type the subgraph user will see on the parent input.
                "GraphInput" => node
                    .output_by_name("out")
                    .map(|s| s.socket_type)
                    .unwrap_or(SocketType::Geometry3d),
                // GraphOutput's input `in` is the type flowing into it.
                "GraphOutput" => node
                    .input_by_name("in")
                    .map(|s| s.socket_type)
                    .unwrap_or(SocketType::Geometry3d),
                _ => continue,
            };
            let binding = PortBinding {
                socket_name: Arc::from(port_name.as_str()),
                template_node_id: node.id,
                socket_type,
            };
            match node.type_id.as_ref() {
                "GraphInput" => { inputs.insert(port_name, binding); }
                "GraphOutput" => { outputs.insert(port_name, binding); }
                _ => {}
            }
        }

        let mut input_list: Vec<PortBinding> = inputs.into_values().collect();
        input_list.sort_by(|a, b| a.socket_name.cmp(&b.socket_name));
        let mut output_list: Vec<PortBinding> = outputs.into_values().collect();
        output_list.sort_by(|a, b| a.socket_name.cmp(&b.socket_name));

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

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        let mut b = InstanceTemplate::builder(alloc);
        for port in &self.inputs {
            b = b.input_opt(port.socket_name.clone(), port.socket_type);
        }
        for port in &self.outputs {
            b = b.output(port.socket_name.clone(), port.socket_type);
        }
        b.build()
    }

    fn properties(&self) -> Vec<PropDef> { Vec::new() }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        // Defensive clone — the template itself stays untouched so
        // multiple instances of this subgraph type evaluate independently.
        let mut scratch = clone_graph(&self.template);

        let local_reg = build_template_registry();

        // Inject parent inputs into matching GraphInput nodes via
        // their `_injected` property.
        for binding in &self.inputs {
            let value = ctx.input_named(&binding.socket_name).clone();
            if let Some(node) = scratch.get_mut(binding.template_node_id) {
                node.properties.insert(Arc::from("_injected"), value);
                node.dirty = true;
            }
        }
        // Mark every node dirty so evaluate_all walks the whole DAG.
        for n in scratch.nodes_mut() {
            n.dirty = true;
        }

        evaluate_all(&mut scratch, &local_reg)
            .map_err(|e| NodeError::msg(format!("subgraph eval: {}", e)))?;

        // Pull each GraphOutput's `out` cached value (passthrough of its
        // `in`) into the parent-facing NodeOutputs.
        let mut out = NodeOutputs::default();
        for binding in &self.outputs {
            let v = scratch
                .get(binding.template_node_id)
                .and_then(|n| {
                    let out_uid = n.output_by_name("out")?.uid;
                    n.cached_outputs.get(&out_uid).cloned()
                })
                .unwrap_or(PortValue::None);
            out.set(binding.socket_name.clone(), v);
        }
        Ok(out)
    }
}

/// Register a `SubgraphNodeDef` built from a template `Graph` into the
/// caller's registry.
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
/// edges. NodeIds and socket uids are preserved (we drive evaluation by
/// stable identity, not position).
fn clone_graph(src: &Graph) -> Graph {
    let mut out = Graph::new();
    for n in src.nodes() {
        let _ = out.add_node(n.clone());
    }
    for n in src.noodles() {
        out.noodles_mut().push(*n);
    }
    out
}

/// Build a registry containing only the built-in node types used inside
/// subgraph templates. Crucially does NOT include `SubgraphNodeDef`s
/// themselves — recursive subgraphs are deferred.
fn build_template_registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    super::register_all(&mut reg);
    reg
}
