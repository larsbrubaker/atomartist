//! End-to-end cover for step B-1b of `docs/boolean-node-plan.md`: a node
//! whose evaluation refuses must reach the user twice over — once as a
//! status-bar notice, once as a badge on the canvas node — and must stop
//! talking the moment it is fixed.
//!
//! No NodeDesigner ancestor: NodeDesigner surfaced node errors from the
//! start, AtomArtist's evaluator swallowed them until now.
//!
//! The failing node is a test-local `NodeDef` rather than a real Boolean
//! with a hostile mesh: the plumbing under test is the evaluator → state
//! → widget path, and a purpose-built node makes "fails" and "is fixed"
//! one property write apart.

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

/// Refuses while its `boom` property is true — the stand-in for a
/// Boolean handed an operand that isn't a closed solid.
struct Fussy;

impl NodeDef for Fussy {
    fn type_id(&self) -> &'static str {
        "Fussy"
    }
    fn display_name(&self) -> &'static str {
        "Fussy"
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
        if ctx.properties.bool_("boom", false) {
            return Err(NodeError::msg("input 'b' is not a closed solid"));
        }
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Number(1.0));
        Ok(out)
    }
}

/// A harness whose graph holds one Fussy node, already failing.
fn harness_with_broken_node() -> (TestHarness, atomartist_lib::graph::node::NodeId) {
    let mut registry = NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut registry);
    registry.register(Fussy);

    let mut graph = Graph::new();
    let id = graph
        .add_new_node("Fussy", [0.0, 0.0], &registry)
        .expect("Fussy is registered");
    graph
        .set_property(id, "boom", PortValue::Bool(true))
        .expect("the node accepts the property");

    let state = AppState::with_storage(graph, registry, test_storage_registry());
    (TestHarness::with_app_state(state), id)
}

/// The `NodeView::error` of every node in the canvas snapshot — the
/// exact projection `AppStateModel::nodes` hands the node editor, whose
/// `Some` entries paint an error badge.
fn badges(harness: &TestHarness) -> Vec<Option<String>> {
    let state = harness.state();
    let graph = state.graph.lock().unwrap();
    node_views(&graph, &state.registry, &state.node_errors_snapshot())
        .into_iter()
        .map(|view| view.error)
        .collect()
}

/// The failure reaches the status bar, names the node, and — however
/// many evaluation passes follow — says so exactly once.
#[test]
fn a_refused_node_posts_one_error_notice_not_one_per_pass() {
    let (harness, _) = harness_with_broken_node();

    let mut posted = Vec::new();
    for _ in 0..5 {
        harness.evaluate_now();
        // The shell drains the queue once a frame.
        posted.extend(harness.state().drain_notices());
    }

    assert_eq!(posted.len(), 1, "one message for a graph that stays broken");
    assert_eq!(posted[0].level, NoticeLevel::Error);
    assert!(
        posted[0].text.contains("input 'b' is not a closed solid"),
        "the node's own message survives the trip: {}",
        posted[0].text
    );
    assert!(
        posted[0].text.starts_with("Fussy"),
        "and names the node: {}",
        posted[0].text
    );
}

/// The canvas snapshot carries the message, so the node paints its
/// error badge — and drops it once the node evaluates cleanly again.
#[test]
fn the_canvas_node_view_carries_the_error_until_the_node_is_fixed() {
    let (harness, id) = harness_with_broken_node();
    harness.evaluate_now();

    let broken = badges(&harness);
    assert_eq!(broken.len(), 1);
    assert!(
        broken[0]
            .as_deref()
            .is_some_and(|e| e.contains("not a closed solid")),
        "the badge source is filled: {:?}",
        broken[0]
    );

    // Fix it the way the user would: change the property, re-evaluate.
    harness
        .state()
        .graph
        .lock()
        .unwrap()
        .set_property(id, "boom", PortValue::Bool(false))
        .expect("the node accepts the property");
    let _ = harness.state().drain_notices();
    harness.evaluate_now();

    let fixed = badges(&harness);
    assert!(fixed[0].is_none(), "the badge clears on a good pass");
    assert!(
        harness.state().drain_notices().is_empty(),
        "a repair is not worth a message"
    );
    assert!(harness.state().node_errors_snapshot().is_empty());
}

/// Sanity: a healthy graph never touches the error surfaces at all.
#[test]
fn a_healthy_graph_posts_no_error_notices() {
    let harness = TestHarness::with_starter_graph();
    harness.evaluate_now();

    let notices: Vec<_> = harness
        .state()
        .drain_notices()
        .into_iter()
        .filter(|n| n.level == NoticeLevel::Error)
        .collect();
    assert!(notices.is_empty(), "{:?}", notices);
    assert!(harness.state().node_errors_snapshot().is_empty());
}
