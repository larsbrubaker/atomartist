//! Tests for colour preservation across a boolean (plan step B-6 of
//! `docs/boolean-node-plan.md`).
//!
//! Expectations come from MatterCAD:
//!   * `ManifoldKernel.ImportAsOriginal` (L670-709) — each operand is
//!     re-tagged so results derived from it report its original id, and a
//!     soup handle that cannot be re-tagged loses its colours, not the
//!     boolean;
//!   * `ExtractFaceColorsFromRuns` (L870-927) — a result face wears the
//!     colour of the operand whose run it lands in, which is what puts the
//!     *remover's* colour on the faces a subtract cuts;
//!   * `BooleanObject3D` / the operation icons (plan §3.5) — the volume
//!     Subtract & Replace keeps in place of the one it cut wears the
//!     "removed material" red `#F20D0D`.
//!
//! The node's own `Color` is an override that beats all of that, matching
//! `compose_with_upstream`'s rule for every other body the node emits.

use std::sync::Arc;

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{Error, MeshGL, OpType};

use super::super::boolean_colors::{
    node_color_override, operand_color, tag_original, vertex_colors_for, Palette, RunPaint,
    REPLACED_TINT,
};
use super::super::boolean_import::import_operand;
use super::super::boolean_ops::{boolean_op, finish_mesh, BooleanOptions};
use super::tests::{op, run_boolean_inputs, run_boolean_with_props, translated};
use crate::geometry::{generate_box, Body, Geometry3d, DEFAULT_GEOMETRY_COLOR, INHERIT_COLOR};
use crate::graph::node::PortValue;

// ---------------------------------------------------------------- helpers

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

/// A 2 mm box at `(x, y, z)` carrying `color` — one operand.
fn box_at(x: f32, y: f32, z: f32, color: [f32; 4]) -> Geometry3d {
    let mesh = translated(&generate_box(2.0, 2.0, 2.0), x, y, z);
    Geometry3d::from_body(Body::from_mesh(Arc::new(mesh)).with_color(color))
}

/// The colours of every **face** of `body` whose centroid satisfies
/// `pred`. Faces, not vertices: colours arrive per face (a run is a span
/// of triangles), and a vertex probe would also catch the corner a
/// neighbouring face happens to share the plane with — the rim of a cut
/// face belongs to the side walls too.
fn colors_where(body: &Body, pred: impl Fn([f32; 3]) -> bool) -> Vec<[f32; 4]> {
    let colors = match body.vertex_colors.as_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let stride = body.mesh.num_prop as usize;
    let pos = |v: u32| {
        let o = v as usize * stride;
        [
            body.mesh.vert_properties[o],
            body.mesh.vert_properties[o + 1],
            body.mesh.vert_properties[o + 2],
        ]
    };
    body.mesh
        .tri_verts
        .chunks_exact(3)
        .filter(|t| {
            let (a, b, c) = (pos(t[0]), pos(t[1]), pos(t[2]));
            pred([
                (a[0] + b[0] + c[0]) / 3.0,
                (a[1] + b[1] + c[1]) / 3.0,
                (a[2] + b[2] + c[2]) / 3.0,
            ])
        })
        .map(|t| {
            let v = t[0] as usize;
            [
                colors[v * 4],
                colors[v * 4 + 1],
                colors[v * 4 + 2],
                colors[v * 4 + 3],
            ]
        })
        .collect()
}

/// Assert every colour in `found` is `expected`, and that there was at
/// least one — an empty probe would pass any assertion vacuously.
fn all_are(found: &[[f32; 4]], expected: [f32; 4], what: &str) {
    assert!(!found.is_empty(), "no vertices matched the {what} probe");
    for c in found {
        assert_eq!(*c, expected, "{what} should be {expected:?}");
    }
}

/// Signed volume of a closed mesh — geometry has to stay correct while the
/// colours land on it.
fn volume(mesh: &MeshGL) -> f64 {
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

/// A **soup** handle: a cube whose triangles share no vertex indices at
/// all, imported through the robust path without the seam weld
/// `import_operand` runs first (that weld is exactly what heals this shape
/// into a manifold, which is why the fixture goes to the kernel directly).
/// A soup is the handle `as_original` refuses.
fn soup_cube() -> Manifold {
    let corners = [
        [-1.0f32, -1.0, -1.0],
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0],
    ];
    let quads = [
        [0usize, 3, 2, 1],
        [4, 5, 6, 7],
        [0, 1, 5, 4],
        [1, 2, 6, 5],
        [2, 3, 7, 6],
        [3, 0, 4, 7],
    ];
    let mut mesh = MeshGL { num_prop: 3, ..Default::default() };
    for q in quads {
        for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            for c in tri {
                mesh.vert_properties.extend_from_slice(&corners[c]);
            }
        }
    }
    mesh.tri_verts = (0..mesh.vert_properties.len() as u32 / 3).collect();
    Manifold::from_mesh_gl_robust(&mesh)
}

// ------------------------------------------------------- node-level tests

#[test]
fn combine_paints_each_part_with_its_own_source_color() {
    // Two overlapping boxes: A spans x ∈ [-1, 1], B spans x ∈ [0.5, 2.5].
    // The far ends of the union can only have come from one of them each.
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Combine"),
        None,
    )
    .expect("combine");
    let body = out.first().expect("one union body");
    assert!(
        body.has_vertex_colors(),
        "a two-colour union must carry per-vertex colours"
    );
    all_are(&colors_where(body, |p| p[0] < -0.9), RED, "A-only end");
    all_are(&colors_where(body, |p| p[0] > 2.4), BLUE, "B-only end");
}

#[test]
fn a_painted_body_still_carries_a_colour_for_consumers_that_drop_the_overlay() {
    // The per-vertex overlay is not universal currency: the 3-D shader
    // uses it *instead of* `Body.color` (`opaque_shaders.rs` reads
    // `v_color` directly), but every consumer that rebuilds a mesh drops
    // it — `compose_with_upstream_and_mesh` sets `vertex_colors: None`, so
    // a downstream op sees only the tint. A painted body must therefore
    // carry a defensible one: the first part's colour, not a white
    // multiplier that would paint the part white downstream.
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Combine"),
        None,
    )
    .expect("combine");
    let body = out.first().expect("one union body").clone();
    assert!(body.has_vertex_colors());
    assert_ne!(
        body.color,
        [1.0, 1.0, 1.0, 1.0],
        "a white tint would paint the part white wherever the overlay is dropped"
    );
    assert_eq!(body.color, RED, "the first part's colour stands in");

    // And that survives the round trip through a node that rebuilds the
    // mesh — the import strips every property, so the operand's colour is
    // all the next operation has to go on.
    let downstream = run_boolean_inputs(
        &[("a", Geometry3d::from_bodies(vec![body]))],
        op("Combine"),
        None,
    )
    .expect("downstream combine");
    assert_eq!(
        downstream.first().expect("one body").color,
        RED,
        "the colour has to survive an op that rebuilds the mesh"
    );
}

#[test]
fn a_subtract_keep_stands_in_with_its_own_colour_not_the_removers() {
    // The removers are imported (and so recorded) first, so "the first
    // colour recorded" is the *cutter's* — using it as a keep's stand-in
    // would repaint a part with the colour of the thing that cut it.
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Subtract"),
        Some(&["b"]),
    )
    .expect("subtract");
    let body = out.first().expect("one keep body");
    assert_eq!(
        body.color, RED,
        "the kept part stands in with its own colour, not the remover's"
    );
}

#[test]
fn one_color_across_the_operands_needs_no_vertex_buffer() {
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, RED)),
        ],
        op("Combine"),
        None,
    )
    .expect("combine");
    let body = out.first().expect("one union body");
    assert!(
        !body.has_vertex_colors(),
        "a single-colour result should say so with its tint, not a buffer"
    );
    assert_eq!(body.color, RED);
}

#[test]
fn uncolored_operands_still_produce_the_default_tint() {
    // The pre-B-6 behaviour, pinned: nothing coloured, nothing overridden,
    // so the result paints exactly as it did before the run data was read.
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, DEFAULT_GEOMETRY_COLOR)),
            ("b", box_at(1.5, 0.0, 0.0, DEFAULT_GEOMETRY_COLOR)),
        ],
        op("Combine"),
        None,
    )
    .expect("combine");
    let body = out.first().expect("one union body");
    assert!(!body.has_vertex_colors());
    assert_eq!(body.color, DEFAULT_GEOMETRY_COLOR);
}

#[test]
fn subtract_leaves_the_removers_color_on_the_cut_faces() {
    // Keep A spans x ∈ [-1, 1] in red; remover B spans x ∈ [0.5, 2.5] in
    // blue. What is left is x ∈ [-1, 0.5], and the face at x = 0.5 is the
    // remover's surface — MatterCAD paints it from the run it came from.
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Subtract"),
        Some(&["b"]),
    )
    .expect("subtract");
    let body = out.first().expect("one keep body");
    assert!(body.has_vertex_colors(), "the cut face is a second colour");
    all_are(&colors_where(body, |p| p[0] < -0.9), RED, "kept face");
    all_are(&colors_where(body, |p| p[0] > 0.4), BLUE, "cut face");
}

#[test]
fn subtract_and_replace_tints_the_replaced_body_red() {
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Subtract & Replace"),
        Some(&["b"]),
    )
    .expect("subtract & replace");
    assert_eq!(out.len(), 2, "the keep and the volume it gave up");
    let replaced = &out.bodies[1];
    assert_eq!(
        replaced.color, REPLACED_TINT,
        "the retained remover wears the operation's 'removed material' red"
    );
    assert!(!replaced.has_vertex_colors());
    // ... and it is the intersection, not an empty shell.
    assert!(volume(&replaced.mesh) > 0.5);
}

#[test]
fn keep_subtracted_parts_keeps_the_removers_own_color() {
    let out = run_boolean_with_props(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Subtract"),
        Some(&["b"]),
        &[(
            crate::nodes::ops_3d::boolean_node::KEEP_SUBTRACTED,
            PortValue::Bool(true),
        )],
    )
    .expect("subtract");
    assert_eq!(out.len(), 2, "the cut keep plus the remover");
    let remover = &out.bodies[1];
    assert_eq!(
        remover.color, BLUE,
        "a part kept aside was not operated on and keeps its colour"
    );
}

#[test]
fn an_explicit_node_color_overrides_the_run_colors() {
    let out = run_boolean_with_props(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Combine"),
        None,
        &[("color", PortValue::Color(GREEN))],
    )
    .expect("combine");
    let body = out.first().expect("one union body");
    assert_eq!(body.color, GREEN);
    assert!(
        !body.has_vertex_colors(),
        "an overridden colour leaves nothing for the run data to say"
    );
}

#[test]
fn an_explicit_node_color_also_overrides_the_replaced_tint() {
    let out = run_boolean_with_props(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Subtract & Replace"),
        Some(&["b"]),
        &[("color", PortValue::Color(GREEN))],
    )
    .expect("subtract & replace");
    assert_eq!(out.len(), 2);
    for body in out.iter() {
        assert_eq!(body.color, GREEN);
    }
}

#[test]
fn intersect_paints_from_the_operands_too() {
    let out = run_boolean_inputs(
        &[
            ("a", box_at(0.0, 0.0, 0.0, RED)),
            ("b", box_at(1.5, 0.0, 0.0, BLUE)),
        ],
        op("Intersect"),
        None,
    )
    .expect("intersect");
    let body = out.first().expect("one body");
    // The shared volume is bounded by faces of both operands, so both
    // colours are on it: A's surface at x = 1, B's at x = 0.5.
    assert!(body.has_vertex_colors());
    all_are(&colors_where(body, |p| p[0] > 0.9), RED, "A's face");
    all_are(&colors_where(body, |p| p[0] < 0.6), BLUE, "B's face");
}

#[test]
fn a_soup_operand_keeps_the_boolean_and_only_loses_its_color() {
    // `as_original` cannot re-tag a soup handle (`ImportAsOriginal`
    // L682-691 documents the same caveat), so the operand has no run of
    // its own to be painted from. What must NOT happen is the boolean
    // failing, or the untagged geometry vanishing.
    let mut palette = Palette::new();
    let soup = soup_cube();
    assert_eq!(soup.status(), Error::NoError, "the soup imported");
    let tagged = tag_original(soup, GREEN, &mut palette);
    assert_eq!(
        tagged.status(),
        Error::NoError,
        "the handle came back whole"
    );
    assert!(
        palette.is_empty(),
        "a soup owns no original id, so it records no colour"
    );

    // Union it with a taggable operand and the result is still right —
    // and the untagged half wears the stand-in the caller chose, which is
    // this body's own part (GREEN, the soup), never some other operand's.
    let solid = import_operand(&translated(&generate_box(2.0, 2.0, 2.0), 1.5, 0.0, 0.0))
        .expect("plain box imports");
    let solid = tag_original(solid, RED, &mut palette);
    let union = boolean_op(&tagged, &solid, OpType::Add, BooleanOptions::default());
    assert_eq!(union.status(), Error::NoError, "the boolean still ran");
    assert!(union.volume() > 8.0, "both parts are in the union");
    // A soup operand takes the whole boolean down the robust path, whose
    // result carries no run relations at all — so the colours degrade
    // *wholesale*, to the stand-in the caller chose (this body's own
    // part), rather than to some other operand's colour.
    assert_eq!(
        palette.paint_for(&union.get_mesh_gl(-1), GREEN),
        Some(RunPaint::Uniform(GREEN)),
        "a soup in the operands costs the run data, and nothing else"
    );
}

#[test]
fn a_boolean_saved_before_b6_loads_with_its_colour_free() {
    // Node creation seeds every schema default into the instance
    // (`Graph::add_new_node`) and those defaults are saved, so a Boolean
    // written before B-6 carries an explicit `DEFAULT_GEOMETRY_COLOR` —
    // which reads as an override and would leave B-6 silently inert on
    // every existing file. `on_loaded` maps exactly that value back to
    // the inherit sentinel.
    let mut reg = crate::registry::NodeRegistry::new();
    crate::nodes::register_all(&mut reg);
    let mut g = crate::graph::graph::Graph::new();
    let b = g.add_new_node("Boolean", [0.0, 0.0], &reg).expect("add");
    assert_eq!(
        g.get(b).expect("node").properties.get("color"),
        Some(&PortValue::Color(INHERIT_COLOR)),
        "a new Boolean starts with no colour of its own"
    );
    // Exactly what a pre-B-6 file holds.
    g.set_property(b, "color", PortValue::Color(DEFAULT_GEOMETRY_COLOR))
        .expect("set");

    let json = crate::serialization::graph_to_json_string(&g);
    let loaded = crate::serialization::graph_from_json_str(&json, &reg).expect("load");
    let reloaded = loaded
        .graph
        .nodes()
        .find(|n| n.type_id.as_ref() == "Boolean")
        .expect("the Boolean survived the round trip");
    assert_eq!(
        reloaded.properties.get("color"),
        Some(&PortValue::Color(INHERIT_COLOR)),
        "an old file's seeded default must not read as an override"
    );

    // A colour the user *did* pick still round-trips as an override.
    g.set_property(b, "color", PortValue::Color(GREEN))
        .expect("set");
    let json = crate::serialization::graph_to_json_string(&g);
    let loaded = crate::serialization::graph_from_json_str(&json, &reg).expect("load");
    let reloaded = loaded
        .graph
        .nodes()
        .find(|n| n.type_id.as_ref() == "Boolean")
        .expect("the Boolean survived the round trip");
    assert_eq!(
        reloaded.properties.get("color"),
        Some(&PortValue::Color(GREEN))
    );
}

// ------------------------------------------------------------ unit tests

/// A run-tagged mesh: `runs` is `(first_triangle, original_id)`, and every
/// triangle is a placeholder — `paint_for` only reads the run arrays and
/// the triangle count.
fn mesh_with_runs(n_tri: usize, runs: &[(usize, u32)]) -> MeshGL {
    let mut mesh = MeshGL { num_prop: 3, ..Default::default() };
    mesh.vert_properties = vec![0.0; n_tri * 9];
    mesh.tri_verts = (0..n_tri as u32 * 3).collect();
    for (start, id) in runs {
        mesh.run_index.push(*start as u32 * 3);
        mesh.run_original_id.push(*id);
    }
    mesh.run_index.push(n_tri as u32 * 3);
    mesh
}

#[test]
fn paint_for_maps_each_run_to_its_operands_color() {
    let mut palette = Palette::new();
    palette.record(7, RED);
    palette.record(9, BLUE);
    let mesh = mesh_with_runs(4, &[(0, 7), (2, 9)]);
    match palette.paint_for(&mesh, GREEN) {
        Some(RunPaint::PerTriangle(tris)) => {
            assert_eq!(tris, vec![RED, RED, BLUE, BLUE]);
        }
        other => panic!("expected per-triangle colours, got {other:?}"),
    }
}

#[test]
fn a_run_from_an_untagged_source_wears_the_callers_stand_in() {
    // The soup case in miniature: id 42 was never recorded. The stand-in
    // is the caller's — the part *this body is* — and emphatically not
    // "the first colour recorded", which for a Subtract is the remover's.
    let mut palette = Palette::new();
    palette.record(7, RED);
    palette.record(9, BLUE);
    let mesh = mesh_with_runs(3, &[(0, 42), (1, 9)]);
    match palette.paint_for(&mesh, GREEN) {
        Some(RunPaint::PerTriangle(tris)) => assert_eq!(tris, vec![GREEN, BLUE, BLUE]),
        other => panic!("expected per-triangle colours, got {other:?}"),
    }
}

#[test]
fn a_malformed_run_index_cannot_repaint_an_earlier_run() {
    // Run data with fewer starts than runs is malformed, not truncated:
    // taking the missing start as 0 would let the tail spray its colour
    // over every triangle an earlier run had already claimed.
    let mut palette = Palette::new();
    palette.record(7, RED);
    palette.record(9, BLUE);
    let mut mesh = mesh_with_runs(4, &[(0, 7)]);
    mesh.run_original_id.push(9); // a second run with no start of its own
    assert_eq!(
        palette.paint_for(&mesh, GREEN),
        Some(RunPaint::Uniform(GREEN)),
        "malformed run data paints nothing per-face"
    );
}

#[test]
fn a_result_of_one_colour_collapses_to_a_uniform_paint() {
    let mut palette = Palette::new();
    palette.record(7, RED);
    palette.record(9, RED);
    let mesh = mesh_with_runs(4, &[(0, 7), (2, 9)]);
    assert_eq!(
        palette.paint_for(&mesh, GREEN),
        Some(RunPaint::Uniform(RED))
    );
}

#[test]
fn a_mesh_without_run_data_falls_back_to_the_callers_stand_in() {
    let mut palette = Palette::new();
    palette.record(7, BLUE);
    let mesh = mesh_with_runs(2, &[]);
    assert_eq!(
        palette.paint_for(&mesh, GREEN),
        Some(RunPaint::Uniform(GREEN))
    );
}

#[test]
fn an_empty_or_disabled_palette_paints_nothing() {
    let mesh = mesh_with_runs(2, &[(0, 7)]);
    assert_eq!(Palette::new().paint_for(&mesh, GREEN), None);
    let mut disabled = Palette::disabled();
    disabled.record(7, RED);
    assert!(disabled.is_empty());
    assert_eq!(disabled.paint_for(&mesh, GREEN), None);
}

#[test]
fn absorbing_a_palette_keeps_both_sides_ids() {
    let mut a = Palette::new();
    a.record(1, RED);
    let mut b = Palette::new();
    b.record(2, BLUE);
    a.absorb(b);
    let mesh = mesh_with_runs(2, &[(0, 1), (1, 2)]);
    assert_eq!(
        a.paint_for(&mesh, GREEN),
        Some(RunPaint::PerTriangle(vec![RED, BLUE]))
    );
    assert_eq!(a.first_recorded(), Some(RED), "the first operand recorded");
}

#[test]
fn vertex_colors_follow_the_split_meshs_corners() {
    // Two triangles, one vertex per corner (what `finish_mesh` leaves).
    let mesh = MeshGL {
        num_prop: 6,
        vert_properties: vec![0.0; 6 * 6],
        tri_verts: vec![0, 1, 2, 3, 4, 5],
        ..Default::default()
    };
    let buf = vertex_colors_for(&[RED, BLUE], &mesh).expect("buffer");
    assert_eq!(buf.len(), 6 * 4);
    assert_eq!(&buf[0..4], &RED);
    assert_eq!(&buf[8..12], &RED);
    assert_eq!(&buf[12..16], &BLUE);
    assert_eq!(&buf[20..24], &BLUE);
}

#[test]
fn the_finished_meshs_triangle_order_is_the_kernels() {
    // The whole mapping rests on one claim: `finish_mesh` (promote →
    // split → flat normals) leaves triangle *t* of the export as triangle
    // *t* of the finished mesh. Pin it on a real boolean result rather
    // than on hand-built index math — a reordering here would silently
    // scatter every colour.
    let mut palette = Palette::new();
    let a = tag_original(
        import_operand(&generate_box(2.0, 2.0, 2.0)).expect("a imports"),
        RED,
        &mut palette,
    );
    let b = tag_original(
        import_operand(&translated(&generate_box(2.0, 2.0, 2.0), 1.5, 0.0, 0.0))
            .expect("b imports"),
        BLUE,
        &mut palette,
    );
    let union = boolean_op(&a, &b, OpType::Add, BooleanOptions::default());
    let raw = union.get_mesh_gl(-1);
    let tris = match palette.paint_for(&raw, RED) {
        Some(RunPaint::PerTriangle(t)) => t,
        other => panic!("expected a two-colour union, got {other:?}"),
    };

    let finished = finish_mesh(raw.clone());
    let buf = vertex_colors_for(&tris, &finished).expect("a colour buffer");
    assert_eq!(
        finished.tri_verts.len() / 3,
        raw.tri_verts.len() / 3,
        "the split must not add or drop triangles"
    );
    // Every finished triangle must sit where the kernel's triangle of the
    // same index sat, and wear that triangle's colour.
    for t in 0..finished.tri_verts.len() / 3 {
        assert_eq!(
            centroid(&finished, t),
            centroid(&raw, t),
            "triangle {t} moved in the split"
        );
        let v = finished.tri_verts[t * 3] as usize;
        assert_eq!(
            [buf[v * 4], buf[v * 4 + 1], buf[v * 4 + 2], buf[v * 4 + 3]],
            tris[t],
            "triangle {t} wears another triangle's colour"
        );
    }
}

/// Centroid of triangle `t`, rounded to the f32 grid both meshes share
/// (the split copies positions verbatim, so equality is exact).
fn centroid(mesh: &MeshGL, t: usize) -> [f32; 3] {
    let stride = mesh.num_prop as usize;
    let pos = |v: u32| {
        let o = v as usize * stride;
        [
            mesh.vert_properties[o],
            mesh.vert_properties[o + 1],
            mesh.vert_properties[o + 2],
        ]
    };
    let (a, b, c) = (
        pos(mesh.tri_verts[t * 3]),
        pos(mesh.tri_verts[t * 3 + 1]),
        pos(mesh.tri_verts[t * 3 + 2]),
    );
    [
        (a[0] + b[0] + c[0]) / 3.0,
        (a[1] + b[1] + c[1]) / 3.0,
        (a[2] + b[2] + c[2]) / 3.0,
    ]
}

#[test]
fn a_triangle_count_mismatch_yields_no_buffer() {
    let mesh = MeshGL {
        num_prop: 6,
        vert_properties: vec![0.0; 6 * 6],
        tri_verts: vec![0, 1, 2, 3, 4, 5],
        ..Default::default()
    };
    // A split that dropped a triangle would leave the tail mis-painted;
    // a uniform tint is the honest answer.
    assert!(vertex_colors_for(&[RED], &mesh).is_none());
}

#[test]
fn an_inherited_operand_colour_resolves_to_the_default() {
    let body = Body::from_mesh(Arc::new(MeshGL::default())).with_color(INHERIT_COLOR);
    assert_eq!(operand_color(&body), DEFAULT_GEOMETRY_COLOR);
    let colored = Body::from_mesh(Arc::new(MeshGL::default())).with_color(BLUE);
    assert_eq!(operand_color(&colored), BLUE);
}

#[test]
fn the_node_color_override_reads_as_unset_by_default() {
    // Through the node, since `node_color_override` needs an `EvalCtx`:
    // an untouched Boolean lets the operands' colours through (proved by
    // `combine_paints_each_part_with_its_own_source_color`), and the unit
    // check here is that the sentinel itself is not an override.
    assert!(node_color_override_of(INHERIT_COLOR).is_none());
    assert_eq!(node_color_override_of(GREEN), Some(GREEN));
}

/// [`node_color_override`] against a one-node context carrying `color`.
fn node_color_override_of(color: [f32; 4]) -> Option<[f32; 4]> {
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::registry::{EvalCtx, NodeInputs, NodeProperties};
    let inst = NodeInstance::new(NodeId(1), "Boolean", [0.0, 0.0]);
    let mut props = NodeProperties::default();
    props.insert("color", PortValue::Color(color));
    let inputs = NodeInputs::default();
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    node_color_override(&ctx)
}

