//! MatterControl `.mcx` scene import.
//!
//! An `.mcx` is a zip archive: a `scene.mcx` JSON document describing an
//! `Object3D` tree, plus an `Assets/` folder holding the referenced mesh
//! files (content-hash-named `.stl`s in every file MatterControl writes).
//!
//! AtomArtist has no equivalent of MatterControl's implicit CSG tree, so
//! import **flattens** the scene to its rendered surfaces: we walk the
//! tree and collect the *shallowest* nodes that carry a `MeshPath` —
//! MatterControl caches each operation's computed result mesh there, so
//! the shallowest mesh in a branch is that branch's final geometry and
//! everything below it is source data (paths, images, CSG operands).
//! Subtrees whose root says `"Visible": false` are skipped outright
//! (that's how MatterControl hides `OperationSourceObject3D` operands).
//!
//! ## Transforms
//!
//! Each node's `Matrix` is 16 floats serialized as a *string* (a
//! MatterControl quirk). MatterControl uses row-vector math with
//! translation in elements 12..15 — memory-layout-identical to a
//! column-major column-vector matrix, so we can interpret the floats
//! verbatim and accumulate `world = parent * local` on the way down.
//! The accumulated world matrix rides along on each [`McxPart`] so the
//! caller can bake it into a MeshNode's `matrix` property.

use std::io::{Cursor, Read, Seek};

use manifold_rust::types::MeshGL;
use serde_json::Value;
use zip::ZipArchive;

use crate::graph::node::identity_matrix;

/// One flattened visible surface from the scene: decoded triangle mesh,
/// accumulated world transform (column-major), optional `#RRGGBB(AA)`
/// color, and a user-facing name for labels.
pub struct McxPart {
    pub name: String,
    pub mesh: MeshGL,
    pub matrix: [f32; 16],
    pub color: Option<[f32; 4]>,
}

#[derive(Debug)]
pub enum McxError {
    /// Not a readable zip archive.
    Zip(String),
    /// No `scene.mcx` (or `*.mcx`) entry inside the archive.
    MissingScene,
    /// The scene JSON failed to parse.
    Json(String),
    /// The scene referenced meshes but none could be decoded.
    NoMeshes,
}

impl std::fmt::Display for McxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McxError::Zip(e) => write!(f, "MCX zip: {e}"),
            McxError::MissingScene => write!(f, "MCX has no scene.mcx entry"),
            McxError::Json(e) => write!(f, "MCX scene JSON: {e}"),
            McxError::NoMeshes => write!(f, "MCX scene contains no visible meshes"),
        }
    }
}

impl std::error::Error for McxError {}

/// Import an `.mcx` byte buffer. Returns every visible surface with its
/// world transform; assets that fail to decode are skipped with a
/// warning entry pushed to `warnings` rather than failing the whole
/// import (MatterControl archives in the wild carry stray PNGs and
/// occasionally truncated meshes).
pub fn import_mcx(bytes: &[u8], warnings: &mut Vec<String>) -> Result<Vec<McxPart>, McxError> {
    let mut zip =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| McxError::Zip(e.to_string()))?;

    let scene_name = find_scene_entry(&mut zip).ok_or(McxError::MissingScene)?;
    let scene: Value = {
        let mut entry = zip
            .by_name(&scene_name)
            .map_err(|e| McxError::Zip(e.to_string()))?;
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|e| McxError::Zip(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| McxError::Json(e.to_string()))?
    };

    let mut parts = Vec::new();
    collect_parts(&scene, identity_matrix(), &mut zip, &mut parts, warnings);
    if parts.is_empty() {
        return Err(McxError::NoMeshes);
    }
    Ok(parts)
}

/// MatterControl writes `scene.mcx`; fall back to any root-level `.mcx`
/// entry for older archives.
fn find_scene_entry<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Option<String> {
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    if names.iter().any(|n| n == "scene.mcx") {
        return Some("scene.mcx".to_string());
    }
    names
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(".mcx") && !n.contains('/'))
}

/// Depth-first walk implementing the shallowest-`MeshPath`-wins rule.
fn collect_parts<R: Read + Seek>(
    node: &Value,
    parent: [f32; 16],
    zip: &mut ZipArchive<R>,
    parts: &mut Vec<McxPart>,
    warnings: &mut Vec<String>,
) {
    if node.get("Visible").and_then(Value::as_bool) == Some(false) {
        return;
    }
    let world = mat_mul(parent, parse_matrix(node.get("Matrix")));

    if let Some(mesh_path) = node.get("MeshPath").and_then(Value::as_str) {
        match load_asset_mesh(zip, mesh_path) {
            Ok(mesh) => {
                let name = node
                    .get("Name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(mesh_path)
                    .to_string();
                let color = node
                    .get("Color")
                    .and_then(Value::as_str)
                    .and_then(parse_hex_color);
                parts.push(McxPart {
                    name,
                    mesh,
                    matrix: world,
                    color,
                });
            }
            Err(e) => warnings.push(format!("mcx asset {mesh_path}: {e}")),
        }
        // The cached mesh already includes everything below this node.
        return;
    }

    if let Some(children) = node.get("Children").and_then(Value::as_array) {
        for child in children {
            collect_parts(child, world, zip, parts, warnings);
        }
    }
}

/// Fetch `Assets/<name>` from the archive and decode by extension.
fn load_asset_mesh<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    mesh_path: &str,
) -> Result<MeshGL, String> {
    // Scene entries reference bare filenames; the archive stores them
    // under `Assets/` (either separator flavour, depending on the OS
    // that wrote the file).
    let mut bytes = Vec::new();
    let candidates = [
        format!("Assets/{mesh_path}"),
        format!("Assets\\{mesh_path}"),
        mesh_path.to_string(),
    ];
    let mut found = false;
    for name in &candidates {
        if let Ok(mut entry) = zip.by_name(name) {
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            found = true;
            break;
        }
    }
    if !found {
        return Err("asset entry not found in archive".to_string());
    }
    let ext = mesh_path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "stl" => super::mesh_io::import_stl(&bytes).map_err(|e| e.to_string()),
        "obj" => super::mesh_obj::import_obj(&bytes).map_err(|e| e.to_string()),
        "3mf" => super::mesh_3mf::import_3mf(&bytes).map_err(|e| e.to_string()),
        other => Err(format!("unsupported asset extension .{other}")),
    }
}

/// Parse MatterControl's `Matrix` field: 16 comma-separated floats,
/// usually wrapped in a JSON *string* (`"[1.0,0.0,…]"`), occasionally a
/// real JSON array. Missing / malformed → identity, matching how
/// MatterControl treats nodes without a transform.
fn parse_matrix(value: Option<&Value>) -> [f32; 16] {
    let mut m = identity_matrix();
    let Some(value) = value else { return m };
    let floats: Vec<f32> = match value {
        Value::String(s) => s
            .trim_matches(|c| c == '[' || c == ']' || c == ' ')
            .split(',')
            .filter_map(|t| t.trim().parse::<f32>().ok())
            .collect(),
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect(),
        _ => return m,
    };
    if floats.len() == 16 {
        m.copy_from_slice(&floats);
    }
    m
}

/// `#RRGGBB` / `#RRGGBBAA` → linear-ish [r, g, b, a] in 0..=1. (The
/// renderer treats node colors as plain sRGB floats, same as the color
/// picker, so no gamma conversion here.)
fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let r = byte(0)? as f32 / 255.0;
    let g = byte(2)? as f32 / 255.0;
    let b = byte(4)? as f32 / 255.0;
    let a = if hex.len() == 8 {
        byte(6)? as f32 / 255.0
    } else {
        1.0
    };
    Some([r, g, b, a])
}

/// Column-major 4×4 multiply: `a * b` (apply `b` first, then `a`).
fn mat_mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut acc = 0.0;
            for k in 0..4 {
                acc += a[k * 4 + row] * b[col * 4 + k];
            }
            out[col * 4 + row] = acc;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{generate_box, mesh3d::num_tris};
    use crate::serialization::mesh_io::export_stl;
    use std::io::Write;

    /// Build a minimal in-memory .mcx: one visible mesh under a
    /// translated group, one hidden operand subtree, and a nested
    /// cached-result node whose children must be skipped.
    fn synthetic_mcx() -> Vec<u8> {
        let stl = export_stl(&generate_box(1.0, 1.0, 1.0));
        let scene = serde_json::json!({
            "ID": "root",
            "Name": "test.mcx",
            "Children": [
                {
                    "Name": "Group",
                    "Matrix": "[1,0,0,0, 0,1,0,0, 0,0,1,0, 10,20,30,1]",
                    "Children": [
                        {
                            "Name": "Cached Result",
                            "MeshPath": "AAA.stl",
                            "Color": "#FF0000",
                            // Source data below a cached mesh must be ignored.
                            "Children": [
                                { "Name": "Source Path", "MeshPath": "AAA.stl" }
                            ]
                        }
                    ]
                },
                {
                    "Name": "Hidden Operand",
                    "Visible": false,
                    "Children": [
                        { "Name": "Operand Mesh", "MeshPath": "AAA.stl" }
                    ]
                }
            ]
        });
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("scene.mcx", opts).unwrap();
            zip.write_all(scene.to_string().as_bytes()).unwrap();
            zip.start_file("Assets/AAA.stl", opts).unwrap();
            zip.write_all(&stl).unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn imports_shallowest_visible_meshes_with_world_transforms() {
        let mut warnings = Vec::new();
        let parts = import_mcx(&synthetic_mcx(), &mut warnings).unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        // Exactly one part: the cached result. Its child source and the
        // hidden operand subtree must not import.
        assert_eq!(parts.len(), 1);
        let p = &parts[0];
        assert_eq!(p.name, "Cached Result");
        assert_eq!(num_tris(&p.mesh), 12);
        // Group translation carried through the accumulated matrix.
        assert_eq!(&p.matrix[12..15], &[10.0, 20.0, 30.0]);
        // #FF0000 → opaque red.
        assert_eq!(p.color, Some([1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn matrix_string_and_missing_matrix_both_parse() {
        assert_eq!(parse_matrix(None), identity_matrix());
        let v = Value::String("[2,0,0,0,0,2,0,0,0,0,2,0,1,2,3,1]".into());
        let m = parse_matrix(Some(&v));
        assert_eq!(m[0], 2.0);
        assert_eq!(&m[12..15], &[1.0, 2.0, 3.0]);
        // Garbage falls back to identity rather than erroring.
        let bad = Value::String("not a matrix".into());
        assert_eq!(parse_matrix(Some(&bad)), identity_matrix());
    }

    #[test]
    fn real_zip_without_scene_entry_is_missing_scene() {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("readme.txt", opts).unwrap();
            zip.write_all(b"nope").unwrap();
            zip.finish().unwrap();
        }
        let mut warnings = Vec::new();
        assert!(matches!(
            import_mcx(&buf.into_inner(), &mut warnings),
            Err(McxError::MissingScene)
        ));
    }
}
