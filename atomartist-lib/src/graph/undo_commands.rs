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
}

impl AddNodeCmd {
    pub fn new(graph: Arc<Mutex<Graph>>, node: NodeInstance) -> Self {
        let id = node.id;
        Self { graph, pending: Some(node), id }
    }
}

impl UndoRedoCommand for AddNodeCmd {
    fn name(&self) -> &str { "Add Node" }
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
}

/// Move a node on the canvas. Captures the previous position for undo.
pub struct MoveNodeCmd {
    graph: Arc<Mutex<Graph>>,
    id: NodeId,
    new_pos: [f64; 2],
    old_pos: Option<[f64; 2]>,
}

impl MoveNodeCmd {
    pub fn new(graph: Arc<Mutex<Graph>>, id: NodeId, new_pos: [f64; 2]) -> Self {
        Self { graph, id, new_pos, old_pos: None }
    }
}

impl UndoRedoCommand for MoveNodeCmd {
    fn name(&self) -> &str { "Move Node" }
    fn do_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        if let Some(n) = g.get(self.id) {
            self.old_pos = Some(n.position);
        }
        let _ = g.set_position(self.id, self.new_pos);
    }
    fn undo_it(&mut self) {
        if let Some(old) = self.old_pos {
            let mut g = self.graph.lock().unwrap();
            let _ = g.set_position(self.id, old);
        }
    }
}

/// Change a property value. Captures the previous value for undo.
pub struct ChangePropertyCmd {
    graph: Arc<Mutex<Graph>>,
    id: NodeId,
    name: Arc<str>,
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
}

impl UndoRedoCommand for ChangePropertyCmd {
    fn name(&self) -> &str { "Change Property" }
    fn do_it(&mut self) {
        if let Some(new_v) = self.new_value.take() {
            let mut g = self.graph.lock().unwrap();
            self.old_value = g.get(self.id)
                .and_then(|n| n.properties.get(&self.name).cloned());
            let _ = g.set_property(self.id, self.name.clone(), new_v.clone());
            // Stash the new value back so redo can replay it.
            self.new_value = Some(new_v);
        } else if let Some(new_v) = self.new_value.clone() {
            let mut g = self.graph.lock().unwrap();
            let _ = g.set_property(self.id, self.name.clone(), new_v);
        }
    }
    fn undo_it(&mut self) {
        if let Some(old) = self.old_value.clone() {
            let mut g = self.graph.lock().unwrap();
            let _ = g.set_property(self.id, self.name.clone(), old);
        }
    }
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
