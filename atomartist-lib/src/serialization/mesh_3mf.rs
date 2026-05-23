//! 3MF mesh import + export.
//!
//! 3MF is a ZIP-based container whose `3D/3dmodel.model` entry holds an
//! OPC-style XML mesh description. We support the minimal core spec:
//!
//! - `unit` attribute on `<model>` (default: millimeter; values are scaled
//!   to millimeter on read so AtomArtist sees one consistent unit).
//! - One or more `<object type="model">` entries each containing a
//!   `<mesh>` with `<vertices>` + `<triangles>`.
//! - `<build><item objectid="…"/></build>` selecting which objects ship
//!   in the file.
//!
//! Per-vertex colors, multi-material slices, and beam-lattice extensions
//! are ignored on read. On write we always emit a single object in
//! millimeter units — that's the canonical project shape AtomArtist
//! produces and it round-trips losslessly with itself.
//!
//! 3MF is the format AtomArtist writes when persisting a mesh asset.
//! Other formats (`.stl`, `.obj`) are import-only.

use std::io::{Cursor, Read, Write};

use manifold_rust::types::MeshGL;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::geometry::mesh3d::{get_pos, make_mesh, num_tris, num_verts, STRIDE};

/// Errors raised by the 3MF codec.
#[derive(Debug)]
pub enum ThreemfError {
    Zip(zip::result::ZipError),
    Io(std::io::Error),
    MissingModelEntry,
    InvalidXml(String),
    InvalidVertex,
    InvalidTriangle,
    EmptyMesh,
}

impl std::fmt::Display for ThreemfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThreemfError::Zip(e) => write!(f, "3MF zip error: {e}"),
            ThreemfError::Io(e) => write!(f, "3MF I/O error: {e}"),
            ThreemfError::MissingModelEntry => {
                write!(f, "3MF archive missing 3D/3dmodel.model entry")
            }
            ThreemfError::InvalidXml(s) => write!(f, "3MF XML error: {s}"),
            ThreemfError::InvalidVertex => write!(f, "3MF vertex element missing coordinates"),
            ThreemfError::InvalidTriangle => write!(f, "3MF triangle element missing indices"),
            ThreemfError::EmptyMesh => write!(f, "3MF archive contained no triangles"),
        }
    }
}

impl std::error::Error for ThreemfError {}

impl From<zip::result::ZipError> for ThreemfError {
    fn from(e: zip::result::ZipError) -> Self {
        ThreemfError::Zip(e)
    }
}

impl From<std::io::Error> for ThreemfError {
    fn from(e: std::io::Error) -> Self {
        ThreemfError::Io(e)
    }
}

const MODEL_ENTRY: &str = "3D/3dmodel.model";

/// Encode a `MeshGL` to a `.3mf` byte buffer with a single object in
/// millimeter units. Vertex normals are dropped — 3MF doesn't carry
/// per-vertex normals in the core spec; importers recompute them.
pub fn export_3mf(mesh: &MeshGL) -> Result<Vec<u8>, ThreemfError> {
    let buf = Cursor::new(Vec::<u8>::new());
    let mut zip = zip::ZipWriter::new(buf);
    let opts: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("[Content_Types].xml", opts)?;
    zip.write_all(CONTENT_TYPES_XML.as_bytes())?;

    zip.start_file("_rels/.rels", opts)?;
    zip.write_all(ROOT_RELS_XML.as_bytes())?;

    zip.start_file(MODEL_ENTRY, opts)?;
    let xml = build_model_xml(mesh);
    zip.write_all(xml.as_bytes())?;

    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}

/// Decode a `.3mf` byte buffer into a `MeshGL`. All `<object>` meshes
/// referenced by `<build>` items are concatenated; if there's no
/// `<build>` block, every model object is taken. Vertex coordinates are
/// scaled to millimeters based on the model's `unit` attribute.
pub fn import_3mf(data: &[u8]) -> Result<MeshGL, ThreemfError> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let mut model = archive
        .by_name(MODEL_ENTRY)
        .map_err(|_| ThreemfError::MissingModelEntry)?;
    let mut xml = String::new();
    model.read_to_string(&mut xml)?;
    drop(model);
    parse_model_xml(&xml)
}

fn build_model_xml(mesh: &MeshGL) -> String {
    let mut out = String::new();
    out.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <model unit=\"millimeter\" xml:lang=\"en-US\" \
         xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n\
         <resources>\n<object id=\"1\" type=\"model\"><mesh>\n<vertices>\n",
    );
    for v in 0..num_verts(mesh) {
        let p = get_pos(mesh, v);
        // 3MF requires `x y z` as decimals; format with enough precision to
        // round-trip f32 cleanly.
        out.push_str(&format!(
            "<vertex x=\"{:.7}\" y=\"{:.7}\" z=\"{:.7}\"/>\n",
            p[0], p[1], p[2]
        ));
    }
    out.push_str("</vertices>\n<triangles>\n");
    for t in 0..num_tris(mesh) {
        let i = t * 3;
        out.push_str(&format!(
            "<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n",
            mesh.tri_verts[i], mesh.tri_verts[i + 1], mesh.tri_verts[i + 2]
        ));
    }
    out.push_str("</triangles>\n</mesh></object>\n</resources>\n");
    out.push_str("<build><item objectid=\"1\"/></build>\n</model>\n");
    out
}

/// Per-object accumulator while walking the model XML.
#[derive(Default)]
struct ObjectMesh {
    id: String,
    positions: Vec<[f32; 3]>,
    tris: Vec<[u32; 3]>,
}

fn parse_model_xml(xml: &str) -> Result<MeshGL, ThreemfError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut unit_scale: f32 = 1.0; // default millimeter
    let mut objects: Vec<ObjectMesh> = Vec::new();
    let mut current: Option<ObjectMesh> = None;
    let mut build_items: Vec<String> = Vec::new();
    let mut saw_build_block = false;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| ThreemfError::InvalidXml(e.to_string()))?
        {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name();
                let local = name.as_ref();
                match local {
                    b"model" => {
                        for attr in e.attributes().with_checks(false).flatten() {
                            if attr.key.as_ref() == b"unit" {
                                let s = String::from_utf8_lossy(&attr.value).to_lowercase();
                                unit_scale = unit_scale_to_mm(&s);
                            }
                        }
                    }
                    b"object" => {
                        let mut obj = ObjectMesh::default();
                        for attr in e.attributes().with_checks(false).flatten() {
                            if attr.key.as_ref() == b"id" {
                                obj.id = String::from_utf8_lossy(&attr.value).into_owned();
                            }
                        }
                        current = Some(obj);
                    }
                    b"vertex" => {
                        if let Some(obj) = current.as_mut() {
                            let (mut x, mut y, mut z) = (None, None, None);
                            for attr in e.attributes().with_checks(false).flatten() {
                                match attr.key.as_ref() {
                                    b"x" => x = parse_attr_f32(&attr.value),
                                    b"y" => y = parse_attr_f32(&attr.value),
                                    b"z" => z = parse_attr_f32(&attr.value),
                                    _ => {}
                                }
                            }
                            match (x, y, z) {
                                (Some(x), Some(y), Some(z)) => obj.positions.push([x, y, z]),
                                _ => return Err(ThreemfError::InvalidVertex),
                            }
                        }
                    }
                    b"triangle" => {
                        if let Some(obj) = current.as_mut() {
                            let (mut a, mut b, mut c) = (None, None, None);
                            for attr in e.attributes().with_checks(false).flatten() {
                                match attr.key.as_ref() {
                                    b"v1" => a = parse_attr_u32(&attr.value),
                                    b"v2" => b = parse_attr_u32(&attr.value),
                                    b"v3" => c = parse_attr_u32(&attr.value),
                                    _ => {}
                                }
                            }
                            match (a, b, c) {
                                (Some(a), Some(b), Some(c)) => obj.tris.push([a, b, c]),
                                _ => return Err(ThreemfError::InvalidTriangle),
                            }
                        }
                    }
                    b"item" => {
                        if saw_build_block {
                            for attr in e.attributes().with_checks(false).flatten() {
                                if attr.key.as_ref() == b"objectid" {
                                    build_items
                                        .push(String::from_utf8_lossy(&attr.value).into_owned());
                                }
                            }
                        }
                    }
                    b"build" => saw_build_block = true,
                    _ => {}
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == b"object" {
                    if let Some(obj) = current.take() {
                        objects.push(obj);
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    let selected: Vec<&ObjectMesh> = if build_items.is_empty() {
        objects.iter().collect()
    } else {
        build_items
            .iter()
            .filter_map(|id| objects.iter().find(|o| &o.id == id))
            .collect()
    };
    if selected.is_empty() {
        return Err(ThreemfError::EmptyMesh);
    }

    // Concatenate every selected object into one MeshGL. Positions are
    // duplicated per-triangle so we can attach a face normal even though
    // 3MF doesn't carry one.
    let mut vert_props: Vec<f32> = Vec::new();
    let mut tri_verts: Vec<u32> = Vec::new();
    for obj in selected {
        for tri in &obj.tris {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;
            if i0 >= obj.positions.len()
                || i1 >= obj.positions.len()
                || i2 >= obj.positions.len()
            {
                return Err(ThreemfError::InvalidTriangle);
            }
            let p0 = scale3(obj.positions[i0], unit_scale);
            let p1 = scale3(obj.positions[i1], unit_scale);
            let p2 = scale3(obj.positions[i2], unit_scale);
            let n = face_normal(p0, p1, p2);
            let base = (vert_props.len() / STRIDE) as u32;
            vert_props.extend_from_slice(&[p0[0], p0[1], p0[2], n[0], n[1], n[2]]);
            vert_props.extend_from_slice(&[p1[0], p1[1], p1[2], n[0], n[1], n[2]]);
            vert_props.extend_from_slice(&[p2[0], p2[1], p2[2], n[0], n[1], n[2]]);
            tri_verts.extend_from_slice(&[base, base + 1, base + 2]);
        }
    }
    if tri_verts.is_empty() {
        return Err(ThreemfError::EmptyMesh);
    }
    Ok(make_mesh(vert_props, tri_verts))
}

fn parse_attr_f32(bytes: &[u8]) -> Option<f32> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn parse_attr_u32(bytes: &[u8]) -> Option<u32> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// 3MF declares `unit="millimeter"` by default; other allowed units are
/// micron, centimeter, inch, foot, meter. We normalise to mm so the rest
/// of AtomArtist sees one consistent unit.
fn unit_scale_to_mm(unit: &str) -> f32 {
    match unit {
        "micron" => 0.001,
        "millimeter" => 1.0,
        "centimeter" => 10.0,
        "inch" => 25.4,
        "foot" => 304.8,
        "meter" => 1000.0,
        _ => 1.0,
    }
}

fn scale3(p: [f32; 3], s: f32) -> [f32; 3] {
    [p[0] * s, p[1] * s, p[2] * s]
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

const CONTENT_TYPES_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n\
<Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n\
</Types>\n";

const ROOT_RELS_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n\
<Relationship Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\" \
Target=\"/3D/3dmodel.model\" Id=\"rel-1\"/>\n\
</Relationships>\n";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{bounds, generate_box};

    #[test]
    fn round_trip_box_via_3mf() {
        let mesh = generate_box(2.0, 3.0, 4.0);
        let bytes = export_3mf(&mesh).unwrap();
        let mesh2 = import_3mf(&bytes).unwrap();
        assert_eq!(num_tris(&mesh2), num_tris(&mesh));
        let a = bounds(&mesh).unwrap();
        let b = bounds(&mesh2).unwrap();
        for k in 0..3 {
            assert!((a.0[k] - b.0[k]).abs() < 1e-5);
            assert!((a.1[k] - b.1[k]).abs() < 1e-5);
        }
    }

    #[test]
    fn micron_units_scale_into_millimeters() {
        // Single triangle at (1000,0,0)/(0,1000,0)/(0,0,1000) micron should
        // come out at (1,0,0)/(0,1,0)/(0,0,1) mm.
        let xml = "<?xml version=\"1.0\"?>\n\
            <model unit=\"micron\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n\
            <resources><object id=\"1\" type=\"model\"><mesh>\n\
            <vertices>\
              <vertex x=\"1000\" y=\"0\" z=\"0\"/>\
              <vertex x=\"0\" y=\"1000\" z=\"0\"/>\
              <vertex x=\"0\" y=\"0\" z=\"1000\"/>\
            </vertices>\n\
            <triangles><triangle v1=\"0\" v2=\"1\" v3=\"2\"/></triangles>\n\
            </mesh></object></resources>\n\
            <build><item objectid=\"1\"/></build></model>";
        let mesh = parse_model_xml(xml).unwrap();
        let bounds = bounds(&mesh).unwrap();
        assert!((bounds.1[0] - 1.0).abs() < 1e-5, "micron should scale to 1 mm");
    }

    #[test]
    fn empty_object_returns_error() {
        let xml = "<?xml version=\"1.0\"?>\n\
            <model unit=\"millimeter\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n\
            <resources><object id=\"1\" type=\"model\"><mesh>\n\
            <vertices></vertices><triangles></triangles>\n\
            </mesh></object></resources></model>";
        let r = parse_model_xml(xml);
        assert!(matches!(r, Err(ThreemfError::EmptyMesh)));
    }

    #[test]
    fn missing_model_entry_returns_error() {
        // Hand-roll a zip with just an empty [Content_Types].xml — no
        // 3D/3dmodel.model entry. Importer should fail cleanly.
        let buf = Cursor::new(Vec::<u8>::new());
        let mut zip = zip::ZipWriter::new(buf);
        let opts: zip::write::SimpleFileOptions =
            zip::write::SimpleFileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(b"<Types/>").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let r = import_3mf(&bytes);
        assert!(matches!(r, Err(ThreemfError::MissingModelEntry)));
    }
}
