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
//!
//! ### The instantiation model
//!
//! `NodeDef` is a factory + behavior trait. It mints the initial socket
//! layout for new instances (`instantiate`), describes its property
//! schema, and exposes connection-time hooks. After instantiation, the
//! per-instance socket layout lives on the `NodeInstance` itself — the
//! trait does not answer "what sockets does this instance have?" queries
//! at lookup time. This is the lesson from NodeDesigner: locking sockets
//! to a static type-side description forced churn for every dynamic-input
//! node (Combine, Group Output, …).

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::graph::socket::{Socket, SocketUid, SocketUidAlloc};
use crate::socket_types::SocketType;

/// Editor hint for a property — how the UI layer should render an inline
/// editor for the property's current value.
///
/// The variants intentionally describe *intent*, not pixels — the actual
/// widget (drag-value, slider, color picker, matrix sub-panel) is chosen
/// by the UI implementation. Keeping the hint in the schema lets headless
/// callers (tests, serialization, future bevy-style inspectors) reason
/// about the editor shape without depending on `agg-gui`.
#[derive(Clone, Debug, PartialEq)]
pub enum EditorKind {
    /// Click-and-drag horizontally to edit a number. Default for `Number`
    /// properties.
    NumberDrag(NumberAttrs),
    /// Horizontal slider between `min` and `max`. NodeDesigner's "slider"
    /// widget maps here.
    Slider(NumberAttrs),
    /// Boolean checkbox toggle.
    Toggle,
    /// Color swatch + picker.
    ColorPicker,
    /// 4×4 matrix — typically rendered as a compact button that opens
    /// a translation/rotation/scale sub-panel.
    Matrix,
    /// Read-only text display. Used when a property's value isn't
    /// directly editable on the node row (e.g. a derived value).
    Display,
}

impl EditorKind {
    /// `NumberDrag` editor with `[min, max]` range.
    pub fn drag_range(min: f64, max: f64) -> Self {
        EditorKind::NumberDrag(NumberAttrs {
            min: Some(min),
            max: Some(max),
            ..Default::default()
        })
    }

    /// `Slider` editor with `[min, max]` range.
    pub fn slider_range(min: f64, max: f64) -> Self {
        EditorKind::Slider(NumberAttrs {
            min: Some(min),
            max: Some(max),
            ..Default::default()
        })
    }

    /// Inclusive numeric range when this editor is numeric, else `None`.
    pub fn numeric_range(&self) -> (Option<f64>, Option<f64>) {
        match self {
            EditorKind::NumberDrag(a) | EditorKind::Slider(a) => (a.min, a.max),
            _ => (None, None),
        }
    }

    /// Numeric editor attributes when this editor is numeric.
    pub fn number_attrs(&self) -> Option<&NumberAttrs> {
        match self {
            EditorKind::NumberDrag(a) | EditorKind::Slider(a) => Some(a),
            _ => None,
        }
    }
}

impl Default for EditorKind {
    fn default() -> Self {
        EditorKind::Display
    }
}

/// Numeric editor attributes — used by [`EditorKind::NumberDrag`] and
/// [`EditorKind::Slider`]. Mirrors NodeDesigner's `addWidget("slider", ...)`
/// option bag so existing scenes have a direct translation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NumberAttrs {
    /// Inclusive minimum.
    pub min: Option<f64>,
    /// Inclusive maximum.
    pub max: Option<f64>,
    /// Drag step (smallest delta per pixel of motion). `None` lets the
    /// editor pick a sensible default for the range.
    pub step: Option<f64>,
    /// Display + clamp as an integer.
    pub integer: bool,
    /// Power-of-N easing applied to slider drag deltas. NodeDesigner's
    /// "easeIn: 2" maps here.
    pub ease_in: Option<f64>,
    /// Snap drag deltas to a screen-space grid. NodeDesigner's
    /// "useSnapGrid" maps here.
    pub snap_grid: bool,
}

impl NumberAttrs {
    pub fn with_range(min: f64, max: f64) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
            ..Default::default()
        }
    }
    pub fn integer(mut self) -> Self {
        self.integer = true;
        self
    }
    pub fn with_step(mut self, step: f64) -> Self {
        self.step = Some(step);
        self
    }
    pub fn with_ease_in(mut self, e: f64) -> Self {
        self.ease_in = Some(e);
        self
    }
    pub fn with_snap_grid(mut self) -> Self {
        self.snap_grid = true;
        self
    }
}

/// Field-level metadata that pairs with a typed property struct field —
/// declared once per field and consumed both by [`PropDef::from_attrs`]
/// (to mint a `PropDef`) and by the UI layer to render the field's
/// editor + label.
#[derive(Clone, Debug, Default)]
pub struct NodeFieldAttrs {
    pub label: Option<Arc<str>>,
    pub editor: EditorKind,
    /// When `Some(socket_name)`, the field is paired with the input
    /// socket of that name: the canvas draws the field's inline editor on
    /// the socket's row, and the editor is hidden when the socket is
    /// connected.
    pub bound_input: Option<Arc<str>>,
}

impl NodeFieldAttrs {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub fn with_editor(mut self, editor: EditorKind) -> Self {
        self.editor = editor;
        self
    }
    pub fn bound_to(mut self, socket: impl Into<Arc<str>>) -> Self {
        self.bound_input = Some(socket.into());
        self
    }
}

/// Description of one settable property on a node type.
///
/// Properties are values stored on the node itself (as opposed to flowing in
/// over a socket connection). They appear as widgets on the node box on the
/// canvas, and as rows in the right-side property panel when the node is
/// selected.
#[derive(Clone, Debug)]
pub struct PropDef {
    pub name: Arc<str>,
    pub default: PortValue,
    /// Inclusive minimum for numeric properties. Mirrored from
    /// `editor.numeric_range()` for backwards compatibility.
    pub min: Option<f64>,
    /// Inclusive maximum.
    pub max: Option<f64>,
    /// Display label override. Falls back to `name` when `None`.
    pub label: Option<Arc<str>>,
    /// Editor hint — the UI layer picks the widget; the schema describes
    /// the intent + numeric range / integer-ness.
    pub editor: EditorKind,
    /// When `Some(socket_name)`, the property is rendered inline on that
    /// input socket's row. The editor hides itself when the socket is
    /// connected.
    pub bound_input: Option<Arc<str>>,
}

impl PropDef {
    pub fn new(name: impl Into<Arc<str>>, default: PortValue) -> Self {
        Self {
            name: name.into(),
            default,
            min: None,
            max: None,
            label: None,
            editor: EditorKind::default(),
            bound_input: None,
        }
    }

    /// Set an inclusive numeric range. Updates both the legacy `min`/`max`
    /// fields and the `editor`'s numeric attrs so callers using either
    /// API see consistent values.
    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        match &mut self.editor {
            EditorKind::NumberDrag(a) | EditorKind::Slider(a) => {
                a.min = Some(min);
                a.max = Some(max);
            }
            other => {
                *other = EditorKind::NumberDrag(NumberAttrs {
                    min: Some(min),
                    max: Some(max),
                    ..Default::default()
                });
            }
        }
        self
    }

    /// Override the editor hint. Numeric ranges on the hint are mirrored
    /// onto `min`/`max` so legacy callers keep working.
    pub fn with_editor(mut self, editor: EditorKind) -> Self {
        let (mn, mx) = editor.numeric_range();
        if mn.is_some() {
            self.min = mn;
        }
        if mx.is_some() {
            self.max = mx;
        }
        self.editor = editor;
        self
    }

    /// Set the human-readable display label.
    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Bind the property to an input socket: the canvas will render the
    /// inline editor on that socket's row, and hide it once the socket is
    /// connected.
    pub fn bind_input(mut self, socket_name: impl Into<Arc<str>>) -> Self {
        self.bound_input = Some(socket_name.into());
        self
    }

    /// Construct a `PropDef` from a [`NodeFieldAttrs`] + default value.
    /// Used by reflected property structs to mint their `PropDef`s.
    pub fn from_attrs(name: impl Into<Arc<str>>, default: PortValue, attrs: &NodeFieldAttrs) -> Self {
        let mut p = PropDef::new(name, default).with_editor(attrs.editor.clone());
        if let Some(l) = &attrs.label {
            p = p.with_label(l.clone());
        }
        if let Some(s) = &attrs.bound_input {
            p = p.bind_input(s.clone());
        }
        p
    }
}

/// Initial socket + property layout for a new node instance — what
/// [`NodeDef::instantiate`] returns. The graph populates a new
/// `NodeInstance` directly from this struct.
pub struct InstanceTemplate {
    pub inputs: Vec<Socket>,
    pub outputs: Vec<Socket>,
    pub initial_properties: HashMap<Arc<str>, PortValue>,
}

impl InstanceTemplate {
    /// Start a builder bound to a uid allocator. The allocator hands out
    /// graph-unique uids so every newly minted socket can be referenced
    /// by edges immediately.
    pub fn builder<'a>(alloc: &'a mut SocketUidAlloc) -> TemplateBuilder<'a> {
        TemplateBuilder::new(alloc)
    }
}

/// Fluent builder for [`InstanceTemplate`].
pub struct TemplateBuilder<'a> {
    alloc: &'a mut SocketUidAlloc,
    inputs: Vec<Socket>,
    outputs: Vec<Socket>,
    initial_properties: HashMap<Arc<str>, PortValue>,
}

impl<'a> TemplateBuilder<'a> {
    pub fn new(alloc: &'a mut SocketUidAlloc) -> Self {
        Self {
            alloc,
            inputs: Vec::new(),
            outputs: Vec::new(),
            initial_properties: HashMap::new(),
        }
    }

    /// Add a required input socket.
    pub fn input(mut self, name: impl Into<Arc<str>>, socket_type: SocketType) -> Self {
        let uid = self.alloc.allocate();
        self.inputs.push(Socket::new(uid, name, socket_type, false));
        self
    }

    /// Add an optional input socket — the executor passes `PortValue::None`
    /// when no edge is wired and the node's `evaluate` is responsible for
    /// the fallback (typically a stored property).
    pub fn input_opt(mut self, name: impl Into<Arc<str>>, socket_type: SocketType) -> Self {
        let uid = self.alloc.allocate();
        self.inputs.push(Socket::new(uid, name, socket_type, true));
        self
    }

    /// Add an input socket with a display-label override.
    pub fn input_with_label(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        socket_type: SocketType,
        optional: bool,
    ) -> Self {
        let uid = self.alloc.allocate();
        let socket = Socket::new(uid, name, socket_type, optional).with_label(label);
        self.inputs.push(socket);
        self
    }

    /// Add an output socket.
    pub fn output(mut self, name: impl Into<Arc<str>>, socket_type: SocketType) -> Self {
        let uid = self.alloc.allocate();
        // Outputs have no "optional" semantics from the executor's POV —
        // they always carry whatever the node wrote in `evaluate`.
        self.outputs.push(Socket::new(uid, name, socket_type, false));
        self
    }

    /// Add an output socket with a display-label override.
    pub fn output_with_label(
        mut self,
        name: impl Into<Arc<str>>,
        label: impl Into<Arc<str>>,
        socket_type: SocketType,
    ) -> Self {
        let uid = self.alloc.allocate();
        let socket = Socket::new(uid, name, socket_type, false).with_label(label);
        self.outputs.push(socket);
        self
    }

    /// Set an initial property value. Properties not set here pick up
    /// `PropDef::default` from the type's `properties()` list when the
    /// instance is constructed by [`crate::graph::Graph::add_new_node`].
    pub fn property(mut self, name: impl Into<Arc<str>>, value: PortValue) -> Self {
        self.initial_properties.insert(name.into(), value);
        self
    }

    pub fn build(self) -> InstanceTemplate {
        InstanceTemplate {
            inputs: self.inputs,
            outputs: self.outputs,
            initial_properties: self.initial_properties,
        }
    }
}

/// Inputs handed to `NodeDef::evaluate` — for each connected input socket
/// (resolved by uid), the executor inserts the upstream value. Disconnected
/// optional inputs are absent from the map; node code should use the
/// `ctx.input*` accessors which fall back to `PortValue::None`.
#[derive(Default)]
pub struct NodeInputs {
    pub by_uid: HashMap<SocketUid, PortValue>,
}

impl NodeInputs {
    pub fn insert(&mut self, uid: SocketUid, value: PortValue) {
        self.by_uid.insert(uid, value);
    }
    pub fn get(&self, uid: SocketUid) -> &PortValue {
        self.by_uid.get(&uid).unwrap_or(&PortValue::None)
    }
}

/// Property snapshot handed to `NodeDef::evaluate`. The executor copies a
/// node's `properties` map into here at evaluation time so node code never
/// touches mutable state.
#[derive(Default)]
pub struct NodeProperties {
    pub by_name: HashMap<Arc<str>, PortValue>,
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

    pub fn insert(&mut self, name: impl Into<Arc<str>>, value: PortValue) {
        self.by_name.insert(name.into(), value);
    }
}

/// Outputs returned by `NodeDef::evaluate`, keyed by socket name. The
/// executor resolves each name against the node instance's `outputs` list
/// to find the producing socket's uid, then stores the value in
/// `cached_outputs` under that uid. Keeping node code name-keyed is the
/// ergonomic choice — nodes don't need to track uids themselves.
#[derive(Default)]
pub struct NodeOutputs {
    pub by_name: HashMap<Arc<str>, PortValue>,
}

impl NodeOutputs {
    pub fn set(&mut self, name: impl Into<Arc<str>>, value: PortValue) {
        self.by_name.insert(name.into(), value);
    }
}

/// Errors a node may raise during evaluation.
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

/// Evaluation context handed to [`NodeDef::evaluate`]. Provides
/// uid-keyed access to inputs plus name-keyed convenience accessors
/// that resolve names against the instance's current socket list.
///
/// Nodes that declare static socket layouts in `instantiate` should
/// prefer the name-keyed accessors (`ctx.input_named("size")`) so their
/// `evaluate` bodies don't need to track uids. Dynamic nodes
/// (Output, future Combine) may want uid access for stable identity.
pub struct EvalCtx<'a> {
    pub instance: &'a crate::graph::node::NodeInstance,
    pub properties: &'a NodeProperties,
    pub inputs: &'a NodeInputs,
}

impl<'a> EvalCtx<'a> {
    /// Look up an input value by socket uid.
    pub fn input(&self, uid: SocketUid) -> &PortValue {
        self.inputs.get(uid)
    }

    /// Look up an input value by socket name (resolved against the
    /// instance's current `inputs`). Returns `PortValue::None` when no
    /// such socket exists or it has no incoming edge.
    pub fn input_named(&self, name: &str) -> &PortValue {
        match self.instance.input_by_name(name) {
            Some(s) => self.input(s.uid),
            None => &PortValue::None,
        }
    }

    /// All connected inputs, in instance-order. Skips disconnected
    /// optional sockets (which are absent from `inputs`). Convenient for
    /// dynamic nodes that iterate over whatever the user wired up.
    pub fn connected_inputs(&self) -> impl Iterator<Item = (&Socket, &PortValue)> {
        self.instance
            .inputs
            .iter()
            .filter_map(|s| self.inputs.by_uid.get(&s.uid).map(|v| (s, v)))
    }
}

/// Context passed to [`NodeDef::validate_input_connection`] — invoked
/// *before* the edge is inserted. Returning `Err` rejects the connection
/// and surfaces the reason to the UI.
pub struct ValidateCtx<'a> {
    pub graph: &'a crate::graph::graph::Graph,
    pub this_node: crate::graph::node::NodeId,
    pub target_socket: SocketUid,
    pub source_node: crate::graph::node::NodeId,
    pub source_socket: SocketUid,
}

/// Context passed to [`NodeDef::on_input_connected`] — invoked *after*
/// the edge has been inserted. The hook may further mutate the target
/// node's sockets via `graph`'s socket-mutation API (rename / append /
/// remove). Each such mutation may recursively fire connection hooks on
/// neighbors, so keep the work minimal and idempotent.
pub struct ConnectCtx<'a> {
    pub graph: &'a mut crate::graph::graph::Graph,
    pub this_node: crate::graph::node::NodeId,
    pub target_socket: SocketUid,
    pub source_node: crate::graph::node::NodeId,
    pub source_socket: SocketUid,
}

/// Context passed to [`NodeDef::on_input_disconnected`] — invoked *after*
/// the edge has been removed. The hook may collapse the now-orphan input
/// slot.
pub struct DisconnectCtx<'a> {
    pub graph: &'a mut crate::graph::graph::Graph,
    pub this_node: crate::graph::node::NodeId,
    pub target_socket: SocketUid,
}

/// One registered node type — describes its factory + behavior.
pub trait NodeDef: Send + Sync {
    /// Stable identifier used for serialization and registry lookup.
    fn type_id(&self) -> &'static str;

    /// Human-readable label used in the "add node" menu.
    fn display_name(&self) -> &'static str {
        self.type_id()
    }

    /// Menu category, e.g. "Primitives 3D", "Operations 3D", "Math".
    fn category(&self) -> &'static str;

    /// Mint the initial socket layout for a new instance. The graph
    /// supplies a uid allocator so every socket lands with a unique,
    /// graph-stable identifier.
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate;

    /// Property schema (default values, editor hints). Used by the UI to
    /// render inline editors on the node row. Defaults to empty — most
    /// nodes that have properties declare them here as well as in
    /// `instantiate`'s `property()` calls (the former drives the editor;
    /// the latter seeds the value on a brand-new instance).
    fn properties(&self) -> Vec<PropDef> {
        Vec::new()
    }

    /// Compute outputs from inputs and properties. Pure function — must not
    /// stash mutable state on `&self` and must be safe to call from a
    /// background thread.
    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError>;

    /// Pre-connect veto hook. Return `Err(reason)` to reject the
    /// connection. Default: allow.
    fn validate_input_connection(&self, _ctx: &ValidateCtx) -> Result<(), String> {
        Ok(())
    }

    /// Invoked after an edge connects to one of this node's inputs. The
    /// default is no-op; dynamic-input nodes override this to adopt the
    /// source's name and type, append a trailing empty slot, or mint a
    /// matching output mirror.
    fn on_input_connected(&self, _ctx: &mut ConnectCtx) {}

    /// Invoked after an edge is removed from one of this node's inputs.
    /// Default is no-op; dynamic-input nodes override to collapse the
    /// now-orphan slot.
    fn on_input_disconnected(&self, _ctx: &mut DisconnectCtx) {}
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
    use crate::graph::node::{NodeId, NodeInstance};

    struct DummyNode;
    impl NodeDef for DummyNode {
        fn type_id(&self) -> &'static str { "Dummy" }
        fn category(&self) -> &'static str { "Test" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            let mut o = NodeOutputs::default();
            o.set("out", PortValue::Number(42.0));
            Ok(o)
        }
    }

    struct OtherNode;
    impl NodeDef for OtherNode {
        fn type_id(&self) -> &'static str { "Other" }
        fn category(&self) -> &'static str { "Test" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc).build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
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
    fn template_builder_mints_uids_sequentially() {
        let mut alloc = SocketUidAlloc::new();
        let tpl = InstanceTemplate::builder(&mut alloc)
            .input("a", SocketType::Number)
            .input_opt("b", SocketType::Number)
            .output("out", SocketType::Number)
            .property("scale", PortValue::Number(1.0))
            .build();
        assert_eq!(tpl.inputs.len(), 2);
        assert_eq!(tpl.outputs.len(), 1);
        // First input gets uid 0, second gets 1, output gets 2.
        assert_eq!(tpl.inputs[0].uid, SocketUid(0));
        assert_eq!(tpl.inputs[1].uid, SocketUid(1));
        assert_eq!(tpl.outputs[0].uid, SocketUid(2));
        assert!(tpl.inputs[0].optional == false);
        assert!(tpl.inputs[1].optional == true);
        assert_eq!(
            tpl.initial_properties.get("scale"),
            Some(&PortValue::Number(1.0))
        );
    }

    #[test]
    fn eval_ctx_input_named_resolves_through_instance() {
        let mut alloc = SocketUidAlloc::new();
        let mut inst = NodeInstance::new(NodeId(1), "Dummy", [0.0, 0.0]);
        let tpl = InstanceTemplate::builder(&mut alloc)
            .input("size", SocketType::Number)
            .build();
        inst.inputs = tpl.inputs;
        let uid = inst.inputs[0].uid;
        let mut ins = NodeInputs::default();
        ins.insert(uid, PortValue::Number(7.5));
        let props = NodeProperties::default();
        let ctx = EvalCtx {
            instance: &inst,
            properties: &props,
            inputs: &ins,
        };
        assert_eq!(ctx.input(uid), &PortValue::Number(7.5));
        assert_eq!(ctx.input_named("size"), &PortValue::Number(7.5));
        assert_eq!(ctx.input_named("missing"), &PortValue::None);
    }
}
