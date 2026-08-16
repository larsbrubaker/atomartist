//! `SubgraphNodeDef` — a runtime-constructed `NodeDef` that wraps a
//! *shared, live* template `Graph` and surfaces it as a single reusable
//! node ("component") in another graph.
//!
//! How it works:
//!   1. The template is scanned for input/output ports:
//!      - **Inputs** come from `GraphInput` nodes (each one's `name`
//!        property names the subgraph's input socket, typed by its `out`
//!        socket type).
//!      - **Outputs** come from the unified `Output` node's mirror
//!        outputs (every output other than the internal `__display__`
//!        is a publishable subgraph output).
//!   2. `SubgraphNodeDef::build` stores the shared `Arc<Mutex<Graph>>`
//!      template — it does **not** snapshot the port mapping. The scan is
//!      recomputed on demand from the locked template every time the
//!      engine or UI asks for sockets / properties. This keeps instances
//!      *live*: once the UI drills into the template and edits it (adds /
//!      retypes a `GraphInput`, rewires the `Output`), every already-built
//!      def and every instance observes the change without re-registering.
//!   3. On every parent-graph evaluation, `evaluate`:
//!        a. clones the template graph + scans its ports under one short
//!           lock, then releases the lock (never hold it across the
//!           executor run)
//!        b. injects each input value into the matching `GraphInput`
//!           node's `_injected` property — the wired value for connected
//!           ports, or the instance's per-port override property for
//!           unconnected scalar ports
//!        c. marks every node in the clone dirty + runs `evaluate_all`
//!        d. reads each `Output` mirror's cached value and surfaces it
//!           as the matching parent output
//!
//! Editable defaults: `properties()` mints, for each scalar input port
//! (Number/Bool/String/Color — not Geometry), a `PropDef` named after the
//! port, seeded from the template `GraphInput`'s `default_*` value and
//! bound inline to the socket row (`bind_input`). An unconnected instance
//! input is therefore directly editable on the instance, seeded from the
//! template default (NodeDesigner's `inputValues` semantics).
//!
//! Stale sockets caveat: an instance's socket list is minted at
//! `instantiate` time and is *not* rebuilt when the template interface
//! changes afterward. Live port scans (properties / evaluate) reflect the
//! template immediately, but existing instances keep their old sockets
//! until a deliberate rebuild. Rebuilding existing instances on template
//! edit is deferred to the drill-in-exit step.
//!
//! Owned strings: `NodeDef::type_id()` returns `&'static str`. We
//! satisfy that by leaking the user-supplied subgraph names once at
//! registration via `Box::leak`. One leak per subgraph type registration,
//! bounded by user actions — acceptable for an interactive tool.

use std::sync::{Arc, Mutex};

use crate::graph::executor::evaluate_all;
use crate::graph::node::{NodeId, PortValue};
use crate::graph::socket::{Socket, SocketUidAlloc};
use crate::graph::Graph;
use crate::registry::{
    EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry, PropDef,
};
use crate::socket_types::SocketType;

/// One mapping between a parent-graph socket name and a port-defining
/// node inside the template. Recomputed on demand from the live template
/// (never snapshotted on the def).
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
    /// Shared, live template. The UI drills in and edits this in place;
    /// instances see edits because the scan reads it on demand.
    template: Arc<Mutex<Graph>>,
}

/// Internal output-socket name on `Output` carrying the merged display
/// mesh. Subgraphs skip this socket — it's the viewport's private
/// channel, not a publishable port.
const OUTPUT_DISPLAY_NAME: &str = "__display__";

/// Opaque white — the Color-port default when the template GraphInput has
/// none set. Matches GraphInput's own default.
const DEFAULT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Scan a template graph for its subgraph input/output ports.
///
/// - **Inputs**: one per `GraphInput` node, named by its `name` property,
///   typed by its `out` socket (already resolved by GraphInput's
///   `instantiate` / `on_property_changed`).
/// - **Outputs**: one per mirror output on the unified `Output` node,
///   skipping the private `__display__` socket.
///
/// Both lists are sorted by socket name for stable ordering. O(nodes);
/// graphs are small, so this is recomputed rather than cached.
///
/// Known limitation: two `GraphInput` nodes sharing the same `name`
/// collapse to a single port keyed by that name — the last one visited in
/// node-iteration order wins. Name de-duplication (or a uniqueness
/// constraint at edit time) is future work.
fn scan_ports(template: &Graph) -> (Vec<PortBinding>, Vec<PortBinding>) {
    use std::collections::HashMap;
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
                // Each mirror output on the Output node is a publishable
                // subgraph port. Skip the private `__display__` socket.
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
    (input_list, output_list)
}

impl SubgraphNodeDef {
    /// Build from a *shared* template graph plus the desired type id /
    /// display name / category. The template is kept live (see the module
    /// doc); ports are scanned on demand, not snapshotted here.
    pub fn build(
        type_id: impl Into<String>,
        display_name: impl Into<String>,
        category: impl Into<String>,
        template: Arc<Mutex<Graph>>,
    ) -> Self {
        let type_id_static: &'static str = Box::leak(type_id.into().into_boxed_str());
        let display_name_static: &'static str = Box::leak(display_name.into().into_boxed_str());
        let category_static: &'static str = Box::leak(category.into().into_boxed_str());

        Self {
            type_id: type_id_static,
            display_name: display_name_static,
            category: category_static,
            template,
        }
    }
}

impl NodeDef for SubgraphNodeDef {
    fn type_id(&self) -> &'static str { self.type_id }
    fn display_name(&self) -> &'static str { self.display_name }
    fn category(&self) -> &'static str { self.category }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        // Scan the live template so a freshly-added instance mints sockets
        // typed per the current GraphInput / Output interface. (Existing
        // instances keep their old sockets — see the stale-sockets caveat
        // in the module doc.)
        //
        // Recover a poisoned lock rather than degrading to EMPTY sockets: a
        // payload panic leaves the template structurally intact, so scanning
        // it still yields the correct interface. Minting a socketless
        // instance would be strictly worse — it could serialize as a
        // corrupted project.
        let guard = self.template.lock().unwrap_or_else(|e| e.into_inner());
        let (inputs, outputs) = scan_ports(&guard);
        drop(guard);
        let mut b = InstanceTemplate::builder(alloc);
        for port in &inputs {
            b = b.input_opt(port.socket_name.clone(), port.socket_type);
        }
        for port in &outputs {
            b = b.output(port.socket_name.clone(), port.socket_type);
        }
        b.build()
    }

    /// Editable inline defaults for each scalar input port. Named after
    /// the port and bound to its socket row, so an unconnected component
    /// input is editable directly on the instance. Seeded from the
    /// template GraphInput's matching `default_*` value; the editor
    /// mirrors GraphInput's per-type editor choices.
    fn properties(&self) -> Vec<PropDef> {
        // Recover a poisoned lock rather than degrading to EMPTY props: a
        // payload panic leaves the template structurally intact, so its
        // interface scan is still valid. Returning no props would strip an
        // instance of its editable inline defaults.
        //
        // Template default_* edits affect only *new* instances: a value is
        // baked into the instance's props at `add_new_node` time (the
        // PropDef default is snapshotted then). Existing instances keep
        // their stored prop, except instances lacking the stored prop (e.g.
        // older saves) which fall through to these live defaults —
        // intentional, mirrors NodeDesigner's `inputValues` semantics.
        let template = self.template.lock().unwrap_or_else(|e| e.into_inner());
        let (inputs, _outputs) = scan_ports(&template);
        let mut props = Vec::new();
        for binding in &inputs {
            let node = match template.get(binding.template_node_id) {
                Some(n) => n,
                None => continue,
            };
            let prop = match binding.socket_type {
                SocketType::Number => {
                    let def = match node.properties.get("default_number") {
                        Some(v @ PortValue::Number(_)) => v.clone(),
                        _ => PortValue::Number(0.0),
                    };
                    PropDef::new(binding.socket_name.clone(), def)
                        .with_range(-10000.0, 10000.0)
                        .bind_input(binding.socket_name.clone())
                }
                SocketType::Bool => {
                    let def = match node.properties.get("default_bool") {
                        Some(v @ PortValue::Bool(_)) => v.clone(),
                        _ => PortValue::Bool(false),
                    };
                    PropDef::new(binding.socket_name.clone(), def)
                        .with_editor(EditorKind::Toggle)
                        .bind_input(binding.socket_name.clone())
                }
                SocketType::StringVal => {
                    let def = match node.properties.get("default_string") {
                        Some(v @ PortValue::StringVal(_)) => v.clone(),
                        _ => PortValue::StringVal(Arc::new("".into())),
                    };
                    PropDef::new(binding.socket_name.clone(), def)
                        .with_editor(EditorKind::StringSingleLine)
                        .bind_input(binding.socket_name.clone())
                }
                SocketType::Color => {
                    let def = match node.properties.get("default_color") {
                        Some(v @ PortValue::Color(_)) => v.clone(),
                        _ => PortValue::Color(DEFAULT_COLOR),
                    };
                    PropDef::new(binding.socket_name.clone(), def)
                        .with_editor(EditorKind::ColorPicker)
                        .bind_input(binding.socket_name.clone())
                }
                // Geometry (and any other type) has no scalar inline
                // default — it can only be driven by a wired connection.
                _ => continue,
            };
            props.push(prop);
        }
        props
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        // Clone the template and scan its ports under one short lock, then
        // release the lock before evaluating the clone. The executor run
        // is the expensive part and must never hold the template lock
        // (keeps the door open for the future drill-in editor and for
        // recursive subgraphs, both of which want the lock).
        let (mut scratch, inputs, outputs) = {
            let template = self
                .template
                .lock()
                .map_err(|_| NodeError::msg("subgraph template lock poisoned"))?;
            let scratch = clone_graph(&template);
            let (inputs, outputs) = scan_ports(&template);
            (scratch, inputs, outputs)
        };

        let local_reg = build_template_registry();

        // Inject each parent input into its matching GraphInput node.
        for binding in &inputs {
            let socket_uid = ctx
                .instance
                .input_by_name(&binding.socket_name)
                .map(|s| s.uid);
            let connected = socket_uid
                .map(|u| ctx.inputs.by_uid.contains_key(&u))
                .unwrap_or(false);
            let value = if connected {
                // Connected: forward the wired value verbatim.
                ctx.input_named(&binding.socket_name).clone()
            } else {
                // Unconnected: use the instance's per-port override
                // property (seeded from the template GraphInput's
                // default_*). A Geometry port has no such property, so
                // this yields `None` — indistinguishable from "not
                // injected", so the template GraphInput falls back to its
                // own default. Intentional.
                //
                // Known window: after a template port retype, an instance
                // may still hold a stale override of the *old* type. It is
                // injected here as-is until `sync_instances_to_template`
                // reconciles the instance — which the UI runs on drill-in
                // exit, mitigating the window.
                ctx.properties.get(&binding.socket_name).clone()
            };
            if let Some(node) = scratch.get_mut(binding.template_node_id) {
                node.properties.insert(Arc::from("_injected"), value);
                node.dirty = true;
            }
        }
        // Mark every node dirty so evaluate_all walks the whole DAG.
        for n in scratch.nodes_mut() {
            n.dirty = true;
        }

        let report = evaluate_all(&mut scratch, &local_reg)
            .map_err(|e| NodeError::msg(format!("subgraph eval: {}", e)))?;
        // A node inside the template that refused makes the *instance*
        // fail: the parent graph has no way to badge an inner node, so
        // the component reports the first inner failure as its own.
        if let Some(failure) = report.failures.first() {
            return Err(NodeError::msg(format!("subgraph eval: {}", failure)));
        }

        // Pull each Output-mirror's cached value into the parent-facing
        // NodeOutputs. Each subgraph output port is bound to one mirror
        // output socket on an `Output` node inside the template.
        let mut out = NodeOutputs::default();
        for binding in &outputs {
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

    fn subgraph_template(&self) -> Option<Arc<Mutex<Graph>>> {
        Some(self.template.clone())
    }
}

/// Register a `SubgraphNodeDef` built from a *shared* template `Graph`
/// into the caller's registry. The caller keeps the same `Arc` to later
/// drill in and edit the template; instances observe those edits live.
pub fn register_subgraph(
    reg: &mut NodeRegistry,
    type_id: impl Into<String>,
    display_name: impl Into<String>,
    template: Arc<Mutex<Graph>>,
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

/// Rebuild the instance socket layout of every node in `parent` that is
/// an instance of the component backed by `template`, so stale instances
/// catch up with template interface edits (added / removed / retyped
/// ports). Called by the UI when the user exits a drilled-in component.
///
/// Reconciliation rules (mirroring NodeDesigner's `restoreConnections`):
///   - Ports are matched by **name** — a surviving port keeps its socket
///     uid (and therefore its attached noodles).
///   - A port present in the template but missing on the instance is
///     added (fresh uid from the parent allocator).
///   - A port on the instance that vanished from the template is removed,
///     disconnecting any noodle touching it first (via the graph's
///     GC-on-remove socket helpers).
///   - A port whose type changed is retyped in place; noodles left
///     type-incompatible are then dropped by `revalidate_node_noodles`.
///   - Instance sockets are ordered to match the template scan order.
///
/// Every reconciled instance is marked dirty so the next evaluation
/// recomputes it with the new interface.
///
/// KNOWN v1 GAP (undo): this mutates the parent graph *outside* any undo
/// stack — sockets are added/removed and noodles severed directly on the
/// graph, not through reversible commands. A destructive template edit
/// (removing or retyping a port that had wired instances) can therefore
/// sever root noodles irreversibly: the exit-time reconciliation is not
/// captured on the root undo stack. A follow-up will wrap this in a
/// reversible command (or snapshot the parent noodles) so component
/// interface edits round-trip through undo like every other mutation.
pub fn sync_instances_to_template(
    parent: &mut Graph,
    registry: &NodeRegistry,
    template: &Arc<Mutex<Graph>>,
) {
    // Scan the template interface once (short lock).
    let (want_inputs, want_outputs) = match template.lock() {
        Ok(t) => scan_ports(&t),
        Err(_) => return,
    };

    // Collect the parent nodes that are instances of exactly this
    // template (pointer identity on the shared template Arc).
    let instances: Vec<NodeId> = parent
        .nodes()
        .filter_map(|n| {
            let def = registry.get(&n.type_id)?;
            let tpl = def.subgraph_template()?;
            if Arc::ptr_eq(&tpl, template) {
                Some(n.id)
            } else {
                None
            }
        })
        .collect();

    for node in instances {
        reconcile_one_side(parent, node, &want_inputs, true);
        reconcile_one_side(parent, node, &want_outputs, false);
        // Drop any noodle a retype left type-incompatible.
        let _ = parent.revalidate_node_noodles(node);
        parent.mark_dirty_subtree(node);
        if let Some(n) = parent.get_mut(node) {
            n.dirty = true;
        }
    }
}

/// Reconcile one socket direction (inputs when `is_input`, else outputs)
/// of a single subgraph instance against the desired port list `want`.
/// See [`sync_instances_to_template`] for the matching rules.
fn reconcile_one_side(
    parent: &mut Graph,
    node: NodeId,
    want: &[PortBinding],
    is_input: bool,
) {
    use std::collections::{HashMap, HashSet};

    // Snapshot current sockets (name, uid, type) on this side.
    let current: Vec<(Arc<str>, crate::graph::socket::SocketUid, SocketType)> = {
        let n = match parent.get(node) {
            Some(n) => n,
            None => return,
        };
        let list = if is_input { &n.inputs } else { &n.outputs };
        list.iter()
            .map(|s| (s.name.clone(), s.uid, s.socket_type))
            .collect()
    };
    let want_names: HashSet<&str> = want.iter().map(|p| p.socket_name.as_ref()).collect();

    // Remove sockets whose name vanished from the template (GCs noodles).
    for (name, uid, _) in &current {
        if !want_names.contains(name.as_ref()) {
            if is_input {
                let _ = parent.remove_input_socket(node, *uid);
            } else {
                let _ = parent.remove_output_socket(node, *uid);
            }
        }
    }

    // Retype survivors whose type changed; append ports that are new.
    for binding in want {
        match current
            .iter()
            .find(|(n, _, _)| n.as_ref() == binding.socket_name.as_ref())
        {
            Some((_, uid, ty)) => {
                if *ty != binding.socket_type {
                    let _ = parent.retype_socket(node, *uid, binding.socket_type);
                }
            }
            None => {
                let uid = parent.allocate_socket_uid();
                // Subgraph instance inputs are always optional (an
                // unconnected component input falls back to its inline
                // default) — matching `instantiate`'s `input_opt` minting.
                // Outputs are non-optional. The `optional` flag therefore
                // tracks `is_input`, but name it so the intent is explicit
                // rather than an incidental coincidence.
                let optional = is_input;
                let sock =
                    Socket::new(uid, binding.socket_name.clone(), binding.socket_type, optional);
                if is_input {
                    let _ = parent.append_input_socket(node, sock);
                } else {
                    let _ = parent.append_output_socket(node, sock);
                }
            }
        }
    }

    // Reorder the surviving sockets to match template scan order.
    let order: HashMap<&str, usize> = want
        .iter()
        .enumerate()
        .map(|(i, p)| (p.socket_name.as_ref(), i))
        .collect();
    if let Some(n) = parent.get_mut(node) {
        let list = if is_input { &mut n.inputs } else { &mut n.outputs };
        list.sort_by_key(|s| *order.get(s.name.as_ref()).unwrap_or(&usize::MAX));
    }
}

#[cfg(test)]
mod eval_contract_tests {
    use super::*;
    use crate::graph::graph::Noodle;

    fn str_val(s: &str) -> PortValue {
        PortValue::StringVal(Arc::new(s.into()))
    }

    /// Test fixture: a Number-typed source whose `evaluate` yields
    /// `PortValue::None`. Models an upstream node that is *wired* but
    /// produces no value this frame — the executor still records a
    /// `by_uid` entry for the incoming noodle, so the target port is
    /// classified as connected.
    struct NoneNumberSource;
    impl NodeDef for NoneNumberSource {
        fn type_id(&self) -> &'static str { "NoneNumberSource" }
        fn category(&self) -> &'static str { "Test" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::None);
            Ok(o)
        }
    }

    /// A component input port that is CONNECTED to an upstream producing
    /// `PortValue::None` must inject that wired `None` (letting the
    /// template GraphInput fall back to its *own* default), NOT the
    /// instance's per-port override property. The executor inserts a
    /// `by_uid` entry for every incoming noodle, so `contains_key`
    /// classifies the port as connected even when the wired value is
    /// `None` — this test locks that contract in.
    #[test]
    fn connected_none_upstream_ignores_instance_override() {
        let mut reg = NodeRegistry::new();
        super::super::register_all(&mut reg);
        reg.register(NoneNumberSource);
        let template = Arc::new(Mutex::new(Graph::new()));
        let type_id = register_subgraph(&mut reg, "Comp", "Comp", template.clone());
        let reg = Arc::new(reg);

        // Template: GraphInput "val" (Number, default 7.0) → Output. The
        // Output mirrors "out" so the subgraph publishes an "out" port.
        {
            let mut t = template.lock().unwrap();
            let gin = t.add_new_node("GraphInput", [0.0, 0.0], &reg).unwrap();
            t.set_property_hooked(gin, "port_type", str_val("Number"), &reg)
                .unwrap();
            t.set_property(gin, "name", str_val("val")).unwrap();
            t.set_property(gin, "default_number", PortValue::Number(7.0))
                .unwrap();
            let out = t.add_new_node("Output", [200.0, 0.0], &reg).unwrap();
            let gin_out = t.get(gin).unwrap().output_by_name("out").unwrap().uid;
            let out_in = t.get(out).unwrap().inputs[0].uid;
            t.connect(Noodle::new(gin, gin_out, out, out_in), &reg)
                .unwrap();
        }

        // Parent: NoneNumberSource.out → instance "val". Give the instance
        // a distinct per-port override (999) that must be ignored because
        // the port is connected.
        let mut parent = Graph::new();
        let inst = parent.add_new_node(type_id, [0.0, 0.0], &reg).unwrap();
        parent
            .set_property(inst, "val", PortValue::Number(999.0))
            .unwrap();
        let src = parent
            .add_new_node("NoneNumberSource", [0.0, 0.0], &reg)
            .unwrap();
        let src_out = parent.get(src).unwrap().output_by_name("out").unwrap().uid;
        let in_uid = parent.get(inst).unwrap().input_by_name("val").unwrap().uid;
        parent
            .connect(Noodle::new(src, src_out, inst, in_uid), &reg)
            .unwrap();

        crate::graph::executor::evaluate_all(&mut parent, &reg).unwrap().expect_clean();

        let out_uid = parent.get(inst).unwrap().output_by_name("out").unwrap().uid;
        let value = parent
            .get(inst)
            .unwrap()
            .cached_outputs
            .get(&out_uid)
            .cloned();
        assert_eq!(
            value,
            Some(PortValue::Number(7.0)),
            "connected-to-None injects wired None → GraphInput default (7.0), \
             never the instance override (999)",
        );
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use crate::graph::graph::Noodle;
    use crate::registry::NodeRegistry;

    fn str_val(s: &str) -> PortValue {
        PortValue::StringVal(Arc::new(s.into()))
    }

    /// Build a registry with all built-ins plus one subgraph type named
    /// `"Comp"` backed by a fresh (empty) template. Returns the registry,
    /// the shared template Arc, and the subgraph type id.
    fn setup() -> (Arc<NodeRegistry>, Arc<Mutex<Graph>>, &'static str) {
        let mut reg = NodeRegistry::new();
        super::super::register_all(&mut reg);
        let template = Arc::new(Mutex::new(Graph::new()));
        let type_id = register_subgraph(&mut reg, "Comp", "Comp", template.clone());
        (Arc::new(reg), template, type_id)
    }

    /// Add a typed GraphInput to `template`, returning its NodeId.
    fn add_input_port(
        template: &Arc<Mutex<Graph>>,
        reg: &NodeRegistry,
        name: &str,
        port_type: &str,
    ) -> NodeId {
        let mut t = template.lock().unwrap();
        let gin = t.add_new_node("GraphInput", [0.0, 0.0], reg).unwrap();
        t.set_property_hooked(gin, "port_type", str_val(port_type), reg)
            .unwrap();
        t.set_property(gin, "name", str_val(name)).unwrap();
        gin
    }

    #[test]
    fn template_gains_port_instance_gains_socket() {
        let (reg, template, type_id) = setup();
        // Instance built while template has no ports.
        let mut parent = Graph::new();
        let inst = parent.add_new_node(type_id, [0.0, 0.0], &reg).unwrap();
        assert!(parent.get(inst).unwrap().inputs.is_empty());

        add_input_port(&template, &reg, "width", "Number");
        sync_instances_to_template(&mut parent, &reg, &template);

        let sock = parent
            .get(inst)
            .unwrap()
            .input_by_name("width")
            .expect("instance gained the new 'width' socket");
        assert_eq!(sock.socket_type, SocketType::Number);
    }

    #[test]
    fn template_drops_port_disconnects_and_removes_socket() {
        let (reg, template, type_id) = setup();
        let gin = add_input_port(&template, &reg, "width", "Number");

        let mut parent = Graph::new();
        let inst = parent.add_new_node(type_id, [0.0, 0.0], &reg).unwrap();
        let src = parent.add_new_node("NumberConst", [0.0, 0.0], &reg).unwrap();
        let src_out = parent.get(src).unwrap().output_by_name("out").unwrap().uid;
        let in_uid = parent.get(inst).unwrap().input_by_name("width").unwrap().uid;
        parent
            .connect(Noodle::new(src, src_out, inst, in_uid), &reg)
            .unwrap();
        assert_eq!(parent.noodle_count(), 1);

        // Remove the port from the template, then sync.
        template.lock().unwrap().remove_node(gin).unwrap();
        sync_instances_to_template(&mut parent, &reg, &template);

        assert!(parent.get(inst).unwrap().input_by_name("width").is_none());
        assert_eq!(parent.noodle_count(), 0, "noodle to removed socket dropped");
    }

    #[test]
    fn retype_number_to_string_drops_incompatible_noodle() {
        let (reg, template, type_id) = setup();
        let gin = add_input_port(&template, &reg, "val", "Number");

        let mut parent = Graph::new();
        let inst = parent.add_new_node(type_id, [0.0, 0.0], &reg).unwrap();
        let src = parent.add_new_node("NumberConst", [0.0, 0.0], &reg).unwrap();
        let src_out = parent.get(src).unwrap().output_by_name("out").unwrap().uid;
        let in_uid = parent.get(inst).unwrap().input_by_name("val").unwrap().uid;
        parent
            .connect(Noodle::new(src, src_out, inst, in_uid), &reg)
            .unwrap();
        assert_eq!(parent.noodle_count(), 1);

        // Retype the template port Number → String.
        template
            .lock()
            .unwrap()
            .set_property_hooked(gin, "port_type", str_val("String"), &reg)
            .unwrap();
        sync_instances_to_template(&mut parent, &reg, &template);

        let sock = parent.get(inst).unwrap().input_by_name("val").unwrap();
        assert_eq!(sock.socket_type, SocketType::StringVal, "socket retyped");
        assert_eq!(
            parent.noodle_count(),
            0,
            "Number noodle into a String socket dropped",
        );
    }

    #[test]
    fn unchanged_ports_keep_uids_and_noodles() {
        let (reg, template, type_id) = setup();
        add_input_port(&template, &reg, "val", "Number");

        let mut parent = Graph::new();
        let inst = parent.add_new_node(type_id, [0.0, 0.0], &reg).unwrap();
        let src = parent.add_new_node("NumberConst", [0.0, 0.0], &reg).unwrap();
        let src_out = parent.get(src).unwrap().output_by_name("out").unwrap().uid;
        let in_uid = parent.get(inst).unwrap().input_by_name("val").unwrap().uid;
        parent
            .connect(Noodle::new(src, src_out, inst, in_uid), &reg)
            .unwrap();

        sync_instances_to_template(&mut parent, &reg, &template);

        assert_eq!(
            parent.get(inst).unwrap().input_by_name("val").unwrap().uid,
            in_uid,
            "unchanged port keeps its socket uid",
        );
        assert_eq!(parent.noodle_count(), 1, "unchanged port keeps its noodle");
    }
}
