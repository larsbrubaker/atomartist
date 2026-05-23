//! Ported from NodeDesigner's `tests/unit/matter-graph-node-sockets.test.ts`.
//!
//! Covers the socket-lookup surface: by-name, by-type, and "first free"
//! queries. atomartist's API mostly already exists (`input_by_name`,
//! `output_by_name`); the find-by-type and find-free helpers were added
//! to land this port (`NodeInstance::input_by_type` /
//! `output_by_type`, `Graph::input_is_free` / `output_is_free` /
//! `first_free_input` / `first_free_output`).
//!
//! ## Out of scope
//!
//! - `getConnectionPos`, `getSocketInPosition` — pixel-position /
//!   hit-testing on the canvas. Engine-layer tests skip these; they
//!   belong in `atomartist-ui-test` once we have a parallel suite.
//! - `MatterGraph.EVENT` socket-type filtering — we don't yet model
//!   event/action sockets (NodeDesigner's flow-control sockets). When
//!   we add them, the `typesNotAccepted` JS test maps directly to
//!   a filter on the iterator inside `first_free_input`.

#[path = "common/mod.rs"]
mod common;

use atomartist_lib::graph::graph::{Graph, Noodle};
use atomartist_lib::SocketType;

use common::{in_ep, out_ep, registry};

// ===========================================================================
// findInputSocket / findOutputSocket — by-name lookup
// ===========================================================================

/// JS: "findInputSocket returns -1 when no inputs exist"
#[test]
fn input_by_name_returns_none_when_no_inputs() {
    let reg = registry();
    let mut g = Graph::new();
    // ProducerNumber has only an output, no inputs.
    let n = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    assert!(g.get(n).unwrap().input_by_name("anything").is_none());
}

/// JS: "findInputSocket finds socket by name"
#[test]
fn input_by_name_returns_socket_when_present() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::PassthroughNumber", [0.0, 0.0], &reg).unwrap();
    let sock = g.get(n).unwrap().input_by_name("in").unwrap();
    assert_eq!(sock.name.as_ref(), "in");
    assert_eq!(sock.socket_type, SocketType::Number);
}

/// JS: "findInputSocket returns -1 for non-matching name"
#[test]
fn input_by_name_returns_none_for_unknown_name() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ConsumerNumber", [0.0, 0.0], &reg).unwrap();
    assert!(g.get(n).unwrap().input_by_name("nonexistent").is_none());
}

/// JS: "findOutputSocket returns -1 when no outputs exist"
#[test]
fn output_by_name_returns_none_when_no_outputs() {
    let reg = registry();
    let mut g = Graph::new();
    // ConsumerNumber has only an input, no outputs.
    let n = g.add_new_node("test::ConsumerNumber", [0.0, 0.0], &reg).unwrap();
    assert!(g.get(n).unwrap().output_by_name("anything").is_none());
}

/// JS: "findOutputSocket finds socket by name"
#[test]
fn output_by_name_returns_socket_when_present() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let sock = g.get(n).unwrap().output_by_name("out").unwrap();
    assert_eq!(sock.name.as_ref(), "out");
    assert_eq!(sock.socket_type, SocketType::Number);
}

// ===========================================================================
// findInputSocketFree / findOutputSocketFree — first un-wired socket
// ===========================================================================

/// JS: "findInputSocketFree returns -1 when no inputs exist"
#[test]
fn first_free_input_returns_none_when_no_inputs() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    assert!(g.first_free_input(n).is_none());
}

/// JS: "findInputSocketFree finds first free socket"
///
/// Adapted: atomartist's fixture nodes have a single input each, so we
/// chain two `PassthroughNumber` nodes' inputs through a single
/// producer to recreate the JS shape: a node with two inputs, the
/// first wired, the second free. We construct that via a custom
/// fixture-free approach — wire one of the inputs of a Combine node
/// (which has up to many slots after the first connect).
///
/// To keep this engine-level we register a simple two-input fixture
/// inline.
#[test]
fn first_free_input_skips_wired_inputs() {
    use atomartist_lib::graph::node::PortValue;
    use atomartist_lib::graph::socket::SocketUidAlloc;
    use atomartist_lib::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    };

    struct TwoInputNumber;
    impl NodeDef for TwoInputNumber {
        fn type_id(&self) -> &'static str { "test::TwoInputNumber" }
        fn category(&self) -> &'static str { "Test" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .input("a", SocketType::Number)
                .input("b", SocketType::Number)
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
    }
    let mut reg = registry();
    reg.register(TwoInputNumber);

    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let _ = PortValue::None; // keep PortValue import alive on platforms that strip unused.
    let tgt = g.add_new_node("test::TwoInputNumber", [100.0, 0.0], &reg).unwrap();

    // Wire input "a"; input "b" remains free.
    g.connect(
        Noodle {
            from: out_ep(&g, src, "out"),
            to: in_ep(&g, tgt, "a"),
        },
        &reg,
    )
    .unwrap();

    let free = g.first_free_input(tgt).unwrap();
    let free_sock = g.get(tgt).unwrap().input_by_uid(free).unwrap();
    assert_eq!(free_sock.name.as_ref(), "b", "free input should be 'b'");
}

/// JS: "findInputSocketFree returns -1 when all sockets connected"
#[test]
fn first_free_input_returns_none_when_all_wired() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let consumer = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    g.connect(
        Noodle {
            from: out_ep(&g, src, "out"),
            to: in_ep(&g, consumer, "in"),
        },
        &reg,
    )
    .unwrap();
    assert!(g.first_free_input(consumer).is_none());
}

/// JS: "findOutputSocketFree returns -1 when no outputs exist"
#[test]
fn first_free_output_returns_none_when_no_outputs() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ConsumerNumber", [0.0, 0.0], &reg).unwrap();
    assert!(g.first_free_output(n).is_none());
}

/// JS: "findOutputSocketFree finds first free socket"
///
/// Adapted with a custom two-output fixture (mirror of TwoInputNumber).
#[test]
fn first_free_output_skips_wired_outputs() {
    use atomartist_lib::graph::socket::SocketUidAlloc;
    use atomartist_lib::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    };

    struct TwoOutputNumber;
    impl NodeDef for TwoOutputNumber {
        fn type_id(&self) -> &'static str { "test::TwoOutputNumber" }
        fn category(&self) -> &'static str { "Test" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .output("a", SocketType::Number)
                .output("b", SocketType::Number)
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
    }
    let mut reg = registry();
    reg.register(TwoOutputNumber);

    let mut g = Graph::new();
    let src = g.add_new_node("test::TwoOutputNumber", [0.0, 0.0], &reg).unwrap();
    let consumer = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();

    // Wire output "a"; output "b" remains free.
    g.connect(
        Noodle {
            from: out_ep(&g, src, "a"),
            to: in_ep(&g, consumer, "in"),
        },
        &reg,
    )
    .unwrap();

    let free = g.first_free_output(src).unwrap();
    let free_sock = g.get(src).unwrap().output_by_uid(free).unwrap();
    assert_eq!(free_sock.name.as_ref(), "b");
}

// ===========================================================================
// findSocketByType / findInputSocketByType / findOutputSocketByType
// ===========================================================================

/// JS: "findSocketByType returns -1 when no matching sockets"
#[test]
fn input_by_type_returns_none_when_no_match() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ConsumerNumber", [0.0, 0.0], &reg).unwrap();
    assert!(
        g.get(n)
            .unwrap()
            .input_by_type(SocketType::StringVal)
            .is_none()
    );
}

/// JS: "findSocketByType finds input socket by type"
///
/// Mirror of the JS test: declare two inputs, look up by the second
/// one's type. Uses a custom two-input fixture so we have heterogeneous
/// types to disambiguate.
#[test]
fn input_by_type_finds_matching_socket() {
    use atomartist_lib::graph::socket::SocketUidAlloc;
    use atomartist_lib::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    };

    struct StrThenNum;
    impl NodeDef for StrThenNum {
        fn type_id(&self) -> &'static str { "test::StrThenNum" }
        fn category(&self) -> &'static str { "Test" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .input("str", SocketType::StringVal)
                .input("num", SocketType::Number)
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
    }
    let mut reg = registry();
    reg.register(StrThenNum);

    let mut g = Graph::new();
    let n = g.add_new_node("test::StrThenNum", [0.0, 0.0], &reg).unwrap();
    let by_type = g.get(n).unwrap().input_by_type(SocketType::Number).unwrap();
    assert_eq!(by_type.name.as_ref(), "num");
}

/// JS: "findSocketByType finds output socket by type"
#[test]
fn output_by_type_finds_matching_socket() {
    use atomartist_lib::graph::socket::SocketUidAlloc;
    use atomartist_lib::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    };

    struct StrThenNumOut;
    impl NodeDef for StrThenNumOut {
        fn type_id(&self) -> &'static str { "test::StrThenNumOut" }
        fn category(&self) -> &'static str { "Test" }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .output("str", SocketType::StringVal)
                .output("num", SocketType::Number)
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
    }
    let mut reg = registry();
    reg.register(StrThenNumOut);

    let mut g = Graph::new();
    let n = g.add_new_node("test::StrThenNumOut", [0.0, 0.0], &reg).unwrap();
    let by_type = g.get(n).unwrap().output_by_type(SocketType::Number).unwrap();
    assert_eq!(by_type.name.as_ref(), "num");
}

/// JS: "findInputSocketByType returns -1 for non-matching type"
#[test]
fn input_by_type_returns_none_for_non_matching_type() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ConsumerNumber", [0.0, 0.0], &reg).unwrap();
    assert!(
        g.get(n)
            .unwrap()
            .input_by_type(SocketType::Bool)
            .is_none()
    );
}

/// JS counterpart: "findSocketByType treats empty string and * as wildcard"
///
/// JS treats "" and "*" as "match any type". atomartist's by-type
/// queries are exact-match; the wildcard concept lives on the
/// compatibility-check side (target type `None` accepts any source —
/// see `node_connections.rs::target_side_wildcard_accepts_any_source_type`).
/// No wildcard semantics on lookups; document the divergence and move on.
#[test]
fn input_by_type_is_exact_match_no_wildcards() {
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ConsumerNumber", [0.0, 0.0], &reg).unwrap();
    // None is the placeholder type for empty trailing slots, not a
    // wildcard. A node with a concrete-typed input does not match a
    // None query.
    assert!(
        g.get(n)
            .unwrap()
            .input_by_type(SocketType::None)
            .is_none()
    );
}

// ===========================================================================
// is_free queries (engine-only — JS exposes this indirectly via findSocketFree)
// ===========================================================================

#[test]
fn input_is_free_flips_after_connect_and_disconnect() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let to_uid = in_ep(&g, dst, "in").socket;
    assert!(g.input_is_free(dst, to_uid));
    let noodle = Noodle {
        from: out_ep(&g, src, "out"),
        to: in_ep(&g, dst, "in"),
    };
    g.connect(noodle, &reg).unwrap();
    assert!(!g.input_is_free(dst, to_uid));
    g.disconnect(&noodle, &reg).unwrap();
    assert!(g.input_is_free(dst, to_uid));
}

#[test]
fn output_is_free_flips_after_connect_and_disconnect() {
    let reg = registry();
    let mut g = Graph::new();
    let src = g.add_new_node("test::ProducerNumber", [0.0, 0.0], &reg).unwrap();
    let dst = g.add_new_node("test::ConsumerNumber", [100.0, 0.0], &reg).unwrap();
    let from_uid = out_ep(&g, src, "out").socket;
    assert!(g.output_is_free(src, from_uid));
    let noodle = Noodle {
        from: out_ep(&g, src, "out"),
        to: in_ep(&g, dst, "in"),
    };
    g.connect(noodle, &reg).unwrap();
    assert!(!g.output_is_free(src, from_uid));
    g.disconnect(&noodle, &reg).unwrap();
    assert!(g.output_is_free(src, from_uid));
}

#[test]
fn is_free_returns_false_for_phantom_socket_uid() {
    use atomartist_lib::graph::socket::SocketUid;
    let reg = registry();
    let mut g = Graph::new();
    let n = g.add_new_node("test::ConsumerNumber", [0.0, 0.0], &reg).unwrap();
    // A uid the node doesn't own — defensive default is "not free"
    // (returning true would invite the caller to wire to a nonexistent
    // socket).
    assert!(!g.input_is_free(n, SocketUid(9999)));
    assert!(!g.output_is_free(n, SocketUid(9999)));
}
