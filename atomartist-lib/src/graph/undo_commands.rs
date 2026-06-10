//! Undo / redo commands for graph mutations.
//!
//! Each command captures enough state in `do_it` to reverse itself in
//! `undo_it`. Commands hold an `Arc<Mutex<Graph>>` so the same `Graph`
//! instance is shared between the UI thread (where commands run) and the
//! background evaluator thread (which only reads).
//!
//! All commands implement agg-gui's `UndoRedoCommand` trait so they push
//! into a standard `UndoBuffer`.

use std::sync::{Arc, Mutex};

use agg_gui::undo::UndoRedoCommand;

use crate::graph::graph::{Noodle, Graph};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::registry::NodeRegistry;

/// Add a node to the graph. On do, inserts the node; on undo, removes it
/// and stashes it for redo.
pub struct AddNodeCmd {
    graph: Arc<Mutex<Graph>>,
    /// On do: the node is moved into the graph; this becomes None.
    /// On undo: the node is moved back here.
    pending: Option<NodeInstance>,
    id: NodeId,
    label: &'static str,
}

impl AddNodeCmd {
    pub fn new(graph: Arc<Mutex<Graph>>, node: NodeInstance) -> Self {
        let id = node.id;
        Self { graph, pending: Some(node), id, label: "Add Node" }
    }

    /// Override the undo-menu label. Defaults to `"Add Node"`. Callers
    /// like the mesh-drop importer pick a more specific phrase
    /// ("Import Mesh") so users see what they actually did.
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }
}

impl UndoRedoCommand for AddNodeCmd {
    fn name(&self) -> &str { self.label }
    fn do_it(&mut self) {
        if let Some(node) = self.pending.take() {
            let mut g = self.graph.lock().unwrap();
            let _ = g.add_node(node);
        }
    }
    fn undo_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        if let Ok((node, _detached)) = g.remove_node(self.id) {
            self.pending = Some(node);
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Remove a node, capturing the node + any incident noodles so undo can
/// restore them exactly.
pub struct RemoveNodeCmd {
    graph: Arc<Mutex<Graph>>,
    id: NodeId,
    snapshot: Option<(NodeInstance, Vec<Noodle>)>,
}

impl RemoveNodeCmd {
    pub fn new(graph: Arc<Mutex<Graph>>, id: NodeId) -> Self {
        Self { graph, id, snapshot: None }
    }
}

impl UndoRedoCommand for RemoveNodeCmd {
    fn name(&self) -> &str { "Remove Node" }
    fn do_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        if let Ok(snap) = g.remove_node(self.id) {
            self.snapshot = Some(snap);
        }
    }
    fn undo_it(&mut self) {
        if let Some((node, noodles)) = self.snapshot.take() {
            let mut g = self.graph.lock().unwrap();
            let _ = g.add_node(node);
            for n in noodles {
                g.noodles_mut().push(n);
            }
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Connect two sockets. Stores the noodle so undo can disconnect it precisely.
pub struct ConnectCmd {
    graph: Arc<Mutex<Graph>>,
    registry: Arc<NodeRegistry>,
    noodle: Noodle,
    succeeded: bool,
}

impl ConnectCmd {
    pub fn new(graph: Arc<Mutex<Graph>>, registry: Arc<NodeRegistry>, noodle: Noodle) -> Self {
        Self { graph, registry, noodle, succeeded: false }
    }
}

impl UndoRedoCommand for ConnectCmd {
    fn name(&self) -> &str { "Connect" }
    fn do_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        self.succeeded = g.connect(self.noodle, &self.registry).is_ok();
    }
    fn undo_it(&mut self) {
        if self.succeeded {
            let mut g = self.graph.lock().unwrap();
            let _ = g.disconnect(&self.noodle, &self.registry);
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

pub struct DisconnectCmd {
    graph: Arc<Mutex<Graph>>,
    registry: Arc<NodeRegistry>,
    noodle: Noodle,
    succeeded: bool,
}

impl DisconnectCmd {
    pub fn new(graph: Arc<Mutex<Graph>>, registry: Arc<NodeRegistry>, noodle: Noodle) -> Self {
        Self { graph, registry, noodle, succeeded: false }
    }
}

impl UndoRedoCommand for DisconnectCmd {
    fn name(&self) -> &str { "Disconnect" }
    fn do_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        self.succeeded = g.disconnect(&self.noodle, &self.registry).unwrap_or(false);
    }
    fn undo_it(&mut self) {
        if self.succeeded {
            let mut g = self.graph.lock().unwrap();
            // Re-insert directly; bypasses validation since the noodle was
            // valid at original-do time.
            g.noodles_mut().push(self.noodle);
            g.mark_dirty_subtree(self.noodle.to.node);
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Move a node on the canvas. Captures the previous position for undo.
///
/// Drag-coalescing: a single user drag fires `set_node_position` ~60×/s,
/// one event per mouse-move frame. The bridge calls
/// [`MoveNodeCmd::extend_into`] on the top-of-stack `MoveNodeCmd` to
/// update `new_pos` in place, so the whole drag becomes one undo step.
/// `old_pos` is set on the first `do_it` and never overwritten.
pub struct MoveNodeCmd {
    graph: Arc<Mutex<Graph>>,
    pub id: NodeId,
    new_pos: [f64; 2],
    old_pos: Option<[f64; 2]>,
}

impl MoveNodeCmd {
    pub fn new(graph: Arc<Mutex<Graph>>, id: NodeId, new_pos: [f64; 2]) -> Self {
        Self { graph, id, new_pos, old_pos: None }
    }

    /// Coalesce a mid-drag update into this command. Caller has already
    /// verified the target id matches. Updates `new_pos` and applies
    /// the move directly — no new undo step pushed.
    pub fn extend_into(&mut self, new_pos: [f64; 2]) {
        self.new_pos = new_pos;
        let mut g = self.graph.lock().unwrap();
        let _ = g.set_position(self.id, new_pos);
    }
}

impl UndoRedoCommand for MoveNodeCmd {
    fn name(&self) -> &str { "Move Node" }
    fn do_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        if let Some(n) = g.get(self.id) {
            // Only capture old_pos on the FIRST do — coalesce-and-redo
            // cycles must preserve the pre-stroke baseline.
            if self.old_pos.is_none() {
                self.old_pos = Some(n.position);
            }
        }
        let _ = g.set_position(self.id, self.new_pos);
    }
    fn undo_it(&mut self) {
        if let Some(old) = self.old_pos {
            let mut g = self.graph.lock().unwrap();
            let _ = g.set_position(self.id, old);
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Change a property value. Captures the previous value for undo.
///
/// Slider-coalescing: dragging a number-drag widget fires
/// `set_property` per pixel. The bridge calls
/// [`ChangePropertyCmd::extend_into`] on the top-of-stack matching
/// command to update `new_value` in place — the whole drag is one
/// undo step. `old_value` captured once at first `do_it` and never
/// overwritten.
pub struct ChangePropertyCmd {
    graph: Arc<Mutex<Graph>>,
    pub id: NodeId,
    pub name: Arc<str>,
    new_value: Option<PortValue>,
    old_value: Option<PortValue>,
}

impl ChangePropertyCmd {
    pub fn new(
        graph: Arc<Mutex<Graph>>,
        id: NodeId,
        name: impl Into<Arc<str>>,
        new_value: PortValue,
    ) -> Self {
        Self { graph, id, name: name.into(), new_value: Some(new_value), old_value: None }
    }

    /// Coalesce a mid-stroke property update into this command. Caller
    /// has verified the target id + name match.
    pub fn extend_into(&mut self, new_value: PortValue) {
        let mut g = self.graph.lock().unwrap();
        let _ = g.set_property(self.id, self.name.clone(), new_value.clone());
        self.new_value = Some(new_value);
    }
}

impl UndoRedoCommand for ChangePropertyCmd {
    fn name(&self) -> &str { "Change Property" }
    fn do_it(&mut self) {
        let new_v = match self.new_value.clone() {
            Some(v) => v,
            None => return,
        };
        let mut g = self.graph.lock().unwrap();
        // Only capture old_value on the FIRST do — coalesce + redo
        // cycles must preserve the pre-stroke baseline.
        if self.old_value.is_none() {
            self.old_value = g.get(self.id)
                .and_then(|n| n.properties.get(&self.name).cloned());
        }
        let _ = g.set_property(self.id, self.name.clone(), new_v);
    }
    fn undo_it(&mut self) {
        if let Some(old) = self.old_value.clone() {
            let mut g = self.graph.lock().unwrap();
            let _ = g.set_property(self.id, self.name.clone(), old);
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Change **several properties of one node** as a single undo step,
/// with mid-stroke coalescing like [`ChangePropertyCmd`].
///
/// The 3-D height control writes `height` + `matrix` together every
/// drag frame (the matrix carries the base-lock compensation for the
/// height change); as two separate commands they would alternate on
/// the stack and defeat top-of-stack coalescing. Bundled here the
/// whole stroke is one command — and one Ctrl+Z restores both values,
/// matching MatterCAD's single "Scale" undo entry.
pub struct ChangePropsCmd {
    graph: Arc<Mutex<Graph>>,
    pub id: NodeId,
    props: Vec<PropSlot>,
}

struct PropSlot {
    name: Arc<str>,
    new_value: PortValue,
    /// Captured on first `do_it`, never overwritten — the pre-stroke
    /// baseline that `undo_it` restores.
    old_value: Option<PortValue>,
}

impl ChangePropsCmd {
    pub fn new(
        graph: Arc<Mutex<Graph>>,
        id: NodeId,
        props: Vec<(Arc<str>, PortValue)>,
    ) -> Self {
        let props = props
            .into_iter()
            .map(|(name, new_value)| PropSlot { name, new_value, old_value: None })
            .collect();
        Self { graph, id, props }
    }

    /// Whether this command targets `id` with exactly the property
    /// names in `names` (order-sensitive) — the caller's coalesce test.
    pub fn matches(&self, id: NodeId, names: &[&str]) -> bool {
        self.id == id
            && self.props.len() == names.len()
            && self.props.iter().zip(names).all(|(s, n)| &*s.name == *n)
    }

    /// Coalesce a mid-stroke update into this command: apply the new
    /// values to the graph and replace the `new_value`s, leaving the
    /// captured `old_value` baselines untouched. Caller has verified
    /// [`Self::matches`]; `values` pairs with the command's props by
    /// order.
    pub fn extend_into(&mut self, values: &[PortValue]) {
        let mut g = self.graph.lock().unwrap();
        for (slot, v) in self.props.iter_mut().zip(values) {
            let _ = g.set_property(self.id, slot.name.clone(), v.clone());
            slot.new_value = v.clone();
        }
    }
}

impl UndoRedoCommand for ChangePropsCmd {
    fn name(&self) -> &str { "Change Properties" }
    fn do_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        for slot in &mut self.props {
            if slot.old_value.is_none() {
                slot.old_value = g
                    .get(self.id)
                    .and_then(|n| n.properties.get(&slot.name).cloned());
            }
            let _ = g.set_property(self.id, slot.name.clone(), slot.new_value.clone());
        }
    }
    fn undo_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        for slot in self.props.iter_mut().rev() {
            if let Some(old) = slot.old_value.clone() {
                let _ = g.set_property(self.id, slot.name.clone(), old);
            }
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Bundle of commands run as one atomic undo step (e.g. a multi-node delete).
pub struct BatchCmd {
    name: String,
    children: Vec<Box<dyn UndoRedoCommand>>,
}

impl BatchCmd {
    pub fn new(name: impl Into<String>, children: Vec<Box<dyn UndoRedoCommand>>) -> Self {
        Self { name: name.into(), children }
    }
}

impl UndoRedoCommand for BatchCmd {
    fn name(&self) -> &str { &self.name }
    fn do_it(&mut self) {
        for c in &mut self.children {
            c.do_it();
        }
    }
    fn undo_it(&mut self) {
        for c in self.children.iter_mut().rev() {
            c.undo_it();
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::graph::Noodle;
    use crate::graph::socket::SocketUidAlloc;
    use crate::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    };
    use crate::socket_types::SocketType;
    use agg_gui::undo::UndoBuffer;

    struct ConstNode;
    impl NodeDef for ConstNode {
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
    struct TwoIn;
    impl NodeDef for TwoIn {
        fn type_id(&self) -> &'static str { "TwoIn" }
        fn category(&self) -> &'static str { "Math" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .input("a", SocketType::Number)
                .output("out", SocketType::Number)
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
    }

    fn setup() -> (Arc<Mutex<Graph>>, Arc<NodeRegistry>) {
        let mut r = NodeRegistry::new();
        r.register(ConstNode);
        r.register(TwoIn);
        (Arc::new(Mutex::new(Graph::new())), Arc::new(r))
    }

    #[test]
    fn add_then_undo_leaves_graph_empty() {
        let (g, _reg) = setup();
        let id = g.lock().unwrap().allocate_id();
        let node = NodeInstance::new(id, "Const", [0.0, 0.0]);
        let mut cmd = AddNodeCmd::new(g.clone(), node);
        cmd.do_it();
        assert_eq!(g.lock().unwrap().node_count(), 1);
        cmd.undo_it();
        assert_eq!(g.lock().unwrap().node_count(), 0);
        cmd.do_it();
        assert_eq!(g.lock().unwrap().node_count(), 1, "redo restores");
    }

    /// The height drag's paired write: `height` + `matrix` land as ONE
    /// command — mid-stroke samples coalesce into it, and a single
    /// undo restores both pre-stroke values (MatterCAD's one "Scale"
    /// undo entry).
    #[test]
    fn change_props_cmd_coalesces_and_undoes_both_values() {
        let (g, reg) = setup();
        let id = {
            let mut graph = g.lock().unwrap();
            let id = graph.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
            let _ = graph.set_property(id, "height", PortValue::Number(20.0));
            let _ = graph.set_property(id, "matrix", PortValue::Matrix4x4([1.0; 16]));
            id
        };
        let mut buf = UndoBuffer::new();
        let mk = |h: f64, m: f32| -> Vec<(Arc<str>, PortValue)> {
            vec![
                (Arc::from("matrix"), PortValue::Matrix4x4([m; 16])),
                (Arc::from("height"), PortValue::Number(h)),
            ]
        };
        buf.add_and_do(Box::new(ChangePropsCmd::new(g.clone(), id, mk(25.0, 2.0))));

        // Mid-stroke sample coalesces — still one undo entry.
        let coalesced = buf.try_coalesce_last(|top| {
            if let Some(cmd) = top.as_any_mut().downcast_mut::<ChangePropsCmd>() {
                if cmd.matches(id, &["matrix", "height"]) {
                    cmd.extend_into(&[
                        PortValue::Matrix4x4([3.0; 16]),
                        PortValue::Number(30.0),
                    ]);
                    return true;
                }
            }
            false
        });
        assert!(coalesced, "same node + names must coalesce");

        let read = |name: &str| g.lock().unwrap().get(id).unwrap().properties.get(name).cloned();
        assert_eq!(read("height"), Some(PortValue::Number(30.0)));
        assert_eq!(read("matrix"), Some(PortValue::Matrix4x4([3.0; 16])));

        // ONE undo restores both pre-stroke values.
        assert!(buf.can_undo());
        buf.undo();
        assert_eq!(read("height"), Some(PortValue::Number(20.0)), "undo restores height");
        assert_eq!(read("matrix"), Some(PortValue::Matrix4x4([1.0; 16])), "undo restores matrix");
        assert!(!buf.can_undo(), "the whole stroke was a single undo entry");

        // Redo replays the final coalesced pair.
        assert!(buf.can_redo());
        buf.redo();
        assert_eq!(read("height"), Some(PortValue::Number(30.0)));
        assert_eq!(read("matrix"), Some(PortValue::Matrix4x4([3.0; 16])));
    }

    #[test]
    fn undo_buffer_full_round_trip() {
        let (g, reg) = setup();
        let mut buf = UndoBuffer::new();
        let (a, b) = {
            let mut graph = g.lock().unwrap();
            let a = graph.add_new_node("Const", [0.0, 0.0], &reg).unwrap();
            let b = graph.add_new_node("TwoIn", [100.0, 0.0], &reg).unwrap();
            (a, b)
        };

        let (out_a, in_a_b) = {
            let graph = g.lock().unwrap();
            let out_a = graph.get(a).unwrap().output_by_name("out").unwrap().uid;
            let in_a_b = graph.get(b).unwrap().input_by_name("a").unwrap().uid;
            (out_a, in_a_b)
        };

        buf.add_and_do(Box::new(ConnectCmd::new(
            g.clone(),
            reg.clone(),
            Noodle::new(a, out_a, b, in_a_b),
        )));

        assert_eq!(g.lock().unwrap().node_count(), 2);
        assert_eq!(g.lock().unwrap().noodle_count(), 1);

        buf.undo();
        assert_eq!(g.lock().unwrap().noodle_count(), 0);
        buf.redo();
        assert_eq!(g.lock().unwrap().noodle_count(), 1);
    }

    #[test]
    fn change_property_undo_redo() {
        let (g, _reg) = setup();
        let id = g.lock().unwrap().allocate_id();
        let mut node = NodeInstance::new(id, "Const", [0.0, 0.0]);
        node.properties.insert(Arc::from("value"), PortValue::Number(2.0));
        g.lock().unwrap().add_node(node).unwrap();

        let mut cmd = ChangePropertyCmd::new(g.clone(), id, "value", PortValue::Number(7.0));
        cmd.do_it();
        let cur = g.lock().unwrap().get(id).unwrap().properties.get("value").cloned().unwrap();
        assert_eq!(cur, PortValue::Number(7.0));
        cmd.undo_it();
        let cur = g.lock().unwrap().get(id).unwrap().properties.get("value").cloned().unwrap();
        assert_eq!(cur, PortValue::Number(2.0));
    }
}
