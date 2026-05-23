//! Importer for NodeDesigner `.example/scene.json` files.
//!
//! NodeDesigner used a different graph schema:
//!   * Type ids are `"geometry/box"` etc. — we map to AtomArtist's
//!     short names (`"Box"`).
//!   * Edges (`"noodles"`) reference sockets by UUID. We resolve those
//!     against each node's `inputSockets` / `outputSockets` arrays to
//!     recover the socket name.
//!   * Property names are usually 1:1 (`width`, `height`, `depth`, ...).
//!
//! Result is fed straight into `crate::serialization::graph_json::load_graph`.
//! Unknown node types are skipped with a warning rather than aborted —
//! same forward-compat policy as the JSON loader.

use std::collections::HashMap;

use serde::Deserialize;

use crate::graph::graph::{Edge, Graph};
use crate::graph::node::PortValue;
use crate::registry::NodeRegistry;
use crate::serialization::graph_json::{JsonPortValue, LoadResult};

#[derive(Deserialize, Debug)]
struct NdScene {
    #[serde(default)]
    nodes: Vec<NdNode>,
    #[serde(default)]
    noodles: Vec<NdNoodle>,
}

#[derive(Deserialize, Debug)]
struct NdNode {
    /// Newer NodeDesigner schemas store ids as UUIDs (strings); older
    /// ones use u64. We deserialize as a flexible value and hash it.
    id: NdId,
    #[serde(rename = "type")]
    type_id: String,
    #[serde(default)]
    pos: NdPos,
    #[serde(default)]
    #[serde(rename = "inputSockets")]
    input_sockets: Vec<NdSocket>,
    #[serde(default)]
    #[serde(rename = "outputSockets")]
    output_sockets: Vec<NdSocket>,
    #[serde(default)]
    properties: HashMap<String, serde_json::Value>,
    /// Component instances reference an embedded sub-scene by name.
    /// We don't translate it on import — the parent example just
    /// gets the component-instance node skipped. Re-import as a
    /// runtime subgraph in a future pass.
    #[serde(default)]
    #[serde(rename = "componentName")]
    _component_name: Option<String>,
}

/// Either a numeric or string id.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(untagged)]
enum NdId {
    Num(u64),
    Str(String),
}

impl NdId {
    /// Stable numeric form — hashes the string variant deterministically.
    fn to_u64(&self) -> u64 {
        match self {
            NdId::Num(n) => *n,
            NdId::Str(s) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                s.hash(&mut h);
                // Avoid collision with low integer ids.
                h.finish() | (1u64 << 63)
            }
        }
    }
}

#[derive(Deserialize, Debug, Default)]
#[serde(untagged)]
enum NdPos {
    #[default]
    Empty,
    Tuple([f64; 2]),
    Object {
        #[serde(rename = "0")] x: f64,
        #[serde(rename = "1")] y: f64,
    },
}

impl NdPos {
    fn xy(&self) -> [f64; 2] {
        match self {
            NdPos::Empty => [0.0, 0.0],
            NdPos::Tuple(xy) => *xy,
            NdPos::Object { x, y } => [*x, *y],
        }
    }
}

#[derive(Deserialize, Debug)]
struct NdSocket {
    name: String,
    socket_id: String,
}

/// `noodles` is a flat array of 6-tuples: `[link_id, src_node, src_socket_uuid, dst_node, dst_socket_uuid, type]`.
/// Node ids may be u64 or UUID string (NdId handles both).
type NdNoodle = (
    serde_json::Value,
    NdId,
    String,
    NdId,
    String,
    serde_json::Value,
);

/// Map from NodeDesigner `geometry/foo` ids to AtomArtist `Foo` ids.
/// `None` means the type isn't supported yet — the node is dropped on
/// import with a warning.
fn nd_type_to_atomartist(nd: &str) -> Option<&'static str> {
    match nd {
        // 3D primitives
        "geometry/box"      => Some("Box"),
        "geometry/cylinder" => Some("Cylinder"),
        "geometry/sphere"   => Some("Sphere"),
        "geometry/cone"     => Some("Cone"),
        "geometry/torus"    => Some("Torus"),
        "geometry/pyramid"  => Some("Pyramid"),
        "geometry/wedge"    => Some("Wedge"),
        // 2D primitives
        "geometry/rectangle" => Some("Rectangle"),
        // Operations
        "geometry/transform"     => Some("Transform"),
        "geometry/combine"       => Some("Combine"),
        "geometry/extrude"       => Some("Extrude"),
        "geometry/inflate"       => Some("Inflate"),
        "geometry/stroke"        => Some("Stroke"),
        "geometry/smooth_paths"  => Some("SmoothPaths"),
        "geometry/align"         => Some("Align"),
        "geometry/fit-to-bounds" => Some("FitToBounds"),
        // I/O
        "graph/input"  => Some("GraphInput"),
        "graph/output" => Some("Output"),
        // Math / scalar
        "basic/const" => Some("NumberConst"),
        // Unknown / unsupported on this import pass — skipped.
        _ => None,
    }
}

/// Translate a NodeDesigner property value (raw JSON) into the closest
/// `JsonPortValue`. Numbers, bools, strings, hex-color strings, and
/// 16-element matrix arrays are recognized; everything else degrades to
/// `None`.
fn translate_property(v: &serde_json::Value) -> Option<JsonPortValue> {
    use serde_json::Value;
    match v {
        Value::Number(n) => n.as_f64().map(JsonPortValue::Number),
        Value::Bool(b)   => Some(JsonPortValue::Bool(*b)),
        Value::String(s) => {
            // Hex color (#RRGGBB or #RRGGBBAA)?
            if let Some(rgba) = parse_hex_color(s) {
                return Some(JsonPortValue::Color(rgba));
            }
            Some(JsonPortValue::StringVal(s.clone()))
        }
        Value::Array(a) => {
            if a.len() == 16 && a.iter().all(|x| x.is_number()) {
                let mut m = [0.0f32; 16];
                for (i, x) in a.iter().enumerate() {
                    m[i] = x.as_f64().unwrap_or(0.0) as f32;
                }
                Some(JsonPortValue::Matrix4x4(m))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
    let s = s.strip_prefix('#')?;
    let (r, g, b, a) = match s.len() {
        6 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            255u8,
        ),
        8 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            u8::from_str_radix(&s[6..8], 16).ok()?,
        ),
        _ => return None,
    };
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0])
}

/// Translate the NodeDesigner socket name into AtomArtist's. Most are
/// case-shifted equivalents (`"Width"` → `"width"`, `"Geometry"` →
/// `"out"` for outputs). Returns `None` when no AtomArtist analogue is
/// known — the importer drops the edge then.
fn map_socket_name(node_atomartist_type: &str, nd_name: &str, is_output: bool) -> Option<&'static str> {
    // Output side is almost always our "out", with a few node types
    // whose output names were promoted to readable identifiers in the
    // AtomArtist schema.
    if is_output {
        return match (node_atomartist_type, nd_name) {
            ("Extrude", _) => Some("Geometry"),
            _ => Some("out"),
        };
    }
    // Input side depends on the node.
    match (node_atomartist_type, nd_name) {
        // Output node has a Geometry input named "in" in our model.
        ("Output", "Geometry") | ("Output", "geometry") => Some("in"),
        // Transform / Inflate keep the legacy `input` name; Extrude was
        // promoted to NodeDesigner-style socket names so we pass each
        // NodeDesigner socket through to its AtomArtist counterpart.
        ("Transform", "Geometry") => Some("input"),
        ("Extrude",   "Paths")    => Some("Paths"),
        ("Extrude",   "Height")   => Some("Height"),
        ("Extrude",   "Radius")   => Some("Radius"),
        ("Extrude",   "Segments") => Some("Segments"),
        ("Extrude",   "Bottom Radius")   => Some("Bottom Radius"),
        ("Extrude",   "Bottom Segments") => Some("Bottom Segments"),
        ("Extrude",   "Color")    => Some("Color"),
        ("Extrude",   "Matrix")   => Some("Matrix"),
        ("Inflate",   "Paths")    => Some("input"),
        // Combine — NodeDesigner has "Geometry 1" .. "Geometry 8";
        // we use "input_1" .. "input_8".
        ("Combine", n) if n.starts_with("Geometry") => {
            // Trailing index parse from the name's numeric suffix.
            let idx = n.trim_start_matches("Geometry").trim();
            // Map to one of our known static slot names.
            match idx {
                "" | "1" => Some("input_1"),
                "2" => Some("input_2"),
                "3" => Some("input_3"),
                "4" => Some("input_4"),
                "5" => Some("input_5"),
                "6" => Some("input_6"),
                "7" => Some("input_7"),
                "8" => Some("input_8"),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Translate the NodeDesigner property name to AtomArtist's. Most are
/// already 1:1 (lowercase) — the few exceptions are listed here.
fn map_property_name(_atomartist_type: &str, nd_name: &str) -> Option<String> {
    // Translation: NodeDesigner uses "depth" in Box, but agg-gui's Box
    // also uses "depth" — same name. NodeDesigner sometimes uses "size",
    // we don't. Keep the table small for now.
    match nd_name {
        // Pass-through: most names align case-shift-free.
        "width" | "height" | "depth" | "radius" | "segments" | "tx" | "ty" | "tz"
        | "rx" | "ry" | "rz" | "sx" | "sy" | "sz" | "delta" | "value" | "name"
        | "outer_radius" | "inner_radius" | "points" | "segments_u" | "segments_v"
            => Some(nd_name.to_string()),
        // Skip props we don't model yet.
        "color" | "matrix" => None,
        _ => None,
    }
}

/// Convert a NodeDesigner scene into our `GraphFile`, then load via the
/// existing JSON loader. Returns the same `LoadResult` shape so callers
/// can collect warnings about skipped node types or sockets.
pub fn import_nodedesigner_scene_str(
    json: &str,
    registry: &NodeRegistry,
) -> Result<LoadResult, serde_json::Error> {
    let scene: NdScene = serde_json::from_str(json)?;
    Ok(import_scene(scene, registry))
}

fn import_scene(scene: NdScene, registry: &NodeRegistry) -> LoadResult {
    let mut warnings = Vec::new();
    let mut graph = Graph::new();

    // Map NodeDesigner node ids → freshly-allocated AtomArtist NodeIds,
    // and (id, socket_uuid) → (atomartist socket name, is_output) so
    // edges can resolve target sockets by uid later.
    let mut nd_to_am: HashMap<u64, crate::graph::node::NodeId> = HashMap::new();
    let mut node_atype: HashMap<u64, &'static str> = HashMap::new();
    let mut socket_lookup: HashMap<u64, HashMap<String, (String, bool)>> = HashMap::new();

    // Pass 1: create the live nodes.
    for nd_node in &scene.nodes {
        let nid = nd_node.id.to_u64();
        let am_type = match nd_type_to_atomartist(&nd_node.type_id) {
            Some(t) => t,
            None => {
                warnings.push(format!(
                    "skipping unsupported node type '{}'",
                    nd_node.type_id
                ));
                continue;
            }
        };
        let pos = nd_node.pos.xy();
        let new_id = match graph.add_new_node(am_type, [pos[0], -pos[1]], registry) {
            Ok(id) => id,
            Err(e) => {
                warnings.push(format!("failed to create node '{}': {}", am_type, e));
                continue;
            }
        };
        nd_to_am.insert(nid, new_id);
        node_atype.insert(nid, am_type);

        // Translate + apply properties.
        for (k, v) in &nd_node.properties {
            if let Some(name) = map_property_name(am_type, k) {
                if let Some(jv) = translate_property(v) {
                    let _ = graph.set_property(new_id, name, jv.into_port_value());
                }
            }
        }

        // Record the socket-UUID lookup for the edge pass.
        let mut sockets: HashMap<String, (String, bool)> = HashMap::new();
        for s in &nd_node.input_sockets {
            sockets.insert(s.socket_id.clone(), (s.name.clone(), false));
        }
        for s in &nd_node.output_sockets {
            sockets.insert(s.socket_id.clone(), (s.name.clone(), true));
        }
        socket_lookup.insert(nid, sockets);
    }

    // Pass 2: translate edges. NodeDesigner edges reference sockets by
    // UUID inside the original scene; we resolve UUID → NodeDesigner
    // socket name → AtomArtist socket name → uid on the live instance.
    for noodle in &scene.noodles {
        let (_link_id, src_node, src_uuid, dst_node, dst_uuid, _kind) = noodle;
        let src_nd_id = src_node.to_u64();
        let dst_nd_id = dst_node.to_u64();
        let src_id = match nd_to_am.get(&src_nd_id) {
            Some(n) => *n,
            None => continue,
        };
        let dst_id = match nd_to_am.get(&dst_nd_id) {
            Some(n) => *n,
            None => continue,
        };
        let src_type = *node_atype.get(&src_nd_id).unwrap_or(&"");
        let dst_type = *node_atype.get(&dst_nd_id).unwrap_or(&"");

        let src_socket_nd = match socket_lookup.get(&src_nd_id).and_then(|m| m.get(src_uuid)) {
            Some((name, _)) => name.clone(),
            None => continue,
        };
        let dst_socket_nd = match socket_lookup.get(&dst_nd_id).and_then(|m| m.get(dst_uuid)) {
            Some((name, _)) => name.clone(),
            None => continue,
        };
        let src_socket = match map_socket_name(src_type, &src_socket_nd, true) {
            Some(s) => s,
            None => {
                warnings.push(format!(
                    "edge source socket '{}.{}' has no AtomArtist analogue",
                    src_type, src_socket_nd
                ));
                continue;
            }
        };
        let dst_socket = match map_socket_name(dst_type, &dst_socket_nd, false) {
            Some(s) => s,
            None => {
                warnings.push(format!(
                    "edge target socket '{}.{}' has no AtomArtist analogue",
                    dst_type, dst_socket_nd
                ));
                continue;
            }
        };

        let src_uid = match graph.get(src_id).and_then(|n| n.output_by_name(src_socket)) {
            Some(s) => s.uid,
            None => {
                warnings.push(format!(
                    "source socket '{}' not found on imported {} node",
                    src_socket, src_type
                ));
                continue;
            }
        };
        let dst_uid = match graph.get(dst_id).and_then(|n| n.input_by_name(dst_socket)) {
            Some(s) => s.uid,
            None => {
                warnings.push(format!(
                    "target socket '{}' not found on imported {} node",
                    dst_socket, dst_type
                ));
                continue;
            }
        };

        if let Err(e) = graph.connect(Edge::new(src_id, src_uid, dst_id, dst_uid), registry) {
            warnings.push(format!("edge {}→{}: {}", src_type, dst_type, e));
        }
    }

    // Help the borrow checker: silence "unused" on PortValue import in
    // the conversion path above.
    let _ = PortValue::None;

    LoadResult { graph, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::executor::evaluate_all;
    use crate::graph::node::PortValue;
    use crate::nodes;

    // Gated on the `local_fds_examples` feature because the include_str!
    // path reaches into a sibling `MatterHackers/FDS` checkout that isn't
    // present in every developer's environment. Enable with
    // `cargo test -p atomartist-lib --features local_fds_examples` if you
    // have NodeDesigner cloned alongside this repo.
    #[cfg(feature = "local_fds_examples")]
    #[test]
    fn imports_simple_box_example() {
        let json = include_str!(
            "../../../../../MatterHackers/FDS/NodeDesigner/static/Examples/Basic/Simple Box.example/scene.json"
        );
        let mut reg = NodeRegistry::new();
        nodes::register_all(&mut reg);
        let result = import_nodedesigner_scene_str(json, &reg).unwrap();
        // Expect: a Box and an Output, connected.
        assert_eq!(result.graph.node_count(), 2, "warnings: {:?}", result.warnings);
        assert_eq!(result.graph.edge_count(), 1, "warnings: {:?}", result.warnings);

        // Evaluate the imported graph and verify the Output sees a Box mesh.
        let mut g = result.graph;
        evaluate_all(&mut g, &reg).unwrap();
        let output_node = g.nodes()
            .find(|n| n.type_id == "Output")
            .expect("Output node missing");
        let v = output_node.cached_outputs.get("out").cloned().unwrap();
        match v {
            PortValue::Geometry3d(m) => {
                assert!(m.tri_verts.len() / 3 >= 12, "Box should have at least 12 tris");
            }
            other => panic!("expected Geometry3d, got {:?}", other.socket_type()),
        }
    }
}
