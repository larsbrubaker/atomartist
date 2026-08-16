//! Tests for the Boolean node's toggles and hole semantics (plan step
//! B-4 of `docs/boolean-node-plan.md`).
//!
//! Split from `boolean_node_tests.rs` (the B-1 import floor + B-2 enum)
//! and `boolean_nary_tests.rs` (B-3 operands + selection) to keep every
//! file well under the 800-line limit; the fixtures live in the first and
//! are re-used here.
//!
//! Expectations come from MatterCAD's `BooleanObject3D.cs:163-173` (the
//! three toggles) and `UpdateControls` L397-408 (which rows show),
//! `BooleanMeshBuilder.WindingRule` L71-73 (Nonzero when Keep Inside Out),
//! `CombineMeshes` L104-192 (holes unioned separately, subtracted from the
//! solids; a hole-only combine stays a hole) and `SubtractMeshes` L269-285
//! (Keep Subtracted Parts re-adds the removers).

use std::sync::Arc;

use manifold_rust::types::MeshGL;

use super::tests::{op, run_boolean_inputs, run_boolean_with_props, translated, volume};
use super::{
    keep_subtracted_row_available, BooleanNode, KEEP_INSIDE_OUT, KEEP_SUBTRACTED, REPAIR_WINDING,
};
use crate::geometry::{generate_box, Body, BodyRole, Geometry3d};
use crate::graph::executor::evaluate_all;
use crate::graph::graph::{Graph, Noodle};
use crate::graph::node::PortValue;
use crate::nodes;
use crate::nodes::ops_3d::mark_hole_node::{OUTPUT_TYPE, OUTPUT_TYPES};
use crate::registry::{NodeDef, NodeProperties, NodeRegistry};
use crate::serialization::graph_json::{graph_from_json_str, graph_to_json_string, LoadResult};

// ---------------------------------------------------------------- helpers

fn registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    nodes::register_all(&mut r);
    r
}

/// A 2 mm box at the origin: `[-1,1]^3`, volume 8.
fn box_mesh() -> MeshGL {
    generate_box(2.0, 2.0, 2.0)
}

/// The same box shifted by `(x, 0, 0)`.
fn box_at(x: f32) -> MeshGL {
    translated(&box_mesh(), x, 0.0, 0.0)
}

/// Every triangle wound the other way round: a closed shell that bounds
/// its volume *inwards*. This is the "inside out geometry" both kernel
/// toggles exist for, and it is not detectable from a body's matrix — the
/// mirror rewind in `boolean_ops` cannot help here.
fn inside_out(mesh: &MeshGL) -> MeshGL {
    let mut out = mesh.clone();
    for t in out.tri_verts.chunks_exact_mut(3) {
        t.swap(1, 2);
    }
    out
}

fn geom(mesh: MeshGL) -> Geometry3d {
    Geometry3d::from_mesh(Arc::new(mesh))
}

fn hole(mesh: MeshGL) -> Geometry3d {
    Geometry3d::from_body(Body::from_mesh(Arc::new(mesh)).with_role(BodyRole::Hole))
}

fn total_volume(g: &Geometry3d) -> f64 {
    g.iter().map(|b| volume(&b.mesh)).sum()
}

fn on(key: &str) -> [(&str, PortValue); 1] {
    [(key, PortValue::Bool(true))]
}

/// Combine a solid box at the origin with an *inside-out* box overlapping
/// it by half, under the given extra properties.
fn combine_with_inverted_operand(extra: &[(&str, PortValue)]) -> Geometry3d {
    run_boolean_with_props(
        &[
            ("a", geom(box_mesh())),
            ("b", geom(inside_out(&box_at(1.0)))),
        ],
        op("Combine"),
        None,
        extra,
    )
    .expect("the operands are closed shells — only their winding is wrong")
}

/// Union volume of the two boxes above: 8 + 8 − 4 overlap.
const UNION_VOLUME: f64 = 12.0;
/// What the default (positive-winding) union produces instead: the
/// inverted shell cancels the volume it overlaps, so `a` comes back with a
/// bite taken out of it — MatterCAD's "a model that should be solid comes
/// back with parts missing".
const EATEN_VOLUME: f64 = 4.0;

// ------------------------------------------------- Keep Inside Out Geometry

/// The symptom the toggle exists for, pinned so a future engine change
/// cannot make the two toggle tests below pass vacuously.
#[test]
fn an_inside_out_operand_eats_volume_by_default() {
    let out = combine_with_inverted_operand(&[]);
    assert!(
        (total_volume(&out) - EATEN_VOLUME).abs() < 1e-6,
        "expected the inverted shell to cancel the overlap ({EATEN_VOLUME}), got {}",
        total_volume(&out)
    );
}

/// Keep Inside Out Geometry → `WindingRule::Nonzero`: a region wound the
/// wrong way still counts as material, so the union is the whole union.
#[test]
fn keep_inside_out_geometry_unions_an_inverted_shell_as_material() {
    let out = combine_with_inverted_operand(&on(KEEP_INSIDE_OUT));
    assert_eq!(out.len(), 1);
    assert!(
        (total_volume(&out) - UNION_VOLUME).abs() < 1e-6,
        "expected the full union ({UNION_VOLUME}), got {}",
        total_volume(&out)
    );
}

/// Repair Winding Order → `repair_orientation()` on the imported handle:
/// the data is fixed, so the *default* positive rule already gives the
/// full union. Same answer as the rule toggle, arrived at the other way.
#[test]
fn repair_winding_order_rewinds_an_inverted_source_shell() {
    let out = combine_with_inverted_operand(&on(REPAIR_WINDING));
    assert_eq!(out.len(), 1);
    assert!(
        (total_volume(&out) - UNION_VOLUME).abs() < 1e-6,
        "expected the full union ({UNION_VOLUME}), got {}",
        total_volume(&out)
    );
}

/// The two toggles are independent and compose — neither undoes the other.
#[test]
fn the_two_kernel_toggles_compose() {
    let out = combine_with_inverted_operand(&[
        (KEEP_INSIDE_OUT, PortValue::Bool(true)),
        (REPAIR_WINDING, PortValue::Bool(true)),
    ]);
    assert!((total_volume(&out) - UNION_VOLUME).abs() < 1e-6);
}

/// The toggles reach *every* operation, not just Combine: the same
/// inverted shell used as a remover has to cut, rather than being read as
/// the complement of itself.
#[test]
fn repair_winding_order_reaches_the_subtract_path() {
    let operands = [
        ("a", geom(box_mesh())),
        ("b", geom(inside_out(&box_at(1.0)))),
    ];
    let repaired = run_boolean_with_props(&operands, op("Subtract"), None, &on(REPAIR_WINDING))
        .expect("closed shells, only wound wrong");
    assert!(
        (total_volume(&repaired) - EATEN_VOLUME).abs() < 1e-6,
        "a repaired remover cuts the overlap away: expected {EATEN_VOLUME}, got {}",
        total_volume(&repaired)
    );
}

// --------------------------------------------------- Keep Subtracted Parts

/// The removers come back as extra bodies, untouched by the subtraction
/// that used them (`SubtractMeshes` L269-285).
#[test]
fn keep_subtracted_parts_returns_the_removers_as_extra_bodies() {
    let operands = [("a", geom(box_mesh())), ("b", geom(box_at(1.0)))];

    let plain = run_boolean_inputs(&operands, op("Subtract"), None).unwrap();
    assert_eq!(plain.len(), 1, "one keep, one remover → one result body");
    assert!((total_volume(&plain) - EATEN_VOLUME).abs() < 1e-6);

    let kept =
        run_boolean_with_props(&operands, op("Subtract"), None, &on(KEEP_SUBTRACTED)).unwrap();
    assert_eq!(
        kept.len(),
        2,
        "the remover is handed back beside the result"
    );
    let volumes: Vec<f64> = kept.iter().map(|b| volume(&b.mesh)).collect();
    assert!(
        (volumes[0] - EATEN_VOLUME).abs() < 1e-6,
        "the cut keep comes first: {volumes:?}"
    );
    assert!(
        (volumes[1] - 8.0).abs() < 1e-6,
        "the remover is the whole original box, not the part that did the cutting: {volumes:?}"
    );
}

/// Subtract & Replace already emits the removed volume, so the toggle is
/// not offered — and must not apply if a stored value says otherwise.
#[test]
fn keep_subtracted_parts_does_not_apply_to_subtract_and_replace() {
    let operands = [("a", geom(box_mesh())), ("b", geom(box_at(1.0)))];
    let out = run_boolean_with_props(
        &operands,
        op("Subtract & Replace"),
        None,
        &on(KEEP_SUBTRACTED),
    )
    .unwrap();
    assert_eq!(
        out.len(),
        2,
        "the kept remainder and the replaced volume — no third body"
    );
    let total: f64 = total_volume(&out);
    assert!(
        (total - 8.0).abs() < 1e-6,
        "remainder (4) + replaced (4) = the original keep, got {total}"
    );
}

// -------------------------------------------------------- Hole / solid

/// Combine unions the holes separately and cuts them out of the solids.
#[test]
fn combine_subtracts_a_hole_from_the_solids() {
    let out = run_boolean_inputs(
        &[("a", geom(box_mesh())), ("b", hole(box_at(1.0)))],
        op("Combine"),
        None,
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert!(
        (total_volume(&out) - EATEN_VOLUME).abs() < 1e-6,
        "8 solid − 4 overlapping hole, got {}",
        total_volume(&out)
    );
    assert_eq!(
        out.first().unwrap().role,
        BodyRole::Solid,
        "the result of cutting holes out of material is material"
    );
}

/// Holes are unioned with each other first — two overlapping holes cut one
/// combined void, not two counted twice.
#[test]
fn holes_are_unioned_with_each_other_before_they_cut() {
    let out = run_boolean_inputs(
        &[
            ("a", geom(box_mesh())),
            ("b", hole(box_at(1.0))),
            ("c", hole(box_at(1.5))),
        ],
        op("Combine"),
        None,
    )
    .unwrap();
    assert!(
        (total_volume(&out) - EATEN_VOLUME).abs() < 1e-6,
        "the second hole overlaps the first inside the solid, so it removes nothing new: {}",
        total_volume(&out)
    );
}

/// A Combine with nothing but holes keeps the hole union *as a hole*
/// rather than returning empty — MatterCAD adds it to the scene as a
/// `PrintOutputTypes.Hole` child (L137-192), so it can still cut something
/// downstream.
#[test]
fn a_combine_of_only_holes_stays_a_hole() {
    let out = run_boolean_inputs(
        &[("a", hole(box_mesh())), ("b", hole(box_at(1.0)))],
        op("Combine"),
        None,
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert!((total_volume(&out) - UNION_VOLUME).abs() < 1e-6);
    assert_eq!(
        out.first().unwrap().role,
        BodyRole::Hole,
        "a union of holes is a hole, not empty and not solid"
    );
}

/// Only Combine reads the role — MatterCAD's other three operations take
/// every child as an ordinary participant.
#[test]
fn intersect_treats_a_hole_as_an_ordinary_participant() {
    let out = run_boolean_inputs(
        &[("a", geom(box_mesh())), ("b", hole(box_at(1.0)))],
        op("Intersect"),
        None,
    )
    .unwrap();
    assert!(
        (total_volume(&out) - 4.0).abs() < 1e-6,
        "the shared volume of the two boxes, hole or not: {}",
        total_volume(&out)
    );
    assert_eq!(out.first().unwrap().role, BodyRole::Solid);
}

/// End to end through a real graph: Mark Hole is the producer of the role
/// the Boolean consumes, and the role survives the wire.
#[test]
fn mark_hole_feeds_a_combine_through_a_real_graph() {
    let reg = registry();
    let mut g = Graph::new();
    let solid = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
    let cutter = g.add_new_node("Box", [0.0, 200.0], &reg).unwrap();
    let mark = g.add_new_node("Mark Hole", [200.0, 200.0], &reg).unwrap();
    let bool_node = g.add_new_node("Boolean", [400.0, 0.0], &reg).unwrap();

    // Both boxes 2 mm. The cutter is nudged 1 mm along X *on top of*
    // whatever the primitive's default matrix is (a Box arrives parked on
    // the bed, not at the origin), so exactly half of it overlaps.
    for n in [solid, cutter] {
        for p in ["width", "height", "depth"] {
            g.set_property(n, p, PortValue::Number(2.0)).unwrap();
        }
    }
    let mut m = match g.get(cutter).unwrap().properties.get("matrix") {
        Some(PortValue::Matrix4x4(m)) => *m,
        _ => crate::graph::node::identity_matrix(),
    };
    m[12] += 1.0;
    g.set_property(cutter, "matrix", PortValue::Matrix4x4(m))
        .unwrap();

    let cutter_out = g.get(cutter).unwrap().output_by_name("out").unwrap().uid;
    let mark_in = g.get(mark).unwrap().input_by_name("input").unwrap().uid;
    g.connect(Noodle::new(cutter, cutter_out, mark, mark_in), &reg)
        .unwrap();

    let solid_out = g.get(solid).unwrap().output_by_name("out").unwrap().uid;
    let mark_out = g.get(mark).unwrap().output_by_name("out").unwrap().uid;
    let e1 = g.get(bool_node).unwrap().inputs[0].uid;
    g.connect(Noodle::new(solid, solid_out, bool_node, e1), &reg)
        .unwrap();
    let e2 = g.get(bool_node).unwrap().inputs.last().unwrap().uid;
    g.connect(Noodle::new(mark, mark_out, bool_node, e2), &reg)
        .unwrap();
    g.set_property(
        bool_node,
        "operation",
        PortValue::StringVal(Arc::new("Combine".into())),
    )
    .unwrap();

    evaluate_all(&mut g, &reg).unwrap().expect_clean();
    let uid = g.get(bool_node).unwrap().output_by_name("out").unwrap().uid;
    match g.get(bool_node).unwrap().cached_outputs.get(&uid) {
        Some(PortValue::Geometry3d(geo)) => {
            assert_eq!(geo.len(), 1);
            assert!(
                (total_volume(geo) - EATEN_VOLUME).abs() < 1e-5,
                "the marked branch cut its overlap out of the solid: {}",
                total_volume(geo)
            );
        }
        other => panic!("expected Geometry3d, got {:?}", other),
    }
}

/// Flipping Mark Hole back to Solid makes it a plain pass-through, and the
/// Combine unions instead of cutting.
#[test]
fn mark_hole_set_to_solid_unions_again() {
    let solid_marked =
        Geometry3d::from_body(Body::from_mesh(Arc::new(box_at(1.0))).with_role(BodyRole::Solid));
    let out = run_boolean_inputs(
        &[("a", geom(box_mesh())), ("b", solid_marked)],
        op("Combine"),
        None,
    )
    .unwrap();
    assert!((total_volume(&out) - UNION_VOLUME).abs() < 1e-6);
    assert_eq!(OUTPUT_TYPES, ["Hole", "Solid"]);
}

// ---------------------------------------------------------- row visibility

/// `UpdateControls` L397-408: Keep Subtracted Parts is a plain-Subtract
/// row; the two kernel toggles are always available for mesh operands,
/// which is every operand this node can take.
#[test]
fn row_visibility_matches_the_operation() {
    let node = BooleanNode;
    for (operation, keep_subtracted_visible) in [
        ("Combine", false),
        ("Subtract", true),
        ("Intersect", false),
        ("Subtract & Replace", false),
    ] {
        let mut props = NodeProperties::default();
        props.insert("operation", op(operation));
        assert_eq!(
            node.row_visible(KEEP_SUBTRACTED, &props),
            keep_subtracted_visible,
            "Keep Subtracted Parts under {operation}"
        );
        assert_eq!(
            keep_subtracted_row_available(&props),
            keep_subtracted_visible
        );
        for key in [KEEP_INSIDE_OUT, REPAIR_WINDING] {
            assert!(
                node.row_visible(key, &props),
                "{key} is a kernel option — always visible, including under {operation}"
            );
        }
    }
}

// ------------------------------------------------------------ persistence

/// The three toggles are ordinary `Bool` properties and survive a save /
/// load, as does the Mark Hole node's `output_type`. The *role* itself is
/// runtime state — it is re-stamped by Mark Hole on every evaluation, so
/// nothing about a body needs to be written to the file.
#[test]
fn the_toggles_and_the_hole_marking_round_trip_through_a_project_file() {
    let reg = registry();
    let mut g = Graph::new();
    let b = g.add_new_node("Boolean", [0.0, 0.0], &reg).unwrap();
    let mark = g.add_new_node("Mark Hole", [200.0, 0.0], &reg).unwrap();
    for key in [KEEP_INSIDE_OUT, REPAIR_WINDING, KEEP_SUBTRACTED] {
        g.set_property(b, key, PortValue::Bool(true)).unwrap();
    }
    g.set_property(
        mark,
        OUTPUT_TYPE,
        PortValue::StringVal(Arc::new("Solid".into())),
    )
    .unwrap();

    let json = graph_to_json_string(&g);
    let LoadResult {
        graph: g2,
        warnings,
        ..
    } = graph_from_json_str(&json, &reg).unwrap();
    assert!(warnings.is_empty(), "warnings: {:?}", warnings);

    let reloaded = g2
        .nodes()
        .find(|n| n.type_id.as_ref() == "Boolean")
        .unwrap();
    for key in [KEEP_INSIDE_OUT, REPAIR_WINDING, KEEP_SUBTRACTED] {
        assert_eq!(
            reloaded.properties.get(key),
            Some(&PortValue::Bool(true)),
            "{key} did not survive the round trip"
        );
    }
    let reloaded_mark = g2
        .nodes()
        .find(|n| n.type_id.as_ref() == "Mark Hole")
        .unwrap();
    match reloaded_mark.properties.get(OUTPUT_TYPE) {
        Some(PortValue::StringVal(s)) => assert_eq!(s.as_str(), "Solid"),
        other => panic!("output_type round-tripped as {:?}", other),
    }
}

// ------------------------------------------------- the node's own frame

/// The Boolean node's own `matrix` property, as the fixture hands it over.
fn node_matrix(tx: f32) -> (&'static str, PortValue) {
    let mut m = crate::graph::node::identity_matrix();
    m[12] = tx;
    ("matrix", PortValue::Matrix4x4(m))
}

/// A kept remover has to live in the same frame as the result it was cut
/// from. Left carrying only its upstream matrix it would stay behind when
/// the Boolean node's own transform moved everything else — the parts
/// would come apart on the first gizmo drag.
#[test]
fn kept_subtracted_removers_move_with_the_boolean_node() {
    let remover = Geometry3d::from_body(
        Body::from_mesh(Arc::new(box_mesh()))
            .with_matrix(super::tests::translate_matrix(1.0, 0.0, 0.0)),
    );
    let out = run_boolean_with_props(
        &[("a", geom(box_mesh())), ("b", remover)],
        op("Subtract"),
        None,
        &[node_matrix(10.0), (KEEP_SUBTRACTED, PortValue::Bool(true))],
    )
    .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(
        out.bodies[0].matrix[12], 10.0,
        "the cut keep sits in the node's frame"
    );
    assert_eq!(
        out.bodies[1].matrix[12], 11.0,
        "the kept remover is the node's frame composed with its own \
         (10 + 1), not its own alone",
    );
}

/// The same rule on the pass-through path: an operation with nothing to do
/// still hands its parts back in the node's frame.
#[test]
fn passed_through_parts_move_with_the_boolean_node() {
    let out = run_boolean_with_props(
        &[("a", geom(box_mesh()))],
        op("Subtract"),
        None,
        &[node_matrix(10.0)],
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(
        out.bodies[0].matrix[12], 10.0,
        "a lone operand passes through, but through the node's transform"
    );
}

// --------------------------------------------- the role survives an op

/// MatterCAD keeps a child's `OutputType` across a subtract: cutting a
/// hole does not turn it into material. Without this, `Mark Hole →
/// Subtract → Combine` quietly stops cutting.
#[test]
fn a_hole_marked_keep_stays_a_hole_through_a_subtract() {
    let trimmed = run_boolean_inputs(
        &[("a", hole(box_mesh())), ("b", geom(box_at(1.5)))],
        op("Subtract"),
        None,
    )
    .unwrap();
    assert_eq!(trimmed.len(), 1);
    assert_eq!(
        trimmed.first().unwrap().role,
        BodyRole::Hole,
        "the trimmed hole is still a hole"
    );

    // …and it still cuts downstream.
    let combined = run_boolean_inputs(
        &[("solid", geom(box_mesh())), ("hole", trimmed)],
        op("Combine"),
        None,
    )
    .unwrap();
    assert!(
        total_volume(&combined) < 7.9,
        "the trimmed hole must still cut the solid, got {}",
        total_volume(&combined)
    );
}

/// A transform between the marking and the boolean must not lose the
/// role — `compose_with_upstream` carries it.
#[test]
fn a_transform_downstream_of_mark_hole_keeps_the_hole() {
    let reg = registry();
    let mut g = Graph::new();
    let bx = g.add_new_node("Box", [0.0, 0.0], &reg).unwrap();
    let mark = g.add_new_node("Mark Hole", [200.0, 0.0], &reg).unwrap();
    let tf = g.add_new_node("Transform", [400.0, 0.0], &reg).unwrap();
    let bx_out = g.get(bx).unwrap().output_by_name("out").unwrap().uid;
    let mark_in = g.get(mark).unwrap().input_by_name("input").unwrap().uid;
    g.connect(Noodle::new(bx, bx_out, mark, mark_in), &reg).unwrap();
    let mark_out = g.get(mark).unwrap().output_by_name("out").unwrap().uid;
    let tf_in = g.get(tf).unwrap().inputs[0].uid;
    g.connect(Noodle::new(mark, mark_out, tf, tf_in), &reg).unwrap();

    evaluate_all(&mut g, &reg).unwrap().expect_clean();
    let uid = g.get(tf).unwrap().outputs[0].uid;
    match g.get(tf).unwrap().cached_outputs.get(&uid) {
        Some(PortValue::Geometry3d(geo)) => assert_eq!(
            geo.first().unwrap().role,
            BodyRole::Hole,
            "moving a hole leaves it a hole"
        ),
        other => panic!("expected Geometry3d, got {:?}", other),
    }
}

/// Two levels: a hole-only Combine's result is a hole, so feeding it into a
/// second Combine cuts. This is what makes the hole-only case worth keeping
/// rather than erroring.
#[test]
fn a_hole_only_combine_cuts_in_a_second_combine() {
    let holes = run_boolean_inputs(
        &[("a", hole(box_at(1.0))), ("b", hole(box_at(1.5)))],
        op("Combine"),
        None,
    )
    .unwrap();
    let out = run_boolean_inputs(
        &[("solid", geom(box_mesh())), ("holes", holes)],
        op("Combine"),
        None,
    )
    .unwrap();
    assert!(
        (total_volume(&out) - EATEN_VOLUME).abs() < 1e-6,
        "the first Combine's hole union cuts in the second: {}",
        total_volume(&out)
    );
}

/// Mark Hole marks *every* body of a group, not just the first.
#[test]
fn mark_hole_marks_every_body_in_a_group() {
    let node = crate::nodes::ops_3d::mark_hole_node::MarkHoleNode;
    let group = Geometry3d::from_bodies(vec![
        Body::from_mesh(Arc::new(box_mesh())),
        Body::from_mesh(Arc::new(box_at(3.0))),
        Body::from_mesh(Arc::new(box_at(6.0))),
    ]);
    let out = run_single_input(&node, group);
    assert_eq!(out.len(), 3);
    assert!(
        out.iter().all(|b| b.role == BodyRole::Hole),
        "every body is marked, got {:?}",
        out.iter().map(|b| b.role).collect::<Vec<_>>()
    );
}

/// Evaluate a one-input node over a whole group, without a graph.
fn run_single_input(node: &dyn NodeDef, input: Geometry3d) -> Geometry3d {
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::graph::socket::SocketUidAlloc;
    use crate::registry::{EvalCtx, NodeInputs};

    let mut alloc = SocketUidAlloc::new();
    let tpl = node.instantiate(&mut alloc);
    let mut inst = NodeInstance::new(NodeId(1), node.type_id(), [0.0, 0.0]);
    inst.inputs = tpl.inputs;
    inst.outputs = tpl.outputs;
    let uid = inst.input_by_name("input").unwrap().uid;
    let mut inputs = NodeInputs::default();
    inputs.insert(uid, PortValue::Geometry3d(Arc::new(input)));
    let props = NodeProperties::default();
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    match node.evaluate(&ctx).unwrap().by_name.get("out") {
        Some(PortValue::Geometry3d(g)) => (**g).clone(),
        other => panic!("expected Geometry3d, got {:?}", other),
    }
}

/// The hole/solid split must not renumber the parts: a refusal still names
/// the part the user can count to in their own input.
#[test]
fn a_refusal_names_the_original_part_number_after_the_hole_split() {
    // Part 1 is a hole, part 2 is an open sheet the kernel cannot take.
    let open = MeshGL {
        num_prop: 6,
        vert_properties: vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 0.0, 0.0, 1.0,
        ],
        tri_verts: vec![0, 1, 2],
        ..Default::default()
    };
    let group = Geometry3d::from_bodies(vec![
        Body::from_mesh(Arc::new(box_mesh())).with_role(BodyRole::Hole),
        Body::from_mesh(Arc::new(open)),
    ]);
    let err = run_boolean_inputs(&[("in", group)], op("Combine"), None)
        .expect_err("an open sheet is not a solid");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("part 2"),
        "the message must still count parts the way the user's input does: {msg}"
    );
}
