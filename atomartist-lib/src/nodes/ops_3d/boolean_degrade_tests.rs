//! Tests for the Boolean node's degradation policy (plan step B-5 of
//! `docs/boolean-node-plan.md`).
//!
//! Expectations come from MatterCAD:
//!   * `Object3DBooleanOperations.GetTouchingMeshes` (L108-156) — scattered
//!     parts are unioned per connected AABB set, never with each other;
//!   * `ManifoldKernel.RunBoolean` (L143-146) — a refused operand is
//!     skippable for a **union** only;
//!   * `RepairAndRetryUnion` / `ClassifyRepairedOperand` (L513-673, L458)
//!     — the three-way triage, driven here through the repair *seam* for
//!     the same reason MatterCAD made it one: the branches are the thing
//!     worth testing and no fixture can promise a real repair reaches them;
//!   * `CopyRescuedOperand` (L315) — a part the kernel refused is still in
//!     the result afterwards;
//!   * `BooleanObject3D.ReportSkippedOperands` (L577) — the message names
//!     the parts.

use std::sync::Arc;

use manifold_rust::types::MeshGL;

use super::super::boolean_colors::Palette;
use super::super::boolean_degrade::{
    classify_repaired, union_degrading, union_degrading_with, DegradedBody, RepairedOperandUse,
};
use super::super::boolean_ops::{BooleanOptions, Operand};
use super::tests::{op, run_boolean_outputs, translated, volume};
use crate::geometry::mesh3d::STRIDE;
use crate::geometry::{generate_box, Body, BodyRole, Geometry3d};

// ---------------------------------------------------------------- helpers

/// A 2 mm box translated to `(x, y, z)`, as a single-body group.
fn box_at(x: f32, y: f32, z: f32) -> Geometry3d {
    Geometry3d::from_mesh(Arc::new(translated(&generate_box(2.0, 2.0, 2.0), x, y, z)))
}

/// One world-space operand for the union helpers, named for messages.
/// The source body is the mesh as it arrived (identity matrix), which is
/// what a rescue reads its colour and role from.
fn operand(label: &str, mesh: MeshGL) -> Operand {
    Operand {
        label: label.to_string(),
        source: Body::from_mesh(Arc::new(mesh.clone())),
        mesh,
    }
}

/// A box with one face removed — not a closed solid on any tolerance, so
/// neither the import nor the weld repair can take it.
fn open_box() -> MeshGL {
    let mut m = generate_box(2.0, 2.0, 2.0);
    let keep = m.tri_verts.len() - 6;
    m.tri_verts.truncate(keep);
    m
}

/// A box whose +X face was nudged 0.01 mm off the seam: a hundred times
/// wider than the import's float-rounding weld (1e-5 × the 17.3 mm
/// diagonal ≈ 1.7e-4), still inside the repair weld's coarser one
/// (1e-3 × 17.3 ≈ 0.017). The "repaired part rejoins the union" fixture.
fn wide_seam_box() -> MeshGL {
    let mut m = generate_box(10.0, 10.0, 10.0);
    for i in 0..4 {
        m.vert_properties[i * STRIDE] += 0.01;
    }
    m
}

fn total_volume(g: &Geometry3d) -> f64 {
    g.iter().map(|b| volume(&b.mesh)).sum()
}

// ------------------------------------------------------- touching sets

/// Three boxes that touch nothing: three bodies, each the original solid.
/// Volumes are exact because no CSG ran on them at all (`GetTouchingMeshes`).
#[test]
fn scattered_parts_come_out_as_their_own_bodies() {
    let outs = run_boolean_outputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)),
            ("b", box_at(5.0, 0.0, 0.0)),
            ("c", box_at(10.0, 0.0, 0.0)),
        ],
        op("Combine"),
        None,
        &[],
    )
    .expect("Combine of three scattered operands failed");
    let geom = &outs.geometry;
    assert_eq!(geom.len(), 3, "one body per touching set");
    for body in geom.iter() {
        let v = volume(&body.mesh);
        assert!(
            (v - 8.0).abs() < 1e-9,
            "body volume {} should be exactly 8",
            v
        );
    }
    assert!(outs.warnings.is_empty(), "nothing was degraded");
}

/// Two overlapping parts and one apart: the overlapping pair is one
/// unioned body, the loner is its own.
#[test]
fn touching_parts_union_and_the_far_one_stays_separate() {
    let outs = run_boolean_outputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)), // [-1,1]^3
            ("b", box_at(1.0, 0.0, 0.0)), // [0,2]x[-1,1]^2 — overlaps a
            ("c", box_at(9.0, 0.0, 0.0)), // far away
        ],
        op("Combine"),
        None,
        &[],
    )
    .expect("Combine failed");
    let geom = &outs.geometry;
    assert_eq!(geom.len(), 2, "one union body plus the untouched part");
    let mut volumes: Vec<f64> = geom.iter().map(|b| volume(&b.mesh)).collect();
    volumes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    assert!((volumes[0] - 8.0).abs() < 1e-9, "volumes {:?}", volumes);
    // a ∪ b = 8 + 8 - 4 (the overlap [0,1]x[-1,1]^2)
    assert!((volumes[1] - 12.0).abs() < 1e-6, "volumes {:?}", volumes);
}

/// Two parts that only share a face are one set: they are exactly the
/// parts a union has work to do on (`Intersects` is inclusive).
#[test]
fn parts_that_share_a_face_are_one_set() {
    let outs = run_boolean_outputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)), // [-1,1]^3
            ("b", box_at(2.0, 0.0, 0.0)), // [1,3]x[-1,1]^2 — shares x = 1
        ],
        op("Combine"),
        None,
        &[],
    )
    .expect("Combine failed");
    assert_eq!(outs.geometry.len(), 1, "a shared face is a touch");
    let v = total_volume(&outs.geometry);
    assert!((v - 16.0).abs() < 1e-6, "volume {}", v);
}

// ------------------------------------------------------- partial union

/// One refused operand among three: the other two still union, the refused
/// part is still in the output as its own body, and the node **succeeds**
/// with a warning that names it.
#[test]
fn a_refused_operand_is_rescued_and_named_instead_of_failing_the_combine() {
    let outs = run_boolean_outputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)),
            ("b", box_at(1.0, 0.0, 0.0)),
            (
                "broken",
                Geometry3d::from_mesh(Arc::new(translated(&open_box(), 20.0, 0.0, 0.0))),
            ),
        ],
        op("Combine"),
        None,
        &[],
    )
    .expect("a refused operand must not fail a Combine");
    let geom = &outs.geometry;
    assert_eq!(geom.len(), 2, "the union plus the rescued part");
    let warning = outs
        .warnings
        .iter()
        .find(|w| w.contains("not watertight"))
        .expect("the skipped part is reported");
    assert!(warning.contains("1 of 3 parts"), "{}", warning);
    assert!(warning.contains("'broken'"), "{}", warning);
    // The rescued body carries the user's own triangles, uncombined.
    let rescued = geom
        .iter()
        .find(|b| b.mesh.tri_verts.len() / 3 == open_box().tri_verts.len() / 3)
        .expect("the refused part's own geometry is in the output");
    assert!(rescued.mesh.num_prop == 6, "the rescue is render-ready");
}

/// Every operand refused: the node still succeeds, every part is in the
/// output, and the message counts them all.
#[test]
fn a_union_of_only_refused_operands_still_returns_the_parts() {
    let outs = run_boolean_outputs(
        &[
            ("x", Geometry3d::from_mesh(Arc::new(open_box()))),
            (
                "y",
                Geometry3d::from_mesh(Arc::new(translated(&open_box(), 20.0, 0.0, 0.0))),
            ),
        ],
        op("Combine"),
        None,
        &[],
    )
    .expect("an all-refused Combine is still an answer");
    assert_eq!(outs.geometry.len(), 2);
    assert!(
        outs.warnings[0].contains("2 of 2 parts"),
        "{}",
        outs.warnings[0]
    );
}

/// A rescued part keeps the colour it arrived with. The part that
/// visibly failed to combine must not also silently change colour —
/// MatterCAD paints its rescues with the operand's own colour for the
/// same reason (`CopyRescuedOperand`, L315-347).
#[test]
fn a_rescued_part_keeps_its_upstream_colour() {
    let orange = [0.9, 0.5, 0.1, 1.0];
    let broken = Geometry3d::from_bodies(vec![Body {
        color: orange,
        ..Body::from_mesh(Arc::new(translated(&open_box(), 20.0, 0.0, 0.0)))
    }]);
    let outs = run_boolean_outputs(
        &[("a", box_at(0.0, 0.0, 0.0)), ("broken", broken)],
        op("Combine"),
        None,
        &[],
    )
    .expect("a refused operand must not fail a Combine");

    let rescued = outs
        .geometry
        .iter()
        .find(|b| b.mesh.tri_verts.len() / 3 == open_box().tri_verts.len() / 3)
        .expect("the refused part is in the output");
    assert_eq!(
        rescued.color, orange,
        "the rescue kept its own colour, not the node's default"
    );
}

// ------------------------------------------- where a rescue actually goes

/// A rescued **solid** is emitted, uncut by the holes: the kernel would
/// not take it as a boolean operand either, so the alternative to leaving
/// it whole is losing it.
#[test]
fn a_rescued_solid_is_emitted_uncut_by_the_holes() {
    let hole = Geometry3d::from_body(
        Body::from_mesh(Arc::new(generate_box(2.0, 2.0, 2.0))).with_role(BodyRole::Hole),
    );
    // The open box sits where the hole is, so a *cut* rescue would lose
    // triangles; an uncut one keeps every one of them.
    let broken = Geometry3d::from_mesh(Arc::new(open_box()));
    let outs = run_boolean_outputs(
        &[
            ("solid", box_at(8.0, 0.0, 0.0)),
            ("broken", broken),
            ("hole", hole),
        ],
        op("Combine"),
        None,
        &[],
    )
    .expect("a refused operand must not fail a Combine");

    let rescued = outs
        .geometry
        .iter()
        .find(|b| b.mesh.tri_verts.len() / 3 == open_box().tri_verts.len() / 3)
        .expect("the rescued solid is in the output, whole");
    assert!(!rescued.is_hole());
    assert!(
        outs.warnings
            .iter()
            .any(|w| w.contains("still in the result") && w.contains("'broken'")),
        "the message says the part is there: {:?}",
        outs.warnings
    );
}

/// A rescued **hole** is the one case where geometry does not come out:
/// it cuts nothing, and a void emitted as a body would show material
/// where the user asked for none. The message has to say *that*, not
/// "still in the result".
#[test]
fn a_rescued_hole_is_dropped_and_the_warning_says_so() {
    let broken_hole =
        Geometry3d::from_body(Body::from_mesh(Arc::new(open_box())).with_role(BodyRole::Hole));
    let outs = run_boolean_outputs(
        &[("solid", box_at(0.0, 0.0, 0.0)), ("bad hole", broken_hole)],
        op("Combine"),
        None,
        &[],
    )
    .expect("a refused hole must not fail a Combine");

    assert_eq!(outs.geometry.len(), 1, "only the solid comes out");
    let said = outs.warnings.join(" ");
    assert!(
        said.contains("could not be used as holes") && said.contains("'bad hole'"),
        "the drop is named: {}",
        said
    );
    assert!(
        !said.contains("still in the result"),
        "and is not described as kept: {}",
        said
    );
}

/// With no solids at all the hole side *is* the answer, so everything on
/// it comes out — the rescue included, still marked as a hole (a
/// downstream Combine can still cut with what did import).
#[test]
fn a_hole_only_combine_emits_its_rescue_too() {
    let good_hole = Geometry3d::from_body(
        Body::from_mesh(Arc::new(generate_box(2.0, 2.0, 2.0))).with_role(BodyRole::Hole),
    );
    let broken_hole = Geometry3d::from_body(
        Body::from_mesh(Arc::new(translated(&open_box(), 20.0, 0.0, 0.0)))
            .with_role(BodyRole::Hole),
    );
    let outs = run_boolean_outputs(
        &[("hole", good_hole), ("bad hole", broken_hole)],
        op("Combine"),
        None,
        &[],
    )
    .expect("a hole-only Combine is still an answer");

    assert_eq!(outs.geometry.len(), 2, "the hole union plus the rescue");
    assert!(
        outs.geometry.iter().all(|b| b.is_hole()),
        "a hole-only Combine stays a hole, rescue included"
    );
    let said = outs.warnings.join(" ");
    assert!(
        said.contains("still in the result") && said.contains("'bad hole'"),
        "nothing was dropped, and the message says so: {}",
        said
    );
}

// ----------------------------------------------------------- the triage

/// A seam wider than the import tolerates but narrower than the repair's:
/// the repair welds it closed, the kernel takes it, and it goes **into**
/// the union — one body, not a rescued one beside it.
#[test]
fn a_repaired_operand_rejoins_the_union() {
    let refused = union_degrading(
        vec![operand("wide seam", wide_seam_box())],
        BooleanOptions::default(),
        Palette::new(),
    );
    assert_eq!(refused.bodies.len(), 1);
    assert!(
        matches!(refused.bodies[0], DegradedBody::Solid { .. }),
        "the repaired part is a kernel solid, not a rescue"
    );
    assert!(refused.report.skipped.is_empty(), "nothing was left out");
    assert_eq!(refused.report.repaired, vec!["wide seam".to_string()]);
    let message = refused.report.warnings();
    assert!(
        message[0].contains("automatically repaired"),
        "{:?}",
        message
    );
}

/// A repair whose result is watertight but self-intersecting is carried
/// **beside** the union rather than into it — the case that keeps a
/// combine terminating (`ClassifyRepairedOperand`, L442-452).
#[test]
fn a_self_intersecting_repair_is_kept_beside_the_union() {
    // The repair hands back two boxes crossing through each other: closed
    // (so the kernel takes it) and self-intersecting (so the union of it
    // is the open-ended one MatterCAD refuses to start).
    let repair = |_: &MeshGL| {
        let a = generate_box(4.0, 4.0, 4.0);
        let b = translated(&generate_box(4.0, 4.0, 4.0), 1.0, 1.0, 1.0);
        Some(concat(&a, &b))
    };
    let good = generate_box(2.0, 2.0, 2.0);
    let degraded = union_degrading_with(
        vec![operand("good", good), operand("crossed", open_box())],
        BooleanOptions::default(),
        Palette::new(),
        &repair,
    );
    assert_eq!(
        degraded.bodies.len(),
        2,
        "the union and the repair, separately"
    );
    assert!(degraded.report.skipped.is_empty());
    assert_eq!(degraded.report.repaired, vec!["crossed".to_string()]);
    assert!(
        degraded
            .bodies
            .iter()
            .all(|b| matches!(b, DegradedBody::Solid { .. })),
        "both bodies came from the kernel"
    );
}

/// The classifier itself, on the three shapes a repair can come back as.
#[test]
fn classify_repaired_sorts_the_three_outcomes() {
    let opts = BooleanOptions::default();
    let (clean, handle) = classify_repaired(&generate_box(2.0, 2.0, 2.0), opts);
    assert_eq!(clean, RepairedOperandUse::RejoinsUnion);
    assert!(handle.is_some());

    let crossed = concat(
        &generate_box(4.0, 4.0, 4.0),
        &translated(&generate_box(4.0, 4.0, 4.0), 1.0, 1.0, 1.0),
    );
    let (verdict, handle) = classify_repaired(&crossed, opts);
    assert_eq!(verdict, RepairedOperandUse::KeptSeparate);
    assert!(handle.is_some());

    let (failed, handle) = classify_repaired(&open_box(), opts);
    assert_eq!(failed, RepairedOperandUse::RepairFailed);
    assert!(handle.is_none());
}

/// A repair that produces nothing usable leaves the user's ORIGINAL
/// geometry in the result — not the repair's eroded remains.
#[test]
fn a_failed_repair_rescues_the_original_geometry() {
    let original = open_box();
    let degraded = union_degrading_with(
        vec![operand("broken", original.clone())],
        BooleanOptions::default(),
        Palette::new(),
        &|_| None,
    );
    let skipped: Vec<&str> = degraded
        .report
        .skipped
        .iter()
        .map(|s| s.label.as_str())
        .collect();
    assert_eq!(skipped, vec!["broken"]);
    assert!(
        degraded.report.skipped[0]
            .detail
            .contains("not a closed solid"),
        "the rescue keeps B-1's sentence about why: {}",
        degraded.report.skipped[0].detail
    );
    assert!(degraded.report.repaired.is_empty());
    assert!(
        degraded.report.skipped[0].kept,
        "a rescued solid is in the output, and the message says so"
    );
    match &degraded.bodies[0] {
        DegradedBody::Rescued { mesh, label, .. } => {
            assert_eq!(label, "broken");
            assert_eq!(
                mesh.tri_verts, original.tri_verts,
                "the rescue is the geometry the user modelled"
            );
        }
        _ => panic!("a part beyond repair must be rescued as itself"),
    }
}

// ---------------------------------------------- the other three operations

/// Subtract's remover union does **not** degrade: `DoSubtract` (L188-193)
/// calls `CombineParticipants` with no operand report, and that parameter's
/// own docs say a null report "keeps the old all-or-nothing behaviour".
/// Losing a remover would silently move the cut.
#[test]
fn a_refused_remover_still_fails_the_subtract() {
    let err = run_boolean_outputs(
        &[
            ("keep", box_at(0.0, 0.0, 0.0)),
            ("cut1", box_at(1.0, 1.0, 1.0)),
            ("cut2", Geometry3d::from_mesh(Arc::new(open_box()))),
        ],
        op("Subtract"),
        Some(&["cut1", "cut2"]),
        &[],
    )
    .expect_err("a remover the kernel refuses must fail the node");
    assert!(err.to_string().contains("cut2"), "{}", err);
}

/// Intersect is defined by every operand, so a refused one is still a
/// hard failure (`RunBoolean`, L143-146).
#[test]
fn a_refused_operand_still_fails_an_intersect() {
    let err = run_boolean_outputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0)),
            ("bad", Geometry3d::from_mesh(Arc::new(open_box()))),
        ],
        op("Intersect"),
        None,
        &[],
    )
    .expect_err("Intersect must not degrade");
    assert!(err.to_string().contains("'bad'"), "{}", err);
}

/// Concatenate two `num_prop = 6` meshes into one (indices rebased).
fn concat(a: &MeshGL, b: &MeshGL) -> MeshGL {
    let mut out = a.clone();
    let base = (a.vert_properties.len() / a.num_prop as usize) as u32;
    out.vert_properties.extend_from_slice(&b.vert_properties);
    out.tri_verts.extend(b.tri_verts.iter().map(|i| i + base));
    out
}
