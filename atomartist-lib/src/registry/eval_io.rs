//! The three value bags `NodeDef::evaluate` speaks in — [`NodeInputs`],
//! [`NodeProperties`], [`NodeOutputs`] — and the error it may raise,
//! [`NodeError`].
//!
//! Split out of `registry.rs` (which was at the 800-line limit) when the
//! warning channel landed on [`NodeOutputs`]. Purely a move: everything
//! here is re-exported from [`crate::registry`], so call sites are
//! unchanged. The parent file keeps the registry itself, the [`NodeDef`]
//! trait and the hook contexts.
//!
//! [`NodeDef`]: crate::registry::NodeDef

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUid;

/// Inputs handed to `NodeDef::evaluate` — for each connected input socket
/// (resolved by uid), the executor inserts the upstream value. Disconnected
/// optional inputs are absent from the map; node code should use the
/// `ctx.input*` accessors which fall back to `PortValue::None`.
#[derive(Default)]
pub struct NodeInputs {
    pub by_uid: HashMap<SocketUid, PortValue>,
    /// Source node id of the noodle landing on each input socket.
    /// Lets dynamic nodes (Output, Combine) tag their downstream
    /// payload with "which node directly fed this slot" — used by
    /// the 3-D pick path so clicking a rendered body selects the
    /// first node wired into Output's matching slot, matching
    /// NodeDesigner's `meshData.sourceNodeId` rule.
    pub sources: HashMap<SocketUid, crate::graph::node::NodeId>,
}

impl NodeInputs {
    pub fn insert(&mut self, uid: SocketUid, value: PortValue) {
        self.by_uid.insert(uid, value);
    }
    pub fn insert_with_source(
        &mut self,
        uid: SocketUid,
        value: PortValue,
        source: crate::graph::node::NodeId,
    ) {
        self.by_uid.insert(uid, value);
        self.sources.insert(uid, source);
    }
    pub fn get(&self, uid: SocketUid) -> &PortValue {
        self.by_uid.get(&uid).unwrap_or(&PortValue::None)
    }
    /// Direct upstream source of the noodle landing on `uid`, or
    /// `None` when the slot is disconnected.
    pub fn source(&self, uid: SocketUid) -> Option<crate::graph::node::NodeId> {
        self.sources.get(&uid).copied()
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

    /// Convenience accessor that unwraps `PortValue::Matrix4x4`,
    /// returning `default` if the property is missing or wrong-typed.
    /// Used by every geometry-producing node to pull its per-node
    /// transform property into the emitted `Geometry3d`.
    pub fn matrix4x4(&self, name: &str, default: [f32; 16]) -> [f32; 16] {
        match self.get(name) {
            PortValue::Matrix4x4(m) => *m,
            _ => default,
        }
    }

    /// Convenience accessor that unwraps `PortValue::Color`,
    /// returning `default` if the property is missing or wrong-typed.
    pub fn color(&self, name: &str, default: [f32; 4]) -> [f32; 4] {
        match self.get(name) {
            PortValue::Color(c) => *c,
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
    /// Things the user should know that are **not** failures: the node
    /// produced real output and everything downstream of it is valid.
    ///
    /// A [`NodeError`] is all-or-nothing — the executor blocks the node's
    /// whole downstream cone and the canvas badges it — which is the wrong
    /// answer for a degraded result. The Boolean node's partial union
    /// (plan step B-5) is the first case: parts the kernel refused are
    /// carried into the output un-unioned rather than lost, and the *only*
    /// thing wrong is that the user should hear about it. Warnings ride
    /// out with the outputs, reach [`EvalReport`](crate::graph::executor::EvalReport)`::warnings`,
    /// and surface as status-bar notices without failing anything.
    pub warnings: Vec<String>,
}

impl NodeOutputs {
    pub fn set(&mut self, name: impl Into<Arc<str>>, value: PortValue) {
        self.by_name.insert(name.into(), value);
    }

    /// Record one non-fatal message about this evaluation.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
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
