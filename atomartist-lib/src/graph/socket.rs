//! Socket — the unit of input/output on a node instance.
//!
//! Sockets live on the [`NodeInstance`](super::node::NodeInstance), not on
//! the type definition. Each socket carries a [`SocketUid`] — a stable,
//! graph-unique identifier that survives renames, type changes, and
//! reorderings. Noodles reference sockets exclusively by `(NodeId, SocketUid)`,
//! so user-visible mutations (renaming a slot, dragging the row order,
//! adopting a source's type on connect) leave noodles intact.
//!
//! `NodeDef::instantiate` mints the initial socket layout for a new node
//! instance; from that point on, the instance owns its sockets and a
//! `NodeDef`'s static "what sockets do I have?" question no longer applies.

use std::sync::Arc;

use crate::socket_types::SocketType;

/// Stable identity for one socket on one node instance.
///
/// Allocated monotonically by the graph. Two sockets that share a `name`
/// never share a `SocketUid`; conversely, a socket's uid is preserved
/// across renames, type changes, and reorder. Noodles reference uids, so
/// renames don't invalidate connections — the lesson from NodeDesigner
/// where name-keyed noodles forced socket churn to walk every link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SocketUid(pub u64);

/// One input or output socket on a node instance.
///
/// All fields are mutable on the instance — `on_input_connected` is the
/// canonical site for dynamic-input nodes (like Output) to mutate `name`,
/// `display_label`, and `socket_type` after the user wires a source up.
#[derive(Clone, Debug)]
pub struct Socket {
    pub uid: SocketUid,
    /// Internal / serialization name. Used as the canonical key for
    /// `EvalCtx::input_named` and `NodeOutputs::set`; not necessarily
    /// what the user sees on the canvas.
    pub name: Arc<str>,
    /// Human-readable override shown next to the socket row. Falls back
    /// to `name` when `None`. Dynamic nodes use this to show
    /// "{SourceTitle} - {SourceSocket}" while keeping `name` stable.
    pub display_label: Option<Arc<str>>,
    /// Currently-resolved socket type. Mutable so dynamic-input nodes
    /// can adopt the source's type at connect time.
    pub socket_type: SocketType,
    /// True when this socket is allowed to be unconnected. The executor
    /// passes `PortValue::None` if an optional input has no incoming
    /// noodle.
    pub optional: bool,
}

impl Socket {
    /// Build a socket with a graph-fresh uid (caller supplies the allocator).
    pub fn new(
        uid: SocketUid,
        name: impl Into<Arc<str>>,
        socket_type: SocketType,
        optional: bool,
    ) -> Self {
        Self {
            uid,
            name: name.into(),
            display_label: None,
            socket_type,
            optional,
        }
    }

    /// Attach a display label override.
    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.display_label = Some(label.into());
        self
    }

    /// Human-readable label — `display_label` if set, else `name`.
    pub fn label(&self) -> &str {
        self.display_label.as_deref().unwrap_or(&self.name)
    }
}

/// Hands out fresh [`SocketUid`]s. One allocator per [`Graph`](super::graph::Graph);
/// monotonically increasing, never reused even after a socket is removed,
/// so undo commands can restore deleted sockets without uid collisions.
#[derive(Clone, Debug, Default)]
pub struct SocketUidAlloc {
    next: u64,
}

impl SocketUidAlloc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resume allocation from a specific value — used by the loader after
    /// deserializing a save so newly-allocated uids don't collide with
    /// uids already in the file.
    pub fn resume_from(next: u64) -> Self {
        Self { next }
    }

    pub fn allocate(&mut self) -> SocketUid {
        let uid = SocketUid(self.next);
        self.next += 1;
        uid
    }

    /// The value the next `allocate` will return. Serialized into save
    /// files so reload resumes the sequence.
    pub fn peek_next(&self) -> u64 {
        self.next
    }

    /// Bump the allocator past `seen` so the next `allocate` won't collide.
    /// Used while loading saved nodes whose socket uids are dictated by
    /// the file.
    pub fn observe(&mut self, seen: SocketUid) {
        if seen.0 + 1 > self.next {
            self.next = seen.0 + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_is_monotonic() {
        let mut a = SocketUidAlloc::new();
        assert_eq!(a.allocate(), SocketUid(0));
        assert_eq!(a.allocate(), SocketUid(1));
        assert_eq!(a.allocate(), SocketUid(2));
        assert_eq!(a.peek_next(), 3);
    }

    #[test]
    fn observe_bumps_past_known_value() {
        let mut a = SocketUidAlloc::new();
        a.observe(SocketUid(99));
        assert_eq!(a.allocate(), SocketUid(100));
    }

    #[test]
    fn observe_below_current_is_noop() {
        let mut a = SocketUidAlloc::new();
        a.observe(SocketUid(50));
        a.allocate(); // 51
        a.observe(SocketUid(10));
        assert_eq!(a.allocate(), SocketUid(52));
    }

    #[test]
    fn resume_from_picks_up_sequence() {
        let mut a = SocketUidAlloc::resume_from(42);
        assert_eq!(a.allocate(), SocketUid(42));
        assert_eq!(a.allocate(), SocketUid(43));
    }

    #[test]
    fn socket_label_falls_back_to_name() {
        let s = Socket::new(SocketUid(0), "size", SocketType::Number, false);
        assert_eq!(s.label(), "size");
        let s = s.with_label("Size (mm)");
        assert_eq!(s.label(), "Size (mm)");
    }
}
