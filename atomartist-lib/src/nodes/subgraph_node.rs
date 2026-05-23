//! `SubgraphNodeDef` — a runtime-constructed `NodeDef` that wraps a saved
//! template `Graph` and surfaces it as a single reusable node in another
//! graph.
//!
//! How it works:
//!   1. Caller scans a template graph for input/output ports:
//!      - **Inputs** come from `GraphInput` nodes (each one's `name`
//!        property names the subgraph's input socket).
//!      - **Outputs** come from the unified `Output` node's mirror
//!        outputs (every output other than the internal `__display__`
//!        is a publishable subgraph output).
//!   2. `SubgraphNodeDef::build` snapshots the template + the I/O port
//!      mapping into a struct ready to register.
//!   3. On every parent-graph evaluation, `evaluate`:
//!        a. clones the template graph (so concurrent / repeated calls
//!           don't see stale state)
//!        b. injects each parent-supplied input value into the matching
//!           `GraphInput` node's `_injected` property
//!        c. marks every node in the clone dirty + runs `evaluate_all`
//!        d. reads each `Output` mirror's cached value and surfaces it
//!           as the matching parent output
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

/// One mapping between a parent-graph socket name and a port-defining
/// node inside the template.
///
/// For input ports the template node is a `GraphInput` and the value
/// flows in via its `_injected` property.
///
/// For output ports the template node is an `Output` node, and the
/// `mirror_uid` field names the specific mirror output socket on that
/// Output node that this subgraph port reads from. (An Output node can
/// publish many mirror outputs — one per input slot the user wired.)
#[derive(Clone, Debug)]
struct PortBinding {
    socket_name: Arc<str>,
    template_node_id: NodeId,
    socket_type: SocketType,
    /// For output ports only: the uid of the specific output socket on
    /// the template Output node. Unused for input ports.
    mirror_uid: Option<crate::graph::socket::SocketUid>,
}

pub struct SubgraphNodeDef {
    type_id: &'static str,
    display_name: &'static str,
    category: &'static str,
    template: Graph,
    inputs: Vec<PortBinding>,
    outputs: Vec<PortBinding>,
}

/// Internal output-socket name on `Output` carrying the merged display
/// mesh. Subgraphs skip this socket — it's the viewport's private
/// channel, not a publishable port.
const OUTPUT_DISPLAY_NAME: &str = "__display__";

impl SubgraphNodeDef {
    /// Build from a template graph plus the desired type id / display
    /// name. Scans the template for `GraphInput` nodes (one input port
    /// each, named by their `name` property) and `Output` nodes (one
    /// output port per mirror output socket, named by that socket's
    /// internal name).
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
            match node.type_id.as_ref() {
                "GraphInput" => {
                    let port_name = match node.properties.get("name") {
                        Some(PortValue::StringVal(s)) => s.as_str().to_string(),
                        _ => continue,
                    };
                    let socket_type = node
                        .output_by_name("out")
                        .map(|s| s.socket_type)
                        .unwrap_or(SocketType::Geometry3d);
                    inputs.insert(
                        port_name.clone(),
                        PortBinding {
                            socket_name: Arc::from(port_name.as_str()),
                            template_node_id: node.id,
                            socket_type,
                            mirror_uid: None,
                        },
                    );
                }
                "Output" => {
                    // Each mirror output on the Output node is a
                    // publishable subgraph port. Skip the private
                    // `__display__` synthetic socket.
                    for sock in &node.outputs {
                        if sock.name.as_ref() == OUTPUT_DISPLAY_NAME {
                            continue;
                        }
                        let port_name = sock.name.to_string();
                        outputs.insert(
                            port_name.clone(),
                            PortBinding {
                                socket_name: Arc::from(port_name.as_str()),
                                template_node_id: node.id,
                                socket_type: sock.socket_type,
                                mirror_uid: Some(sock.uid),
                            },
                        );
                    }
                }
                _ => continue,
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

        // Pull each Output-mirror's cached value into the parent-facing
        // NodeOutputs. Each subgraph output port is bound to one mirror
        // output socket on an `Output` node inside the template; the
        // mirror's uid was recorded at build time.
        let mut out = NodeOutputs::default();
        for binding in &self.outputs {
            let mirror_uid = match binding.mirror_uid {
                Some(uid) => uid,
                None => continue,
            };
            let v = scratch
                .get(binding.template_node_id)
                .and_then(|n| n.cached_outputs.get(&mirror_uid).cloned())
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
