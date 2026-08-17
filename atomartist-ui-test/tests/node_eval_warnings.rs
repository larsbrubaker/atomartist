//! End-to-end cover for the warning channel added in step B-5 of
//! `docs/boolean-node-plan.md`: a node that succeeds with a *degraded*
//! result (a Boolean union that had to leave a part out) must reach the
//! status bar — and must NOT be badged as broken, or block anything
//! downstream of it.
//!
//! No NodeDesigner ancestor; the shape comes from MatterCAD's
//! `BooleanObject3D.ReportSkippedOperands`, which logs the skipped parts
//! while the boolean's own result stays in the scene.
//!
//! The warning node is a test-local `NodeDef` for the same reason the
//! failing one in `node_eval_errors.rs` is: the plumbing under test is
//! evaluator → state → status bar, and a purpose-built node makes
//! "warns" and "is quiet" one property write apart. The real Boolean's
//! degradation is covered by `atomartist-lib`'s `boolean_degrade_tests`.

use atomartist_lib::graph::node::PortValue;
use atomartist_lib::graph::socket::SocketUidAlloc;
use atomartist_lib::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry,
};
use atomartist_lib::socket_types::SocketType;
use atomartist_lib::Graph;
use atomartist_ui::app_state_model::node_views;
use atomartist_ui::{AppState, NoticeLevel};
use atomartist_ui_test::{test_storage_registry, TestHarness};

/// Succeeds, but says it had to leave a part out while `degraded` is set.
struct Partial;

impl NodeDef for Partial {
    fn type_id(&self) -> &'static str {
        "Partial"
    }
    fn display_name(&self) -> &'static str {
        "Partial"
    }
    fn category(&self) -> &'static str {
        "Operations 3D"
    }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Number)
            .build()
    }
    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Number(1.0));
        if ctx.properties.bool_("degraded", false) {
            out.warn("1 of 3 parts are not watertight solids: 'b'");
        }
        Ok(out)
    }
}

/// A node downstream of the warning one, so the test can prove the
/// warning did not block the cone the way a failure does.
struct Passthrough;

impl NodeDef for Passthrough {
    fn type_id(&self) -> &'static str {
        "Passthrough"
    }
    fn display_name(&self) -> &'static str {
        "Passthrough"
    }
    fn category(&self) -> &'static str {
        "Operations 3D"
    }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .input("a", SocketType::Number)
            .output("out", SocketType::Number)
            .build()
    }
    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let a = match ctx.input_named("a") {
            PortValue::Number(n) => *n,
            _ => 0.0,
        };
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Number(a + 1.0));
        Ok(out)
    }
}

type Ids = (
    atomartist_lib::graph::node::NodeId,
    atomartist_lib::graph::node::NodeId,
);

/// A harness holding `Partial → Passthrough`, with the warning armed.
fn harness_with_degraded_node() -> (TestHarness, Ids) {
    let mut registry = NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut registry);
    registry.register(Partial);
    registry.register(Passthrough);

    let mut graph = Graph::new();
    let warner = graph
        .add_new_node("Partial", [0.0, 0.0], &registry)
        .expect("Partial is registered");
    let sink = graph
        .add_new_node("Passthrough", [200.0, 0.0], &registry)
        .expect("Passthrough is registered");
    graph
        .set_property(warner, "degraded", PortValue::Bool(true))
        .expect("the node accepts the property");
    let from = graph
        .get(warner)
        .unwrap()
        .output_by_name("out")
        .unwrap()
        .uid;
    let to = graph.get(sink).unwrap().input_by_name("a").unwrap().uid;
    graph
        .connect(
            atomartist_lib::graph::graph::Noodle::new(warner, from, sink, to),
            &registry,
        )
        .expect("the sockets are compatible");

    let state = AppState::with_storage(graph, registry, test_storage_registry());
    (TestHarness::with_app_state(state), (warner, sink))
}

/// The `NodeView::error` of every node in the canvas snapshot — the
/// *red* badge. (A warning badges amber instead; that projection is
/// covered by `node_warning_badge.rs`.)
fn badges(harness: &TestHarness) -> Vec<Option<String>> {
    let state = harness.state();
    let graph = state.graph.lock().unwrap();
    node_views(
        &graph,
        &state.registry,
        &state.node_errors_snapshot(),
        &state.node_warnings_snapshot(),
    )
    .into_iter()
    .map(|view| view.error)
    .collect()
}

/// The warning reaches the status bar at [`NoticeLevel::Warning`], names
/// the node, and — however many passes follow — says so once.
#[test]
fn a_degraded_node_posts_one_warning_notice_not_one_per_pass() {
    let (harness, _) = harness_with_degraded_node();

    let mut posted = Vec::new();
    for _ in 0..5 {
        harness.evaluate_now();
        posted.extend(harness.state().drain_notices());
    }

    assert_eq!(
        posted.len(),
        1,
        "one message for a graph that stays degraded"
    );
    assert_eq!(posted[0].level, NoticeLevel::Warning);
    assert!(
        posted[0].text.contains("not watertight"),
        "the node's own message survives the trip: {}",
        posted[0].text
    );
    assert!(
        posted[0].text.starts_with("Partial"),
        "and names the node: {}",
        posted[0].text
    );
}

/// The node is **not** badged as broken and its dependents evaluate
/// normally — the whole point of a warning being something other than a
/// failure. (It does wear the amber badge; see `node_warning_badge.rs`.)
#[test]
fn a_warning_neither_errors_the_node_nor_blocks_downstream() {
    let (harness, (_, sink)) = harness_with_degraded_node();
    harness.evaluate_now();

    assert!(
        badges(&harness).iter().all(|b| b.is_none()),
        "a degraded result is not an error badge: {:?}",
        badges(&harness)
    );
    assert!(harness.state().node_errors_snapshot().is_empty());

    let state = harness.state();
    let graph = state.graph.lock().unwrap();
    let node = graph.get(sink).expect("the downstream node is there");
    assert!(!node.failed, "nothing downstream is marked failed");
    let uid = node.output_by_name("out").unwrap().uid;
    assert_eq!(
        node.cached_outputs.get(&uid),
        Some(&PortValue::Number(2.0)),
        "the dependent evaluated against the degraded result"
    );
}

/// A node that stops warning goes quiet — no "it's fine now" message.
#[test]
fn a_node_that_stops_warning_says_nothing() {
    let (harness, (warner, _)) = harness_with_degraded_node();
    harness.evaluate_now();
    let _ = harness.state().drain_notices();

    harness
        .state()
        .graph
        .lock()
        .unwrap()
        .set_property(warner, "degraded", PortValue::Bool(false))
        .expect("the node accepts the property");
    harness.evaluate_now();

    assert!(
        harness.state().drain_notices().is_empty(),
        "recovery is silent"
    );
}
