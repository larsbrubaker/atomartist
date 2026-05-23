//! Serialize and deserialize a `Graph` to and from JSON.
//!
//! ## Format
//!
//! Schema v2: per-node socket layouts and uid-keyed noodles. Every socket
//! carries a stable [`SocketUid`](crate::graph::socket::SocketUid) that
//! survives renames and reorder; noodles reference uids, not names.
//!
//! `PortValue` variants that wrap heap geometry (`Path2d`, `Geometry3d`)
//! are skipped — they're computed outputs that don't survive a round trip
//! and are recomputed by the executor on load. Property values that are
//! plain numbers, bools, strings, colors, or matrices are preserved.
//!
//! Forward compatibility: unknown node types are skipped with a warning
//! (returned in `LoadResult.warnings`), not a hard error.
//!
//! Backward compatibility: schema v1 is **not** loadable. The user
//! explicitly authorized the break during the Stage 1 engine refactor.
//! v1 saves can be opened by the previous binary if needed.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::graph::graph::{Noodle, NoodleEndpoint, Graph};
use crate::graph::node::{NodeId, NodeInstance, PortValue};
use crate::graph::socket::{Socket, SocketUid};
use crate::registry::NodeRegistry;
use crate::socket_types::SocketType;

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct GraphFile {
    pub version: u32,
    /// Next-available `SocketUid` when the graph was saved — used by the
    /// loader to resume the allocator so newly-minted uids don't collide
    /// with restored ones.
    #[serde(default)]
    pub next_socket_uid: u64,
    pub nodes: Vec<NodeFile>,
    pub noodles: Vec<NoodleFile>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NodeFile {
    pub id: u64,
    pub type_id: String,
    pub position: [f64; 2],
    /// Input socket list, in display order.
    #[serde(default)]
    pub inputs: Vec<SocketFile>,
    /// Output socket list, in display order.
    #[serde(default)]
    pub outputs: Vec<SocketFile>,
    /// Property values keyed by name. JSON-friendly representation.
    #[serde(default)]
    pub properties: HashMap<String, JsonPortValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SocketFile {
    pub uid: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub socket_type: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NoodleFile {
    pub from_node: u64,
    pub from_uid: u64,
    pub to_node: u64,
    pub to_uid: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", content = "value")]
pub enum JsonPortValue {
    None,
    Number(f64),
    Bool(bool),
    StringVal(String),
    Color([f32; 4]),
    Matrix4x4([f32; 16]),
}

impl JsonPortValue {
    pub fn from_port_value(v: &PortValue) -> Option<Self> {
        match v {
            PortValue::None => Some(JsonPortValue::None),
            PortValue::Number(n) => Some(JsonPortValue::Number(*n)),
            PortValue::Bool(b) => Some(JsonPortValue::Bool(*b)),
            PortValue::StringVal(s) => Some(JsonPortValue::StringVal(s.as_str().to_string())),
            PortValue::Color(c) => Some(JsonPortValue::Color(*c)),
            PortValue::Matrix4x4(m) => Some(JsonPortValue::Matrix4x4(*m)),
            // Heap-backed values are computed; the executor regenerates them.
            PortValue::Path2d(_) | PortValue::Geometry3d(_) => None,
        }
    }

    pub fn into_port_value(self) -> PortValue {
        match self {
            JsonPortValue::None => PortValue::None,
            JsonPortValue::Number(n) => PortValue::Number(n),
            JsonPortValue::Bool(b) => PortValue::Bool(b),
            JsonPortValue::StringVal(s) => PortValue::StringVal(std::sync::Arc::new(s)),
            JsonPortValue::Color(c) => PortValue::Color(c),
            JsonPortValue::Matrix4x4(m) => PortValue::Matrix4x4(m),
        }
    }
}

pub const SCHEMA_VERSION: u32 = 2;

fn socket_to_file(s: &Socket) -> SocketFile {
    SocketFile {
        uid: s.uid.0,
        name: s.name.to_string(),
        label: s.display_label.as_ref().map(|l| l.to_string()),
        socket_type: socket_type_to_string(s.socket_type),
        optional: s.optional,
    }
}

fn socket_from_file(f: &SocketFile) -> Socket {
    Socket {
        uid: SocketUid(f.uid),
        name: Arc::from(f.name.as_str()),
        display_label: f.label.as_ref().map(|l| Arc::from(l.as_str())),
        socket_type: string_to_socket_type(&f.socket_type),
        optional: f.optional,
    }
}

fn socket_type_to_string(t: SocketType) -> String {
    match t {
        SocketType::None => "None",
        SocketType::Number => "Number",
        SocketType::Bool => "Bool",
        SocketType::StringVal => "StringVal",
        SocketType::Color => "Color",
        SocketType::Matrix4x4 => "Matrix4x4",
        SocketType::Path2d => "Path2d",
        SocketType::Geometry3d => "Geometry3d",
    }
    .to_string()
}

fn string_to_socket_type(s: &str) -> SocketType {
    match s {
        "Number" => SocketType::Number,
        "Bool" => SocketType::Bool,
        "StringVal" => SocketType::StringVal,
        "Color" => SocketType::Color,
        "Matrix4x4" => SocketType::Matrix4x4,
        "Path2d" => SocketType::Path2d,
        "Geometry3d" => SocketType::Geometry3d,
        _ => SocketType::None,
    }
}

/// Build a `GraphFile` from the live graph. The result is JSON-ready.
pub fn save_graph(graph: &Graph) -> GraphFile {
    let mut nodes = Vec::with_capacity(graph.node_count());
    for n in graph.nodes() {
        let mut props = HashMap::new();
        for (k, v) in &n.properties {
            if let Some(jv) = JsonPortValue::from_port_value(v) {
                props.insert(k.to_string(), jv);
            }
        }
        nodes.push(NodeFile {
            id: n.id.0,
            type_id: n.type_id.to_string(),
            position: n.position,
            inputs: n.inputs.iter().map(socket_to_file).collect(),
            outputs: n.outputs.iter().map(socket_to_file).collect(),
            properties: props,
        });
    }
    nodes.sort_by_key(|n| n.id);

    let mut noodles = Vec::with_capacity(graph.noodle_count());
    for e in graph.noodles() {
        noodles.push(NoodleFile {
            from_node: e.from.node.0,
            from_uid: e.from.socket.0,
            to_node: e.to.node.0,
            to_uid: e.to.socket.0,
        });
    }
    noodles.sort_by_key(|n| (n.from_node, n.to_node, n.from_uid, n.to_uid));

    GraphFile {
        version: SCHEMA_VERSION,
        next_socket_uid: graph.peek_next_socket_uid(),
        nodes,
        noodles,
    }
}

/// Outcome of loading a graph file. Warnings collect non-fatal issues
/// (unknown node types, missing noodles) so callers can surface them
/// to the user.
pub struct LoadResult {
    pub graph: Graph,
    pub warnings: Vec<String>,
}

/// Reconstruct a `Graph` from a `GraphFile`. Unknown nodes are skipped
/// with a warning. Noodles referencing skipped nodes or unknown sockets are
/// silently dropped.
pub fn load_graph(file: GraphFile, registry: &NodeRegistry) -> LoadResult {
    let mut graph = Graph::new();
    let mut warnings = Vec::new();

    if file.version != SCHEMA_VERSION {
        warnings.push(format!(
            "graph file version {} differs from current {} — loading anyway, but compatibility is not guaranteed",
            file.version, SCHEMA_VERSION
        ));
    }

    // Bump the socket-uid allocator past every uid we are about to
    // restore from the file. This handles both the file's recorded
    // next-uid AND the explicit observation of each socket below.
    if file.next_socket_uid > 0 {
        graph.socket_alloc().observe(SocketUid(file.next_socket_uid.saturating_sub(1)));
    }

    let mut id_map: HashMap<u64, NodeId> = HashMap::new();

    for nf in file.nodes {
        if registry.get(&nf.type_id).is_none() {
            warnings.push(format!("unknown node type '{}' — skipped", nf.type_id));
            continue;
        }
        let new_id = NodeId(nf.id);
        let mut node = NodeInstance::new(new_id, nf.type_id.as_str(), nf.position);
        node.inputs = nf.inputs.iter().map(socket_from_file).collect();
        node.outputs = nf.outputs.iter().map(socket_from_file).collect();

        // Restore declared property values from the file; for any
        // property the type declares but the file omits, seed the
        // default so node code doesn't see PortValue::None unexpectedly.
        if let Some(def) = registry.get(&nf.type_id) {
            for prop_def in def.properties() {
                let key = prop_def.name.clone();
                let value = nf
                    .properties
                    .get(prop_def.name.as_ref())
                    .map(|j| j.clone().into_port_value())
                    .unwrap_or_else(|| prop_def.default.clone());
                node.properties.insert(key, value);
            }
        }
        // Also preserve any extra properties the file carried (e.g.
        // dynamic-node configs that aren't in the declared schema).
        for (k, v) in &nf.properties {
            let key = Arc::<str>::from(k.as_str());
            if !node.properties.contains_key(&key) {
                node.properties.insert(key, v.clone().into_port_value());
            }
        }

        let _ = graph.add_node(node);
        id_map.insert(nf.id, new_id);
    }

    for nf in file.noodles {
        let from_node = match id_map.get(&nf.from_node) {
            Some(n) => *n,
            None => continue,
        };
        let to_node = match id_map.get(&nf.to_node) {
            Some(n) => *n,
            None => continue,
        };
        let from_uid = SocketUid(nf.from_uid);
        let to_uid = SocketUid(nf.to_uid);

        // Validate the sockets still exist on the restored instances.
        // If the file's sockets were lost (e.g. a node type evolved its
        // socket layout between saves), drop the noodle with a warning
        // rather than refusing to load the whole project.
        let from_ok = graph
            .get(from_node)
            .map(|n| n.output_by_uid(from_uid).is_some())
            .unwrap_or(false);
        let to_ok = graph
            .get(to_node)
            .map(|n| n.input_by_uid(to_uid).is_some())
            .unwrap_or(false);
        if !from_ok || !to_ok {
            warnings.push(format!(
                "noodle {}:{} → {}:{} dropped — socket uid no longer present",
                nf.from_node, nf.from_uid, nf.to_node, nf.to_uid
            ));
            continue;
        }

        graph.noodles_mut().push(Noodle {
            from: NoodleEndpoint { node: from_node, socket: from_uid },
            to: NoodleEndpoint { node: to_node, socket: to_uid },
        });
    }

    LoadResult { graph, warnings }
}

/// Serialize a graph to a pretty-printed JSON string.
pub fn graph_to_json_string(graph: &Graph) -> String {
    serde_json::to_string_pretty(&save_graph(graph)).unwrap_or_else(|_| "{}".into())
}

/// Parse a JSON string back into a `LoadResult`.
pub fn graph_from_json_str(s: &str, registry: &NodeRegistry) -> Result<LoadResult, serde_json::Error> {
    let file: GraphFile = serde_json::from_str(s)?;
    Ok(load_graph(file, registry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::graph::Noodle;
    use crate::nodes;

    fn registry() -> NodeRegistry {
        let mut r = NodeRegistry::new();
        nodes::register_all(&mut r);
        r
    }

    #[test]
    fn round_trip_box_transform_graph() {
        let reg = registry();
        let mut g = Graph::new();
        let a = g.add_new_node("Box", [10.0, 20.0], &reg).unwrap();
        let b = g.add_new_node("Transform", [220.0, 30.0], &reg).unwrap();
        g.set_property(a, "width", PortValue::Number(7.5)).unwrap();
        g.set_property(a, "height", PortValue::Number(8.0)).unwrap();
        g.set_property(a, "depth", PortValue::Number(9.0)).unwrap();
        g.set_property(b, "ty", PortValue::Number(2.5)).unwrap();

        let out_a = g.get(a).unwrap().output_by_name("out").unwrap().uid;
        let in_b = g.get(b).unwrap().input_by_name("input").unwrap().uid;
        g.connect(Noodle::new(a, out_a, b, in_b), &reg).unwrap();

        let json = graph_to_json_string(&g);
        let LoadResult { graph: g2, warnings } = graph_from_json_str(&json, &reg).unwrap();
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);
        assert_eq!(g2.node_count(), 2);
        assert_eq!(g2.noodle_count(), 1);

        let box_node = g2
            .nodes()
            .find(|n| n.type_id.as_ref() == "Box")
            .expect("Box not present after reload");
        match box_node.properties.get("width").unwrap() {
            PortValue::Number(w) => assert!((*w - 7.5).abs() < 1e-9),
            _ => panic!(),
        }
        // Noodle UID stable across round-trip.
        assert_eq!(g2.noodles()[0].from.socket, out_a);
        assert_eq!(g2.noodles()[0].to.socket, in_b);
    }

    #[test]
    fn round_trip_extrude_preserves_color_and_matrix_properties() {
        let reg = registry();
        let mut g = Graph::new();
        let id = g.add_new_node("Extrude", [0.0, 0.0], &reg).unwrap();
        g.set_property(id, "height", PortValue::Number(8.5)).unwrap();
        g.set_property(id, "bevel_radius", PortValue::Number(1.25)).unwrap();
        g.set_property(id, "bevel_segments", PortValue::Number(12.0)).unwrap();
        g.set_property(id, "color", PortValue::Color([0.25, 0.5, 0.75, 1.0])).unwrap();
        let m: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            2.0, 3.0, 4.0, 1.0,
        ];
        g.set_property(id, "matrix", PortValue::Matrix4x4(m)).unwrap();

        let json = graph_to_json_string(&g);
        let LoadResult { graph: g2, warnings } = graph_from_json_str(&json, &reg).unwrap();
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);

        let restored = g2.nodes().find(|n| n.type_id.as_ref() == "Extrude").unwrap();
        match restored.properties.get("color").unwrap() {
            PortValue::Color(c) => assert_eq!(*c, [0.25, 0.5, 0.75, 1.0]),
            _ => panic!("color did not round-trip as Color"),
        }
        match restored.properties.get("matrix").unwrap() {
            PortValue::Matrix4x4(mm) => assert_eq!(*mm, m),
            _ => panic!("matrix did not round-trip as Matrix4x4"),
        }
    }

    #[test]
    fn unknown_node_type_is_skipped_with_warning() {
        let reg = registry();
        let json = r#"{
            "version": 2,
            "next_socket_uid": 0,
            "nodes": [
                {"id": 0, "type_id": "WidgetFromTheFuture", "position": [0,0], "inputs": [], "outputs": [], "properties": {}},
                {"id": 1, "type_id": "Box", "position": [10,10], "inputs": [], "outputs": [], "properties": {}}
            ],
            "noodles": []
        }"#;
        let result = graph_from_json_str(json, &reg).unwrap();
        assert_eq!(result.graph.node_count(), 1);
        assert!(result.warnings.iter().any(|w| w.contains("WidgetFromTheFuture")));
    }
}
