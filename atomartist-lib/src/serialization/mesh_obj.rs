//! Wavefront OBJ import — minimal, geometry-only.
//!
//! We parse `v` (vertex) and `f` (face) lines. `vn` and `vt` references in
//! face tokens (`v/vt/vn` form) are tolerated but only the position index
//! is used: imported normals are recomputed per-face so the mesh matches
//! AtomArtist's `num_prop = 6` shape. Materials, groups, smoothing, and
//! ASCII curve/surface primitives are ignored.
//!
//! Quads are triangulated as `(v0,v1,v2)` + `(v0,v2,v3)`; longer polygons
//! are fanned around `v0`. Negative indices (relative to the current
//! vertex count, as the OBJ spec allows) are resolved.
//!
//! OBJ is currently import-only — projects always re-export meshes as
//! `.3mf`, which preserves units, color, and multi-part data the OBJ
//! geometry layer can't carry.

use manifold_rust::types::MeshGL;

use crate::geometry::mesh3d::{make_mesh, STRIDE};

/// Errors raised by the OBJ importer.
#[derive(Debug, Clone)]
pub enum ObjError {
    InvalidUtf8,
    InvalidVertex(usize),
    InvalidFace(usize),
    EmptyMesh,
}

impl std::fmt::Display for ObjError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjError::InvalidUtf8 => write!(f, "OBJ data is not valid UTF-8"),
            ObjError::InvalidVertex(line) => write!(f, "invalid vertex on line {line}"),
            ObjError::InvalidFace(line) => write!(f, "invalid face on line {line}"),
            ObjError::EmptyMesh => write!(f, "OBJ contained no triangles"),
        }
    }
}

impl std::error::Error for ObjError {}

/// Decode an OBJ byte buffer into a `MeshGL` with `num_prop = 6` and
/// per-face flat normals duplicated on each vertex.
///
/// The importer is lenient: blank lines, comments (`# …`), and unknown
/// directives are skipped silently. Faces with fewer than 3 indices fail;
/// faces with more than 3 are triangulated as a fan around the first vertex.
pub fn import_obj(data: &[u8]) -> Result<MeshGL, ObjError> {
    let text = std::str::from_utf8(data).map_err(|_| ObjError::InvalidUtf8)?;

    // Source vertex positions, indexed 1-based per OBJ convention.
    let mut positions: Vec<[f32; 3]> = Vec::new();
    // Per-output-vertex flat buffer; positions get duplicated per-face so
    // we can attach a face normal without smoothing across edges.
    let mut vert_props: Vec<f32> = Vec::new();
    let mut tri_verts: Vec<u32> = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut tok = line.split_ascii_whitespace();
        let kind = match tok.next() {
            Some(k) => k,
            None => continue,
        };
        match kind {
            "v" => {
                let coords: Result<Vec<f32>, _> = tok.take(3).map(|s| s.parse::<f32>()).collect();
                let coords = coords.map_err(|_| ObjError::InvalidVertex(line_no))?;
                if coords.len() < 3 {
                    return Err(ObjError::InvalidVertex(line_no));
                }
                positions.push([coords[0], coords[1], coords[2]]);
            }
            "f" => {
                let indices: Vec<i64> = tok
                    .map(|tk| {
                        // `f v`, `f v/vt`, `f v/vt/vn`, `f v//vn` — first
                        // slash-segment is the position index.
                        let head = tk.split('/').next().unwrap_or("");
                        head.parse::<i64>().map_err(|_| ObjError::InvalidFace(line_no))
                    })
                    .collect::<Result<_, _>>()?;
                if indices.len() < 3 {
                    return Err(ObjError::InvalidFace(line_no));
                }
                // Resolve 1-based / negative indices into 0-based positions.
                let resolved: Vec<usize> = indices
                    .iter()
                    .map(|&i| resolve_index(i, positions.len()))
                    .collect::<Result<_, _>>()
                    .map_err(|_| ObjError::InvalidFace(line_no))?;

                // Fan-triangulate around vertex 0.
                for tri in 1..resolved.len() - 1 {
                    let i0 = resolved[0];
                    let i1 = resolved[tri];
                    let i2 = resolved[tri + 1];
                    let p0 = positions[i0];
                    let p1 = positions[i1];
                    let p2 = positions[i2];
                    let n = face_normal(p0, p1, p2);
                    let base = (vert_props.len() / STRIDE) as u32;
                    vert_props.extend_from_slice(&[p0[0], p0[1], p0[2], n[0], n[1], n[2]]);
                    vert_props.extend_from_slice(&[p1[0], p1[1], p1[2], n[0], n[1], n[2]]);
                    vert_props.extend_from_slice(&[p2[0], p2[1], p2[2], n[0], n[1], n[2]]);
                    tri_verts.extend_from_slice(&[base, base + 1, base + 2]);
                }
            }
            _ => {
                // Ignore vn, vt, g, s, o, mtllib, usemtl, …
            }
        }
    }

    if tri_verts.is_empty() {
        return Err(ObjError::EmptyMesh);
    }
    Ok(make_mesh(vert_props, tri_verts))
}

fn resolve_index(i: i64, count: usize) -> Result<usize, ()> {
    if i > 0 {
        let idx = (i as usize).checked_sub(1).ok_or(())?;
        if idx >= count {
            return Err(());
        }
        Ok(idx)
    } else if i < 0 {
        // Negative indices reference back from the current end.
        let off = (-i) as usize;
        count.checked_sub(off).ok_or(())
    } else {
        Err(())
    }
}

fn face_normal(p0: [f32; 3], p1: [f32; 3], p2: [f32; 3]) -> [f32; 3] {
    let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
    let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
    let n = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
    [n[0] / l, n[1] / l, n[2] / l]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::mesh3d::{get_pos, num_tris, num_verts};

    /// Unit tetrahedron with explicit triangles — exercises every parser
    /// branch (vertex, integer face, multiple-line file).
    const TETRAHEDRON_OBJ: &str = "\
# four vertices
v 0 0 0
v 1 0 0
v 0 1 0
v 0 0 1
f 1 2 3
f 1 3 4
f 1 4 2
f 2 4 3
";

    #[test]
    fn simple_tetrahedron_imports_with_four_triangles() {
        let mesh = import_obj(TETRAHEDRON_OBJ.as_bytes()).unwrap();
        assert_eq!(num_tris(&mesh), 4);
        // Per-face duplication: 4 tris × 3 verts = 12 imported verts.
        assert_eq!(num_verts(&mesh), 12);
        // Positions still inside the unit corner.
        for i in 0..num_verts(&mesh) {
            let p = get_pos(&mesh, i);
            assert!(p.iter().all(|c| (0.0..=1.0).contains(c)));
        }
    }

    #[test]
    fn quads_are_triangulated_via_fan() {
        // Square in z=0 plane as a single quad → 2 triangles.
        let src = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
        let mesh = import_obj(src.as_bytes()).unwrap();
        assert_eq!(num_tris(&mesh), 2);
    }

    #[test]
    fn face_tokens_with_slash_separators_are_parsed() {
        // Real-world OBJ exporters emit v/vt/vn even for trivial meshes.
        let src = "\
v 0 0 0
v 1 0 0
v 0 1 0
vn 0 0 1
f 1//1 2//1 3//1
";
        let mesh = import_obj(src.as_bytes()).unwrap();
        assert_eq!(num_tris(&mesh), 1);
    }

    #[test]
    fn negative_indices_reference_recent_vertices() {
        // `-1` is "the last vertex", `-2` is "second to last".
        let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n";
        let mesh = import_obj(src.as_bytes()).unwrap();
        assert_eq!(num_tris(&mesh), 1);
    }

    #[test]
    fn empty_obj_returns_error() {
        let r = import_obj(b"# nothing to see here\n");
        assert!(matches!(r, Err(ObjError::EmptyMesh)));
    }

    #[test]
    fn imported_normals_are_unit_length() {
        let mesh = import_obj(TETRAHEDRON_OBJ.as_bytes()).unwrap();
        let stride = mesh.num_prop as usize;
        for i in 0..num_verts(&mesh) {
            let n = [
                mesh.vert_properties[i * stride + 3],
                mesh.vert_properties[i * stride + 4],
                mesh.vert_properties[i * stride + 5],
            ];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((l - 1.0).abs() < 1e-5);
        }
    }
}
