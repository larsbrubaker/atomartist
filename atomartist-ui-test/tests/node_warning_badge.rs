//! The **amber node badge** — plan step B-5a of
//! `docs/boolean-node-plan.md`.
//!
//! B-5 gave a degraded Combine a status-bar notice, posted once per
//! change. That is the right volume for a transient message and the
//! wrong surface for a permanent state: once the notice scrolls away, a
//! Boolean that is *still* dropping a part looks exactly like a clean
//! one. So a degraded node now also wears the node-editor's badge in the
//! palette's warning colour, for as long as it stays degraded.
//!
//! No NodeDesigner ancestor for the colour split; the behaviour mirrors
//! MatterCAD, where a part the boolean could not take stays visibly
//! flagged in the scene tree rather than only in the log.
//!
//! The badge *painting* is covered in agg-gui
//! (`node-editor/src/widget/tests_error_badge.rs`). What this file adds
//! is the projection: a real degraded Boolean reaches the canvas
//! snapshot as `NodeView::warning`, clears when repaired, and loses to
//! an error on the same node.

use std::sync::Arc;

use atomartist_lib::geometry::{generate_box, Geometry3d};
use atomartist_lib::graph::graph::Noodle;
use atomartist_lib::graph::node::{NodeId, PortValue};
use atomartist_lib::graph::socket::SocketUidAlloc;
use atomartist_lib::registry::{
    EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeRegistry,
};
use atomartist_lib::socket_types::SocketType;
use atomartist_lib::Graph;
use atomartist_ui::app_state_model::node_views;
use atomartist_ui::AppState;
use atomartist_ui_test::{test_storage_registry, TestHarness};

/// A 3-D source that emits a box which is **open** (one face removed)
/// while `broken` is set, and a closed one otherwise — "repair the
/// input" is one property write.
///
/// An open box is not a closed solid on any tolerance, so the Boolean's
/// import refuses it, the degradation policy rescues the original
/// geometry, and the node reports a warning while still producing a
/// result (see `boolean_degrade.rs`).
struct MaybeOpenBox;

impl NodeDef for MaybeOpenBox {
    fn type_id(&self) -> &'static str {
        "MaybeOpenBox"
    }
    fn display_name(&self) -> &'static str {
        "Maybe Open Box"
    }
    fn category(&self) -> &'static str {
        "Primitives 3D"
    }
    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Geometry3d)
            .build()
    }
    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let mut mesh = generate_box(2.0, 2.0, 2.0);
        if ctx.properties.bool_("broken", false) {
            let keep = mesh.tri_verts.len() - 6;
            mesh.tri_verts.truncate(keep);
        }
        let mut out = NodeOutputs::default();
        out.set(
            "out",
            PortValue::Geometry3d(Arc::new(Geometry3d::from_mesh(Arc::new(mesh)))),
        );
        Ok(out)
    }
}

/// `Box → Boolean(Combine) ← MaybeOpenBox`, the second operand armed to
/// be non-solid. Returns the harness plus the Boolean's and the source's
/// ids.
fn harness_with_degraded_combine() -> (TestHarness, NodeId, NodeId) {
    let mut registry = NodeRegistry::new();
    atomartist_lib::nodes::register_all(&mut registry);
    registry.register(MaybeOpenBox);

    let mut graph = Graph::new();
    let boolean = graph
        .add_new_node("Boolean", [140.0, 360.0], &registry)
        .expect("Boolean is registered");
    let good = graph
        .add_new_node("Box", [-200.0, 460.0], &registry)
        .expect("Box is registered");
    let bad = graph
        .add_new_node("MaybeOpenBox", [-200.0, 260.0], &registry)
        .expect("the test node is registered");
    graph
        .set_property(bad, "broken", PortValue::Bool(true))
        .expect("the node accepts the property");

    for src in [good, bad] {
        let out = graph.get(src).unwrap().output_by_name("out").unwrap().uid;
        let slot = graph.get(boolean).unwrap().inputs.last().unwrap().uid;
        graph
            .connect(Noodle::new(src, out, boolean, slot), &registry)
            .expect("Geometry3d into a Boolean operand slot");
    }

    let state = AppState::with_storage(graph, registry, test_storage_registry());
    (TestHarness::with_app_state(state), boolean, bad)
}

/// The canvas snapshot's badge pair for one node.
fn badge_of(h: &TestHarness, node: NodeId) -> (Option<String>, Option<String>) {
    let state = h.state();
    let graph = state.graph.lock().unwrap();
    let views = node_views(
        &graph,
        &state.registry,
        &state.node_errors_snapshot(),
        &state.node_warnings_snapshot(),
    );
    let view = views
        .into_iter()
        .find(|v| v.id.0 == node.0)
        .expect("the node is in the canvas snapshot");
    (view.error, view.warning)
}

/// A Combine that had to rescue an operand wears the amber badge, and
/// keeps wearing it across further evaluation passes — the state is
/// permanent until the input is fixed, unlike the one-shot notice.
#[test]
fn a_degraded_combine_wears_an_amber_badge_until_repaired() {
    let (harness, boolean, source) = harness_with_degraded_combine();
    harness.evaluate_now();

    let (error, warning) = badge_of(&harness, boolean);
    assert!(error.is_none(), "a degraded result is not an error");
    let text = warning.expect("the degraded Boolean is badged");
    assert!(
        text.contains("watertight"),
        "the badge carries the node's own sentence: {text}"
    );

    // Two more passes: the notice is silent by the changed-only rule,
    // but the badge must not go with it.
    harness.evaluate_now();
    harness.evaluate_now();
    assert_eq!(
        badge_of(&harness, boolean).1,
        Some(text),
        "the badge survives passes that post nothing"
    );

    // Repair the input; the badge clears the same way an error's does.
    harness
        .state()
        .graph
        .lock()
        .unwrap()
        .set_property(source, "broken", PortValue::Bool(false))
        .expect("the node accepts the property");
    harness.evaluate_now();
    assert_eq!(
        badge_of(&harness, boolean),
        (None, None),
        "a repaired node wears no badge"
    );
}

/// Only one badge fits on a node, and the canvas resolves the tie in
/// favour of the error — pinned here from the projection side, where
/// both maps can name the same node.
#[test]
fn an_error_beats_a_warning_on_the_same_node() {
    let (harness, boolean, _) = harness_with_degraded_combine();
    harness.evaluate_now();
    assert!(badge_of(&harness, boolean).1.is_some());

    // Break the Boolean outright: an all-remover Subtract is a named
    // failure, so the node lands in both maps for one pass.
    harness
        .state()
        .graph
        .lock()
        .unwrap()
        .set_property(
            boolean,
            "operation",
            PortValue::StringVal(Arc::new("Intersect".to_string())),
        )
        .expect("the node accepts the property");
    harness.evaluate_now();

    let (error, warning) = badge_of(&harness, boolean);
    assert!(
        error.is_some(),
        "the refused Boolean reaches the canvas as an error"
    );
    assert!(
        warning.is_some(),
        "and keeps the warning it earned last pass — a failed pass \
         neither succeeds nor prunes"
    );
    // Which of the two the user sees is the canvas's call, pinned in
    // agg-gui by `an_error_beats_a_warning_on_the_same_node`; what this
    // side owes is handing it both without dropping either.
}
