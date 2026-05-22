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
///
/// `display_label` overrides the socket's display name in the canvas row;
/// the underlying `name` is still used for serialization and lookups. This
/// lets us keep socket identifiers stable (e.g. `"bevel_radius"`) while
/// the canvas shows a human-readable label (e.g. `"Radius"`), matching
/// NodeDesigner's input rows.
#[derive(Clone, Debug)]
pub struct SocketDef {
    pub name: &'static str,
    pub socket_type: SocketType,
    /// True when the socket is allowed to be unconnected (the executor will
    /// pass `PortValue::None` if no edge is wired).
    pub optional: bool,
    /// Human-readable label shown next to the socket on the canvas. When
    /// `None` the canvas falls back to `name`.
    pub display_label: Option<&'static str>,
}

impl SocketDef {
    pub fn required(name: &'static str, socket_type: SocketType) -> Self {
        Self { name, socket_type, optional: false, display_label: None }
    }
    pub fn optional(name: &'static str, socket_type: SocketType) -> Self {
        Self { name, socket_type, optional: true, display_label: None }
    }
    /// Add a human-readable display label. The canvas uses this when
    /// drawing the socket row; serialization and graph lookups still use
    /// `name`.
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.display_label = Some(label);
        self
    }
}

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
///
/// This is the "Bevy-style" attribute surface: property structs derive
/// [`bevy_reflect::Reflect`] so callers can iterate fields by type at
/// runtime, and `NodeFieldAttrs` rides alongside each field as the
/// editor / label / binding metadata — keeping the per-node UI wiring
/// in one place instead of duplicated across `input_sockets()` and
/// `properties()`.
#[derive(Clone, Debug, Default)]
pub struct NodeFieldAttrs {
    /// Human-readable label (overrides the field name).
    pub label: Option<&'static str>,
    /// Editor shape for the field's value.
    pub editor: EditorKind,
    /// When `Some(socket_name)`, the field is paired with the input
    /// socket of that name: the canvas draws the field's inline editor on
    /// the socket's row, and the editor is hidden when the socket is
    /// connected.
    pub bound_input: Option<&'static str>,
}

impl NodeFieldAttrs {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
    pub fn with_editor(mut self, editor: EditorKind) -> Self {
        self.editor = editor;
        self
    }
    pub fn bound_to(mut self, socket: &'static str) -> Self {
        self.bound_input = Some(socket);
        self
    }
}

/// Description of one settable property on a node type.
///
/// Properties are values stored on the node itself (as opposed to flowing in
/// over a socket connection). They appear as widgets on the node box on the
/// canvas, and as rows in the right-side property panel when the node is
/// selected. The `editor` hint drives the inline editor shape; the
/// optional `bound_input` pairs the property with an input socket so the
/// editor draws on the socket's row and disappears once the socket is
/// connected — matching NodeDesigner's row-pairing model.
#[derive(Clone, Debug)]
pub struct PropDef {
    pub name: &'static str,
    pub default: PortValue,
    /// Inclusive minimum for numeric properties. Mirrored from
    /// `editor.numeric_range()` for backwards compatibility.
    pub min: Option<f64>,
    /// Inclusive maximum.
    pub max: Option<f64>,
    /// Display label override. Falls back to `name` when `None`.
    pub label: Option<&'static str>,
    /// Editor hint — the UI layer picks the widget; the schema describes
    /// the intent + numeric range / integer-ness.
    pub editor: EditorKind,
    /// When `Some(socket_name)`, the property is rendered inline on that
    /// input socket's row. The editor hides itself when the socket is
    /// connected.
    pub bound_input: Option<&'static str>,
}

impl PropDef {
    pub fn new(name: &'static str, default: PortValue) -> Self {
        Self {
            name,
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
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    /// Bind the property to an input socket: the canvas will render the
    /// inline editor on that socket's row, and hide it once the socket is
    /// connected.
    pub fn bind_input(mut self, socket_name: &'static str) -> Self {
        self.bound_input = Some(socket_name);
        self
    }

    /// Construct a `PropDef` from a [`NodeFieldAttrs`] + default value.
    /// Used by reflected property structs to mint their `PropDef`s.
    pub fn from_attrs(name: &'static str, default: PortValue, attrs: &NodeFieldAttrs) -> Self {
        let mut p = PropDef::new(name, default).with_editor(attrs.editor.clone());
        if let Some(l) = attrs.label {
            p = p.with_label(l);
        }
        if let Some(s) = attrs.bound_input {
            p = p.bind_input(s);
        }
        p
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

    #[test]
    fn socket_def_with_label_overrides_display() {
        let s = SocketDef::optional("bevel_radius", SocketType::Number).with_label("Radius");
        assert_eq!(s.name, "bevel_radius");
        assert_eq!(s.display_label, Some("Radius"));
        assert!(s.optional);
    }

    #[test]
    fn prop_def_with_editor_mirrors_range_to_min_max() {
        let p = PropDef::new("height", PortValue::Number(5.0))
            .with_editor(EditorKind::slider_range(0.1, 40.0));
        assert_eq!(p.min, Some(0.1));
        assert_eq!(p.max, Some(40.0));
        match p.editor {
            EditorKind::Slider(a) => {
                assert_eq!(a.min, Some(0.1));
                assert_eq!(a.max, Some(40.0));
            }
            _ => panic!("expected Slider editor"),
        }
    }

    #[test]
    fn prop_def_bind_input_records_socket_name() {
        let p = PropDef::new("height", PortValue::Number(5.0))
            .with_editor(EditorKind::slider_range(0.1, 40.0))
            .bind_input("Height");
        assert_eq!(p.bound_input, Some("Height"));
    }

    #[test]
    fn prop_def_from_attrs_carries_label_editor_binding() {
        let attrs = NodeFieldAttrs::new()
            .with_label("Bevel Radius")
            .with_editor(EditorKind::slider_range(0.0, 10.0))
            .bound_to("Radius");
        let p = PropDef::from_attrs("bevel_radius", PortValue::Number(0.0), &attrs);
        assert_eq!(p.label, Some("Bevel Radius"));
        assert_eq!(p.bound_input, Some("Radius"));
        assert_eq!(p.min, Some(0.0));
        assert_eq!(p.max, Some(10.0));
    }

    #[test]
    fn number_attrs_builders_compose() {
        let a = NumberAttrs::with_range(1.0, 30.0)
            .integer()
            .with_step(1.0)
            .with_ease_in(2.0)
            .with_snap_grid();
        assert_eq!(a.min, Some(1.0));
        assert_eq!(a.max, Some(30.0));
        assert!(a.integer);
        assert_eq!(a.step, Some(1.0));
        assert_eq!(a.ease_in, Some(2.0));
        assert!(a.snap_grid);
    }
}
