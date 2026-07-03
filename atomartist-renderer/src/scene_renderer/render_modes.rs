//! Per-mesh edge analysis for the wireframe render modes — the CPU side
//! of MatterCAD's `RenderTypes` (Outlines / NonManifold / Polygons),
//! ported to operate on a `manifold_rust::MeshGL`.
//!
//! Unlike the first cut (which emitted a separate line-segment list per
//! mode), this produces **per-vertex barycentric + edge-hint data** that
//! the [`super::edge_overlay`] pass feeds to a shader. The shader
//! reconstructs the wireframe with screen-space derivatives
//! (`fwidth(barycentric)`), exactly like MatterCAD's `NodeDesignerScene.hlsl`
//! `WireframeEdgeFactors` — so edges are resolution-independent, anti-
//! aliased, and their alpha can follow the surface's own transparency.
//!
//! For each vertex the buffer carries 6 floats: `[bary.xyz, hint.xyz]`.
//! `bary` is the vertex's barycentric corner (`(1,0,0)`, `(0,1,0)` or
//! `(0,0,1)`); `hint` marks, for the vertex's triangle, which of its
//! three edges should be drawn (`1` = draw, `0` = skip). The three
//! corners of a triangle share the same `hint`. The mapping matches
//! MatterCAD's `GetFaceEdgeIndex`: `hint.x` is the edge opposite corner
//! 0 (between corners 1 and 2), `hint.y` opposite corner 1, `hint.z`
//! opposite corner 2 — aligning each hint component with the barycentric
//! component that goes to zero along that edge.
//!
//! Overhang is no longer here — it moved into the surface shaders
//! (computed live from the world normal), so this module is purely edge
//! topology now.

use manifold_rust::types::MeshGL;
use rustc_hash::{FxHashMap, FxHashSet};

/// MatterCAD's `OutlineFeatureAngleRadians = Tau / 8` — an edge is a
/// feature edge when its two adjacent faces' normals differ by more
/// than 45°.
pub const OUTLINE_FEATURE_ANGLE_RAD: f32 = std::f32::consts::TAU / 8.0;

/// Which edges a render mode wants drawn over the shaded surface.
/// Mirrors MatterCAD's per-`RenderTypes` classification
/// (`SceneEdgeShaderDataPlugin.BuildEdgeHintsByFace`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// Feature edges (adjacent faces > 45°) — the **Outlines** mode.
    Feature,
    /// Non-manifold + boundary edges (≠ 2 adjacent faces) — the
    /// **Non-Manifold** mode.
    NonManifold,
    /// Every geometric edge — the **Polygons** mode.
    All,
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

/// Geometric face normal of triangle `t` from its winding. Zero-length
/// (degenerate) triangles yield `[0, 0, 0]`, treated by the feature test
/// as "no meaningful angle".
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

/// Adjacent triangles for one geometric edge.
struct EdgeInfo {
    faces: Vec<usize>,
}

/// Build the geometric-edge → adjacency map, keyed by position so
/// split-vertex meshes reconstruct their true topology. Uses
/// `FxHashMap` (fast non-cryptographic hashing) because this runs on
/// the render thread whenever a mesh changes and the key is raw
/// position bits, not attacker-controlled.
fn edge_faces(mesh: &MeshGL) -> FxHashMap<(PosKey, PosKey), EdgeInfo> {
    let mut map: FxHashMap<(PosKey, PosKey), EdgeInfo> =
        FxHashMap::with_capacity_and_hasher(tri_count(mesh) * 3, Default::default());
    for t in 0..tri_count(mesh) {
        let [a, b, c] = tri(mesh, t);
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let key = edge_key(pos_key(mesh, u), pos_key(mesh, v));
            map.entry(key)
                .or_insert_with(|| EdgeInfo { faces: Vec::new() })
                .faces
                .push(t);
        }
    }
    map
}

fn valid(mesh: &MeshGL) -> bool {
    mesh.num_prop >= 6 && !mesh.vert_properties.is_empty() && mesh.tri_verts.len() >= 3
}

/// Per-triangle edge hints for `kind` — one `[hint.x, hint.y, hint.z]`
/// per triangle, marking which of the triangle's three edges should be
/// drawn (`1` = draw, `0` = skip). The surface shaders fold these into
/// the polygon pass: barycentric comes from the (de-indexed) vertex
/// index, and each hint component gates the edge that the matching
/// barycentric component vanishes along —
///
/// * `hint.x` ↔ edge opposite corner 0 (between corners 1 and 2),
/// * `hint.y` ↔ edge opposite corner 1,
/// * `hint.z` ↔ edge opposite corner 2,
///
/// matching MatterCAD's `GetFaceEdgeIndex`. The renderer replicates the
/// per-triangle hint across the triangle's three de-indexed corners.
///
/// * **All** (Polygons) needs no topology — every edge is drawn.
/// * **Feature** / **NonManifold** build the position-keyed adjacency
///   once and mark the qualifying edges.
pub fn edge_hints(mesh: &MeshGL, kind: EdgeKind) -> Vec<[f32; 3]> {
    if !valid(mesh) {
        return Vec::new();
    }
    let tris = tri_count(mesh);

    // Set of qualifying edge keys — `None` means "every edge qualifies"
    // (Polygons), so we skip the topology build entirely.
    let qualifying: Option<FxHashSet<(PosKey, PosKey)>> = match kind {
        EdgeKind::All => None,
        EdgeKind::Feature | EdgeKind::NonManifold => {
            let cos_thresh = OUTLINE_FEATURE_ANGLE_RAD.cos();
            let faces = edge_faces(mesh);
            let mut set: FxHashSet<(PosKey, PosKey)> =
                FxHashSet::with_capacity_and_hasher(faces.len(), Default::default());
            for (key, info) in &faces {
                let q = match kind {
                    // Feature: shared by exactly two faces meeting at
                    // more than the threshold angle (cos below cos θ).
                    EdgeKind::Feature => {
                        info.faces.len() == 2 && {
                            let n0 = face_normal(mesh, info.faces[0]);
                            let n1 = face_normal(mesh, info.faces[1]);
                            let dot = n0[0] * n1[0] + n0[1] * n1[1] + n0[2] * n1[2];
                            dot < cos_thresh
                        }
                    }
                    // Non-manifold / boundary: any edge not shared by
                    // exactly two faces.
                    EdgeKind::NonManifold => info.faces.len() != 2,
                    EdgeKind::All => true,
                };
                if q {
                    set.insert(*key);
                }
            }
            Some(set)
        }
    };

    let qualifies = |a: usize, b: usize| -> bool {
        match &qualifying {
            None => true,
            Some(set) => set.contains(&edge_key(pos_key(mesh, a), pos_key(mesh, b))),
        }
    };

    let mut out: Vec<[f32; 3]> = Vec::with_capacity(tris);
    for t in 0..tris {
        let [a, b, c] = tri(mesh, t);
        out.push([
            qualifies(b, c) as u32 as f32,
            qualifies(c, a) as u32 as f32,
            qualifies(a, b) as u32 as f32,
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_rust::types::MeshGL;

    /// A unit cube centred at the origin, split-vertex (24 verts / 12
    /// tris) with flat per-face normals — like AtomArtist's meshes.
    fn cube() -> MeshGL {
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
        let faces = [
            ([0, 3, 2, 1], [0.0, 0.0, -1.0]),
            ([4, 5, 6, 7], [0.0, 0.0, 1.0]),
            ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
            ([2, 3, 7, 6], [0.0, 1.0, 0.0]),
            ([1, 2, 6, 5], [1.0, 0.0, 0.0]),
            ([0, 4, 7, 3], [-1.0, 0.0, 0.0]),
        ];
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

    /// Count hint-flagged triangle-edges across the mesh. Each geometric
    /// edge shared by two triangles is flagged on both, so a "hard" cube
    /// edge contributes 2.
    fn drawn_edge_count(mesh: &MeshGL, kind: EdgeKind) -> usize {
        edge_hints(mesh, kind)
            .iter()
            .map(|h| (h[0] > 0.5) as usize + (h[1] > 0.5) as usize + (h[2] > 0.5) as usize)
            .sum()
    }

    #[test]
    fn feature_hints_flag_the_twelve_hard_cube_edges() {
        let m = cube();
        // 18 geometric edges (12 hard + 6 coplanar diagonals). Feature
        // keeps the 12 hard edges; each is shared by 2 triangle-corners
        // → 24 flagged triangle-edges.
        assert_eq!(drawn_edge_count(&m, EdgeKind::Feature), 12 * 2);
        // Polygons flags every triangle edge: 12 tris × 3 = 36.
        assert_eq!(drawn_edge_count(&m, EdgeKind::All), 12 * 3);
        // Closed cube → no non-manifold edges.
        assert_eq!(drawn_edge_count(&m, EdgeKind::NonManifold), 0);
    }

    #[test]
    fn edge_hints_have_one_entry_per_triangle() {
        let m = cube();
        assert_eq!(edge_hints(&m, EdgeKind::All).len(), tri_count(&m));
        // Polygons flags all three edges of every triangle.
        assert!(edge_hints(&m, EdgeKind::All)
            .iter()
            .all(|h| *h == [1.0, 1.0, 1.0]));
    }

    #[test]
    fn coplanar_quad_has_no_feature_edge_but_all_edges_present() {
        // Two coplanar tris forming a quad: the shared diagonal is not a
        // feature edge; the boundary edges have one face each so they're
        // not feature edges either → zero feature hints. Polygons flags
        // all 6 triangle-edges.
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
        assert_eq!(drawn_edge_count(&m, EdgeKind::Feature), 0);
        assert_eq!(drawn_edge_count(&m, EdgeKind::All), 6);
    }

    #[test]
    fn single_triangle_edges_are_all_non_manifold() {
        // One triangle: all 3 edges are boundary (1 face) → non-manifold.
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
        assert_eq!(drawn_edge_count(&m, EdgeKind::NonManifold), 3);
    }
}
