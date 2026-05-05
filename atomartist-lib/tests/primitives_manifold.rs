//! Ported from NodeDesigner's `tests/unit/primitives-manifold.test.ts`.
//!
//! Validates that every shipped 3D primitive produces a closed,
//! watertight mesh: after merging coincident vertices, every edge
//! appears in exactly 2 triangles. We use that as a structural check
//! AND round-trip through manifold-rust's `Manifold::from_mesh_gl` to
//! catch any topology issues the Boolean pipeline would reject.

use std::collections::HashMap;

use atomartist_lib::geometry::{
    generate_box, generate_cone, generate_cylinder, generate_pyramid, generate_sphere,
    generate_torus, generate_wedge, num_tris,
};
use manifold_rust::manifold::Manifold;
use manifold_rust::types::MeshGL;

/// Returns `Ok` when every edge of the merged mesh appears in exactly
/// two triangles (orientation-agnostic). Returns `Err` with a count of
/// edges that fail the rule.
fn check_manifold_edges(mesh: &MeshGL) -> Result<(), String> {
    if mesh.num_prop == 0 || mesh.vert_properties.is_empty() {
        return Err("empty mesh".into());
    }
    let stride = mesh.num_prop as usize;
    let n = mesh.vert_properties.len() / stride;

    // Vertex-merge by quantized position so per-face-flat-normal duplicate
    // verts fold back into one logical vertex.
    let scale = 1e5;
    let mut bucket: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let mut remap: Vec<u32> = Vec::with_capacity(n);
    let mut next_id: u32 = 0;
    for i in 0..n {
        let off = i * stride;
        let key = (
            (mesh.vert_properties[off] as f64 * scale).round() as i64,
            (mesh.vert_properties[off + 1] as f64 * scale).round() as i64,
            (mesh.vert_properties[off + 2] as f64 * scale).round() as i64,
        );
        let id = *bucket.entry(key).or_insert_with(|| {
            let v = next_id;
            next_id += 1;
            v
        });
        remap.push(id);
    }

    // Tally edges (a, b) with a < b so each undirected edge counts once
    // per oriented half-edge.
    let mut edge_use: HashMap<(u32, u32), usize> = HashMap::new();
    for tri in mesh.tri_verts.chunks_exact(3) {
        let v0 = remap[tri[0] as usize];
        let v1 = remap[tri[1] as usize];
        let v2 = remap[tri[2] as usize];
        // Skip degenerate triangles (often produced after merging).
        if v0 == v1 || v1 == v2 || v0 == v2 {
            continue;
        }
        for &(a, b) in &[(v0, v1), (v1, v2), (v2, v0)] {
            let key = if a < b { (a, b) } else { (b, a) };
            *edge_use.entry(key).or_insert(0) += 1;
        }
    }

    let bad = edge_use.values().filter(|&&n| n != 2).count();
    if bad == 0 {
        Ok(())
    } else {
        Err(format!(
            "{} of {} edges shared by ≠2 triangles",
            bad,
            edge_use.len()
        ))
    }
}

/// Stronger check: round-trip through manifold-rust. If `Manifold::from_mesh_gl`
/// succeeds with a non-empty result, the mesh passes Manifold's stricter
/// definition (orientability, no self-intersections, etc.).
fn check_manifold_roundtrip(mesh: &MeshGL) {
    // Strip per-face flat normals first — Manifold expects manifold
    // topology with shared verts at edges.
    let stripped = strip_to_positions_only(mesh);
    let m = Manifold::from_mesh_gl(&stripped);
    assert!(
        !m.is_empty(),
        "manifold-rust rejected the input as non-manifold"
    );
    assert!(
        m.num_tri() > 0,
        "manifold-rust produced 0 triangles after import"
    );
}

fn strip_to_positions_only(mesh: &MeshGL) -> MeshGL {
    let stride = mesh.num_prop.max(3) as usize;
    let n = mesh.vert_properties.len() / stride;
    let mut bucket: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let mut out_pos: Vec<f32> = Vec::new();
    let mut remap: Vec<u32> = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * stride;
        let x = mesh.vert_properties[off];
        let y = mesh.vert_properties[off + 1];
        let z = mesh.vert_properties[off + 2];
        let key = (
            (x as f64 * 1e5).round() as i64,
            (y as f64 * 1e5).round() as i64,
            (z as f64 * 1e5).round() as i64,
        );
        let id = *bucket.entry(key).or_insert_with(|| {
            let v = (out_pos.len() / 3) as u32;
            out_pos.extend_from_slice(&[x, y, z]);
            v
        });
        remap.push(id);
    }
    let new_tris: Vec<u32> = mesh.tri_verts.iter().map(|i| remap[*i as usize]).collect();
    let mut filtered: Vec<u32> = Vec::with_capacity(new_tris.len());
    for tri in new_tris.chunks_exact(3) {
        if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
            filtered.extend_from_slice(tri);
        }
    }
    MeshGL {
        num_prop: 3,
        vert_properties: out_pos,
        tri_verts: filtered,
        ..Default::default()
    }
}

#[test]
fn box_is_manifold() {
    let m = generate_box(2.0, 3.0, 4.0);
    check_manifold_edges(&m).expect("box edge check");
    assert!(num_tris(&m) >= 12);
    check_manifold_roundtrip(&m);
}

#[test]
fn cylinder_is_manifold_at_minimum_segments() {
    let m = generate_cylinder(2.0, 5.0, 3);
    check_manifold_edges(&m).expect("cylinder min-segments edge check");
    check_manifold_roundtrip(&m);
}

#[test]
fn cylinder_is_manifold_at_default_segments() {
    let m = generate_cylinder(2.0, 5.0, 32);
    check_manifold_edges(&m).expect("cylinder default-segments edge check");
    check_manifold_roundtrip(&m);
}

#[test]
fn sphere_is_manifold() {
    let m = generate_sphere(2.0, 16, 8);
    check_manifold_edges(&m).expect("sphere edge check");
    check_manifold_roundtrip(&m);
}

#[test]
fn cone_is_manifold() {
    let m = generate_cone(2.0, 5.0, 24);
    check_manifold_edges(&m).expect("cone edge check");
    check_manifold_roundtrip(&m);
}

#[test]
fn torus_is_manifold() {
    let m = generate_torus(5.0, 1.5, 16, 8);
    check_manifold_edges(&m).expect("torus edge check");
    check_manifold_roundtrip(&m);
}

#[test]
fn pyramid_is_manifold() {
    let m = generate_pyramid(4.0, 5.0, 4.0);
    check_manifold_edges(&m).expect("pyramid edge check");
    check_manifold_roundtrip(&m);
}

#[test]
fn wedge_is_manifold() {
    let m = generate_wedge(4.0, 3.0, 2.0);
    check_manifold_edges(&m).expect("wedge edge check");
    check_manifold_roundtrip(&m);
}
