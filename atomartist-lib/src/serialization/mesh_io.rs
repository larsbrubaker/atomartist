//! Mesh import / export — binary STL (read + write).
//!
//! 3MF (a ZIP wrapping XML) and OBJ are deferred to Phase 8 alongside the
//! mesh-import nodes. STL is the most-used format and the simplest binary,
//! so it's the right minimum for Phase 6.
//!
//! Binary STL format (per the standard):
//!   - 80 bytes:           header (free-form, often a tool name)
//!   - 4 bytes (u32 LE):   triangle count
//!   - per triangle (50 bytes):
//!       - 12 bytes: face normal (3 × f32 LE)
//!       - 36 bytes: 3 vertices × (3 × f32 LE)
//!       - 2 bytes:  attribute byte count (almost always zero)
//!
//! Total file size: `84 + 50 * n_tris` bytes. AtomArtist's exporter emits
//! a fixed header `"AtomArtist binary STL                                                          "`.

use std::io::Write;

use manifold_rust::types::MeshGL;

use crate::geometry::mesh3d::{get_pos, num_tris, NUM_PROP, STRIDE};
#[cfg(test)]
use crate::geometry::mesh3d::num_verts;

/// Errors raised by the STL codec.
#[derive(Debug, Clone)]
pub enum StlError {
    Truncated,
    UnsupportedAscii,
    InvalidTriangleCount,
}

impl std::fmt::Display for StlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StlError::Truncated => write!(f, "STL data truncated"),
            StlError::UnsupportedAscii => write!(f, "ASCII STL not supported (use binary STL)"),
            StlError::InvalidTriangleCount => write!(f, "STL triangle count exceeds payload"),
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

/// Decode a binary STL into a `MeshGL` with `num_prop = 6` (per-vertex
/// duplicated normals). ASCII STL is rejected.
pub fn import_stl(data: &[u8]) -> Result<MeshGL, StlError> {
    if data.len() >= 5 && &data[..5] == b"solid" {
        // Could still be binary — some tools mis-tag headers. Disambiguate
        // by checking the file size: binary always equals 84 + 50*n_tris.
        if data.len() < 84 {
            return Err(StlError::UnsupportedAscii);
        }
        let claimed = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
        if data.len() != 84 + 50 * claimed {
            return Err(StlError::UnsupportedAscii);
        }
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
}
