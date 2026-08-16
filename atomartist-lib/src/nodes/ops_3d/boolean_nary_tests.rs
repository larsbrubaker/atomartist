//! Tests for the Boolean node's n-ary operands and part selection
//! (plan step B-3 of `docs/boolean-node-plan.md`).
//!
//! Split from `boolean_node_tests.rs` (which covers the B-1 import floor
//! and the B-2 operation enum) to keep both files well under the 800-line
//! limit; the shared fixtures live there and are re-used from here.
//!
//! Expectations come from MatterCAD's `Object3DBooleanOperations.DoSubtract`
//! (union the removers, subtract per keep, keeps stay separate),
//! `BooleanMeshBuilder.GetSubtractItems` (a lone part is a keep),
//! `BooleanObject3D.ChooseDefaultsForWrappedChildren` (the last part is the
//! default remover) and `CleanUpSelectedChildrenIDs` (stale entries are
//! dropped).

use std::sync::Arc;

use super::tests::{op, run_boolean_inputs, translate_matrix, translated, volume};
use super::{operation_of, selection_row_available, BooleanNode};
use crate::geometry::{generate_box, Geometry3d};
use crate::graph::graph::{Graph, Noodle};
use crate::graph::node::PortValue;
use crate::nodes;
use crate::nodes::ops_3d::boolean_selection::{self, SUBTRACT_PARTS};
use crate::registry::{NodeDef, NodeProperties, NodeRegistry};
use crate::serialization::graph_json::{graph_from_json_str, graph_to_json_string, LoadResult};

/// A 2 mm box translated to `(x, y, z)`, as a single-body group.
fn box_at(x: f32, y: f32, z: f32) -> Geometry3d {
    Geometry3d::from_mesh(Arc::new(translated(
        &generate_box(2.0, 2.0, 2.0),
        x,
        y,
        z,
    )))
}

fn registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    nodes::register_all(&mut r);
    r
}

fn total_volume(g: &Geometry3d) -> f64 {
    g.iter().map(|b| volume(&b.mesh)).sum()
}

// ------------------------------------------------------- n-ary operations

/// Combine unions every connected input, not just the first two.
#[test]
fn combine_unions_three_operands() {
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)),
            ("b", box_at(5.0, 0.0, 0.0)),
            ("c", box_at(10.0, 0.0, 0.0)),
        ],
        op("Combine"),
        None,
    )
    .expect("Combine of three operands failed");
    assert_eq!(out.len(), 1, "a union is one body");
    let v = total_volume(&out);
    assert!(
        (v - 24.0).abs() < 1e-3,
        "union of three disjoint 2mm boxes has volume {}, expected 24",
        v
    );
}

/// Intersect is defined by *every* operand, so a third one narrows the
/// result further.
#[test]
fn intersect_of_three_keeps_only_the_common_core() {
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)),   // [-1,1]^3
            ("b", box_at(1.0, 1.0, 1.0)),   // [0,2]^3
            ("c", box_at(1.5, 1.5, 1.5)),   // [0.5,2.5]^3
        ],
        op("Intersect"),
        None,
    )
    .expect("Intersect of three operands failed");
    let v = total_volume(&out);
    assert!(
        (v - 0.125).abs() < 1e-3,
        "intersection of the three boxes has volume {}, expected 0.125",
        v
    );
}

/// Two removers, two keeps: the removers union first and each keep is cut
/// separately, so the result is two bodies — not one merged shell
/// (`DoSubtract` builds one result item per keep).
#[test]
fn subtract_cuts_every_remover_from_every_keep_and_keeps_them_separate() {
    let out = run_boolean_inputs(
        &[
            ("keep1", box_at(0.0, 0.0, 0.0)),      // [-1,1]^3, volume 8
            ("keep2", box_at(5.0, 0.0, 0.0)),      // [4,6]x[-1,1]^2, volume 8
            ("cut1", box_at(1.0, 1.0, 1.0)),       // removes [0,1]^3 from keep1
            ("cut2", box_at(6.0, 1.0, 1.0)),       // removes [5,6]x[0,1]^2 from keep2
        ],
        op("Subtract"),
        Some(&["cut1", "cut2"]),
    )
    .expect("Subtract with two removers failed");
    assert_eq!(out.len(), 2, "each keep stays its own body");
    for body in out.iter() {
        let v = volume(&body.mesh);
        assert!(
            (v - 7.0).abs() < 1e-3,
            "each keep should lose exactly 1 mm³; got {}",
            v
        );
    }
}

/// Subtract & Replace generalises to n keeps: each keep contributes the cut
/// result *and* the volume that was removed from it.
#[test]
fn subtract_and_replace_yields_a_kept_and_a_replaced_body_per_keep() {
    let out = run_boolean_inputs(
        &[
            ("keep1", box_at(0.0, 0.0, 0.0)),
            ("keep2", box_at(5.0, 0.0, 0.0)),
            ("cut", box_at(1.0, 1.0, 1.0)),
        ],
        op("Subtract & Replace"),
        Some(&["cut"]),
    )
    .expect("Subtract & Replace with two keeps failed");
    // keep1 → 7 (cut) + 1 (replaced); keep2 is untouched by the remover, so
    // it contributes its keep alone — an empty intersection is omitted.
    assert_eq!(out.len(), 3, "bodies: {:?}", out.len());
    let mut volumes: Vec<f64> = out.iter().map(|b| volume(&b.mesh)).collect();
    volumes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    assert!((volumes[0] - 1.0).abs() < 1e-3, "volumes {:?}", volumes);
    assert!((volumes[1] - 7.0).abs() < 1e-3, "volumes {:?}", volumes);
    assert!((volumes[2] - 8.0).abs() < 1e-3, "volumes {:?}", volumes);
}

/// Every body of a multi-body input is an operand in its own right — an
/// input carrying two boxes unions as two boxes.
#[test]
fn every_body_of_an_input_is_its_own_operand() {
    let pair = Geometry3d::from_bodies(vec![
        crate::geometry::Body::from_mesh(Arc::new(translated(
            &generate_box(2.0, 2.0, 2.0),
            0.0,
            0.0,
            0.0,
        ))),
        crate::geometry::Body::from_mesh(Arc::new(translated(
            &generate_box(2.0, 2.0, 2.0),
            5.0,
            0.0,
            0.0,
        ))),
    ]);
    let out = run_boolean_inputs(
        &[("group", pair), ("c", box_at(10.0, 0.0, 0.0))],
        op("Combine"),
        None,
    )
    .expect("Combine of a multi-body input failed");
    let v = total_volume(&out);
    assert!(
        (v - 24.0).abs() < 1e-3,
        "a two-body input must contribute both bodies; volume {}, expected 24",
        v
    );
}

/// A remover input contributes *all* of its bodies as removers — the
/// selection is per input (MatterCAD's per-child "Part(s) to Subtract"),
/// not per body.
#[test]
fn a_selected_input_contributes_all_of_its_bodies_as_removers() {
    let removers = Geometry3d::from_bodies(vec![
        crate::geometry::Body::from_mesh(Arc::new(translated(
            &generate_box(2.0, 2.0, 2.0),
            1.0,
            1.0,
            1.0,
        ))), // removes [0,1]^3
        crate::geometry::Body::from_mesh(Arc::new(translated(
            &generate_box(2.0, 2.0, 2.0),
            -1.0,
            -1.0,
            -1.0,
        ))), // removes [-1,0]^3
    ]);
    let out = run_boolean_inputs(
        &[("keep", box_at(0.0, 0.0, 0.0)), ("cut", removers)],
        op("Subtract"),
        Some(&["cut"]),
    )
    .expect("Subtract with a multi-body remover failed");
    let v = total_volume(&out);
    assert!(
        (v - 6.0).abs() < 1e-3,
        "both bodies of the remover input must cut; volume {}, expected 6",
        v
    );
}

// ------------------------------------------------------- part selection

/// With no explicit choice, the last connected input is the remover
/// (`ChooseDefaultsForWrappedChildren`).
#[test]
fn the_last_connected_input_is_the_default_remover() {
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)),
            ("b", box_at(5.0, 0.0, 0.0)),
            ("c", box_at(1.0, 1.0, 1.0)), // overlaps 'a' by 1 mm³
        ],
        op("Subtract"),
        None,
    )
    .expect("Subtract with the default selection failed");
    assert_eq!(out.len(), 2, "two keeps → two bodies");
    let v = total_volume(&out);
    assert!(
        (v - 15.0).abs() < 1e-3,
        "expected 8 + (8-1) = 15 with 'c' as the remover; got {}",
        v
    );
}

/// A single operand is always a keep, so Subtract passes it through
/// instead of erasing it (`GetSubtractItems` for a single child).
#[test]
fn a_lone_operand_passes_through_subtract_untouched() {
    let out = run_boolean_inputs(&[("a", box_at(0.0, 0.0, 0.0))], op("Subtract"), None)
        .expect("Subtract with one operand failed");
    assert_eq!(out.len(), 1);
    let v = total_volume(&out);
    assert!((v - 8.0).abs() < 1e-3, "lone operand volume {}, expected 8", v);
}

/// A selection that names nothing live — explicitly emptied by the user,
/// or left holding only uids whose sockets are gone — means "nothing to cut
/// with", not "cut everything": every part survives as a keep. (Whether a
/// given uid is live is [`boolean_selection`]'s own unit test; this one
/// pins the *node's* behaviour when the answer is "none of them".)
#[test]
fn a_selection_that_names_nothing_live_leaves_every_part_a_keep() {
    let out = run_boolean_inputs(
        &[("a", box_at(0.0, 0.0, 0.0)), ("b", box_at(1.0, 1.0, 1.0))],
        op("Subtract"),
        Some(&["gone"]), // resolves to no live socket → an empty selection
    )
    .expect("Subtract with an empty selection failed");
    assert_eq!(out.len(), 2, "both parts survive as keeps");
    let v = total_volume(&out);
    assert!((v - 16.0).abs() < 1e-3, "volume {}, expected 8 + 8", v);
}

// ------------------------------------------------------- matrix baking

/// The bake also applies to the removers, not just the keeps.
#[test]
fn a_removers_body_matrix_is_baked_before_the_boolean() {
    let keep = box_at(0.0, 0.0, 0.0); // [-1,1]^3
    let cut = Geometry3d::from_mesh(Arc::new(generate_box(2.0, 2.0, 2.0)))
        .with_matrix(translate_matrix(1.0, 1.0, 1.0)); // → [0,2]^3
    let out = run_boolean_inputs(&[("keep", keep), ("cut", cut)], op("Subtract"), Some(&["cut"]))
        .expect("Subtract with a transformed remover failed");
    let v = total_volume(&out);
    assert!(
        (v - 7.0).abs() < 1e-3,
        "volume {}, expected 7 (8 means the remover's matrix was ignored and it \
         cut nothing; 0 means it cut everything)",
        v
    );
}

/// A mirroring matrix (negative determinant) turns the baked mesh inside
/// out, and the kernel would then read the operand as the *complement* of
/// the part — a subtract would eat everything except the box. The bake
/// reverses the winding to match.
#[test]
fn a_mirrored_operand_is_rewound_so_it_cuts_the_right_side() {
    // Mirror in X about x = 0.5: the symmetric box [-1,1]^3 lands on
    // [0,2]^3, geometrically identical to a plain translation — the only
    // difference is the winding.
    let mirror = [
        -1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        1.0, 1.0, 1.0, 1.0,
    ];
    let cut = Geometry3d::from_mesh(Arc::new(generate_box(2.0, 2.0, 2.0))).with_matrix(mirror);
    let out = run_boolean_inputs(
        &[("keep", box_at(0.0, 0.0, 0.0)), ("cut", cut)],
        op("Subtract"),
        Some(&["cut"]),
    )
    .expect("Subtract with a mirrored remover failed");
    let v = total_volume(&out);
    assert!(
        (v - 7.0).abs() < 1e-3,
        "volume {}, expected 7 — an inside-out remover cuts the complement",
        v
    );
}

/// Selecting *every* operand as a remover leaves nothing to cut from.
/// Returning empty geometry would be the silent vanish this node exists to
/// prevent, so it is an error the user can read.
#[test]
fn selecting_every_part_as_a_remover_is_a_named_error() {
    let err = match run_boolean_inputs(
        &[("a", box_at(0.0, 0.0, 0.0)), ("b", box_at(1.0, 1.0, 1.0))],
        op("Subtract"),
        Some(&["a", "b"]),
    ) {
        Ok(g) => panic!("expected an error, got {} bodies", g.len()),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("remover") && err.to_lowercase().contains("nothing"),
        "error does not explain the problem: {}",
        err
    );
}

/// The default remover is the last operand **socket**, not the last input
/// that happened to produce geometry: an input whose upstream is empty this
/// frame must keep its role, or the cut silently moves to a part the user
/// never selected — and one of their keeps loses volume.
#[test]
fn an_empty_newest_input_is_still_the_default_remover() {
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)),
            ("b", box_at(1.0, 1.0, 1.0)), // overlaps 'a' — would cut it
            ("c", Geometry3d::empty()),   // newest, but produced nothing
        ],
        op("Subtract"),
        None,
    )
    .expect("Subtract with an empty newest input failed");
    assert_eq!(out.len(), 2, "both keeps survive");
    let v = total_volume(&out);
    assert!(
        (v - 16.0).abs() < 1e-3,
        "volume {}, expected 8 + 8 — 15 means 'b' was cut into 'a' because the \
         empty newest input lost its role as the remover",
        v
    );
}

// ------------------------------------------------------- row visibility

/// The part-selection row is offered only by the operations that cut
/// (`UpdateControls`, `BooleanObject3D.cs:397-408`).
#[test]
fn the_selection_row_is_available_only_for_the_cutting_operations() {
    let with = |operation: &str| {
        let mut p = NodeProperties::default();
        p.insert("operation", op(operation));
        p
    };
    assert!(selection_row_available(&with("Subtract")));
    assert!(selection_row_available(&with("Subtract & Replace")));
    assert!(!selection_row_available(&with("Combine")));
    assert!(!selection_row_available(&with("Intersect")));
    // The legacy numeric encoding resolves the same way.
    let mut legacy = NodeProperties::default();
    legacy.insert("operation", PortValue::Number(1.0));
    assert_eq!(operation_of(&legacy), "Subtract");
    assert!(selection_row_available(&legacy));
}

/// …and until an editor exists that can draw one checkbox per *connected
/// input*, the row itself stays out of the property panel: the schema is
/// per node type and cannot vary its row count with the instance. The
/// operation gate above is live and tested; only the widget is missing.
#[test]
fn the_selection_row_is_still_hidden_pending_a_per_input_editor() {
    let mut props = NodeProperties::default();
    props.insert("operation", op("Subtract"));
    assert!(!BooleanNode.row_visible(SUBTRACT_PARTS, &props));
    assert!(
        BooleanNode.row_visible("operation", &props),
        "the operation row is unaffected"
    );
}

// ------------------------------------------------------- serialization

/// A graph saved before the dynamic inputs landed carries fixed `a` / `b`
/// sockets. It must load without complaint and evaluate to the same solid:
/// the two sockets simply *are* the first two dynamic operands, and the
/// stored `Number` operation still migrates to Subtract.
#[test]
fn a_legacy_two_input_graph_loads_and_evaluates() {
    let reg = registry();
    let json = r#"{
        "version": 1,
        "next_socket_uid": 40,
        "nodes": [
            {"id": 1, "type_id": "Box", "position": [0,0], "inputs": [],
             "outputs": [{"uid": 1, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {}},
            {"id": 2, "type_id": "Box", "position": [0,100], "inputs": [],
             "outputs": [{"uid": 2, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {"width": {"kind": "Number", "value": 10},
                            "height": {"kind": "Number", "value": 10},
                            "depth": {"kind": "Number", "value": 10}}},
            {"id": 3, "type_id": "Boolean", "position": [200,0],
             "inputs": [{"uid": 10, "name": "a", "socket_type": "Geometry3d"},
                        {"uid": 11, "name": "b", "socket_type": "Geometry3d"}],
             "outputs": [{"uid": 12, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {"operation": {"kind": "Number", "value": 1}}}
        ],
        "noodles": [
            {"from_node": 1, "from_uid": 1, "to_node": 3, "to_uid": 10},
            {"from_node": 2, "from_uid": 2, "to_node": 3, "to_uid": 11}
        ]
    }"#;
    let LoadResult { graph: mut g, warnings, .. } =
        graph_from_json_str(json, &reg).expect("legacy graph did not parse");
    assert!(warnings.is_empty(), "warnings: {:?}", warnings);

    crate::graph::executor::evaluate_all(&mut g, &reg)
        .expect("evaluation failed")
        .expect_clean();

    let boolean = g
        .nodes()
        .find(|n| n.type_id.as_ref() == "Boolean")
        .map(|n| n.id)
        .expect("Boolean missing after load");
    let out_uid = g.get(boolean).unwrap().output_by_name("out").unwrap().uid;
    match g.get(boolean).unwrap().cached_outputs.get(&out_uid) {
        Some(PortValue::Geometry3d(geo)) => {
            // The default 20 mm cube (8000 mm³) minus the 10 mm cube
            // (1000 mm³) nested inside it: both sit on the bed via their
            // own `matrix`, which the boolean now bakes in.
            let v = total_volume(geo);
            assert!(
                (v - 7000.0).abs() < 1.0,
                "legacy a/b graph evaluated to volume {}, expected 7000",
                v
            );
        }
        other => panic!("expected Geometry3d, got {:?}", other),
    }
}

/// The same legacy graph, but with a noodle into the `Color` param socket
/// this node used to mint. Operand-hood is decided by socket **type**, so
/// the colour input is simply not an operand — before that rule it was
/// treated as a nameless operand and the whole node failed with
/// "input 'Color' must be Geometry3d", taking the graph's output with it.
#[test]
fn a_legacy_graph_with_the_color_socket_wired_still_evaluates() {
    let reg = registry();
    let json = r#"{
        "version": 1,
        "next_socket_uid": 40,
        "nodes": [
            {"id": 1, "type_id": "Box", "position": [0,0], "inputs": [],
             "outputs": [{"uid": 1, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {}},
            {"id": 2, "type_id": "Box", "position": [0,100], "inputs": [],
             "outputs": [{"uid": 2, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {"width": {"kind": "Number", "value": 10},
                            "height": {"kind": "Number", "value": 10},
                            "depth": {"kind": "Number", "value": 10}}},
            {"id": 4, "type_id": "ColorConst", "position": [0,200], "inputs": [],
             "outputs": [{"uid": 3, "name": "out", "socket_type": "Color"}],
             "properties": {}},
            {"id": 3, "type_id": "Boolean", "position": [200,0],
             "inputs": [{"uid": 10, "name": "a", "socket_type": "Geometry3d"},
                        {"uid": 11, "name": "b", "socket_type": "Geometry3d"},
                        {"uid": 13, "name": "Color", "socket_type": "Color"}],
             "outputs": [{"uid": 12, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {"operation": {"kind": "StringVal", "value": "Subtract"}}}
        ],
        "noodles": [
            {"from_node": 1, "from_uid": 1, "to_node": 3, "to_uid": 10},
            {"from_node": 2, "from_uid": 2, "to_node": 3, "to_uid": 11},
            {"from_node": 4, "from_uid": 3, "to_node": 3, "to_uid": 13}
        ]
    }"#;
    let LoadResult { graph: mut g, warnings, .. } =
        graph_from_json_str(json, &reg).expect("legacy graph did not parse");
    assert!(warnings.is_empty(), "warnings: {:?}", warnings);
    crate::graph::executor::evaluate_all(&mut g, &reg)
        .expect("evaluation failed")
        .expect_clean();

    let boolean = g
        .nodes()
        .find(|n| n.type_id.as_ref() == "Boolean")
        .map(|n| n.id)
        .expect("Boolean missing after load");
    let out_uid = g.get(boolean).unwrap().output_by_name("out").unwrap().uid;
    match g.get(boolean).unwrap().cached_outputs.get(&out_uid) {
        Some(PortValue::Geometry3d(geo)) => {
            let v = total_volume(geo);
            assert!(
                (v - 7000.0).abs() < 1.0,
                "the wired Color socket must be ignored, not treated as an \
                 operand; volume {}, expected 7000",
                v
            );
        }
        other => panic!("expected Geometry3d, got {:?}", other),
    }
}

/// A legacy two-input Boolean has no trailing empty slot and every slot it
/// does have is occupied, so without a load-time fix-up it could never take
/// a third operand. `NodeDef::on_loaded` re-appends the placeholder.
#[test]
fn a_legacy_two_input_boolean_can_still_grow_a_third_input() {
    let reg = registry();
    let json = r#"{
        "version": 1,
        "next_socket_uid": 40,
        "nodes": [
            {"id": 1, "type_id": "Box", "position": [0,0], "inputs": [],
             "outputs": [{"uid": 1, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {}},
            {"id": 3, "type_id": "Boolean", "position": [200,0],
             "inputs": [{"uid": 10, "name": "a", "socket_type": "Geometry3d"},
                        {"uid": 11, "name": "b", "socket_type": "Geometry3d"}],
             "outputs": [{"uid": 12, "name": "out", "socket_type": "Geometry3d"}],
             "properties": {}}
        ],
        "noodles": []
    }"#;
    let LoadResult { graph: mut g, .. } =
        graph_from_json_str(json, &reg).expect("legacy graph did not parse");
    let boolean = g
        .nodes()
        .find(|n| n.type_id.as_ref() == "Boolean")
        .map(|n| n.id)
        .expect("Boolean missing after load");
    let slots = &g.get(boolean).unwrap().inputs;
    assert_eq!(slots.len(), 3, "a trailing empty slot must have been added");
    assert_eq!(slots[2].name.as_ref(), "");

    // …and it really accepts a connection.
    let bx = g.nodes().find(|n| n.type_id.as_ref() == "Box").unwrap().id;
    let src = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
    let empty = g.get(boolean).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(bx, src, boolean, empty), &reg)
        .expect("the restored placeholder must accept a connection");
    assert_eq!(
        g.get(boolean).unwrap().inputs.len(),
        4,
        "the connect hook adopts the slot and appends a fresh empty"
    );
}

/// The dynamic input list and the part selection both survive a save /
/// load round trip — the selection is keyed by socket uid, and uids are
/// what the file stores.
#[test]
fn dynamic_inputs_and_the_selection_survive_a_round_trip() {
    let reg = registry();
    let mut g = Graph::new();
    let bx1 = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
    let bx2 = g.add_new_node("Box", [0.0, 100.0], &reg).unwrap();
    let b = g.add_new_node("Boolean", [200.0, 0.0], &reg).unwrap();
    let o1 = g.get(bx1).unwrap().output_by_name("out").unwrap().uid;
    let o2 = g.get(bx2).unwrap().output_by_name("out").unwrap().uid;
    let e1 = g.get(b).unwrap().inputs[0].uid;
    g.connect(Noodle::new(bx1, o1, b, e1), &reg).unwrap();
    let e2 = g.get(b).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(bx2, o2, b, e2), &reg).unwrap();
    assert_eq!(
        g.get(b).unwrap().inputs.len(),
        3,
        "two operands plus the trailing empty"
    );
    // Choose the FIRST input as the remover — the opposite of the default,
    // so a round trip that lost the value would be indistinguishable from
    // one that kept it.
    g.set_property(
        b,
        SUBTRACT_PARTS,
        PortValue::StringVal(Arc::new(boolean_selection::encode(&[e1]))),
    )
    .unwrap();

    let json = graph_to_json_string(&g);
    let LoadResult { graph: g2, warnings, .. } = graph_from_json_str(&json, &reg).unwrap();
    assert!(warnings.is_empty(), "warnings: {:?}", warnings);
    let reloaded = g2.nodes().find(|n| n.type_id.as_ref() == "Boolean").unwrap();
    assert_eq!(reloaded.inputs.len(), 3);
    assert_eq!(reloaded.inputs[0].uid, e1, "socket uids must be stable");
    assert_eq!(reloaded.inputs[2].name.as_ref(), "");
    match reloaded.properties.get(SUBTRACT_PARTS) {
        Some(PortValue::StringVal(s)) => {
            assert_eq!(s.as_str(), boolean_selection::encode(&[e1]))
        }
        other => panic!("selection round-tripped as {:?}", other),
    }
}

/// Disconnecting a selected input drops it from the selection —
/// `CleanUpSelectedChildrenIDs`. Leaving the uid behind would let it
/// re-attach itself to whatever socket is minted with that number next.
#[test]
fn disconnecting_a_selected_input_prunes_the_selection() {
    let reg = registry();
    let mut g = Graph::new();
    let bx1 = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
    let bx2 = g.add_new_node("Box", [0.0, 100.0], &reg).unwrap();
    let b = g.add_new_node("Boolean", [200.0, 0.0], &reg).unwrap();
    let o1 = g.get(bx1).unwrap().output_by_name("out").unwrap().uid;
    let o2 = g.get(bx2).unwrap().output_by_name("out").unwrap().uid;
    let e1 = g.get(b).unwrap().inputs[0].uid;
    g.connect(Noodle::new(bx1, o1, b, e1), &reg).unwrap();
    let e2 = g.get(b).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(bx2, o2, b, e2), &reg).unwrap();
    g.set_property(
        b,
        SUBTRACT_PARTS,
        PortValue::StringVal(Arc::new(boolean_selection::encode(&[e1, e2]))),
    )
    .unwrap();

    g.disconnect(&Noodle::new(bx1, o1, b, e1), &reg).unwrap();

    match g.get(b).unwrap().properties.get(SUBTRACT_PARTS) {
        Some(PortValue::StringVal(s)) => assert_eq!(
            s.as_str(),
            boolean_selection::encode(&[e2]),
            "the disconnected input's uid must be gone"
        ),
        other => panic!("selection is {:?}", other),
    }
}
