//! Ported from NodeDesigner's `tests/unit/matter-graph-node-connections.test.ts`.
//!
//! Test-by-test mirror of the JS connection suite — same scenarios,
//! adapted to atomartist's uid-keyed `Noodle` API. Each `#[test]` carries
//! a doc-comment citing the JS test name so future readers can
//! cross-check both sides.
//!
//! Notes on the translation:
//! - JS `node.connect(0, other, 0)` (by socket *index*) becomes uid-keyed
//!   `graph.connect(Noodle::new(...))`. Index access doesn't exist at the
//!   engine layer.
//! - JS `node.connect("name", other, "name")` (by name) is exercised by
//!   resolving names → uids via `input_by_name` / `output_by_name`.
//! - JS auto-replaces an existing input connection on re-connect;
//!   atomartist returns `InputAlreadyConnected` and leaves replacement
//!   to the caller (see the atomartist-ui adapter for the UI-side
//!   replace-then-retry behavior). Engine tests assert the engine-side
//!   refusal directly.
//! - JS notifies both sides on connect via `onConnectionsChange(type)`;
//!   atomartist's engine only fires `on_input_connected` (target side).
//!   The source-side hook is a deliberate omission for now — flagged
//!   inline where it surfaces.
//! - Tests for JS-only convenience helpers (`connectByType`,
//!   `connectByTypeOutput`, by-id polymorphism) are skipped because
//!   atomartist's API surface is more uniform. The behavior they probe
//!   is covered indirectly by the equivalent uid-based tests.

#[path = "common/mod.rs"]
mod common;

use atomartist_lib::graph::executor::evaluate_all;
use atomartist_lib::graph::graph::{Graph, GraphError, Noodle, NoodleEndpoint};
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::socket::SocketUid;
use atomartist_lib::SocketType;

use common::{
    in_ep, noodle_into, noodles_from, out_ep, registry, registry_with_counters,
};

// ===========================================================================
// connect() — Basic Connection
// ===========================================================================

/// JS: "connect creates link between two nodes"
#[test]
fn connect_creates_link_between_two_nodes() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let b = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let from = out_ep(&g, a, "out");
    let to = in_ep(&g, b, "in");

    g.connect(Noodle { from, to }, &reg).unwrap();

    assert_eq!(g.noodles().len(), 1);
    assert_eq!(noodles_from(&g, a, from.socket), 1);
    assert!(noodle_into(&g, b, to.socket).is_some());
}

/// JS: "connect returns null when node not in graph"
///
/// atomartist's `connect` requires both endpoints' nodes to exist in the
/// graph; the engine returns `NodeNotFound` rather than silently
/// no-oping. (We can't build a `Noodle` endpoint without a `NodeId`, so
/// we synthesize a bogus id and assert the engine refuses it.)
#[test]
fn connect_refuses_when_target_node_not_in_graph() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let from = out_ep(&g, a, "out");
    let phantom = NodeId(9999);
    let bogus = Noodle {
        from,
        to: NoodleEndpoint::new(phantom, SocketUid(0)),
    };
    assert_eq!(
        g.connect(bogus, &reg),
        Err(GraphError::NodeNotFound(phantom)),
    );
}

/// JS: "connect by slot name works"
///
/// atomartist's connect API is uid-based; this test exercises the
/// name → uid resolution helper that mirrors JS's name-lookup overload.
#[test]
fn connect_by_socket_name_works() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let b = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let from = out_ep(&g, a, "out");
    let to = in_ep(&g, b, "in");
    g.connect(Noodle { from, to }, &reg).unwrap();
    assert!(noodle_into(&g, b, to.socket).is_some());
}

/// JS: "connect returns null for non-existent output slot"
#[test]
fn connect_refuses_non_existent_output_socket() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let b = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let to = in_ep(&g, b, "in");
    let bogus = Noodle {
        from: NoodleEndpoint::new(a, SocketUid(9999)),
        to,
    };
    assert!(matches!(
        g.connect(bogus, &reg),
        Err(GraphError::SocketNotFound { .. }),
    ));
}

/// JS: "connect returns null for non-existent input slot"
#[test]
fn connect_refuses_non_existent_input_socket() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let b = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let from = out_ep(&g, a, "out");
    let bogus = Noodle {
        from,
        to: NoodleEndpoint::new(b, SocketUid(9999)),
    };
    assert!(matches!(
        g.connect(bogus, &reg),
        Err(GraphError::SocketNotFound { .. }),
    ));
}

/// JS: "connect returns null for self-connection"
///
/// atomartist detects the self-loop as a cycle (has_path(start, start)
/// returns true immediately) and returns `CycleDetected`.
#[test]
fn connect_refuses_self_connection() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::LoopableNumber", [0.0, 0.0], &reg).unwrap();
    let from = out_ep(&g, a, "out");
    let to = in_ep(&g, a, "in");
    assert_eq!(
        g.connect(Noodle { from, to }, &reg),
        Err(GraphError::CycleDetected),
    );
}

/// JS: "connect returns null for incompatible types"
#[test]
fn connect_refuses_incompatible_types() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let b = g.add_new_node("test::ConsumerString", [100.0, 0.0], &reg).unwrap();
    let from = out_ep(&g, a, "out");
    let to = in_ep(&g, b, "in");
    assert!(matches!(
        g.connect(Noodle { from, to }, &reg),
        Err(GraphError::TypeMismatch { .. }),
    ));
}

/// JS counterpart: "connect with wildcard type succeeds" (output `*`).
///
/// JS has bidirectional wildcards: an output typed `*` matches any
/// input. atomartist only supports the wildcard on the *target* side
/// (`SocketType::None` placeholder, as used by the Output node's
/// trailing empty slot). Source-side wildcards are intentionally not
/// modeled — outputs always carry a concrete type. This test verifies
/// the target-side rule we *do* support.
#[test]
fn target_side_wildcard_accepts_any_source_type() {
    assert!(SocketType::Number.is_compatible_with(SocketType::None));
    assert!(SocketType::Geometry3d.is_compatible_with(SocketType::None));
    // Conversely, a None *source* doesn't satisfy a concrete target.
    assert!(!SocketType::None.is_compatible_with(SocketType::Number));
}

// ===========================================================================
// connect() — Replaces Existing Connection
// ===========================================================================

/// JS: "connect replaces existing input connection"
///
/// JS auto-disconnects the existing wire when a second one targets the
/// same input. atomartist returns `InputAlreadyConnected` and requires
/// the caller to disconnect explicitly. The atomartist-ui adapter
/// implements the JS replacement policy on top by handling that error
/// (see `app_state_model::try_add_noodle`). The engine-level contract
/// is the refusal — that's what we assert here.
#[test]
fn connect_refuses_duplicate_input_until_caller_disconnects() {
    let reg = registry();
    let mut g = Graph::new();
    let p1 = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let p2 = g.add_new_node("test::ProducerNumber", [0.0, 100.0], &reg).unwrap();
    let consumer = g.add_new_node("test::ConsumerNumber", [200.0, 50.0], &reg).unwrap();

    let to = in_ep(&g, consumer, "in");
    g.connect(Noodle { from: out_ep(&g, p1, "out"), to }, &reg).unwrap();

    let second = Noodle { from: out_ep(&g, p2, "out"), to };
    assert_eq!(
        g.connect(second, &reg),
        Err(GraphError::InputAlreadyConnected),
    );

    // After explicit disconnect of the first wire, the second succeeds
    // — the engine-equivalent of JS's auto-replace.
    let first = Noodle { from: out_ep(&g, p1, "out"), to };
    g.disconnect(&first, &reg).unwrap();
    g.connect(second, &reg).unwrap();
    assert_eq!(g.noodles().len(), 1);
    assert!(noodle_into(&g, consumer, to.socket).is_some());
    assert_eq!(noodles_from(&g, p1, first.from.socket), 0);
}

// ===========================================================================
// connect() — Multiple Outputs (one source, many targets)
// ===========================================================================

/// JS: "output can connect to multiple inputs"
#[test]
fn output_can_connect_to_multiple_inputs() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let c1 = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let c2 = g.add_new_node("test::ConsumerNumber", [100.0, 100.0], &reg).unwrap();
    let from = out_ep(&g, src, "out");
    g.connect(Noodle { from, to: in_ep(&g, c1, "in") }, &reg).unwrap();
    g.connect(Noodle { from, to: in_ep(&g, c2, "in") }, &reg).unwrap();
    assert_eq!(noodles_from(&g, src, from.socket), 2);
}

// ===========================================================================
// connect() — Callbacks (hooks)
// ===========================================================================

/// JS: "connect triggers onConnectionsChange on target"
#[test]
fn connect_fires_on_input_connected_on_target() {
    let (reg, counters) = registry_with_counters();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::CountingConsumer", [100.0, 0.0], &reg).unwrap();
    g.connect(
        Noodle {
            from: out_ep(&g, src, "out"),
            to: in_ep(&g, dst, "in"),
        },
        &reg,
    )
    .unwrap();
    assert_eq!(counters.connect_count(), 1);
}

/// JS: "connect triggers onConnectionsChange on source"
///
/// **Not yet supported in atomartist.** The engine notifies the target
/// node via `on_input_connected` but does not symmetrically notify the
/// source. If we need source-side observability later (e.g. for a node
/// that wants to track its consumers), we'd add an
/// `on_output_connected` hook in `NodeDef`. Documented here so the gap
/// is visible — no `#[test]` until we add the hook.

/// JS: "connect can be blocked by onConnectInput callback"
///
/// atomartist's mapping is `validate_input_connection`. A node that
/// returns `Err(reason)` causes `connect` to fail with
/// `ConnectionRejected`.
#[test]
fn connect_can_be_blocked_by_validate_input_connection() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::BlockingConsumer", [100.0, 0.0], &reg).unwrap();
    let result = g.connect(
        Noodle {
            from: out_ep(&g, src, "out"),
            to: in_ep(&g, dst, "in"),
        },
        &reg,
    );
    assert!(matches!(result, Err(GraphError::ConnectionRejected(_))));
    assert_eq!(g.noodles().len(), 0);
}

/// JS: "connect can be blocked by onConnectOutput callback"
///
/// **Not yet supported.** atomartist has no source-side veto hook (no
/// equivalent of JS `onConnectOutput`). Symmetric with the missing
/// `on_output_connected`; would land together if a use-case surfaces.

// ===========================================================================
// disconnectOutput() — sweep all noodles from a given output socket
// ===========================================================================

/// JS: "disconnectOutput removes all connections from output"
///
/// JS exposes `node.disconnectOutput(slot)` which sweeps every wire
/// leaving the given output socket. atomartist has no single-call
/// equivalent; we walk the noodle list and disconnect each.
#[test]
fn disconnecting_every_noodle_from_an_output_clears_targets() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let c1 = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let c2 = g.add_new_node("test::ConsumerNumber", [100.0, 100.0], &reg).unwrap();
    let from = out_ep(&g, src, "out");
    g.connect(Noodle { from, to: in_ep(&g, c1, "in") }, &reg).unwrap();
    g.connect(Noodle { from, to: in_ep(&g, c2, "in") }, &reg).unwrap();

    let to_drop: Vec<Noodle> = g
        .noodles()
        .iter()
        .filter(|n| n.from.node == src && n.from.socket == from.socket)
        .copied()
        .collect();
    for n in to_drop {
        g.disconnect(&n, &reg).unwrap();
    }
    assert_eq!(noodles_from(&g, src, from.socket), 0);
    assert!(noodle_into(&g, c1, in_ep(&g, c1, "in").socket).is_none());
    assert!(noodle_into(&g, c2, in_ep(&g, c2, "in").socket).is_none());
}

/// JS: "disconnectOutput to specific target only removes that connection"
#[test]
fn disconnecting_one_specific_target_leaves_others_intact() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let c1 = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let c2 = g.add_new_node("test::ConsumerNumber", [100.0, 100.0], &reg).unwrap();
    let from = out_ep(&g, src, "out");
    let to_c1 = in_ep(&g, c1, "in");
    let to_c2 = in_ep(&g, c2, "in");
    g.connect(Noodle { from, to: to_c1 }, &reg).unwrap();
    g.connect(Noodle { from, to: to_c2 }, &reg).unwrap();

    g.disconnect(&Noodle { from, to: to_c1 }, &reg).unwrap();
    assert_eq!(noodles_from(&g, src, from.socket), 1);
    assert!(noodle_into(&g, c1, to_c1.socket).is_none());
    assert!(noodle_into(&g, c2, to_c2.socket).is_some());
}

/// JS: "disconnectOutput returns false for invalid slot"
///
/// At the engine level, `disconnect` is keyed by the noodle endpoints
/// rather than a "slot" reference. The closest equivalent is: passing a
/// noodle whose endpoint references a non-existent socket is a
/// well-defined no-op — atomartist returns `Ok(false)` (no wire
/// matched).
#[test]
fn disconnecting_a_phantom_noodle_is_a_noop() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let phantom = Noodle {
        from: NoodleEndpoint::new(src, SocketUid(9999)),
        to: NoodleEndpoint::new(dst, SocketUid(9999)),
    };
    let removed = g.disconnect(&phantom, &reg).unwrap();
    assert!(!removed);
}

// ===========================================================================
// disconnectInput() — drop the single incoming wire on an input
// ===========================================================================

/// JS: "disconnectInput removes connection from input"
#[test]
fn disconnect_removes_connection_from_input() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let from = out_ep(&g, src, "out");
    let to = in_ep(&g, dst, "in");
    let noodle = Noodle { from, to };
    g.connect(noodle, &reg).unwrap();
    g.disconnect(&noodle, &reg).unwrap();
    assert!(noodle_into(&g, dst, to.socket).is_none());
    assert_eq!(noodles_from(&g, src, from.socket), 0);
}

/// JS: "disconnectInput triggers onConnectionsChange"
#[test]
fn disconnect_fires_on_input_disconnected_on_target() {
    let (reg, counters) = registry_with_counters();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::CountingConsumer", [100.0, 0.0], &reg).unwrap();
    let noodle = Noodle {
        from: out_ep(&g, src, "out"),
        to: in_ep(&g, dst, "in"),
    };
    g.connect(noodle, &reg).unwrap();
    g.disconnect(&noodle, &reg).unwrap();
    assert_eq!(counters.disconnect_count(), 1);
}

// ===========================================================================
// Data flow through connected nodes
// ===========================================================================

/// JS: "connected nodes can pass data"
///
/// JS does `node1.setOutputSocketData(0, 42)` and immediately reads
/// `node2.getInputSocketData(0)` — no executor pass. atomartist's data
/// flow goes through `cached_outputs` populated by `evaluate_all`, so
/// we drive a Number constant through a passthrough and check the
/// cached output on the downstream node.
#[test]
fn evaluated_graph_passes_data_along_connected_noodles() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    g.set_property(src, "value", PortValue::Number(42.0)).unwrap();
    let pt = g.add_new_node("test::PassthroughNumber", [100.0, 0.0], &reg).unwrap();
    g.connect(
        Noodle {
            from: out_ep(&g, src, "out"),
            to: in_ep(&g, pt, "in"),
        },
        &reg,
    )
    .unwrap();
    evaluate_all(&mut g, &reg).unwrap();
    let pt_out_uid = g.get(pt).unwrap().output_by_name("out").unwrap().uid;
    assert_eq!(
        g.get(pt).unwrap().cached_outputs.get(&pt_out_uid).cloned(),
        Some(PortValue::Number(42.0)),
    );
}

/// JS: "data flows through chain of nodes"
#[test]
fn evaluated_graph_passes_data_through_chain() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    g.set_property(src, "value", PortValue::Number(7.0)).unwrap();
    let p1 = g.add_new_node("test::PassthroughNumber", [100.0, 0.0], &reg).unwrap();
    let p2 = g.add_new_node("test::PassthroughNumber", [200.0, 0.0], &reg).unwrap();
    g.connect(
        Noodle {
            from: out_ep(&g, src, "out"),
            to: in_ep(&g, p1, "in"),
        },
        &reg,
    )
    .unwrap();
    g.connect(
        Noodle {
            from: out_ep(&g, p1, "out"),
            to: in_ep(&g, p2, "in"),
        },
        &reg,
    )
    .unwrap();
    evaluate_all(&mut g, &reg).unwrap();
    let p2_out = g.get(p2).unwrap().output_by_name("out").unwrap().uid;
    assert_eq!(
        g.get(p2).unwrap().cached_outputs.get(&p2_out).cloned(),
        Some(PortValue::Number(7.0)),
    );
}

/// JS: "disconnection stops data flow"
#[test]
fn disconnecting_stops_data_flow() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    g.set_property(src, "value", PortValue::Number(42.0)).unwrap();
    let pt = g.add_new_node("test::PassthroughNumber", [100.0, 0.0], &reg).unwrap();
    let noodle = Noodle {
        from: out_ep(&g, src, "out"),
        to: in_ep(&g, pt, "in"),
    };
    g.connect(noodle, &reg).unwrap();
    evaluate_all(&mut g, &reg).unwrap();
    let pt_out_uid = g.get(pt).unwrap().output_by_name("out").unwrap().uid;
    assert_eq!(
        g.get(pt).unwrap().cached_outputs.get(&pt_out_uid).cloned(),
        Some(PortValue::Number(42.0)),
    );

    g.disconnect(&noodle, &reg).unwrap();
    evaluate_all(&mut g, &reg).unwrap();
    // With nothing wired, the passthrough sees PortValue::None and
    // emits None on its output.
    assert_eq!(
        g.get(pt).unwrap().cached_outputs.get(&pt_out_uid).cloned(),
        Some(PortValue::None),
    );
}

// ===========================================================================
// Graph-level noodle management
// ===========================================================================

/// JS: "links are tracked in graph.noodles" + "graph.removeLink removes connection"
///
/// JS stores noodles in a `graph.noodles[id]` dict keyed by link-id.
/// atomartist stores them as a `Vec<Noodle>` with no per-noodle id;
/// identity is the (from, to) endpoint pair. We assert the engine-side
/// fact: the noodle is observable via `Graph::noodles()` and removing
/// it via `disconnect` clears it.
#[test]
fn noodles_are_tracked_by_graph_and_removed_by_disconnect() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let noodle = Noodle {
        from: out_ep(&g, src, "out"),
        to: in_ep(&g, dst, "in"),
    };
    g.connect(noodle, &reg).unwrap();
    assert_eq!(g.noodles().len(), 1);
    assert_eq!(g.noodles()[0].from.node, src);
    assert_eq!(g.noodles()[0].to.node, dst);

    g.disconnect(&noodle, &reg).unwrap();
    assert_eq!(g.noodles().len(), 0);
}

// ===========================================================================
// Edge cases
// ===========================================================================

/// JS: "connecting to same node/slot multiple times does not duplicate"
///
/// JS auto-disconnects + reconnects. atomartist returns
/// `InputAlreadyConnected` on the second attempt; the wire count stays
/// at one regardless.
#[test]
fn double_connect_does_not_create_two_noodles_into_the_same_input() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let noodle = Noodle {
        from: out_ep(&g, src, "out"),
        to: in_ep(&g, dst, "in"),
    };
    g.connect(noodle, &reg).unwrap();
    let dup = g.connect(noodle, &reg);
    assert_eq!(dup, Err(GraphError::InputAlreadyConnected));
    assert_eq!(g.noodles().len(), 1);
}

/// JS: "removing node cleans up all connections"
#[test]
fn removing_node_cleans_up_all_incident_noodles() {
    let reg = registry();
    let mut g = Graph::new();
    let a = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let b = g.add_new_node("test::PassthroughNumber", [100.0, 0.0], &reg).unwrap();
    let c = g.add_new_node("test::ConsumerNumber", [200.0, 0.0], &reg).unwrap();
    g.connect(
        Noodle {
            from: out_ep(&g, a, "out"),
            to: in_ep(&g, b, "in"),
        },
        &reg,
    )
    .unwrap();
    g.connect(
        Noodle {
            from: out_ep(&g, b, "out"),
            to: in_ep(&g, c, "in"),
        },
        &reg,
    )
    .unwrap();
    assert_eq!(g.noodles().len(), 2);

    let (_removed, detached) = g.remove_node(b).unwrap();
    assert_eq!(detached.len(), 2, "both noodles incident to b come back");
    assert_eq!(g.noodles().len(), 0);
}
