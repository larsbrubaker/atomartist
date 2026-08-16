//! 3D mesh utilities built on `manifold_rust::types::MeshGL`.
//!
//! All meshes use `num_prop = 6` — six f32s per vertex: position xyz at
//! offsets 0,1,2 and normal at offsets 3,4,5. Right-handed coords, **Z-up**
//! (CAD/printer convention — bed lies in the XY plane), triangles wound
//! CCW when viewed from outside the surface (so face normals computed via
//! cross product point outward).

use std::sync::Arc;

use manifold_rust::types::MeshGL;

/// Number of f32 properties per vertex (xyz + nxnynz).
pub const NUM_PROP: u32 = 6;

/// Number of f32s consumed by one vertex.
pub const STRIDE: usize = NUM_PROP as usize;

/// Create a `MeshGL` from a flat list of vertex floats and triangle indices.
/// Caller is responsible for length consistency: `vert_properties.len()` must
/// be a multiple of `STRIDE` and `tri_verts.len()` a multiple of 3.
pub fn make_mesh(vert_properties: Vec<f32>, tri_verts: Vec<u32>) -> MeshGL {
    debug_assert!(vert_properties.len() % STRIDE == 0);
    debug_assert!(tri_verts.len() % 3 == 0);
    MeshGL {
        num_prop: NUM_PROP,
        vert_properties,
        tri_verts,
        ..Default::default()
    }
}

/// Number of vertices in a mesh.
pub fn num_verts(mesh: &MeshGL) -> usize {
    if mesh.num_prop == 0 {
        return 0;
    }
    mesh.vert_properties.len() / mesh.num_prop as usize
}

/// Number of triangles in a mesh.
pub fn num_tris(mesh: &MeshGL) -> usize {
    mesh.tri_verts.len() / 3
}

/// Get the position of vertex `i`.
pub fn get_pos(mesh: &MeshGL, i: usize) -> [f32; 3] {
    let off = i * mesh.num_prop as usize;
    [
        mesh.vert_properties[off],
        mesh.vert_properties[off + 1],
        mesh.vert_properties[off + 2],
    ]
}

/// Get the normal of vertex `i` (assumes `num_prop == 6`).
pub fn get_normal(mesh: &MeshGL, i: usize) -> [f32; 3] {
    let off = i * mesh.num_prop as usize;
    debug_assert!(mesh.num_prop >= 6, "expected num_prop >= 6 for normals");
    [
        mesh.vert_properties[off + 3],
        mesh.vert_properties[off + 4],
        mesh.vert_properties[off + 5],
    ]
}

/// Give every triangle corner its own vertex, so per-face normals can be
/// stored without neighbouring faces overwriting each other.
///
/// [`compute_flat_normals`] writes a face's normal into its three vertex
/// slots; where a vertex is shared between faces — which is how every solid
/// modeller, `manifold-rust` included, hands back a boolean result — only
/// the last face visited keeps its normal and the rest shade wrong. Splitting
/// first is the general fix: it costs vertices (3 per triangle) and buys
/// correct flat shading. Callers that want smooth shading should not use
/// either function.
///
/// Non-position properties are copied from the source vertex; positions and
/// winding are untouched, so the result is geometrically identical.
pub fn split_for_flat_normals(mesh: &MeshGL) -> MeshGL {
    let stride = mesh.num_prop as usize;
    if stride < 3 {
        return mesh.clone();
    }
    let n_verts = mesh.vert_properties.len() / stride;
    let mut verts: Vec<f32> = Vec::with_capacity(mesh.tri_verts.len() * stride);
    let mut tris: Vec<u32> = Vec::with_capacity(mesh.tri_verts.len());
    for &i in &mesh.tri_verts {
        let src = i as usize;
        if src >= n_verts {
            // Defensive: an out-of-range index would otherwise panic here.
            // Drop the corner's triangle rather than fabricate a vertex.
            continue;
        }
        tris.push((verts.len() / stride) as u32);
        verts.extend_from_slice(&mesh.vert_properties[src * stride..src * stride + stride]);
    }
    // A partially dropped triangle would leave a dangling corner count.
    let keep = tris.len() - tris.len() % 3;
    tris.truncate(keep);
    verts.truncate(keep * stride);
    MeshGL {
        num_prop: mesh.num_prop,
        vert_properties: verts,
        tri_verts: tris,
        ..Default::default()
    }
}

/// Compute flat per-triangle face normals and write them into the
/// num_prop=6 layout. Each triangle's three vertices share the same normal,
/// so the input mesh must already have one vertex per triangle-corner —
/// run [`split_for_flat_normals`] first if it might not.
pub fn compute_flat_normals(mesh: &mut MeshGL) {
    if mesh.num_prop != NUM_PROP {
        return;
    }
    let stride = STRIDE;
    for tri in 0..num_tris(mesh) {
        let i0 = mesh.tri_verts[tri * 3] as usize;
        let i1 = mesh.tri_verts[tri * 3 + 1] as usize;
        let i2 = mesh.tri_verts[tri * 3 + 2] as usize;
        let p0 = get_pos(mesh, i0);
        let p1 = get_pos(mesh, i1);
        let p2 = get_pos(mesh, i2);
        let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let n = cross(e1, e2);
        let nn = normalize(n);
        for &i in &[i0, i1, i2] {
            mesh.vert_properties[i * stride + 3] = nn[0];
            mesh.vert_properties[i * stride + 4] = nn[1];
            mesh.vert_properties[i * stride + 5] = nn[2];
        }
    }
}

/// Concatenate multiple meshes into a single one. Vertex properties are
/// appended; triangle indices are offset to point at the new vertex layout.
pub fn merge_meshes(parts: &[Arc<MeshGL>]) -> MeshGL {
    let mut out_verts: Vec<f32> = Vec::new();
    let mut out_tris: Vec<u32> = Vec::new();
    for m in parts {
        if m.num_prop != NUM_PROP {
            continue;
        }
        let vert_offset = (out_verts.len() / STRIDE) as u32;
        out_verts.extend_from_slice(&m.vert_properties);
        out_tris.extend(m.tri_verts.iter().map(|i| i + vert_offset));
    }
    make_mesh(out_verts, out_tris)
}

/// Apply a 4x4 column-major transform to mesh positions. Normals are
/// transformed by the inverse-transpose of the upper 3x3 (correct for
/// non-uniform scale; pure rotation/uniform scale would also work via the
/// matrix itself).
pub fn apply_transform(mesh: &MeshGL, m: &[f32; 16]) -> MeshGL {
    let stride = mesh.num_prop as usize;
    if stride == 0 {
        return mesh.clone();
    }
    let mut out = mesh.clone();

    let normal_mat = inverse_transpose_3x3(extract_3x3(m));

    for i in 0..num_verts(mesh) {
        let off = i * stride;
        let p = [
            mesh.vert_properties[off],
            mesh.vert_properties[off + 1],
            mesh.vert_properties[off + 2],
        ];
        let pt = transform_point(m, p);
        out.vert_properties[off] = pt[0];
        out.vert_properties[off + 1] = pt[1];
        out.vert_properties[off + 2] = pt[2];

        if stride >= 6 {
            let n = [
                mesh.vert_properties[off + 3],
                mesh.vert_properties[off + 4],
                mesh.vert_properties[off + 5],
            ];
            let nt = mat3_mul(normal_mat, n);
            let nn = normalize(nt);
            out.vert_properties[off + 3] = nn[0];
            out.vert_properties[off + 4] = nn[1];
            out.vert_properties[off + 5] = nn[2];
        }
    }
    out
}

/// Compute the axis-aligned bounding box (min, max). Returns `None` for an
/// empty mesh.
pub fn bounds(mesh: &MeshGL) -> Option<([f32; 3], [f32; 3])> {
    let n = num_verts(mesh);
    if n == 0 {
        return None;
    }
    let mut mn = get_pos(mesh, 0);
    let mut mx = mn;
    for i in 1..n {
        let p = get_pos(mesh, i);
        for k in 0..3 {
            if p[k] < mn[k] { mn[k] = p[k]; }
            if p[k] > mx[k] { mx[k] = p[k]; }
        }
    }
    Some((mn, mx))
}

// --- math helpers ----------------------------------------------------------

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len2 = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if len2 < 1e-20 {
        return [0.0, 0.0, 1.0];
    }
    let inv = 1.0 / len2.sqrt();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

fn transform_point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    // Column-major: m[col*4 + row]
    let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
    let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
    let z = m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14];
    let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
    if (w - 1.0).abs() < 1e-6 || w == 0.0 {
        [x, y, z]
    } else {
        [x / w, y / w, z / w]
    }
}

fn extract_3x3(m: &[f32; 16]) -> [f32; 9] {
    [m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10]]
}

fn mat3_mul(m: [f32; 9], v: [f32; 3]) -> [f32; 3] {
    // Column-major 3x3: m[col*3 + row].
    [
        m[0] * v[0] + m[3] * v[1] + m[6] * v[2],
        m[1] * v[0] + m[4] * v[1] + m[7] * v[2],
        m[2] * v[0] + m[5] * v[1] + m[8] * v[2],
    ]
}

fn inverse_transpose_3x3(m: [f32; 9]) -> [f32; 9] {
    // m is column-major: cols c0=(m[0..3]), c1=(m[3..6]), c2=(m[6..9]).
    // Adjugate transpose / det == inverse-transpose.
    let det =
        m[0] * (m[4] * m[8] - m[5] * m[7])
      - m[3] * (m[1] * m[8] - m[2] * m[7])
      + m[6] * (m[1] * m[5] - m[2] * m[4]);
    if det.abs() < 1e-20 {
        // Degenerate; return identity so the caller still gets unit normals.
        return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    }
    let inv_det = 1.0 / det;
    // cofactor matrix (already transposed → adjugate gives inverse*det)
    let c00 =  (m[4] * m[8] - m[5] * m[7]) * inv_det;
    let c01 = -(m[3] * m[8] - m[5] * m[6]) * inv_det;
    let c02 =  (m[3] * m[7] - m[4] * m[6]) * inv_det;
    let c10 = -(m[1] * m[8] - m[2] * m[7]) * inv_det;
    let c11 =  (m[0] * m[8] - m[2] * m[6]) * inv_det;
    let c12 = -(m[0] * m[7] - m[1] * m[6]) * inv_det;
    let c20 =  (m[1] * m[5] - m[2] * m[4]) * inv_det;
    let c21 = -(m[0] * m[5] - m[2] * m[3]) * inv_det;
    let c22 =  (m[0] * m[4] - m[1] * m[3]) * inv_det;
    // Inverse-transpose is the cofactor matrix laid out column-major.
    [c00, c10, c20, c01, c11, c21, c02, c12, c22]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_mesh_one_tri() -> MeshGL {
        // One triangle in the +Z plane with z=0; CCW from +Z gives normal +Z.
        let verts = vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
        ];
        make_mesh(verts, vec![0, 1, 2])
    }

    #[test]
    fn flat_normals_compute_unit_z_for_xy_triangle() {
        let mut m = unit_mesh_one_tri();
        compute_flat_normals(&mut m);
        for i in 0..3 {
            let n = get_normal(&m, i);
            assert!((n[0] - 0.0).abs() < 1e-6);
            assert!((n[1] - 0.0).abs() < 1e-6);
            assert!((n[2] - 1.0).abs() < 1e-6, "expected +Z, got {:?}", n);
        }
    }

    /// Two triangles sharing an edge but facing different ways: with shared
    /// vertices, `compute_flat_normals` can only satisfy one of them.
    fn folded_pair() -> MeshGL {
        let verts = vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, // 0
            1.0, 0.0, 0.0, 0.0, 0.0, 0.0, // 1
            0.0, 1.0, 0.0, 0.0, 0.0, 0.0, // 2
            0.0, 0.0, 1.0, 0.0, 0.0, 0.0, // 3
        ];
        // (0,1,2) lies in the XY plane; (0,2,3) lies in the YZ plane.
        make_mesh(verts, vec![0, 1, 2, 0, 2, 3])
    }

    #[test]
    fn split_for_flat_normals_gives_each_corner_its_own_vertex() {
        let m = folded_pair();
        let split = split_for_flat_normals(&m);
        assert_eq!(num_tris(&split), 2);
        assert_eq!(num_verts(&split), 6, "one vertex per triangle corner");
        assert_eq!(split.tri_verts, vec![0, 1, 2, 3, 4, 5]);
        // Positions are preserved corner for corner.
        for (new_i, &old_i) in m.tri_verts.iter().enumerate() {
            assert_eq!(get_pos(&split, new_i), get_pos(&m, old_i as usize));
        }
    }

    #[test]
    fn split_then_flat_normals_satisfies_every_triangle() {
        let mut shared = folded_pair();
        compute_flat_normals(&mut shared);
        // The shared-vertex mesh cannot satisfy both faces: vertices 0 and 2
        // belong to both, and the second triangle wrote last.
        let n0 = get_normal(&shared, 0);
        assert!(
            (n0[2] - 1.0).abs() > 1e-6,
            "fixture no longer demonstrates the shared-vertex clash: {:?}",
            n0
        );

        let mut split = split_for_flat_normals(&folded_pair());
        compute_flat_normals(&mut split);
        // Triangle 0 faces +Z, triangle 1 (0->2->3) faces +X.
        for i in 0..3 {
            let n = get_normal(&split, i);
            assert!((n[2] - 1.0).abs() < 1e-6, "tri 0 corner {}: {:?}", i, n);
        }
        for i in 3..6 {
            let n = get_normal(&split, i);
            assert!((n[0] - 1.0).abs() < 1e-6, "tri 1 corner {}: {:?}", i, n);
        }
    }

    #[test]
    fn merge_concatenates_and_offsets_indices() {
        let m1 = Arc::new(unit_mesh_one_tri());
        let m2 = Arc::new(unit_mesh_one_tri());
        let merged = merge_meshes(&[m1, m2]);
        assert_eq!(num_verts(&merged), 6);
        assert_eq!(num_tris(&merged), 2);
        assert_eq!(merged.tri_verts, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn translate_shifts_positions_and_keeps_normals() {
        let mut m = unit_mesh_one_tri();
        compute_flat_normals(&mut m);
        let translate_z5: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 5.0, 1.0,
        ];
        let t = apply_transform(&m, &translate_z5);
        for i in 0..3 {
            let p = get_pos(&t, i);
            let p0 = get_pos(&m, i);
            assert!((p[0] - p0[0]).abs() < 1e-6);
            assert!((p[1] - p0[1]).abs() < 1e-6);
            assert!((p[2] - (p0[2] + 5.0)).abs() < 1e-6);
        }
        // Normals unchanged by pure translation.
        let n = get_normal(&t, 0);
        assert!((n[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bounds_of_one_tri_is_correct() {
        let m = unit_mesh_one_tri();
        let (mn, mx) = bounds(&m).unwrap();
        assert_eq!(mn, [0.0, 0.0, 0.0]);
        assert_eq!(mx, [1.0, 1.0, 0.0]);
    }
}
