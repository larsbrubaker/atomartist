//! 3D primitive generators producing `MeshGL` with flat normals.
//!
//! All primitives are centered at the origin and use `num_prop = 6`
//! (xyz + nxnynz). Triangles are wound CCW when viewed from outside; face
//! normals computed via cross product point outward.

use manifold_rust::types::MeshGL;

use crate::geometry::mesh3d::{make_mesh, NUM_PROP};

/// Box of total dimensions (width, height, depth) centered at origin.
/// Produces 24 vertices (6 faces × 4 unique) and 12 triangles.
pub fn generate_box(width: f64, height: f64, depth: f64) -> MeshGL {
    let w = (width * 0.5) as f32;
    let h = (height * 0.5) as f32;
    let d = (depth * 0.5) as f32;

    // Vertex layout per face: 4 verts CCW from outside, normal repeated 4×.
    // Order of faces: +X, -X, +Y, -Y, +Z, -Z.
    let face_data: [([f32; 3], [[f32; 3]; 4]); 6] = [
        // +X: back-bot, back-top, front-top, front-bot
        ([1.0, 0.0, 0.0], [[w, -h, -d], [w,  h, -d], [w,  h,  d], [w, -h,  d]]),
        // -X: front-bot, front-top, back-top, back-bot
        ([-1.0, 0.0, 0.0], [[-w, -h,  d], [-w,  h,  d], [-w,  h, -d], [-w, -h, -d]]),
        // +Y: back-left, front-left, front-right, back-right
        ([0.0, 1.0, 0.0], [[-w,  h, -d], [-w,  h,  d], [ w,  h,  d], [ w,  h, -d]]),
        // -Y: front-left, back-left, back-right, front-right
        ([0.0, -1.0, 0.0], [[-w, -h,  d], [-w, -h, -d], [ w, -h, -d], [ w, -h,  d]]),
        // +Z: bot-right, top-right, top-left, bot-left
        ([0.0, 0.0, 1.0], [[ w, -h,  d], [ w,  h,  d], [-w,  h,  d], [-w, -h,  d]]),
        // -Z: bot-left, top-left, top-right, bot-right
        ([0.0, 0.0, -1.0], [[-w, -h, -d], [-w,  h, -d], [ w,  h, -d], [ w, -h, -d]]),
    ];

    let mut verts: Vec<f32> = Vec::with_capacity(24 * NUM_PROP as usize);
    let mut tris: Vec<u32> = Vec::with_capacity(12 * 3);
    for (face_idx, (n, corners)) in face_data.iter().enumerate() {
        let base = (face_idx * 4) as u32;
        for c in corners {
            verts.extend_from_slice(c);
            verts.extend_from_slice(n);
        }
        // Two triangles per face: (0,1,2) and (0,2,3).
        tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    make_mesh(verts, tris)
}

/// Cylinder along the Y axis, centered at origin. Radius applied in XZ.
/// `segments` controls the side count. Top + bottom caps are flat-shaded.
pub fn generate_cylinder(radius: f64, height: f64, segments: u32) -> MeshGL {
    let segments = segments.max(3);
    let r = radius as f32;
    let h = (height * 0.5) as f32;

    let mut verts: Vec<f32> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();

    // Side strip — emit 4 verts per side quad so each quad has its own
    // outward-facing normal.
    for i in 0..segments {
        let a0 = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32) / (segments as f32) * std::f32::consts::TAU;
        let (x0, z0) = (a0.cos() * r, a0.sin() * r);
        let (x1, z1) = (a1.cos() * r, a1.sin() * r);
        // Side normal at midpoint of the segment (averaged across the quad).
        let nx = (a0.cos() + a1.cos()) * 0.5;
        let nz = (a0.sin() + a1.sin()) * 0.5;
        let n_len = (nx * nx + nz * nz).sqrt().max(1e-6);
        let n = [nx / n_len, 0.0, nz / n_len];

        let base = (verts.len() / NUM_PROP as usize) as u32;
        // bottom-back, top-back, top-front, bottom-front (CCW from outside)
        for &(x, y, z) in &[(x0, -h, z0), (x0, h, z0), (x1, h, z1), (x1, -h, z1)] {
            verts.extend_from_slice(&[x, y, z]);
            verts.extend_from_slice(&n);
        }
        tris.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // Top cap: triangle fan from center (normal +Y).
    let top_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, h, 0.0, 0.0, 1.0, 0.0]);
    let top_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, h, a.sin() * r, 0.0, 1.0, 0.0]);
    }
    for i in 0..segments {
        let v0 = top_center;
        let v1 = top_ring_start + i;
        let v2 = top_ring_start + ((i + 1) % segments);
        // CCW from above (+Y looking down): center → ring[i] → ring[i+1].
        // But +Y looking down has +X right, +Z up in the projected view,
        // so going around 0°→90°→180° increasing-angle CCW. That means
        // i+1 then i — we want (center, ring[i+1], ring[i]).
        tris.extend_from_slice(&[v0, v2, v1]);
    }

    // Bottom cap: triangle fan from center (normal -Y).
    let bot_center = (verts.len() / NUM_PROP as usize) as u32;
    verts.extend_from_slice(&[0.0, -h, 0.0, 0.0, -1.0, 0.0]);
    let bot_ring_start = (verts.len() / NUM_PROP as usize) as u32;
    for i in 0..segments {
        let a = (i as f32) / (segments as f32) * std::f32::consts::TAU;
        verts.extend_from_slice(&[a.cos() * r, -h, a.sin() * r, 0.0, -1.0, 0.0]);
    }
    for i in 0..segments {
        let v0 = bot_center;
        let v1 = bot_ring_start + i;
        let v2 = bot_ring_start + ((i + 1) % segments);
        // CCW from below (looking +Y from -Y): center → ring[i] → ring[i+1].
        tris.extend_from_slice(&[v0, v1, v2]);
    }

    make_mesh(verts, tris)
}

/// UV sphere centered at origin. `segments_u` is the longitudinal count
/// (around Y), `segments_v` the latitudinal count (from south to north
/// pole). Smooth-shaded — the normal at each vertex is its outward
/// direction from the center.
pub fn generate_sphere(radius: f64, segments_u: u32, segments_v: u32) -> MeshGL {
    let su = segments_u.max(3);
    let sv = segments_v.max(2);
    let r = radius as f32;

    let mut verts: Vec<f32> = Vec::with_capacity(((su + 1) * (sv + 1)) as usize * NUM_PROP as usize);
    let mut tris: Vec<u32> = Vec::new();

    // Vertices on a (su+1) x (sv+1) grid (closing seams duplicate).
    for j in 0..=sv {
        let v = j as f32 / sv as f32;
        let phi = v * std::f32::consts::PI; // 0..π (south → north)
        let sin_phi = phi.sin();
        let cos_phi = phi.cos();
        for i in 0..=su {
            let u = i as f32 / su as f32;
            let theta = u * std::f32::consts::TAU; // 0..2π around Y
            let x = sin_phi * theta.cos();
            let y = cos_phi;
            let z = sin_phi * theta.sin();
            verts.extend_from_slice(&[x * r, y * r, z * r, x, y, z]);
        }
    }

    let stride_u = su + 1;
    for j in 0..sv {
        for i in 0..su {
            let v00 = j * stride_u + i;
            let v10 = j * stride_u + (i + 1);
            let v01 = (j + 1) * stride_u + i;
            let v11 = (j + 1) * stride_u + (i + 1);
            // CCW from outside: looking from +radius direction back to origin.
            // For the sphere, going (j → j+1) is south-to-north (Y increases),
            // and (i → i+1) is increasing theta (counter-clockwise when
            // viewed from +Y). So CCW outward triangles for an outward
            // surface patch are (v00, v10, v11) and (v00, v11, v01).
            tris.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
        }
    }

    make_mesh(verts, tris)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::mesh3d::{compute_flat_normals, get_normal, get_pos, num_tris, num_verts};

    #[test]
    fn box_has_24_verts_and_12_tris() {
        let m = generate_box(1.0, 1.0, 1.0);
        assert_eq!(num_verts(&m), 24);
        assert_eq!(num_tris(&m), 12);
    }

    #[test]
    fn box_normals_are_unit_length_and_axis_aligned() {
        let m = generate_box(1.0, 1.0, 1.0);
        for i in 0..num_verts(&m) {
            let n = get_normal(&m, i);
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "vert {} normal not unit: {:?}", i, n);
            // Each component should be 0 or ±1 (axis-aligned faces)
            for k in 0..3 {
                let abs = n[k].abs();
                assert!(abs < 1e-5 || (abs - 1.0).abs() < 1e-5,
                        "non-axis component on box normal: {:?}", n);
            }
        }
    }

    #[test]
    fn box_positions_lie_on_extents() {
        let m = generate_box(2.0, 4.0, 6.0);
        for i in 0..num_verts(&m) {
            let p = get_pos(&m, i);
            assert!((p[0].abs() - 1.0).abs() < 1e-5);
            assert!((p[1].abs() - 2.0).abs() < 1e-5);
            assert!((p[2].abs() - 3.0).abs() < 1e-5);
        }
    }

    #[test]
    fn cylinder_has_correct_vertex_count() {
        let m = generate_cylinder(1.0, 2.0, 8);
        // 8 sides × 4 verts (= 32) + top center + bottom center + 8 top ring
        // + 8 bottom ring = 32 + 1 + 1 + 8 + 8 = 50.
        assert_eq!(num_verts(&m), 50);
        // 8 sides × 2 tris + 8 top fan + 8 bottom fan = 32.
        assert_eq!(num_tris(&m), 32);
    }

    #[test]
    fn sphere_has_outward_unit_normals() {
        let m = generate_sphere(2.0, 16, 8);
        for i in 0..num_verts(&m) {
            let p = get_pos(&m, i);
            let n = get_normal(&m, i);
            // Smooth normals point outward from origin.
            let pl = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((nl - 1.0).abs() < 1e-5, "non-unit normal: {:?}", n);
            // n parallel to p? Check via dot product against normalized p.
            let dot = (p[0] * n[0] + p[1] * n[1] + p[2] * n[2]) / pl.max(1e-6);
            assert!(dot > 0.999, "normal not outward; dot={}", dot);
        }
    }

    #[test]
    fn box_face_normals_match_after_recompute() {
        // Sanity: re-run flat normal computation and verify it matches
        // what we hand-wrote in the generator.
        let mut m = generate_box(3.0, 3.0, 3.0);
        let original = m.vert_properties.clone();
        compute_flat_normals(&mut m);
        // Component-wise comparison; allow tiny tolerance.
        assert_eq!(m.vert_properties.len(), original.len());
        for i in 0..m.vert_properties.len() {
            assert!((m.vert_properties[i] - original[i]).abs() < 1e-5,
                    "mismatch at {}: orig={} new={}",
                    i, original[i], m.vert_properties[i]);
        }
    }
}
