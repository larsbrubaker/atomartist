//! Mesh import / export — STL (binary read + write, ASCII read).
//!
//! Real-world STL files come in two flavours:
//!
//! ## Binary STL (per the standard)
//!
//!   - 80 bytes:           header (free-form, often a tool name)
//!   - 4 bytes (u32 LE):   triangle count
//!   - per triangle (50 bytes):
//!       - 12 bytes: face normal (3 × f32 LE)
//!       - 36 bytes: 3 vertices × (3 × f32 LE)
//!       - 2 bytes:  attribute byte count (almost always zero)
//!
//! Total file size: `84 + 50 * n_tris` bytes. AtomArtist's exporter emits
//! a fixed header `"AtomArtist binary STL                                                          "`.
//!
//! ## ASCII STL (read only)
//!
//! Plain-text equivalent — slower, larger, but the default for many
//! CAD exporters (Tinkercad, FreeCAD, SolidWorks "STL Ascii"). We
//! tolerate it on import so user-dragged files Just Work; on save we
//! always emit binary STL because it's smaller and lossless.
//!
//! ```text
//! solid model
//!   facet normal nx ny nz
//!     outer loop
//!       vertex x y z
//!       vertex x y z
//!       vertex x y z
//!     endloop
//!   endfacet
//!   ...
//! endsolid model
//! ```

use std::io::Write;

use manifold_rust::types::MeshGL;

use crate::geometry::mesh3d::{get_pos, num_tris, NUM_PROP, STRIDE};
#[cfg(test)]
use crate::geometry::mesh3d::num_verts;

/// Errors raised by the STL codec.
#[derive(Debug, Clone)]
pub enum StlError {
    Truncated,
    InvalidTriangleCount,
    /// ASCII STL parser hit a malformed line — surfaces the line number
    /// (1-based) and a short description for the user.
    AsciiParse { line: usize, reason: String },
    /// Neither binary nor recognisable ASCII STL.
    NotStl,
}

impl std::fmt::Display for StlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StlError::Truncated => write!(f, "STL data truncated"),
            StlError::InvalidTriangleCount => write!(f, "STL triangle count exceeds payload"),
            StlError::AsciiParse { line, reason } => {
                write!(f, "ASCII STL parse error at line {}: {}", line, reason)
            }
            StlError::NotStl => write!(f, "buffer is neither binary nor ASCII STL"),
        }
    }
}

impl std::error::Error for StlError {}

const HEADER: [u8; 80] = {
    let mut h = [b' '; 80];
    let banner = b"AtomArtist binary STL";
    let mut i = 0;
    while i < banner.len() {
        h[i] = banner[i];
        i += 1;
    }
    h
};

/// Encode a `MeshGL` to a binary STL byte buffer.
pub fn export_stl(mesh: &MeshGL) -> Vec<u8> {
    let n_tris = num_tris(mesh);
    let mut out = Vec::with_capacity(84 + n_tris * 50);
    out.extend_from_slice(&HEADER);
    out.extend_from_slice(&(n_tris as u32).to_le_bytes());

    let stride = mesh.num_prop as usize;
    for t in 0..n_tris {
        let i0 = mesh.tri_verts[t * 3] as usize;
        let i1 = mesh.tri_verts[t * 3 + 1] as usize;
        let i2 = mesh.tri_verts[t * 3 + 2] as usize;
        let p0 = get_pos(mesh, i0);
        let p1 = get_pos(mesh, i1);
        let p2 = get_pos(mesh, i2);

        let n = if stride >= 6 {
            // Average the three vertex normals if available.
            let n0 = [
                mesh.vert_properties[i0 * stride + 3],
                mesh.vert_properties[i0 * stride + 4],
                mesh.vert_properties[i0 * stride + 5],
            ];
            let n1 = [
                mesh.vert_properties[i1 * stride + 3],
                mesh.vert_properties[i1 * stride + 4],
                mesh.vert_properties[i1 * stride + 5],
            ];
            let n2 = [
                mesh.vert_properties[i2 * stride + 3],
                mesh.vert_properties[i2 * stride + 4],
                mesh.vert_properties[i2 * stride + 5],
            ];
            normalize3([
                (n0[0] + n1[0] + n2[0]) / 3.0,
                (n0[1] + n1[1] + n2[1]) / 3.0,
                (n0[2] + n1[2] + n2[2]) / 3.0,
            ])
        } else {
            face_normal(p0, p1, p2)
        };

        // Per-tri block: normal + 3 verts + 2-byte attr (zero).
        out.write_all(&n[0].to_le_bytes()).unwrap();
        out.write_all(&n[1].to_le_bytes()).unwrap();
        out.write_all(&n[2].to_le_bytes()).unwrap();
        for v in [&p0, &p1, &p2] {
            out.write_all(&v[0].to_le_bytes()).unwrap();
            out.write_all(&v[1].to_le_bytes()).unwrap();
            out.write_all(&v[2].to_le_bytes()).unwrap();
        }
        out.extend_from_slice(&[0u8, 0u8]);
    }

    out
}

/// Decode an STL buffer into a `MeshGL` with `num_prop = 6` (per-vertex
/// duplicated normals).
///
/// Auto-detects binary vs. ASCII. The disambiguation rule: a buffer
/// that starts with `b"solid"` AND whose total length matches the
/// binary formula `84 + 50 * n_tris` (where `n_tris` is read from the
/// header's u32 LE triangle count) is treated as binary. Anything else
/// starting with `solid` is parsed as ASCII. This survives the common
/// real-world case of binary STLs that mis-stamp their first five
/// bytes as `solid`.
pub fn import_stl(data: &[u8]) -> Result<MeshGL, StlError> {
    if data.len() < 5 {
        return Err(StlError::Truncated);
    }
    if &data[..5] == b"solid" && !looks_like_binary_with_solid_header(data) {
        return import_stl_ascii(data);
    }
    if data.len() < 84 {
        return Err(StlError::Truncated);
    }
    let n_tris = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
    if data.len() < 84 + 50 * n_tris {
        return Err(StlError::InvalidTriangleCount);
    }

    let mut vert_properties: Vec<f32> = Vec::with_capacity(n_tris * 3 * STRIDE);
    let mut tri_verts: Vec<u32> = Vec::with_capacity(n_tris * 3);
    let mut cursor = 84;
    for t in 0..n_tris {
        let nx = read_f32_le(&data[cursor..cursor + 4]);
        let ny = read_f32_le(&data[cursor + 4..cursor + 8]);
        let nz = read_f32_le(&data[cursor + 8..cursor + 12]);
        cursor += 12;
        for _ in 0..3 {
            let x = read_f32_le(&data[cursor..cursor + 4]);
            let y = read_f32_le(&data[cursor + 4..cursor + 8]);
            let z = read_f32_le(&data[cursor + 8..cursor + 12]);
            vert_properties.extend_from_slice(&[x, y, z, nx, ny, nz]);
            cursor += 12;
        }
        cursor += 2; // attribute bytes
        let base = (t * 3) as u32;
        tri_verts.extend_from_slice(&[base, base + 1, base + 2]);
    }

    Ok(MeshGL {
        num_prop: NUM_PROP,
        vert_properties,
        tri_verts,
        ..Default::default()
    })
}

/// A buffer that starts with `b"solid"` is binary iff its length
/// matches the `84 + 50 * n_tris` formula derived from the header's
/// claimed triangle count. Some binary exporters (notably MeshLab)
/// stamp `solid …` into the 80-byte header — we have to look past
/// the magic word to disambiguate.
fn looks_like_binary_with_solid_header(data: &[u8]) -> bool {
    if data.len() < 84 {
        return false;
    }
    let claimed = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
    data.len() == 84 + 50 * claimed
}

/// Parse an ASCII STL. Tolerates extra whitespace, leading/trailing
/// blanks, and `endsolid` without a trailing name. We do NOT require
/// the `solid <name>` header line — many emitters omit the name.
///
/// Normals come from `facet normal nx ny nz`. When the file's recorded
/// normal isn't unit-length (some exporters write zeros, or scale the
/// cross-product by triangle area), we recompute from vertex positions
/// so the imported mesh has well-defined lighting.
fn import_stl_ascii(data: &[u8]) -> Result<MeshGL, StlError> {
    let text = std::str::from_utf8(data)
        .map_err(|_| StlError::AsciiParse { line: 0, reason: "not valid UTF-8".into() })?;

    let mut vert_props: Vec<f32> = Vec::new();
    let mut tri_verts: Vec<u32> = Vec::new();

    // Per-facet accumulator. `normal` holds the value read from the
    // `facet normal …` line; `verts` collects the three vertices.
    let mut facet_normal: Option<[f32; 3]> = None;
    let mut facet_verts: Vec<[f32; 3]> = Vec::with_capacity(3);

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let mut tok = line.split_ascii_whitespace();
        let kind = tok.next().unwrap_or("");
        match kind {
            "solid" | "outer" | "endloop" | "endsolid" => {
                // Structural markers — no payload we need.
            }
            "facet" => {
                // `facet normal nx ny nz`
                let next = tok.next().unwrap_or("");
                if next != "normal" {
                    return Err(StlError::AsciiParse {
                        line: line_no,
                        reason: format!("expected `normal` after `facet`, got `{}`", next),
                    });
                }
                let parts: Vec<f32> = tok
                    .by_ref()
                    .take(3)
                    .map(|s| s.parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| StlError::AsciiParse {
                        line: line_no,
                        reason: "facet normal coordinates must be numeric".into(),
                    })?;
                if parts.len() != 3 {
                    return Err(StlError::AsciiParse {
                        line: line_no,
                        reason: "facet normal needs three components".into(),
                    });
                }
                facet_normal = Some([parts[0], parts[1], parts[2]]);
                facet_verts.clear();
            }
            "vertex" => {
                let parts: Vec<f32> = tok
                    .by_ref()
                    .take(3)
                    .map(|s| s.parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| StlError::AsciiParse {
                        line: line_no,
                        reason: "vertex coordinates must be numeric".into(),
                    })?;
                if parts.len() != 3 {
                    return Err(StlError::AsciiParse {
                        line: line_no,
                        reason: "vertex needs three components".into(),
                    });
                }
                facet_verts.push([parts[0], parts[1], parts[2]]);
            }
            "endfacet" => {
                if facet_verts.len() != 3 {
                    return Err(StlError::AsciiParse {
                        line: line_no,
                        reason: format!(
                            "expected exactly 3 vertices per facet, got {}",
                            facet_verts.len()
                        ),
                    });
                }
                // Trust the recorded normal when it's plausibly
                // unit-length; otherwise recompute from positions.
                // Threshold is wide because ASCII exports often write
                // `0 0 0` or area-weighted vectors.
                let recorded = facet_normal.unwrap_or([0.0, 0.0, 0.0]);
                let len_sq = recorded[0] * recorded[0]
                    + recorded[1] * recorded[1]
                    + recorded[2] * recorded[2];
                let n = if (0.5..=2.0).contains(&len_sq) {
                    normalize3(recorded)
                } else {
                    face_normal(facet_verts[0], facet_verts[1], facet_verts[2])
                };
                let base = (vert_props.len() / STRIDE) as u32;
                for v in &facet_verts {
                    vert_props.extend_from_slice(&[v[0], v[1], v[2], n[0], n[1], n[2]]);
                }
                tri_verts.extend_from_slice(&[base, base + 1, base + 2]);
                facet_verts.clear();
                facet_normal = None;
            }
            // Tolerate unknown keywords — some exporters add comment-
            // style metadata that isn't in the standard.
            _ => {}
        }
    }

    if tri_verts.is_empty() {
        return Err(StlError::NotStl);
    }
    Ok(MeshGL {
        num_prop: NUM_PROP,
        vert_properties: vert_props,
        tri_verts,
        ..Default::default()
    })
}

fn read_f32_le(b: &[u8]) -> f32 {
    f32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn face_normal(p0: [f32; 3], p1: [f32; 3], p2: [f32; 3]) -> [f32; 3] {
    let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
    let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
    normalize3([
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ])
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    [v[0] / l, v[1] / l, v[2] / l]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::generate_box;

    #[test]
    fn export_size_matches_spec() {
        let m = generate_box(1.0, 1.0, 1.0);
        let bytes = export_stl(&m);
        assert_eq!(bytes.len(), 84 + 50 * num_tris(&m));
    }

    #[test]
    fn round_trip_box_via_stl() {
        let m = generate_box(2.0, 3.0, 4.0);
        let bytes = export_stl(&m);
        let m2 = import_stl(&bytes).unwrap();
        assert_eq!(num_tris(&m2), num_tris(&m));
        // Every imported vertex appears as part of some triangle in the
        // original mesh's coordinate set. Quick sanity: bounding box matches.
        let bounds_a = crate::geometry::bounds(&m).unwrap();
        let bounds_b = crate::geometry::bounds(&m2).unwrap();
        for k in 0..3 {
            assert!((bounds_a.0[k] - bounds_b.0[k]).abs() < 1e-5);
            assert!((bounds_a.1[k] - bounds_b.1[k]).abs() < 1e-5);
        }
    }

    #[test]
    fn truncated_data_returns_error() {
        let r = import_stl(&[0u8; 10]);
        assert!(matches!(r, Err(StlError::Truncated)));
    }

    #[test]
    fn imported_normals_are_unit_length() {
        let bytes = export_stl(&generate_box(1.0, 1.0, 1.0));
        let m = import_stl(&bytes).unwrap();
        let stride = m.num_prop as usize;
        for i in 0..num_verts(&m) {
            let nx = m.vert_properties[i * stride + 3];
            let ny = m.vert_properties[i * stride + 4];
            let nz = m.vert_properties[i * stride + 5];
            let l = (nx * nx + ny * ny + nz * nz).sqrt();
            assert!((l - 1.0).abs() < 1e-5, "non-unit normal: {} {} {}", nx, ny, nz);
        }
    }

    /// Minimal ASCII STL — one triangle in the z=0 plane — exercises
    /// every keyword we tolerate.
    const ASCII_STL_ONE_TRIANGLE: &str = "solid demo
  facet normal 0 0 1
    outer loop
      vertex 0 0 0
      vertex 1 0 0
      vertex 0 1 0
    endloop
  endfacet
endsolid demo
";

    #[test]
    fn ascii_stl_parses_one_triangle() {
        let m = import_stl(ASCII_STL_ONE_TRIANGLE.as_bytes()).unwrap();
        assert_eq!(num_tris(&m), 1);
        assert_eq!(num_verts(&m), 3);
    }

    #[test]
    fn ascii_stl_recomputes_when_recorded_normal_is_zero() {
        // Exporters that write `normal 0 0 0` (some legacy Tinkercad
        // builds, hand-rolled scripts) shouldn't yield zero-length
        // normals in the imported mesh.
        let src = "solid demo
  facet normal 0 0 0
    outer loop
      vertex 0 0 0
      vertex 1 0 0
      vertex 0 1 0
    endloop
  endfacet
endsolid
";
        let m = import_stl(src.as_bytes()).unwrap();
        let stride = m.num_prop as usize;
        let nz = m.vert_properties[5];
        assert!(nz > 0.9, "recomputed normal should point +Z, got nz={nz}");
        let _ = stride;
    }

    #[test]
    fn ascii_stl_round_trips_through_binary_export() {
        // Importing an ASCII STL and re-exporting as binary should
        // preserve every triangle.
        let imported = import_stl(ASCII_STL_ONE_TRIANGLE.as_bytes()).unwrap();
        let bytes = export_stl(&imported);
        let reloaded = import_stl(&bytes).unwrap();
        assert_eq!(num_tris(&reloaded), num_tris(&imported));
    }

    #[test]
    fn binary_stl_with_solid_header_is_detected_by_size() {
        // Synthesise a tiny binary STL (12 tris from a unit cube) and
        // overwrite its 80-byte header with the magic word `solid`
        // followed by spaces — this is the failure mode MeshLab and a
        // few other binary exporters introduce. The size-formula check
        // should still steer us to the binary parser.
        let m = generate_box(1.0, 1.0, 1.0);
        let mut bytes = export_stl(&m);
        let banner = b"solid foo                                                                       ";
        bytes[..80].copy_from_slice(banner);
        let parsed = import_stl(&bytes).unwrap();
        assert_eq!(num_tris(&parsed), num_tris(&m));
    }
}
