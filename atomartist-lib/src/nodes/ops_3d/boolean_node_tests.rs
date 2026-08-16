//! Tests for the Boolean node's operand-import robustness (plan step B-1).
//!
//! Split out of `boolean_node.rs` to keep both files well under the 800-line
//! limit. Covers the three failure shapes MatterCAD's `ManifoldKernel.Import`
//! handles (ManifoldKernel.cs:500-540 / WeldSeams L623-655):
//!   * closed-but-non-manifold operand — must import as a triangle soup and
//!     produce real geometry (today's strict import silently produced empty
//!     output: the dark-blob screenshot),
//!   * not-closed operand — must surface a named node error, never silence,
//!   * seam-split operand — must import after ONE tolerance-scaled weld retry.

use std::sync::Arc;

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{BooleanConfig, BooleanEngine, Error, MeshGL};

use super::BooleanNode;
use crate::nodes::ops_3d::boolean_import::{import_operand, positions_only};
use crate::geometry::mesh3d::STRIDE;
use crate::geometry::{generate_box, Geometry3d};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::graph::socket::{Socket, SocketUidAlloc};
use crate::registry::{EvalCtx, NodeDef, NodeError, NodeInputs, NodeProperties};
use crate::socket_types::SocketType;

// ---------------------------------------------------------------- helpers

/// Run the Boolean node once over two operand meshes, returning the
/// **first** body's mesh. `operation` is the stored property value, so
/// tests can hand in either a variant name or a legacy `Number`.
fn run_boolean(a: MeshGL, b: MeshGL, operation: PortValue) -> Result<MeshGL, NodeError> {
    Ok(match run_boolean_bodies(a, b, operation)?.first() {
        Some(body) => (*body.mesh).clone(),
        None => MeshGL { num_prop: 6, ..Default::default() },
    })
}

/// Run the Boolean node once and return the whole output geometry —
/// Subtract & Replace produces two bodies, so its tests need the group.
fn run_boolean_bodies(
    a: MeshGL,
    b: MeshGL,
    operation: PortValue,
) -> Result<Geometry3d, NodeError> {
    run_boolean_geoms(
        Geometry3d::from_mesh(Arc::new(a)),
        Geometry3d::from_mesh(Arc::new(b)),
        operation,
    )
}

/// The same, over whole geometry groups (so a test can give an operand a
/// body matrix or several bodies).
pub(super) fn run_boolean_geoms(
    a: Geometry3d,
    b: Geometry3d,
    operation: PortValue,
) -> Result<Geometry3d, NodeError> {
    run_boolean_inputs(&[("a", a), ("b", b)], operation, None)
}

/// Evaluate the Boolean node over an arbitrary operand list.
///
/// Inputs are dynamic now, so the fixture builds the socket layout the
/// canvas would have left behind after each connect: one named slot per
/// operand, then the trailing empty placeholder.
pub(super) fn run_boolean_inputs(
    operands: &[(&str, Geometry3d)],
    operation: PortValue,
    removers: Option<&[&str]>,
) -> Result<Geometry3d, NodeError> {
    run_boolean_with_props(operands, operation, removers, &[])
}

/// [`run_boolean_inputs`] with extra property values written on top — the
/// B-4 toggles, which are plain `Bool` properties.
pub(super) fn run_boolean_with_props(
    operands: &[(&str, Geometry3d)],
    operation: PortValue,
    removers: Option<&[&str]>,
    extra: &[(&str, PortValue)],
) -> Result<Geometry3d, NodeError> {
    Ok(run_boolean_outputs(operands, operation, removers, extra)?.geometry)
}

/// What one Boolean evaluation produced: its geometry *and* its non-fatal
/// messages. The degradation policy (B-5) succeeds and warns, so a fixture
/// that only returned the geometry could not see half of what the node did.
#[derive(Debug)]
pub(super) struct BooleanRun {
    pub geometry: Geometry3d,
    pub warnings: Vec<String>,
}

/// [`run_boolean_with_props`], keeping the warnings.
pub(super) fn run_boolean_outputs(
    operands: &[(&str, Geometry3d)],
    operation: PortValue,
    removers: Option<&[&str]>,
    extra: &[(&str, PortValue)],
) -> Result<BooleanRun, NodeError> {
    let n = BooleanNode;
    let mut alloc = SocketUidAlloc::new();
    let tpl = n.instantiate(&mut alloc);
    let mut inst = NodeInstance::new(NodeId(1), "Boolean", [0.0, 0.0]);
    inst.inputs = Vec::new();
    inst.outputs = tpl.outputs;
    let mut inputs = NodeInputs::default();
    for (name, geom) in operands {
        let uid = alloc.allocate();
        inst.inputs
            .push(Socket::new(uid, *name, SocketType::Geometry3d, true));
        inputs.insert(uid, PortValue::Geometry3d(Arc::new(geom.clone())));
    }
    // The trailing empty placeholder the model always keeps last.
    inst.inputs.push(Socket::new(
        alloc.allocate(),
        "",
        SocketType::Geometry3d,
        true,
    ));

    let mut props = NodeProperties::default();
    props.insert("operation", operation);
    if let Some(names) = removers {
        // The selection is keyed by socket uid, which only exists once the
        // layout above is built — so tests name the removers and the
        // fixture resolves them.
        let uids: Vec<_> = names
            .iter()
            .filter_map(|n| inst.input_by_name(n).map(|s| s.uid))
            .collect();
        props.insert(
            crate::nodes::ops_3d::boolean_selection::SUBTRACT_PARTS,
            PortValue::StringVal(Arc::new(
                crate::nodes::ops_3d::boolean_selection::encode(&uids),
            )),
        );
    }

    for (name, value) in extra {
        props.insert(*name, value.clone());
    }

    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let outs = n.evaluate(&ctx)?;
    let geometry = match outs.by_name.get("out") {
        Some(PortValue::Geometry3d(g)) => (**g).clone(),
        _ => Geometry3d::empty(),
    };
    Ok(BooleanRun { geometry, warnings: outs.warnings })
}

/// Column-major translation matrix — the shape `Body::matrix` carries.
pub(super) fn translate_matrix(x: f32, y: f32, z: f32) -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        x, y, z, 1.0,
    ]
}

/// The stored property value for a named operation.
pub(super) fn op(name: &str) -> PortValue {
    PortValue::StringVal(Arc::new(name.to_string()))
}

/// Translate every vertex position of a `num_prop = 6` mesh.
pub(super) fn translated(mesh: &MeshGL, dx: f32, dy: f32, dz: f32) -> MeshGL {
    let mut out = mesh.clone();
    let stride = out.num_prop as usize;
    let n = out.vert_properties.len() / stride;
    for i in 0..n {
        out.vert_properties[i * stride] += dx;
        out.vert_properties[i * stride + 1] += dy;
        out.vert_properties[i * stride + 2] += dz;
    }
    out
}

/// Concatenate two `num_prop = 6` meshes into one mesh (indices rebased).
fn concat(a: &MeshGL, b: &MeshGL) -> MeshGL {
    let mut out = a.clone();
    let base = (a.vert_properties.len() / a.num_prop as usize) as u32;
    out.vert_properties.extend_from_slice(&b.vert_properties);
    out.tri_verts.extend(b.tri_verts.iter().map(|i| i + base));
    out
}

/// Axis-aligned bounds of a mesh's positions, as ([min], [max]).
fn bounds(mesh: &MeshGL) -> ([f32; 3], [f32; 3]) {
    let stride = mesh.num_prop as usize;
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for v in mesh.vert_properties.chunks_exact(stride) {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    (lo, hi)
}

/// Two unit-ish boxes meeting along exactly one shared edge, expressed as a
/// single mesh. Each box is closed, so the union is closed and orientable —
/// but the shared edge has four incident faces, which no manifold halfedge
/// pairing can represent. This is the operand shape that made the strict
/// import return empty.
fn two_boxes_sharing_an_edge() -> MeshGL {
    let a = generate_box(2.0, 2.0, 2.0); // [-1,1]^3
    let b = translated(&generate_box(2.0, 2.0, 2.0), 2.0, 2.0, 0.0); // [1,3]x[1,3]x[-1,1]
    concat(&a, &b)
}

/// A box with one face removed — closed nowhere near enough to bound a solid.
fn open_box() -> MeshGL {
    let mut m = generate_box(2.0, 2.0, 2.0);
    let keep = m.tri_verts.len() - 6; // drop the last face's two triangles
    m.tri_verts.truncate(keep);
    m
}

/// A box whose +X face was nudged off the seam by 1e-5 — far more than the
/// exact/epsilon weld tolerates, far less than the bbox-scaled seam
/// tolerance. Mirrors the float-rounded seams MatterCAD's `WeldSeams`
/// exists for.
fn seam_split_box() -> MeshGL {
    let mut m = generate_box(10.0, 10.0, 10.0);
    // generate_box lays the +X face down first: vertices 0..4. Nudging only
    // that face's copies of the corners splits the seam it shares with the
    // four side faces, whose own copies stay at x = 5.
    for i in 0..4 {
        m.vert_properties[i * STRIDE] += 1e-5;
    }
    m
}

/// Signed volume of a closed triangle mesh (divergence theorem), for
/// asserting that a boolean produced the *right* solid rather than merely a
/// non-empty one.
pub(super) fn volume(mesh: &MeshGL) -> f64 {
    let stride = mesh.num_prop as usize;
    let p = |i: u32| {
        let o = i as usize * stride;
        [
            mesh.vert_properties[o] as f64,
            mesh.vert_properties[o + 1] as f64,
            mesh.vert_properties[o + 2] as f64,
        ]
    };
    let mut total = 0.0;
    for t in mesh.tri_verts.chunks_exact(3) {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        total += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))
            / 6.0;
    }
    total.abs()
}

/// Two boxes that interpenetrate, expressed as one mesh: each shell is
/// closed, but their surfaces cross. The exact boolean engine's predicates
/// assume no self-intersection, so it double-counts the overlap and returns
/// a wrong solid — this is the operand shape the `Auto` engine default
/// exists for.
fn self_intersecting_pair() -> MeshGL {
    let a = generate_box(2.0, 2.0, 2.0); // [-1,1]^3
    let b = translated(&generate_box(2.0, 2.0, 2.0), 1.0, 1.0, 1.0); // [0,2]^3
    concat(&a, &b)
}

// ------------------------------------------------------------------ tests

#[test]
fn union_of_overlapping_boxes_yields_single_solid() {
    let boxes = (generate_box(2.0, 2.0, 2.0), generate_box(2.0, 2.0, 2.0));
    let out = match run_boolean(boxes.0, boxes.1, op("Combine")) {
        Ok(m) => m,
        Err(e) => panic!("union of two identical boxes failed: {}", e),
    };
    assert!(out.tri_verts.len() / 3 >= 12);
    let v = volume(&out);
    assert!(
        (v - 8.0).abs() < 1e-4,
        "union of two identical 2mm boxes has volume {}, expected 8",
        v
    );
}

/// Every triangle of the result must carry its own face normal on all three
/// corners. Manifold hands back a shared-vertex mesh, where writing face
/// normals into shared vertex slots leaves most triangles wearing whichever
/// neighbour's normal was written last — the flat shading goes to mush (the
/// visual half of the dark-blob report).
#[test]
fn union_result_has_per_triangle_flat_normals() {
    let boxes = (generate_box(2.0, 2.0, 2.0), generate_box(1.0, 3.0, 1.0));
    let out = match run_boolean(boxes.0, boxes.1, op("Combine")) {
        Ok(m) => m,
        Err(e) => panic!("union failed: {}", e),
    };
    let stride = out.num_prop as usize;
    assert_eq!(stride, 6, "output must carry normals");
    let mut disagreeing = 0;
    let total = out.tri_verts.len() / 3;
    for t in out.tri_verts.chunks_exact(3) {
        let p = |i: u32| {
            let o = i as usize * stride;
            [
                out.vert_properties[o] as f64,
                out.vert_properties[o + 1] as f64,
                out.vert_properties[o + 2] as f64,
            ]
        };
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let face = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let len = (face[0] * face[0] + face[1] * face[1] + face[2] * face[2]).sqrt();
        if len <= 0.0 {
            continue;
        }
        let face = [face[0] / len, face[1] / len, face[2] / len];
        for &i in t {
            let o = i as usize * stride;
            let n = [
                out.vert_properties[o + 3] as f64,
                out.vert_properties[o + 4] as f64,
                out.vert_properties[o + 5] as f64,
            ];
            let dot = n[0] * face[0] + n[1] * face[1] + n[2] * face[2];
            if dot < 0.99 {
                disagreeing += 1;
                break;
            }
        }
    }
    assert_eq!(
        disagreeing, 0,
        "{} of {} triangles carry a normal that is not their own face normal",
        disagreeing, total
    );
}

/// An edge-sharing operand (four faces on one edge) must keep both of its
/// bodies.
///
/// **Not** a soup-path test, despite the shape: an edge with four
/// halfedges still pairs 2-and-2, so this operand imports as an ordinary
/// manifold and never reaches
/// [`Manifold::from_mesh_gl_robust`](manifold_rust::manifold::Manifold::from_mesh_gl_robust)'s
/// soup branch — measured while writing B-6, which needed a real soup
/// handle and could not get one through this funnel (`MeshGL::merge` heals
/// the disconnected case, and shared-face and doubled-shell inputs pair
/// too). It is a guard on the *closed-non-manifold import*: the shape a
/// naive weld turns into garbage. A genuine soup fixture, built straight
/// against the kernel, lives in `boolean_colors_tests::soup_cube`.
#[test]
fn union_with_closed_non_manifold_operand_keeps_the_geometry() {
    let operands = (two_boxes_sharing_an_edge(), generate_box(1.0, 1.0, 1.0));
    let out = match run_boolean(operands.0, operands.1, op("Combine")) {
        Ok(m) => m,
        Err(e) => panic!("non-manifold operand rejected: {}", e),
    };
    assert!(
        !out.tri_verts.is_empty(),
        "union with a closed non-manifold operand produced empty geometry"
    );
    let (lo, hi) = bounds(&out);
    // The second box of operand 'a' reaches x = y = 3; empty/garbage output
    // would not.
    assert!(hi[0] > 2.9 && hi[1] > 2.9, "second body lost: max = {:?}", hi);
    assert!(lo[0] < -0.9 && lo[1] < -0.9, "first body lost: min = {:?}", lo);
}

/// A not-closed operand must be reported by name — never silent
/// emptiness. B-5 moved *where* that lands for a Combine: a union
/// degrades rather than fails, so the same sentence arrives as a warning
/// beside a result that still contains every part. Intersect, which is
/// defined by every operand, still fails hard.
#[test]
fn not_closed_operand_reports_a_named_problem() {
    let run = run_boolean_outputs(
        &[
            ("a", Geometry3d::from_mesh(Arc::new(generate_box(2.0, 2.0, 2.0)))),
            ("b", Geometry3d::from_mesh(Arc::new(open_box()))),
        ],
        op("Combine"),
        None,
        &[],
    )
    .expect("a Combine degrades rather than failing");
    let said = run.warnings.join(" ");
    assert!(said.contains("'b'"), "nothing names the operand: {}", said);
    assert!(
        said.to_lowercase().contains("closed"),
        "nothing explains the problem: {}",
        said
    );

    let err = match run_boolean(generate_box(2.0, 2.0, 2.0), open_box(), op("Intersect")) {
        Ok(m) => panic!("open box accepted, produced {} tris", m.tri_verts.len() / 3),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("'b'"), "error does not name the operand: {}", err);
    assert!(
        err.to_lowercase().contains("closed"),
        "error does not explain the problem: {}",
        err
    );
}

/// Hostile input — a triangle index past the end of the vertex array — must
/// come back as a named node error, not a panic from inside the kernel's
/// weld (which runs before any status exists to check).
#[test]
fn out_of_bounds_index_reports_a_named_error() {
    let mut mesh = generate_box(2.0, 2.0, 2.0);
    let n = (mesh.vert_properties.len() / STRIDE) as u32;
    mesh.tri_verts[0] = n + 7;
    // Intersect, because a Combine now degrades instead of failing
    // (B-5) — the *hostile input never panics* half of this test is
    // unchanged either way.
    let err = match run_boolean(generate_box(2.0, 2.0, 2.0), mesh, op("Intersect")) {
        Ok(m) => panic!("out-of-bounds operand accepted, produced {} tris", m.tri_verts.len() / 3),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("'b'"), "error does not name the operand: {}", err);
    assert!(
        err.contains("do not exist"),
        "error does not explain the problem: {}",
        err
    );
}

/// An operand whose triangles all collapse imports cleanly as an *empty*
/// manifold, so the boolean would "succeed" with the part gone. Refuse it:
/// the whole point of this module is that geometry never disappears quietly.
#[test]
fn degenerate_operand_reports_a_named_error() {
    let mut mesh = generate_box(2.0, 2.0, 2.0);
    for v in mesh.vert_properties.chunks_exact_mut(STRIDE) {
        v[0] = 0.0;
        v[1] = 0.0;
        v[2] = 0.0;
    }
    // Intersect: a Combine degrades a refused operand into a warning
    // (B-5), and this test is about the refusal itself.
    let err = match run_boolean(generate_box(2.0, 2.0, 2.0), mesh, op("Intersect")) {
        Ok(m) => panic!("degenerate operand accepted, produced {} tris", m.tri_verts.len() / 3),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("'b'"), "error does not name the operand: {}", err);
    assert!(
        err.contains("no solid geometry"),
        "error does not explain the problem: {}",
        err
    );
}

/// The plain robust import refuses a seam-split mesh; `import_operand`'s one
/// weld retry (bbox-scaled tolerance) recovers it.
#[test]
fn seam_split_operand_imports_after_the_weld_retry() {
    let mesh = seam_split_box();
    let plain = Manifold::from_mesh_gl_robust(&positions_only(&mesh));
    assert_eq!(
        plain.status(),
        Error::NotClosed,
        "test fixture is not seam-split; the retry path would not be exercised"
    );
    match import_operand(&mesh) {
        Ok(m) => assert!(!m.is_empty(), "welded operand imported empty"),
        Err(failure) => panic!("weld retry failed: {:?}", failure),
    }
}

/// And end to end: the seam-split box survives the boolean instead of
/// silently dropping out of the union (which the old strict import did — the
/// union then quietly returned only the *other* operand).
#[test]
fn union_with_seam_split_operand_keeps_the_geometry() {
    let out = match run_boolean(seam_split_box(), generate_box(2.0, 2.0, 2.0), op("Combine")) {
        Ok(m) => m,
        Err(e) => panic!("seam-split operand rejected: {}", e),
    };
    let (lo, hi) = bounds(&out);
    assert!(
        hi[0] > 4.9 && hi[1] > 4.9 && lo[2] < -4.9,
        "the 10mm seam-split box vanished from the union: {:?}..{:?}",
        lo,
        hi
    );
}

/// A self-intersecting operand must produce the correct union volume. The
/// exact engine double-counts the overlap (15.875 instead of 15) because its
/// predicates assume no self-intersection; the `Auto` default routes it to
/// the robust engine instead.
#[test]
fn union_with_self_intersecting_operand_has_the_right_volume() {
    // [-1,1]^3 (8) ∪ [0,2]^3 (8) overlap [0,1]^3 (1) = 15; operand 'b' sits
    // wholly inside the first box, so it adds nothing.
    let operands = (self_intersecting_pair(), generate_box(1.0, 1.0, 1.0));
    let out = match run_boolean(operands.0, operands.1, op("Combine")) {
        Ok(m) => m,
        Err(e) => panic!("self-intersecting operand rejected: {}", e),
    };
    let v = volume(&out);
    assert!(
        (v - 15.0).abs() < 1e-3,
        "union of self-intersecting operands has volume {}, expected 15",
        v
    );
}

/// Evaluating the node installs the `Auto` engine default, so soup operands
/// route to the robust engine while clean ones keep the exact pipeline.
#[test]
fn evaluating_sets_the_auto_engine_default() {
    let _ = run_boolean(generate_box(2.0, 2.0, 2.0), generate_box(2.0, 2.0, 2.0), op("Combine"));
    assert_eq!(BooleanConfig::default_engine(), BooleanEngine::Auto);
}

// ------------------------------------------- the four operations (B-2)

/// The overlapping-box fixture the operation tests share: `a` is
/// `[-1,1]^3` (volume 8) and `b` is `[0,2]^3` (volume 8), overlapping in
/// `[0,1]^3` (volume 1). Combine = 15, Subtract = 7, Intersect = 1, and
/// Subtract & Replace = 7 + 1 as two bodies.
fn overlapping_pair() -> (MeshGL, MeshGL) {
    (
        generate_box(2.0, 2.0, 2.0),
        translated(&generate_box(2.0, 2.0, 2.0), 1.0, 1.0, 1.0),
    )
}

#[test]
fn combine_keeps_the_union_volume() {
    let (a, b) = overlapping_pair();
    let out = match run_boolean(a, b, op("Combine")) {
        Ok(m) => m,
        Err(e) => panic!("Combine failed: {}", e),
    };
    let v = volume(&out);
    assert!((v - 15.0).abs() < 1e-3, "Combine volume {}, expected 15", v);
}

#[test]
fn subtract_removes_b_from_a() {
    let (a, b) = overlapping_pair();
    let out = match run_boolean(a, b, op("Subtract")) {
        Ok(m) => m,
        Err(e) => panic!("Subtract failed: {}", e),
    };
    let v = volume(&out);
    assert!((v - 7.0).abs() < 1e-3, "Subtract volume {}, expected 7", v);
}

#[test]
fn intersect_keeps_only_the_shared_volume() {
    let (a, b) = overlapping_pair();
    let out = match run_boolean(a, b, op("Intersect")) {
        Ok(m) => m,
        Err(e) => panic!("Intersect failed: {}", e),
    };
    let v = volume(&out);
    assert!((v - 1.0).abs() < 1e-3, "Intersect volume {}, expected 1", v);
}

/// Subtract & Replace keeps the cut result AND the volume it removed, as
/// two separate bodies — together they are exactly operand 'a'.
#[test]
fn subtract_and_replace_yields_two_bodies_summing_to_a() {
    let (a, b) = overlapping_pair();
    let geom = match run_boolean_bodies(a, b, op("Subtract & Replace")) {
        Ok(g) => g,
        Err(e) => panic!("Subtract & Replace failed: {}", e),
    };
    assert_eq!(geom.len(), 2, "expected a kept body and a replaced body");
    let kept = volume(&geom.bodies[0].mesh);
    let removed = volume(&geom.bodies[1].mesh);
    assert!((kept - 7.0).abs() < 1e-3, "kept body volume {}, expected 7", kept);
    assert!(
        (removed - 1.0).abs() < 1e-3,
        "replaced body volume {}, expected 1",
        removed
    );
    assert!(
        (kept + removed - 8.0).abs() < 1e-3,
        "the two bodies sum to {}, expected operand 'a' (8)",
        kept + removed
    );
}

/// Operands that do not touch have nothing to replace, so the result is
/// the keep alone. An empty second body would still be a *body* — part
/// counts, exports, and the viewport's per-body iteration would all see
/// a phantom part with no triangles.
#[test]
fn subtract_and_replace_omits_an_empty_intersection() {
    let a = generate_box(2.0, 2.0, 2.0); // [-1,1]^3
    let b = translated(&generate_box(2.0, 2.0, 2.0), 10.0, 0.0, 0.0); // far away
    let geom = match run_boolean_bodies(a, b, op("Subtract & Replace")) {
        Ok(g) => g,
        Err(e) => panic!("Subtract & Replace on disjoint operands failed: {}", e),
    };
    assert_eq!(
        geom.len(),
        1,
        "disjoint operands must yield the keep alone, not a phantom empty body"
    );
    let kept = volume(&geom.bodies[0].mesh);
    assert!((kept - 8.0).abs() < 1e-3, "kept volume {}, expected 8", kept);
}

/// An unknown variant name is not a crash and not a silent nothing — the
/// reader falls back to the declared default (Combine).
#[test]
fn unknown_operation_name_falls_back_to_combine() {
    let (a, b) = overlapping_pair();
    let out = match run_boolean(a, b, op("Frobnicate")) {
        Ok(m) => m,
        Err(e) => panic!("unknown operation errored: {}", e),
    };
    let v = volume(&out);
    assert!((v - 15.0).abs() < 1e-3, "fallback volume {}, expected 15", v);
}

/// A `Body` carries its transform in `Body::matrix` (Transform composes
/// matrices instead of baking them into vertices), so an operand that was
/// moved upstream arrives here still centred on its own origin. The
/// boolean must bake that matrix in before importing — MatterCAD passes
/// each participant's `WorldMatrix` into `BooleanProcessing.Do`
/// (`Object3DBooleanOperations.DoSubtract`). Without the bake, `Box →
/// Transform(+1,+1,+1) → Boolean(Subtract)` subtracts the box from
/// *itself* and the part vanishes.
#[test]
fn an_operands_body_matrix_is_baked_before_the_boolean() {
    let a = generate_box(2.0, 2.0, 2.0); // [-1,1]^3, volume 8
    let b = generate_box(2.0, 2.0, 2.0); // same mesh …
    let moved = translate_matrix(1.0, 1.0, 1.0); // … but moved to [0,2]^3
    let geom = match run_boolean_geoms(
        Geometry3d::from_mesh(Arc::new(a)),
        Geometry3d::from_mesh(Arc::new(b)).with_matrix(moved),
        op("Subtract"),
    ) {
        Ok(g) => g,
        Err(e) => panic!("Subtract with a transformed operand failed: {}", e),
    };
    let v: f64 = geom.iter().map(|body| volume(&body.mesh)).sum();
    assert!(
        (v - 7.0).abs() < 1e-3,
        "Subtract with a translated operand gave volume {}, expected 7 \
         (0 means the operand's matrix was ignored and the box cut itself away)",
        v
    );
}

/// A graph built before the enum landed stores `operation` as a number.
/// Loading migrates it (see `serialization::prop_migration`), but a value
/// that never went through a load — a NodeDesigner import, a test — must
/// still evaluate as the operation the index meant.
#[test]
fn legacy_numeric_operation_still_evaluates_as_subtract() {
    let (a, b) = overlapping_pair();
    let out = match run_boolean(a, b, PortValue::Number(1.0)) {
        Ok(m) => m,
        Err(e) => panic!("legacy numeric operation failed: {}", e),
    };
    let v = volume(&out);
    assert!(
        (v - 7.0).abs() < 1e-3,
        "legacy operation 1 gave volume {}, expected Subtract's 7",
        v
    );
}

// A test for "the boolean RESULT carries a bad status" is deliberately
// absent: every non-`NoError` status reachable after a successful import is
// either `Cancelled` (needs a CancelToken, which B-6 introduces) or
// `ResultTooLarge` (needs a >2^31-triangle result). The check itself is
// exercised by the import-side tests above, which share the same
// status-to-NodeError mapping.
