//! The trailing-empty-slot input model shared by the n-ary geometry
//! operations ([`super::combine_node`], [`super::boolean_node`]).
//!
//! A node using this model starts with one empty input slot typed
//! [`SocketType::Geometry3d`]. On connect the slot adopts the source
//! socket's name (uniquified within the node) plus a
//! `"<Type> - <socket>"` display label, and a fresh trailing empty is
//! appended. On disconnect the slot collapses and the trailing empty is
//! restored.
//!
//! The slot's **uid** is preserved across adoption and survives save /
//! load ([`crate::serialization::graph_json`] stores it), which is what
//! lets a node key per-input state — the Boolean node's "Part(s) to
//! Subtract" selection — off the socket rather than off a name the user
//! can change by rewiring.
//!
//! Extracted from `combine_node` when Boolean became the second consumer:
//! the uniquifying rule and the "always exactly one trailing empty"
//! invariant are the parts that must not drift apart between the two.

use std::sync::Arc;

use crate::graph::graph::Graph;
use crate::graph::node::NodeId;
use crate::graph::socket::Socket;
use crate::registry::{ConnectCtx, DisconnectCtx};
use crate::socket_types::SocketType;

/// Adopt the source socket's name + label onto the just-connected slot and
/// append a fresh trailing empty. Idempotent: re-running it on an already
/// adopted slot re-derives the same name.
pub fn adopt_connected_slot(ctx: &mut ConnectCtx) {
    let (source_name, source_title) = {
        let src = match ctx.graph.get(ctx.source_node) {
            Some(n) => n,
            None => return,
        };
        let name = src
            .output_by_uid(ctx.source_socket)
            .map(|s| s.name.to_string())
            .unwrap_or_default();
        (name, src.type_id.to_string())
    };

    // Unique internal name within this node's input list. Two Box
    // sources both emit "out" → suffix `_1`, `_2`, …
    let unique_name = {
        let me = match ctx.graph.get(ctx.this_node) {
            Some(n) => n,
            None => return,
        };
        let mut candidate = source_name.clone();
        let mut suffix = 1;
        while me
            .inputs
            .iter()
            .any(|s| s.uid != ctx.target_socket && s.name.as_ref() == candidate.as_str())
        {
            candidate = format!("{}_{}", source_name, suffix);
            suffix += 1;
        }
        candidate
    };

    let display_label = format!("{} - {}", source_title, source_name);
    let _ = ctx.graph.rename_socket(
        ctx.this_node,
        ctx.target_socket,
        Arc::<str>::from(unique_name.as_str()),
    );
    let _ = ctx.graph.relabel_socket(
        ctx.this_node,
        ctx.target_socket,
        Some(Arc::<str>::from(display_label.as_str())),
    );
    // Type stays Geometry3d — no retype needed.

    ensure_trailing_empty_input(ctx.graph, ctx.this_node);
}

/// Drop the slot an edge just left (unless it is already the placeholder)
/// and make sure a trailing empty remains.
pub fn collapse_disconnected_slot(ctx: &mut DisconnectCtx) {
    let was_configured = match ctx.graph.get(ctx.this_node) {
        Some(n) => n
            .input_by_uid(ctx.target_socket)
            .map(|s| !s.name.as_ref().is_empty())
            .unwrap_or(false),
        None => return,
    };
    if was_configured {
        let _ = ctx.graph.remove_input_socket(ctx.this_node, ctx.target_socket);
    }
    ensure_trailing_empty_input(ctx.graph, ctx.this_node);
}

/// Append a trailing empty Geometry3d input if the node doesn't already
/// end in one. The empty slot is typed `Geometry3d` (not `None`) so the
/// canvas's compatibility check refuses non-geometry drops up front.
pub fn ensure_trailing_empty_input(graph: &mut Graph, node: NodeId) {
    let needs_one = match graph.get(node) {
        Some(n) => n
            .inputs
            .last()
            .map(|s| !s.name.as_ref().is_empty())
            .unwrap_or(true),
        None => return,
    };
    if needs_one {
        let uid = graph.allocate_socket_uid();
        let _ = graph.append_input_socket(node, Socket::new(uid, "", SocketType::Geometry3d, true));
    }
}
