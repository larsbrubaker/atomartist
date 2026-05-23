//! Ported from NodeDesigner's
//! `tests/unit/matter-graph-socket-management.test.ts`.
//!
//! Covers per-node socket mutation: append + remove of inputs/outputs,
//! and the noodle GC that follows when a wired socket disappears.
//!
//! ## Mapping notes — JS → Rust
//!
//! - JS lets `new MatterGraphNode(...)` start empty and exposes
//!   `addInputSocket` / `addOutputSocket` directly on the node. atomartist
//!   sources initial sockets from `NodeDef::instantiate`; for these tests
//!   we use the `BareNode` fixture (zero sockets) plus
//!   `Graph::append_input_socket` / `append_output_socket` — wrapped by
//!   the shared `add_input` / `add_output` helpers in `common/mod.rs`.
//! - JS identifies sockets by numeric slot index; removing a socket
//!   shifts every later slot's index, which forces the JS implementation
//!   to walk every noodle and decrement `origin_socket` / `target_socket`.
//!   atomartist references sockets by stable `SocketUid`, so the JS tests
//!   that assert "noodle's index field got decremented after removal" are
//!   replaced here by "remaining noodles still reference the correct uids."
//! - JS-only surface area we deliberately skip in this port:
//!   - `onOutputAdded` / `onInputAdded` callbacks — no equivalent hook on
//!     `NodeDef`; sockets are minted in `instantiate` or via the
//!     `Graph::append_*_socket` API, never observed by a per-instance
//!     callback.
//!   - `onOutputRemoved` / `onInputRemoved` callbacks — same reason.
//!   - `addOutputSocket` returning a JS object with mutable fields —
//!     `append_output_socket` returns the new `SocketUid`; tests look the
//!     socket up via `output_by_uid` to assert fields.
//!   - `node.size[1]` growth — agg-gui's node-editor widget computes
//!     layout from the live socket list; engine tests don't model it.
//!   - `addOutputSocket` initializing `outputSockets` array when missing
//!     — `NodeInstance.inputs` / `.outputs` are `Vec<Socket>` and always
//!     exist.
//!   - `addInputSocket` defaulting `type` to `0` — `SocketType` is an
//!     enum, no implicit fallback.
//!   - `addConnection` (the JS "extra non-socket connection point" API)
//!     — atomartist has no equivalent concept; widgets use sockets.

#[path = "common/mod.rs"]
mod common;

use atomartist_lib::graph::graph::{Graph, Noodle};
use atomartist_lib::graph::socket::Socket;
use atomartist_lib::SocketType;

use common::{add_input, add_output, in_ep, out_ep, registry};

// ============================================================================
// addOutput — append output socket
// ============================================================================

/// JS: "addOutputSocket creates output socket with name and type"
#[test]
fn append_output_socket_records_name_and_type() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let uid = add_output(&mut g, n, "value", SocketType::Number);

    let node = g.get(n).unwrap();
    assert_eq!(node.outputs.len(), 1);
    let s = node.output_by_uid(uid).unwrap();
    assert_eq!(s.name.as_ref(), "value");
    assert_eq!(s.socket_type, SocketType::Number);
    // Fresh socket has no noodles leaving it.
    assert!(g.output_is_free(n, uid));
}

/// JS: "addOutputSocket returns the created output object"
///
/// In Rust the helper returns the new uid; we look up the socket via
/// that uid to assert the same identity.
#[test]
fn append_output_socket_returns_usable_uid() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let uid = add_output(&mut g, n, "result", SocketType::StringVal);

    let s = g.get(n).unwrap().output_by_uid(uid).unwrap();
    assert_eq!(s.name.as_ref(), "result");
    assert_eq!(s.socket_type, SocketType::StringVal);
}

/// JS: "addOutputSocket accepts extra_info properties" — the only such
/// property atomartist models is `display_label`. `Socket::with_label`
/// is the engine equivalent of JS's `extra_info.label`.
#[test]
fn appended_output_socket_can_carry_display_label() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let uid = g.allocate_socket_uid();
    let sock = Socket::new(uid, "custom", SocketType::Color, false).with_label("Custom Label");
    g.append_output_socket(n, sock).unwrap();

    let s = g.get(n).unwrap().output_by_uid(uid).unwrap();
    assert_eq!(s.display_label.as_deref().map(|a| a.as_ref()), Some("Custom Label"));
    assert_eq!(s.label(), "Custom Label");
}

/// JS: "addOutputSocket can add multiple outputs sequentially"
#[test]
fn appending_multiple_outputs_preserves_order() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    add_output(&mut g, n, "first", SocketType::Number);
    add_output(&mut g, n, "second", SocketType::StringVal);
    add_output(&mut g, n, "third", SocketType::Number);

    let outs = &g.get(n).unwrap().outputs;
    assert_eq!(outs.len(), 3);
    assert_eq!(outs[0].name.as_ref(), "first");
    assert_eq!(outs[1].name.as_ref(), "second");
    assert_eq!(outs[2].name.as_ref(), "third");
}

// ============================================================================
// addOutputSockets — batched form
// ============================================================================

/// JS: "addOutputSockets adds multiple outputs from array"
///
/// Rust has no single bulk API on `Graph` — the batched JS helper maps to
/// a loop of `append_output_socket`. This test exists to lock in that the
/// loop form really does append in order.
#[test]
fn batch_append_outputs_in_order() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    for (name, ty) in [
        ("out1", SocketType::Number),
        ("out2", SocketType::StringVal),
        ("out3", SocketType::Number),
    ] {
        add_output(&mut g, n, name, ty);
    }

    let outs = &g.get(n).unwrap().outputs;
    assert_eq!(outs.len(), 3);
    assert_eq!(outs[0].name.as_ref(), "out1");
    assert_eq!(outs[1].name.as_ref(), "out2");
    assert_eq!(outs[2].name.as_ref(), "out3");
}

/// JS: "addOutputSockets handles empty array"
#[test]
fn batch_append_empty_is_noop() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let pairs: [(&str, SocketType); 0] = [];
    for (name, ty) in pairs {
        add_output(&mut g, n, name, ty);
    }
    assert_eq!(g.get(n).unwrap().outputs.len(), 0);
}

// ============================================================================
// removeOutputSocket
// ============================================================================

/// JS: "removeOutputSocket removes output at specified socket"
#[test]
fn remove_output_socket_keeps_remaining_outputs() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    add_output(&mut g, n, "keep1", SocketType::Number);
    let mid = add_output(&mut g, n, "remove", SocketType::StringVal);
    add_output(&mut g, n, "keep2", SocketType::Number);

    g.remove_output_socket(n, mid).unwrap();

    let outs = &g.get(n).unwrap().outputs;
    assert_eq!(outs.len(), 2);
    assert_eq!(outs[0].name.as_ref(), "keep1");
    assert_eq!(outs[1].name.as_ref(), "keep2");
}

/// JS: "removeOutputSocket disconnects output before removing"
#[test]
fn remove_output_socket_gcs_noodles_leaving_it() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let out = add_output(&mut g, n1, "out", SocketType::Number);
    let in_uid = add_input(&mut g, n2, "in", SocketType::Number);
    g.connect(Noodle::new(n1, out, n2, in_uid), &reg).unwrap();
    assert_eq!(g.noodle_count(), 1);

    let (_removed, detached) = g.remove_output_socket(n1, out).unwrap();

    assert_eq!(g.get(n1).unwrap().outputs.len(), 0);
    assert_eq!(detached.len(), 1, "removed noodles returned for undo");
    assert_eq!(g.noodle_count(), 0, "noodle GC'd from the graph");
}

/// JS-flavoured: "removeOutputSocket updates noodle origin_socket for
/// remaining outputs".
///
/// The JS behavior is a direct consequence of identifying sockets by
/// numeric index. atomartist references sockets by uid, so removing
/// socket 0 leaves socket 2's uid (and therefore its noodle) untouched.
/// We assert the uid-stability guarantee instead: after removing a
/// sibling, a noodle on a surviving socket still resolves to the right
/// socket.
#[test]
fn remove_output_socket_does_not_disturb_sibling_noodles() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let out0 = add_output(&mut g, n1, "out0", SocketType::Number);
    add_output(&mut g, n1, "out1", SocketType::Number);
    let out2 = add_output(&mut g, n1, "out2", SocketType::Number);
    let in_uid = add_input(&mut g, n2, "in", SocketType::Number);

    g.connect(Noodle::new(n1, out2, n2, in_uid), &reg).unwrap();

    // Remove the first output. JS would have to renumber the noodle; we
    // verify nothing was renumbered because nothing needed to be.
    g.remove_output_socket(n1, out0).unwrap();

    let n1_now = g.get(n1).unwrap();
    assert_eq!(n1_now.outputs.len(), 2);
    // out2 is still present under its original uid.
    assert!(n1_now.output_by_uid(out2).is_some());
    // The noodle still points to that uid.
    let nood = g.noodles()[0];
    assert_eq!(nood.from.socket, out2);
    assert_eq!(nood.to.socket, in_uid);
}

// ============================================================================
// addInputSocket
// ============================================================================

/// JS: "addInputSocket creates input socket with name and type"
#[test]
fn append_input_socket_records_name_and_type() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let uid = add_input(&mut g, n, "value", SocketType::Number);

    let node = g.get(n).unwrap();
    assert_eq!(node.inputs.len(), 1);
    let s = node.input_by_uid(uid).unwrap();
    assert_eq!(s.name.as_ref(), "value");
    assert_eq!(s.socket_type, SocketType::Number);
    assert!(g.input_is_free(n, uid));
}

/// JS: "addInputSocket returns the created input object"
#[test]
fn append_input_socket_returns_usable_uid() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let uid = add_input(&mut g, n, "data", SocketType::StringVal);

    let s = g.get(n).unwrap().input_by_uid(uid).unwrap();
    assert_eq!(s.name.as_ref(), "data");
    assert_eq!(s.socket_type, SocketType::StringVal);
}

/// JS: "addInputSocket accepts extra_info properties"
#[test]
fn appended_input_socket_can_carry_display_label() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let uid = g.allocate_socket_uid();
    let sock = Socket::new(uid, "custom", SocketType::Color, false).with_label("Custom Input");
    g.append_input_socket(n, sock).unwrap();

    let s = g.get(n).unwrap().input_by_uid(uid).unwrap();
    assert_eq!(s.label(), "Custom Input");
}

// ============================================================================
// addInputSockets — batched form
// ============================================================================

/// JS: "addInputSockets adds multiple inputs from array"
#[test]
fn batch_append_inputs_in_order() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    for (name, ty) in [
        ("in1", SocketType::Number),
        ("in2", SocketType::StringVal),
        ("in3", SocketType::Number),
    ] {
        add_input(&mut g, n, name, ty);
    }

    let ins = &g.get(n).unwrap().inputs;
    assert_eq!(ins.len(), 3);
    assert_eq!(ins[0].name.as_ref(), "in1");
    assert_eq!(ins[1].name.as_ref(), "in2");
    assert_eq!(ins[2].name.as_ref(), "in3");
}

// ============================================================================
// removeInputSocket
// ============================================================================

/// JS: "removeInputSocket removes input at specified socket"
#[test]
fn remove_input_socket_keeps_remaining_inputs() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    add_input(&mut g, n, "keep1", SocketType::Number);
    let mid = add_input(&mut g, n, "remove", SocketType::StringVal);
    add_input(&mut g, n, "keep2", SocketType::Number);

    g.remove_input_socket(n, mid).unwrap();

    let ins = &g.get(n).unwrap().inputs;
    assert_eq!(ins.len(), 2);
    assert_eq!(ins[0].name.as_ref(), "keep1");
    assert_eq!(ins[1].name.as_ref(), "keep2");
}

/// JS: "removeInputSocket disconnects input before removing"
#[test]
fn remove_input_socket_gcs_incoming_noodle() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let out = add_output(&mut g, n1, "out", SocketType::Number);
    let in_uid = add_input(&mut g, n2, "in", SocketType::Number);
    g.connect(Noodle::new(n1, out, n2, in_uid), &reg).unwrap();
    assert_eq!(g.noodle_count(), 1);

    let (_removed, detached) = g.remove_input_socket(n2, in_uid).unwrap();

    assert_eq!(g.get(n2).unwrap().inputs.len(), 0);
    assert_eq!(detached.len(), 1);
    assert_eq!(g.noodle_count(), 0);
    // The source's output socket survives — only the consumer side was removed.
    assert!(g.get(n1).unwrap().output_by_uid(out).is_some());
}

/// JS-flavoured: noodle target_socket stability after a sibling input is
/// removed. As with the output side, uid-keyed identity means we just
/// confirm the surviving noodle still resolves correctly.
#[test]
fn remove_input_socket_does_not_disturb_sibling_noodles() {
    let reg = registry();
    let mut g = Graph::new();
    let n1 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let n2 = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let out = add_output(&mut g, n1, "out", SocketType::Number);
    let in0 = add_input(&mut g, n2, "in0", SocketType::Number);
    add_input(&mut g, n2, "in1", SocketType::Number);
    let in2 = add_input(&mut g, n2, "in2", SocketType::Number);

    g.connect(Noodle::new(n1, out, n2, in2), &reg).unwrap();

    g.remove_input_socket(n2, in0).unwrap();

    let n2_now = g.get(n2).unwrap();
    assert_eq!(n2_now.inputs.len(), 2);
    assert!(n2_now.input_by_uid(in2).is_some());
    let nood = g.noodles()[0];
    assert_eq!(nood.to.socket, in2);
}

// ============================================================================
// Edge cases / integration
// ============================================================================

/// JS: "can add and remove outputs in sequence"
#[test]
fn add_remove_add_outputs_sequence() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    add_output(&mut g, n, "a", SocketType::Number);
    let b = add_output(&mut g, n, "b", SocketType::Number);
    add_output(&mut g, n, "c", SocketType::Number);
    assert_eq!(g.get(n).unwrap().outputs.len(), 3);

    g.remove_output_socket(n, b).unwrap();

    let outs = &g.get(n).unwrap().outputs;
    assert_eq!(outs.len(), 2);
    assert_eq!(outs[0].name.as_ref(), "a");
    assert_eq!(outs[1].name.as_ref(), "c");

    add_output(&mut g, n, "d", SocketType::Number);
    let outs = &g.get(n).unwrap().outputs;
    assert_eq!(outs.len(), 3);
    assert_eq!(outs[2].name.as_ref(), "d");
}

/// JS: "can add and remove inputs in sequence"
#[test]
fn add_remove_inputs_sequence() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let x = add_input(&mut g, n, "x", SocketType::Number);
    add_input(&mut g, n, "y", SocketType::Number);
    add_input(&mut g, n, "z", SocketType::Number);
    assert_eq!(g.get(n).unwrap().inputs.len(), 3);

    g.remove_input_socket(n, x).unwrap();

    let ins = &g.get(n).unwrap().inputs;
    assert_eq!(ins.len(), 2);
    assert_eq!(ins[0].name.as_ref(), "y");
    assert_eq!(ins[1].name.as_ref(), "z");
}

/// JS: "removing all outputs leaves empty array"
#[test]
fn remove_only_output_leaves_empty_list() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let only = add_output(&mut g, n, "only", SocketType::Number);
    g.remove_output_socket(n, only).unwrap();
    assert_eq!(g.get(n).unwrap().outputs.len(), 0);
}

/// JS: "removing all inputs leaves empty array"
#[test]
fn remove_only_input_leaves_empty_list() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let only = add_input(&mut g, n, "only", SocketType::Number);
    g.remove_input_socket(n, only).unwrap();
    assert_eq!(g.get(n).unwrap().inputs.len(), 0);
}

/// JS: "complex scenario: add, connect, remove, reconnect"
///
/// Two noodles exist before the mutation. After removing out1, out2 must
/// still be present (under its original uid) with its noodle intact.
#[test]
fn complex_add_connect_remove_keeps_other_noodles_live() {
    let reg = registry();
    let mut g = Graph::new();
    let source = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();
    let target = g.add_new_node("test::BareNode", [0.0, 0.0], &reg).unwrap();

    let out1 = add_output(&mut g, source, "out1", SocketType::Number);
    let out2 = add_output(&mut g, source, "out2", SocketType::Number);
    let in1 = add_input(&mut g, target, "in1", SocketType::Number);
    let in2 = add_input(&mut g, target, "in2", SocketType::Number);

    g.connect(Noodle::new(source, out1, target, in1), &reg).unwrap();
    g.connect(Noodle::new(source, out2, target, in2), &reg).unwrap();
    assert_eq!(g.noodle_count(), 2);

    g.remove_output_socket(source, out1).unwrap();

    // out2 still around under same uid; only one noodle left, the out2→in2 one.
    let s = g.get(source).unwrap();
    assert_eq!(s.outputs.len(), 1);
    assert_eq!(s.outputs[0].name.as_ref(), "out2");
    assert_eq!(s.outputs[0].uid, out2);
    assert_eq!(g.noodle_count(), 1);
    let surviving = g.noodles()[0];
    assert_eq!(surviving.from.socket, out2);
    assert_eq!(surviving.to.socket, in2);

    // And the in_ep / out_ep helpers still resolve cleanly post-mutation.
    let _ = (in_ep(&g, target, "in2"), out_ep(&g, source, "out2"));
}
