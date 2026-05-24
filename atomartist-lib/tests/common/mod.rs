//! Test fixtures shared by ports of NodeDesigner's connectivity test
//! suites.
//!
//! JS lets every `MatterGraphNode` add sockets ad-hoc. atomartist's
//! `NodeDef` is the schema source-of-truth, so we define a handful of
//! purpose-built test types here. Each one mirrors a JS pattern:
//! producer, consumer, passthrough, plus string-typed variants for
//! type-mismatch tests, and behavior-hook variants for veto / counter
//! tests.
//!
//! Test files use `#[path = "common/mod.rs"] mod common;` to pull this
//! in — Rust's integration tests are independent crates, so the
//! conventional `tests/common/mod.rs` layout keeps each test crate
//! lean.

#![allow(dead_code)]

use std::sync::Mutex;

use atomartist_lib::graph::node::PortValue;
use atomartist_lib::graph::socket::SocketUidAlloc;
use atomartist_lib::registry::{
    ConnectCtx, DisconnectCtx, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs,
    NodeRegistry, ValidateCtx,
};
use atomartist_lib::SocketType;

/// Constant Number source. Reads `properties["value"]` (default 0.0)
/// and emits it on `out`. Useful as the "upstream" end of a wire.
pub struct ProducerNumber;
impl NodeDef for ProducerNumber {
    fn type_id(&self) -> &'static str { "test::ProducerNumber" }
    fn category(&self) -> &'static str { "Test" }
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

/// Number sink. One input `in`, no outputs. Useful as the "downstream"
/// end of a wire when the test doesn't care about the value's onward
/// path.
pub struct ConsumerNumber;
impl NodeDef for ConsumerNumber {
    fn type_id(&self) -> &'static str { "test::ConsumerNumber" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::Number)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

/// String sink. Same shape as [`ConsumerNumber`] but typed `StringVal`
/// — lets type-mismatch tests force a refusal.
pub struct ConsumerString;
impl NodeDef for ConsumerString {
    fn type_id(&self) -> &'static str { "test::ConsumerString" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::StringVal)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

/// Number passthrough: forwards its `in` input to its `out` output
/// unchanged. Chains for data-flow tests.
pub struct PassthroughNumber;
impl NodeDef for PassthroughNumber {
    fn type_id(&self) -> &'static str { "test::PassthroughNumber" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::Number)
            .output("out", SocketType::Number)
            .build()
    }
    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.input_named("in").clone();
        let mut o = NodeOutputs::default();
        o.set("out", v);
        Ok(o)
    }
}

/// Node with both an input and an output on the same instance — lets us
/// exercise self-connection cases (cycle detection).
pub struct LoopableNumber;
impl NodeDef for LoopableNumber {
    fn type_id(&self) -> &'static str { "test::LoopableNumber" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::Number)
            .output("out", SocketType::Number)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

/// Consumer that vetoes every incoming connection via
/// `validate_input_connection` — the engine-side mapping for JS's
/// `node.onConnectInput = () => false`.
pub struct BlockingConsumer;
impl NodeDef for BlockingConsumer {
    fn type_id(&self) -> &'static str { "test::BlockingConsumer" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::Number)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
    fn validate_input_connection(&self, _ctx: &ValidateCtx) -> Result<(), String> {
        Err("blocked by validate hook".into())
    }
}

/// Counts the number of times its connect / disconnect hooks fire.
/// JS's `node.onConnectionsChange = () => { ... }` on the input side
/// maps onto atomartist's `on_input_connected` /
/// `on_input_disconnected` — this fixture lets a test assert both
/// fire exactly once per wire mutation.
///
/// State lives behind global mutexes because `NodeDef::on_*` receive
/// `&self` and the registry holds the def behind `Arc<dyn NodeDef>`.
/// Tests must call [`CountingConsumer::reset`] first.
pub struct CountingConsumer;

static CONNECT_COUNT: Mutex<u32> = Mutex::new(0);
static DISCONNECT_COUNT: Mutex<u32> = Mutex::new(0);

impl CountingConsumer {
    pub fn reset() {
        *CONNECT_COUNT.lock().unwrap() = 0;
        *DISCONNECT_COUNT.lock().unwrap() = 0;
    }
    pub fn connect_count() -> u32 {
        *CONNECT_COUNT.lock().unwrap()
    }
    pub fn disconnect_count() -> u32 {
        *DISCONNECT_COUNT.lock().unwrap()
    }
}

impl NodeDef for CountingConsumer {
    fn type_id(&self) -> &'static str { "test::CountingConsumer" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("in", SocketType::Number)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
    fn on_input_connected(&self, _ctx: &mut ConnectCtx) {
        *CONNECT_COUNT.lock().unwrap() += 1;
    }
    fn on_input_disconnected(&self, _ctx: &mut DisconnectCtx) {
        *DISCONNECT_COUNT.lock().unwrap() += 1;
    }
}

/// Node with zero sockets at instantiation. Used by the socket-management
/// port — JS lets `new MatterGraphNode("Test")` start empty and then
/// `node.addInputSocket(...)` / `node.addOutputSocket(...)` at will. Rust
/// nodes get their initial sockets from `NodeDef::instantiate`, so we
/// register this empty fixture as the canonical "blank" starting point.
pub struct BareNode;
impl NodeDef for BareNode {
    fn type_id(&self) -> &'static str { "test::BareNode" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, _alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(_alloc).build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

/// Source with three differently-typed outputs. Mirrors the JS test
/// fixture `test/multi-output` so the Output-node ports cover
/// "connect each of three source outputs into separate slots, then
/// disconnect one and verify the right slot collapses."
pub struct MultiOutputSource;
impl NodeDef for MultiOutputSource {
    fn type_id(&self) -> &'static str { "test::MultiOutputSource" }
    fn category(&self) -> &'static str { "Test" }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("Geometry", SocketType::Geometry3d)
            .output("Paths", SocketType::Path2d)
            .output("Color", SocketType::Color)
            .build()
    }
    fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        Ok(NodeOutputs::default())
    }
}

/// Build a registry pre-populated with every test fixture node.
pub fn registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    r.register(ProducerNumber);
    r.register(ConsumerNumber);
    r.register(ConsumerString);
    r.register(PassthroughNumber);
    r.register(LoopableNumber);
    r.register(BlockingConsumer);
    r.register(CountingConsumer);
    r.register(BareNode);
    r.register(MultiOutputSource);
    // Register all built-in atomartist node types so tests that
    // exercise dynamic-input nodes (Output, Combine, …) can spawn
    // them by `type_id`.
    atomartist_lib::nodes::register_all(&mut r);
    r
}

// ---------------------------------------------------------------------------
// Endpoint / noodle helpers
// ---------------------------------------------------------------------------

use atomartist_lib::graph::graph::{Graph, Noodle, NoodleEndpoint};
use atomartist_lib::graph::node::NodeId;
use atomartist_lib::graph::socket::{Socket, SocketUid};

/// Resolve a (node, output-name) pair into a [`NoodleEndpoint`].
pub fn out_ep(g: &Graph, node: NodeId, name: &str) -> NoodleEndpoint {
    let uid = g.get(node).unwrap().output_by_name(name).unwrap().uid;
    NoodleEndpoint::new(node, uid)
}

/// Resolve a (node, input-name) pair into a [`NoodleEndpoint`].
pub fn in_ep(g: &Graph, node: NodeId, name: &str) -> NoodleEndpoint {
    let uid = g.get(node).unwrap().input_by_name(name).unwrap().uid;
    NoodleEndpoint::new(node, uid)
}

/// Count noodles flowing *out* of (node, socket).
pub fn noodles_from(g: &Graph, node: NodeId, socket: SocketUid) -> usize {
    g.noodles()
        .iter()
        .filter(|n| n.from.node == node && n.from.socket == socket)
        .count()
}

/// Find the single noodle landing on (node, socket), if any.
pub fn noodle_into(g: &Graph, node: NodeId, socket: SocketUid) -> Option<Noodle> {
    g.noodles()
        .iter()
        .find(|n| n.to.node == node && n.to.socket == socket)
        .copied()
}

/// Mint + append an input socket on `node`, returning its uid. Mirrors
/// JS's `node.addInputSocket(name, type)` for the socket-management port.
pub fn add_input(g: &mut Graph, node: NodeId, name: &str, ty: SocketType) -> SocketUid {
    let uid = g.allocate_socket_uid();
    g.append_input_socket(node, Socket::new(uid, name, ty, false))
        .unwrap()
}

/// Mint + append an output socket on `node`, returning its uid. Mirrors
/// JS's `node.addOutputSocket(name, type)`.
pub fn add_output(g: &mut Graph, node: NodeId, name: &str, ty: SocketType) -> SocketUid {
    let uid = g.allocate_socket_uid();
    g.append_output_socket(node, Socket::new(uid, name, ty, false))
        .unwrap()
}
