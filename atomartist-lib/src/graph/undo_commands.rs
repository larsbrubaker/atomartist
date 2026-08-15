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
use crate::graph::socket::SocketUid;
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

/// Connect an output socket to whichever input of `to_node` is free
/// **at the moment the command runs** — the undoable form of
/// NodeDesigner's `autoConnectToOutput`.
///
/// Why this is not just a [`ConnectCmd`] with a pre-computed noodle: the
/// `Output` node is a dynamic-input node. Disconnecting one of its
/// inputs *deletes* that slot and regrows a trailing empty one with a
/// **new** [`SocketUid`], so the uid an undo tore down no longer exists
/// when redo replays. A cached noodle would then fail
/// `SocketNotFound`, silently, and the user would get their node back
/// unwired. Re-resolving the target on every `do_it` reproduces the
/// ancestor's "auto-wire again" semantics exactly.
///
/// `undo_it` disconnects whatever `do_it` actually made, so the pair is
/// symmetric even though the endpoint moves between runs.
pub struct ConnectToFreeInputCmd {
    graph: Arc<Mutex<Graph>>,
    registry: Arc<NodeRegistry>,
    from: NodeId,
    from_socket: SocketUid,
    to_node: NodeId,
    /// The noodle the most recent `do_it` created, if it succeeded.
    connected: Option<Noodle>,
}

impl ConnectToFreeInputCmd {
    pub fn new(
        graph: Arc<Mutex<Graph>>,
        registry: Arc<NodeRegistry>,
        from: NodeId,
        from_socket: SocketUid,
        to_node: NodeId,
    ) -> Self {
        Self {
            graph,
            registry,
            from,
            from_socket,
            to_node,
            connected: None,
        }
    }
}

impl UndoRedoCommand for ConnectToFreeInputCmd {
    fn name(&self) -> &str { "Connect" }
    fn do_it(&mut self) {
        let mut g = self.graph.lock().unwrap();
        let Some(to_socket) = g.first_free_input(self.to_node) else {
            // No slot left: the wire is best-effort, exactly as the
            // ancestor's silent `return false`.
            self.connected = None;
            return;
        };
        let noodle = Noodle::new(self.from, self.from_socket, self.to_node, to_socket);
        self.connected = g.connect(noodle, &self.registry).ok().map(|_| noodle);
    }
    fn undo_it(&mut self) {
        if let Some(noodle) = self.connected.take() {
            let mut g = self.graph.lock().unwrap();
            let _ = g.disconnect(&noodle, &self.registry);
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
    /// Optional registry — when present, the property write routes through
    /// [`Graph::set_property_hooked`] so the type's `on_property_changed`
    /// hook fires (retyping sockets, disconnecting now-incompatible
    /// noodles). Absent → plain `set_property`, byte-for-byte the old
    /// behavior. Mirrors [`ConnectCmd`], which likewise carries a registry.
    registry: Option<Arc<NodeRegistry>>,
    pub id: NodeId,
    pub name: Arc<str>,
    new_value: Option<PortValue>,
    old_value: Option<PortValue>,
    /// Noodles the property-changed hook disconnected because a retyped
    /// socket became incompatible. Captured once (first non-empty) and
    /// re-pushed on undo so the round-trip is lossless.
    disconnected: Vec<Noodle>,
}

impl ChangePropertyCmd {
    pub fn new(
        graph: Arc<Mutex<Graph>>,
        id: NodeId,
        name: impl Into<Arc<str>>,
        new_value: PortValue,
    ) -> Self {
        Self {
            graph,
            registry: None,
            id,
            name: name.into(),
            new_value: Some(new_value),
            old_value: None,
            disconnected: Vec::new(),
        }
    }

    /// Attach a registry so property changes fire the type's
    /// `on_property_changed` hook. Without this the command behaves
    /// exactly as before (no hook, no socket revalidation).
    pub fn with_registry(mut self, registry: Arc<NodeRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Coalesce a mid-stroke property update into this command. Caller
    /// has verified the target id + name match.
    pub fn extend_into(&mut self, new_value: PortValue) {
        // Clone the Arc into a local so the lock guard doesn't hold a
        // borrow of `self` — `apply` needs `&mut self`.
        let graph = self.graph.clone();
        let mut g = graph.lock().unwrap();
        self.apply(&mut g, new_value.clone());
        self.new_value = Some(new_value);
    }

    /// Write `value` to the graph, routing through the hooked path when a
    /// registry is present and stashing the first non-empty set of
    /// hook-disconnected noodles for undo restoration.
    ///
    /// Assumption (a): coalescing via [`Self::extend_into`] assumes at most
    /// one retype-transition per coalesced stroke — the first non-empty
    /// drop set wins and later samples in the same stroke don't introduce a
    /// second, different set of disconnections.
    fn apply(&mut self, g: &mut Graph, value: PortValue) {
        match &self.registry {
            Some(reg) => {
                if let Ok(disc) = g.set_property_hooked(self.id, self.name.clone(), value, reg) {
                    if self.disconnected.is_empty() && !disc.is_empty() {
                        self.disconnected = disc;
                    }
                }
            }
            None => {
                let _ = g.set_property(self.id, self.name.clone(), value);
            }
        }
    }
}

impl UndoRedoCommand for ChangePropertyCmd {
    fn name(&self) -> &str { "Change Property" }
    fn do_it(&mut self) {
        let new_v = match self.new_value.clone() {
            Some(v) => v,
            None => return,
        };
        let graph = self.graph.clone();
        let mut g = graph.lock().unwrap();
        // Only capture old_value on the FIRST do — coalesce + redo
        // cycles must preserve the pre-stroke baseline.
        if self.old_value.is_none() {
            self.old_value = g.get(self.id)
                .and_then(|n| n.properties.get(&self.name).cloned());
        }
        self.apply(&mut g, new_v);
    }
    fn undo_it(&mut self) {
        if let Some(old) = self.old_value.clone() {
            let graph = self.graph.clone();
            let mut g = graph.lock().unwrap();
            // Restore the old value first (re-firing the hook, which
            // retypes sockets back to their compatible state), then
            // re-attach any noodle the do-time retype disconnected.
            self.apply(&mut g, old);
            if self.registry.is_some() {
                for n in &self.disconnected {
                    g.noodles_mut().push(*n);
                    g.mark_dirty_subtree(n.to.node);
                }
            }
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
    /// See [`ChangePropertyCmd::registry`] — optional hook routing.
    registry: Option<Arc<NodeRegistry>>,
    pub id: NodeId,
    props: Vec<PropSlot>,
    /// Noodles the property-changed hook disconnected across the batch.
    /// Captured once (first non-empty) and re-pushed on undo.
    disconnected: Vec<Noodle>,
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
        Self { graph, registry: None, id, props, disconnected: Vec::new() }
    }

    /// Attach a registry so each property change fires the type's
    /// `on_property_changed` hook. The hook fires once per changed
    /// property, in the batch's slot order; disconnected noodles are
    /// accumulated across the whole batch.
    pub fn with_registry(mut self, registry: Arc<NodeRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Write one slot's value, routing through the hooked path when a
    /// registry is present and *accumulating* every hook-disconnected
    /// noodle across the batch for undo restoration.
    ///
    /// Accumulation (rather than first-non-empty-wins) matters when two
    /// slots each retype a different socket and drop a different noodle:
    /// undo must restore both, so we `extend` and de-dupe by noodle
    /// identity instead of keeping only the first slot's drops.
    ///
    /// Assumption (b): undo-time revalidation assumes the type hooks are
    /// symmetric — restoring the old property values reverses the retype
    /// and drops nothing new. Under that assumption the extra `apply_slot`
    /// calls from `undo_it`'s restores contribute no new disconnections,
    /// so accumulating here is safe on both the do and undo paths.
    fn apply_slot(&mut self, g: &mut Graph, name: Arc<str>, value: PortValue) {
        match &self.registry {
            Some(reg) => {
                if let Ok(disc) = g.set_property_hooked(self.id, name, value, reg) {
                    for n in disc {
                        if !self.disconnected.contains(&n) {
                            self.disconnected.push(n);
                        }
                    }
                }
            }
            None => {
                let _ = g.set_property(self.id, name, value);
            }
        }
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
        let graph = self.graph.clone();
        let mut g = graph.lock().unwrap();
        // Collect (name, value) first so `apply_slot` can take `&mut self`
        // without also holding a borrow of `self.props`.
        let updates: Vec<(Arc<str>, PortValue)> = self
            .props
            .iter_mut()
            .zip(values)
            .map(|(slot, v)| {
                slot.new_value = v.clone();
                (slot.name.clone(), v.clone())
            })
            .collect();
        for (name, value) in updates {
            self.apply_slot(&mut g, name, value);
        }
    }
}

impl UndoRedoCommand for ChangePropsCmd {
    fn name(&self) -> &str { "Change Properties" }
    fn do_it(&mut self) {
        let graph = self.graph.clone();
        let mut g = graph.lock().unwrap();
        // Capture pre-stroke baselines + gather the writes, then apply —
        // decoupled so `apply_slot`'s `&mut self` doesn't clash with the
        // `self.props` iteration.
        let mut updates: Vec<(Arc<str>, PortValue)> = Vec::with_capacity(self.props.len());
        for slot in &mut self.props {
            if slot.old_value.is_none() {
                slot.old_value = g
                    .get(self.id)
                    .and_then(|n| n.properties.get(&slot.name).cloned());
            }
            updates.push((slot.name.clone(), slot.new_value.clone()));
        }
        for (name, value) in updates {
            self.apply_slot(&mut g, name, value);
        }
    }
    fn undo_it(&mut self) {
        let graph = self.graph.clone();
        let mut g = graph.lock().unwrap();
        let restores: Vec<(Arc<str>, PortValue)> = self
            .props
            .iter()
            .rev()
            .filter_map(|slot| slot.old_value.clone().map(|old| (slot.name.clone(), old)))
            .collect();
        for (name, value) in restores {
            self.apply_slot(&mut g, name, value);
        }
        // Re-attach any noodle the do-time retype disconnected, now that
        // sockets are back to their old (compatible) types.
        if self.registry.is_some() {
            for n in &self.disconnected {
                g.noodles_mut().push(*n);
                g.mark_dirty_subtree(n.to.node);
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
#[path = "undo_commands_tests.rs"]
mod tests;
