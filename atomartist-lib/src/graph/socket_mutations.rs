//! Granular socket-level mutations on a [`Graph`].
//!
//! These are the building blocks used by dynamic-input nodes (and the
//! eventual rename / reorder UI). They never invoke `on_input_connected`
//! themselves — they assume the caller is already inside a higher-level
//! mutation that has either fired the hook or is itself a node behavior
//! hook.
//!
//! Lives in its own module to keep [`super::graph`] under the 800-line
//! file-length cap from the project guidelines.

use std::sync::Arc;

use crate::graph::graph::{Noodle, Graph, GraphError};
use crate::graph::node::NodeId;
use crate::graph::socket::{Socket, SocketUid};
use crate::socket_types::SocketType;

impl Graph {
    /// Rename a socket's internal name. Noodles reference uids, not names,
    /// so this is purely a label change — no noodle mutation needed.
    pub fn rename_socket(
        &mut self,
        node: NodeId,
        socket: SocketUid,
        name: impl Into<Arc<str>>,
    ) -> Result<(), GraphError> {
        let n = self
            .nodes
            .get_mut(&node)
            .ok_or(GraphError::NodeNotFound(node))?;
        let name = name.into();
        if let Some(s) = n.inputs.iter_mut().find(|s| s.uid == socket) {
            s.name = name;
            return Ok(());
        }
        if let Some(s) = n.outputs.iter_mut().find(|s| s.uid == socket) {
            s.name = name;
            return Ok(());
        }
        Err(GraphError::SocketNotFound { node, socket })
    }

    /// Set or clear a socket's human-readable display label.
    pub fn relabel_socket(
        &mut self,
        node: NodeId,
        socket: SocketUid,
        label: Option<Arc<str>>,
    ) -> Result<(), GraphError> {
        let n = self
            .nodes
            .get_mut(&node)
            .ok_or(GraphError::NodeNotFound(node))?;
        if let Some(s) = n.inputs.iter_mut().find(|s| s.uid == socket) {
            s.display_label = label;
            return Ok(());
        }
        if let Some(s) = n.outputs.iter_mut().find(|s| s.uid == socket) {
            s.display_label = label;
            return Ok(());
        }
        Err(GraphError::SocketNotFound { node, socket })
    }

    /// Change a socket's currently-resolved type. Used by dynamic-input
    /// nodes to adopt the source's type on connect.
    pub fn retype_socket(
        &mut self,
        node: NodeId,
        socket: SocketUid,
        ty: SocketType,
    ) -> Result<(), GraphError> {
        let n = self
            .nodes
            .get_mut(&node)
            .ok_or(GraphError::NodeNotFound(node))?;
        if let Some(s) = n.inputs.iter_mut().find(|s| s.uid == socket) {
            s.socket_type = ty;
            return Ok(());
        }
        if let Some(s) = n.outputs.iter_mut().find(|s| s.uid == socket) {
            s.socket_type = ty;
            return Ok(());
        }
        Err(GraphError::SocketNotFound { node, socket })
    }

    /// Append an input socket on a node. The socket's uid must come from
    /// this graph's allocator — typically via [`Graph::allocate_socket_uid`].
    /// Returns the uid for caller convenience.
    pub fn append_input_socket(
        &mut self,
        node: NodeId,
        socket: Socket,
    ) -> Result<SocketUid, GraphError> {
        let n = self
            .nodes
            .get_mut(&node)
            .ok_or(GraphError::NodeNotFound(node))?;
        let uid = socket.uid;
        n.inputs.push(socket);
        Ok(uid)
    }

    /// Append an output socket on a node.
    pub fn append_output_socket(
        &mut self,
        node: NodeId,
        socket: Socket,
    ) -> Result<SocketUid, GraphError> {
        let n = self
            .nodes
            .get_mut(&node)
            .ok_or(GraphError::NodeNotFound(node))?;
        let uid = socket.uid;
        n.outputs.push(socket);
        Ok(uid)
    }

    /// Remove an input socket and GC every noodle touching it. Returns the
    /// removed socket and the detached noodles so undo can restore them.
    pub fn remove_input_socket(
        &mut self,
        node: NodeId,
        socket: SocketUid,
    ) -> Result<(Socket, Vec<Noodle>), GraphError> {
        let removed = {
            let n = self
                .nodes
                .get_mut(&node)
                .ok_or(GraphError::NodeNotFound(node))?;
            let idx = n
                .inputs
                .iter()
                .position(|s| s.uid == socket)
                .ok_or(GraphError::SocketNotFound { node, socket })?;
            n.inputs.remove(idx)
        };
        let detached: Vec<Noodle> = self
            .noodles()
            .iter()
            .filter(|e| e.to.node == node && e.to.socket == socket)
            .copied()
            .collect();
        self.noodles_mut()
            .retain(|e| !(e.to.node == node && e.to.socket == socket));
        self.mark_dirty_subtree(node);
        Ok((removed, detached))
    }

    /// Remove an output socket and GC every noodle touching it.
    pub fn remove_output_socket(
        &mut self,
        node: NodeId,
        socket: SocketUid,
    ) -> Result<(Socket, Vec<Noodle>), GraphError> {
        let removed = {
            let n = self
                .nodes
                .get_mut(&node)
                .ok_or(GraphError::NodeNotFound(node))?;
            let idx = n
                .outputs
                .iter()
                .position(|s| s.uid == socket)
                .ok_or(GraphError::SocketNotFound { node, socket })?;
            let removed = n.outputs.remove(idx);
            // Clear cached output for this uid.
            n.cached_outputs.remove(&socket);
            removed
        };
        let detached: Vec<Noodle> = self
            .noodles()
            .iter()
            .filter(|e| e.from.node == node && e.from.socket == socket)
            .copied()
            .collect();
        self.noodles_mut()
            .retain(|e| !(e.from.node == node && e.from.socket == socket));
        for e in &detached {
            self.mark_dirty_subtree(e.to.node);
        }
        Ok((removed, detached))
    }

    /// Permute a node's input sockets. `permutation[i] = j` means the
    /// new slot at position `i` is the previous slot at position `j`.
    /// Noodles are unaffected — they reference uids, which travel with the
    /// socket.
    pub fn reorder_input_sockets(
        &mut self,
        node: NodeId,
        permutation: &[usize],
    ) -> Result<(), GraphError> {
        let n = self
            .nodes
            .get_mut(&node)
            .ok_or(GraphError::NodeNotFound(node))?;
        if permutation.len() != n.inputs.len() {
            return Err(GraphError::ConnectionRejected(
                "reorder permutation length mismatch".into(),
            ));
        }
        let mut reordered: Vec<Socket> = Vec::with_capacity(n.inputs.len());
        for &src in permutation {
            if src >= n.inputs.len() {
                return Err(GraphError::ConnectionRejected(
                    "reorder permutation out of range".into(),
                ));
            }
            reordered.push(n.inputs[src].clone());
        }
        n.inputs = reordered;
        Ok(())
    }

    /// True when no noodle lands on the given input socket. The
    /// "free input" concept comes from NodeDesigner's
    /// `findInputSocketFree` — a UI-side scan for the next drop target
    /// when the user picks "connect from here" via right-click.
    /// Returns `false` when the socket doesn't exist (defensive).
    pub fn input_is_free(&self, node: NodeId, socket: SocketUid) -> bool {
        if self
            .nodes
            .get(&node)
            .and_then(|n| n.input_by_uid(socket))
            .is_none()
        {
            return false;
        }
        !self
            .noodles()
            .iter()
            .any(|n| n.to.node == node && n.to.socket == socket)
    }

    /// True when no noodle leaves the given output socket. Outputs have
    /// no exclusivity rule (one output can feed many inputs), so "free"
    /// here means "currently driving zero consumers".
    pub fn output_is_free(&self, node: NodeId, socket: SocketUid) -> bool {
        if self
            .nodes
            .get(&node)
            .and_then(|n| n.output_by_uid(socket))
            .is_none()
        {
            return false;
        }
        !self
            .noodles()
            .iter()
            .any(|n| n.from.node == node && n.from.socket == socket)
    }

    /// First input socket on `node` with no incoming noodle, in
    /// declaration order. Returns `None` if every input is wired (or
    /// the node has no inputs).
    pub fn first_free_input(&self, node: NodeId) -> Option<SocketUid> {
        let n = self.nodes.get(&node)?;
        n.inputs
            .iter()
            .find(|s| self.input_is_free(node, s.uid))
            .map(|s| s.uid)
    }

    /// First output socket on `node` with no outgoing noodles.
    pub fn first_free_output(&self, node: NodeId) -> Option<SocketUid> {
        let n = self.nodes.get(&node)?;
        n.outputs
            .iter()
            .find(|s| self.output_is_free(node, s.uid))
            .map(|s| s.uid)
    }

    /// Every noodle touching the given (node, socket) — either as source or
    /// target. Convenience for hooks that need to look up "is this source
    /// already wired?" or "what's connected to this input?".
    pub fn noodles_touching(
        &self,
        node: NodeId,
        socket: SocketUid,
    ) -> impl Iterator<Item = &Noodle> {
        self.noodles().iter().filter(move |e| {
            (e.from.node == node && e.from.socket == socket)
                || (e.to.node == node && e.to.socket == socket)
        })
    }
}
