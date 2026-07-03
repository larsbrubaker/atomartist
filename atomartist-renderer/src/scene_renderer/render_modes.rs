//! Pure mesh analysis for the non-shaded render modes — the CPU side of
//! MatterCAD's `RenderTypes` (Outlines / NonManifold / Polygons /
//! Overhang), ported to operate on a `manifold_rust::MeshGL`.
//!
//! Everything here is a pure function of the mesh (and, for overhang,
//! the body's world matrix): no GPU, no state. The renderer caches the
//! results per body so this only runs when the mesh or the selected mode
//! changes (see [`super::BodyGpu`]). Keeping it pure also makes it
//! unit-testable without a device.
//!
//! ## Edge overlays
//!
//! [`all_edges`], [`feature_edges`] and [`non_manifold_edges`] each
//! return a flat list of local-space line-segment endpoints (pairs of
//! `[x, y, z]`, two per segment) ready to hand to the gizmo line
//! pipeline. The renderer draws them over the shaded surface, so the
//! surface's depth hides occluded edges.
//!
//! ## Overhang
//!
//! [`overhang_colors`] returns a per-vertex RGBA buffer coloured by the
//! world-space Z of each vertex normal — cyan for up/vertical faces,
//! ramping to red as the face points downward (an overhang for FDM
//! printing). Fed through the renderer's per-vertex colour path.

use manifold_rust::types::MeshGL;

/// MatterCAD's `OutlineFeatureAngleRadians = Tau / 8` — an edge is a
/// feature edge when its two adjacent faces' normals differ by more
/// than 45°.
pub const OUTLINE_FEATURE_ANGLE_RAD: f32 = std::f32::consts::TAU / 8.0;

/// Which edge overlay a render mode wants drawn over the shaded
/// surface. Mirrors MatterCAD's per-`RenderTypes` edge classification
/// (`SceneEdgeShaderDataPlugin.BuildEdgeHintsByFace`): Outlines keeps
/// only feature edges, NonManifold keeps only the ≠2-face edges,
/// Polygons keeps every edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// Feature edges — the **Outlines** mode.
    Feature,
    /// Non-manifold + boundary edges — the **Non-Manifold** mode.
    NonManifold,
    /// Every geometric edge — the **Polygons** mode.
    All,
}

/// Build the local-space edge segment list for `kind` (pairs of
/// endpoints, two per segment). Thin dispatcher over the three edge
/// functions so callers only branch on the mode once.
pub fn edges_for(mesh: &MeshGL, kind: EdgeKind) -> Vec<[f32; 3]> {
    match kind {
        EdgeKind::Feature => feature_edges(mesh, OUTLINE_FEATURE_ANGLE_RAD),
        EdgeKind::NonManifold => non_manifold_edges(mesh),
        EdgeKind::All => all_edges(mesh),
    }
}

#[inline]
fn stride(mesh: &MeshGL) -> usize {
    mesh.num_prop as usize
}

#[inline]
fn pos(mesh: &MeshGL, vi: usize) -> [f32; 3] {
    let o = vi * stride(mesh);
    [
        mesh.vert_properties[o],
        mesh.vert_properties[o + 1],
        mesh.vert_properties[o + 2],
    ]
}

#[inline]
fn normal(mesh: &MeshGL, vi: usize) -> [f32; 3] {
    let o = vi * stride(mesh);
    [
        mesh.vert_properties[o + 3],
        mesh.vert_properties[o + 4],
        mesh.vert_properties[o + 5],
    ]
}

fn tri_count(mesh: &MeshGL) -> usize {
    mesh.tri_verts.len() / 3
}

fn tri(mesh: &MeshGL, t: usize) -> [usize; 3] {
    [
        mesh.tri_verts[t * 3] as usize,
        mesh.tri_verts[t * 3 + 1] as usize,
        mesh.tri_verts[t * 3 + 2] as usize,
    ]
}

/// Face normal of triangle `t` from its winding (not the stored vertex
/// normals — those are per-face-flat but we want the geometric normal
/// for angle comparisons). Zero-length (degenerate) triangles yield
/// `[0, 0, 0]`, which the callers treat as "no meaningful angle".
fn face_normal(mesh: &MeshGL, t: usize) -> [f32; 3] {
    let [a, b, c] = tri(mesh, t);
    let (pa, pb, pc) = (pos(mesh, a), pos(mesh, b), pos(mesh, c));
    let e1 = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
    let e2 = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
    let n = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len < 1e-20 {
        [0.0, 0.0, 0.0]
    } else {
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

/// Position key for topology reconstruction. AtomArtist meshes are
/// split-vertex (one vertex per triangle corner, for flat shading), so
/// the two faces meeting at a geometric edge reference *different*
/// vertex indices for the same position. We therefore key edges by the
/// vertices' POSITIONS, not their indices. Manifold emits bit-identical
/// coordinates for shared corners, so the raw float bits are an exact
/// key (`+ 0.0` folds `-0.0` into `0.0`).
type PosKey = [u32; 3];

#[inline]
fn pos_key(mesh: &MeshGL, vi: usize) -> PosKey {
    let p = pos(mesh, vi);
    [
        (p[0] + 0.0).to_bits(),
        (p[1] + 0.0).to_bits(),
        (p[2] + 0.0).to_bits(),
    ]
}

/// Undirected edge key: the two position keys sorted so `(a,b)` and
/// `(b,a)` collide.
#[inline]
fn edge_key(a: PosKey, b: PosKey) -> (PosKey, PosKey) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Value stored per geometric edge: the adjacent triangles plus one
/// representative `(v0, v1)` vertex-index pair for fetching endpoints.
struct EdgeInfo {
    faces: Vec<usize>,
    verts: (usize, usize),
}

/// Build the geometric-edge → adjacency map, keyed by position so
/// split-vertex meshes reconstruct their true topology.
fn edge_faces(mesh: &MeshGL) -> std::collections::HashMap<(PosKey, PosKey), EdgeInfo> {
    let mut map: std::collections::HashMap<(PosKey, PosKey), EdgeInfo> =
        std::collections::HashMap::with_capacity(tri_count(mesh) * 3);
    for t in 0..tri_count(mesh) {
        let [a, b, c] = tri(mesh, t);
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let key = edge_key(pos_key(mesh, u), pos_key(mesh, v));
            map.entry(key)
                .or_insert_with(|| EdgeInfo {
                    faces: Vec::new(),
                    verts: (u, v),
                })
                .faces
                .push(t);
        }
    }
    map
}

/// Push both endpoints of edge `(a, b)` onto `out`.
fn push_edge(out: &mut Vec<[f32; 3]>, mesh: &MeshGL, a: usize, b: usize) {
    out.push(pos(mesh, a));
    out.push(pos(mesh, b));
}

fn valid(mesh: &MeshGL) -> bool {
    mesh.num_prop >= 6 && !mesh.vert_properties.is_empty() && mesh.tri_verts.len() >= 3
}

/// Every geometric edge (deduplicated by position), for the **Polygons**
/// mode.
pub fn all_edges(mesh: &MeshGL) -> Vec<[f32; 3]> {
    if !valid(mesh) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for info in edge_faces(mesh).values() {
        let (u, v) = info.verts;
        push_edge(&mut out, mesh, u, v);
    }
    out
}

/// Feature edges — those whose two adjacent faces meet at more than
/// `angle_rad` — for the **Outlines** mode. Matches MatterCAD, which
/// only considers edges shared by exactly two faces.
pub fn feature_edges(mesh: &MeshGL, angle_rad: f32) -> Vec<[f32; 3]> {
    if !valid(mesh) {
        return Vec::new();
    }
    let cos_thresh = angle_rad.cos();
    let mut out = Vec::new();
    for info in edge_faces(mesh).values() {
        if info.faces.len() != 2 {
            continue;
        }
        let n0 = face_normal(mesh, info.faces[0]);
        let n1 = face_normal(mesh, info.faces[1]);
        let dot = n0[0] * n1[0] + n0[1] * n1[1] + n0[2] * n1[2];
        // angle > threshold  ⇔  cos(angle) < cos(threshold).
        if dot < cos_thresh {
            let (u, v) = info.verts;
            push_edge(&mut out, mesh, u, v);
        }
    }
    out
}

/// Non-manifold + boundary edges (any geometric edge not shared by
/// exactly two faces) for the **Non-Manifold** mode. Drawn in red by
/// the caller.
pub fn non_manifold_edges(mesh: &MeshGL) -> Vec<[f32; 3]> {
    if !valid(mesh) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for info in edge_faces(mesh).values() {
        if info.faces.len() != 2 {
            let (u, v) = info.verts;
            push_edge(&mut out, mesh, u, v);
        }
    }
    out
}

/// HSL → linear-ish sRGB. `h`, `s`, `l` in `[0, 1]`. Standard
/// formula; matches `ColorF.FromHSL` closely enough for the overhang
/// ramp (exact hue endpoints are what matter).
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h * 6.0).rem_euclid(6.0);
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r1 + m, g1 + m, b1 + m]
}

/// Per-vertex RGBA overhang colours, flat-packed `[r,g,b,a, …]` with one
/// entry per mesh vertex, for the **Overhang** mode.
///
/// Ports MatterCAD's `OverhangRender`: transform each vertex normal to
/// world space by `matrix`, take its Z, and map to an HSL hue — cyan
/// (`223°`) when the face points up or is vertical (`z ≥ 0`), ramping to
/// red (`5°`) as it points straight down (`z = -1`). AtomArtist meshes
/// carry flat per-face normals, so a per-vertex ramp is effectively
/// per-face, exactly like the reference.
pub fn overhang_colors(mesh: &MeshGL, matrix: &[f32; 16]) -> Vec<f32> {
    let n_verts = if valid(mesh) {
        mesh.vert_properties.len() / stride(mesh)
    } else {
        0
    };
    let mut out = Vec::with_capacity(n_verts * 4);

    // Cyan → red hue endpoints (MatterCAD: 223°/360° and 5°/360°).
    const CYAN: f32 = 223.0 / 360.0;
    const RED: f32 = 5.0 / 360.0;

    for v in 0..n_verts {
        let n = normal(mesh, v);
        // World Z of the normal: rotate by the matrix upper-3×3 (column
        // major). Only Z is needed and only its sign/scale matter after
        // renormalising, so compute the full rotated vector and take z.
        let wx = matrix[0] * n[0] + matrix[4] * n[1] + matrix[8] * n[2];
        let wy = matrix[1] * n[0] + matrix[5] * n[1] + matrix[9] * n[2];
        let wz = matrix[2] * n[0] + matrix[6] * n[1] + matrix[10] * n[2];
        let len = (wx * wx + wy * wy + wz * wz).sqrt().max(1e-12);
        let nz = wz / len;

        let hue = if nz < 0.0 {
            // Lerp cyan → red as nz goes 0 → -1.
            CYAN + (RED - CYAN) * (-nz)
        } else {
            CYAN
        };
        let rgb = hsl_to_rgb(hue, 0.99, 0.49);
        out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_rust::types::MeshGL;
    use std::sync::Arc;

    /// A unit cube centred at the origin, 8 verts / 12 tris, with flat
    /// per-face normals (num_prop = 6). Wound CCW outward.
    fn cube() -> MeshGL {
        // 8 corners.
        let c = [
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        // 6 faces × 2 tris, as corner indices, CCW outward.
        let faces = [
            ([0, 3, 2, 1], [0.0, 0.0, -1.0]), // bottom (-z)
            ([4, 5, 6, 7], [0.0, 0.0, 1.0]),  // top (+z)
            ([0, 1, 5, 4], [0.0, -1.0, 0.0]), // -y
            ([2, 3, 7, 6], [0.0, 1.0, 0.0]),  // +y
            ([1, 2, 6, 5], [1.0, 0.0, 0.0]),  // +x
            ([0, 4, 7, 3], [-1.0, 0.0, 0.0]), // -x
        ];
        // Build per-corner-per-face vertices so each face has its own
        // flat normal (24 verts), like AtomArtist's meshes.
        let mut vp: Vec<f32> = Vec::new();
        let mut tv: Vec<u32> = Vec::new();
        for (quad, n) in faces {
            let base = (vp.len() / 6) as u32;
            for &ci in &quad {
                vp.extend_from_slice(&c[ci]);
                vp.extend_from_slice(&n);
            }
            tv.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        MeshGL {
            num_prop: 6,
            vert_properties: vp,
            tri_verts: tv,
            ..Default::default()
        }
    }

    #[test]
    fn feature_edges_of_a_cube_are_its_twelve_hard_edges() {
        let m = cube();
        // Each of the 12 cube edges is a shared 2-face edge meeting at
        // 90° (> 45°), so all 12 are feature edges → 24 endpoints.
        // (The per-face-split verts mean each hard edge appears once per
        // adjacent face pair; the shared geometric edge is what counts.)
        // A triangulated cube has 18 geometric edges (12 hard cube edges
        // + 6 coplanar face diagonals). Feature detection keeps only the
        // 12 hard edges (the 6 diagonals meet at 0°). Non-manifold is 0
        // (closed, every edge shared by 2 faces).
        assert_eq!(all_edges(&m).len(), 18 * 2);
        assert_eq!(feature_edges(&m, OUTLINE_FEATURE_ANGLE_RAD).len(), 12 * 2);
        assert_eq!(non_manifold_edges(&m).len(), 0);
    }

    #[test]
    fn coplanar_faces_have_no_feature_edge() {
        // Two triangles forming a flat quad in the z=0 plane: their
        // shared edge is coplanar (0°) so it is NOT a feature edge.
        let vp = vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            1.0, 1.0, 0.0, 0.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 0.0, 0.0, 1.0, //
        ];
        let tv = vec![0, 1, 2, 0, 2, 3];
        let m = MeshGL {
            num_prop: 6,
            vert_properties: vp,
            tri_verts: tv,
            ..Default::default()
        };
        // The shared diagonal edge (0-2) is coplanar → no feature edge.
        // The 4 boundary edges have only 1 face → not considered.
        assert!(feature_edges(&m, OUTLINE_FEATURE_ANGLE_RAD).is_empty());
    }

    #[test]
    fn non_manifold_flags_boundary_edges() {
        // A single triangle: all 3 edges are boundary (1 face) → all
        // three are "non-manifold / open" → 3 edges × 2 endpoints.
        let vp = vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 0.0, 0.0, 1.0, //
        ];
        let m = MeshGL {
            num_prop: 6,
            vert_properties: vp,
            tri_verts: vec![0, 1, 2],
            ..Default::default()
        };
        assert_eq!(non_manifold_edges(&m).len(), 6);
        // A closed cube has NO non-manifold edges.
        assert!(non_manifold_edges(&cube()).is_empty());
    }

    #[test]
    fn all_edges_dedups_shared_edges() {
        // Flat quad: 2 tris share the diagonal → 5 unique edges, not 6.
        let vp = vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, //
            1.0, 1.0, 0.0, 0.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 0.0, 0.0, 1.0, //
        ];
        let m = MeshGL {
            num_prop: 6,
            vert_properties: vp,
            tri_verts: vec![0, 1, 2, 0, 2, 3],
            ..Default::default()
        };
        assert_eq!(all_edges(&m).len(), 5 * 2);
    }

    #[test]
    fn overhang_colors_ramp_from_cyan_up_to_red_down() {
        // Two verts: one normal up (+z), one straight down (-z).
        let vp = vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, // up
            0.0, 0.0, 0.0, 0.0, 0.0, -1.0, // down
        ];
        let m = MeshGL {
            num_prop: 6,
            vert_properties: vp,
            tri_verts: vec![0, 0, 1],
            ..Default::default()
        };
        let identity = {
            let mut mm = [0.0_f32; 16];
            mm[0] = 1.0;
            mm[5] = 1.0;
            mm[10] = 1.0;
            mm[15] = 1.0;
            mm
        };
        let cols = overhang_colors(&m, &identity);
        assert_eq!(cols.len(), 2 * 4);
        // Up-facing vertex → cyan: low red, high green+blue.
        let up = [cols[0], cols[1], cols[2]];
        assert!(up[0] < up[1] && up[0] < up[2], "up face should be cyan-ish, got {up:?}");
        // Down-facing vertex → red: high red, low green+blue.
        let down = [cols[4], cols[5], cols[6]];
        assert!(down[0] > down[1] && down[0] > down[2], "down face should be red-ish, got {down:?}");
    }

    // Keep `Arc` import meaningful for future callers that share meshes.
    #[allow(dead_code)]
    fn _shared(m: MeshGL) -> Arc<MeshGL> {
        Arc::new(m)
    }
}
