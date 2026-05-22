//! Serialize and deserialize a `Graph` to and from JSON.
//!
//! `PortValue` variants that wrap heap geometry (`Path2d`, `Geometry3d`)
//! are skipped — they are computed outputs that don't survive a round trip
//! and are recomputed by the executor on load. Property values that are
//! plain numbers, bools, strings, colors, or matrices are preserved.
//!
//! Forward compatibility: unknown node types are skipped with a warning
//! (returned in `LoadResult.warnings`), not a hard error. This lets a save
//! file from a future version load on an older binary as long as the
//! shared subset is recognized.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::graph::graph::{Edge, Graph};
use crate::graph::node::{NodeId, NodeInstance, PortValue, SocketId};
use crate::registry::NodeRegistry;

/// Plain JSON shape — every field maps 1-to-1 with `Graph` and
/// `NodeInstance`. Geometry / Path values are dropped on the way out.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct GraphFile {
    /// Schema version. Bumped on breaking changes.
    pub version: u32,
    pub nodes: Vec<NodeFile>,
    pub edges: Vec<EdgeFile>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NodeFile {
    pub id: u64,
    pub type_id: String,
    pub position: [f64; 2],
    /// Property values keyed by name. JSON-friendly representation.
    #[serde(default)]
    pub properties: HashMap<String, JsonPortValue>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EdgeFile {
    pub from_node: u64,
    pub from_socket: String,
    pub to_node: u64,
    pub to_socket: String,
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
            // Path2d / Geometry3d aren't worth round-tripping — they're
            // computed outputs that the executor regenerates.
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

pub const SCHEMA_VERSION: u32 = 1;

/// Build a `GraphFile` from the live graph. The result is JSON-ready.
pub fn save_graph(graph: &Graph) -> GraphFile {
    let mut nodes = Vec::with_capacity(graph.node_count());
    for n in graph.nodes() {
        let mut props = HashMap::new();
        for (k, v) in &n.properties {
            if let Some(jv) = JsonPortValue::from_port_value(v) {
                props.insert((*k).to_string(), jv);
            }
        }
        nodes.push(NodeFile {
            id: n.id.0,
            type_id: n.type_id.to_string(),
            position: n.position,
            properties: props,
        });
    }
    nodes.sort_by_key(|n| n.id);

    let mut edges = Vec::with_capacity(graph.edge_count());
    for e in graph.edges() {
        edges.push(EdgeFile {
            from_node: e.from.node.0,
            from_socket: e.from.name.to_string(),
            to_node: e.to.node.0,
            to_socket: e.to.name.to_string(),
        });
    }
    edges.sort_by_key(|e| (e.from_node, e.to_node, e.from_socket.clone(), e.to_socket.clone()));

    GraphFile {
        version: SCHEMA_VERSION,
        nodes,
        edges,
    }
}

/// Outcome of loading a graph file. Warnings collect non-fatal issues
/// (unknown node types, missing socket names) so callers can surface them
/// to the user.
pub struct LoadResult {
    pub graph: Graph,
    pub warnings: Vec<String>,
}

/// Reconstruct a `Graph` from a `GraphFile`. Unknown nodes are skipped
/// with a warning. Edges referencing skipped nodes or unknown sockets are
/// silently dropped. Property names are leaked as `&'static str` because
/// the runtime registry stores them as `&'static`; we try to reuse the
/// registry-known names where possible to avoid the leak.
pub fn load_graph(file: GraphFile, registry: &NodeRegistry) -> LoadResult {
    let mut graph = Graph::new();
    let mut warnings = Vec::new();

    let mut id_map: HashMap<u64, NodeId> = HashMap::new();
    let mut type_id_intern: HashMap<&str, &'static str> = HashMap::new();

    for nf in file.nodes {
        let interned_type_id = match registry.get(&nf.type_id) {
            Some(def) => def.type_id(),
            None => {
                warnings.push(format!("unknown node type '{}' — skipped", nf.type_id));
                continue;
            }
        };
        let new_id = graph.allocate_id();
        let mut node = NodeInstance::new(new_id, interned_type_id, nf.position);

        // For each known PropDef, look up the JSON value by name.
        if let Some(def) = registry.get(&nf.type_id) {
            for prop_def in def.properties() {
                if let Some(j) = nf.properties.get(prop_def.name) {
                    node.properties.insert(prop_def.name, j.clone().into_port_value());
                } else {
                    node.properties.insert(prop_def.name, prop_def.default.clone());
                }
            }
        }

        let _ = graph.add_node(node);
        id_map.insert(nf.id, new_id);
        type_id_intern.entry(interned_type_id).or_insert(interned_type_id);
    }

    for ef in file.edges {
        let from_node = match id_map.get(&ef.from_node) {
            Some(n) => *n,
            None => continue,
        };
        let to_node = match id_map.get(&ef.to_node) {
            Some(n) => *n,
            None => continue,
        };
        // Look up the static socket names from the registry. If the live
        // registry no longer has a matching socket, drop the edge.
        let from_static = static_socket_name(registry, from_node, &graph, &ef.from_socket, true);
        let to_static = static_socket_name(registry, to_node, &graph, &ef.to_socket, false);
        let (from_static, to_static) = match (from_static, to_static) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                warnings.push(format!(
                    "edge {}.{} → {}.{} dropped — socket no longer exists",
                    ef.from_node, ef.from_socket, ef.to_node, ef.to_socket
                ));
                continue;
            }
        };
        // Use Graph::edges_mut to insert without re-validating type
        // compatibility (registry may have evolved; we accept the saved
        // edge as-is and leave it to the executor to surface a runtime
        // error if it's truly broken).
        graph.edges_mut().push(Edge {
            from: SocketId { node: from_node, name: from_static },
            to: SocketId { node: to_node, name: to_static },
        });
    }

    LoadResult { graph, warnings }
}

fn static_socket_name(
    registry: &NodeRegistry,
    node: NodeId,
    graph: &Graph,
    requested: &str,
    is_output: bool,
) -> Option<&'static str> {
    let n = graph.get(node)?;
    let def = registry.get(n.type_id)?;
    let list = if is_output { def.output_sockets() } else { def.input_sockets() };
    list.into_iter()
        .find(|s| s.name == requested)
        .map(|s| s.name)
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
    use crate::graph::graph::Edge;
    use crate::graph::node::SocketId;
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
        let a = g.allocate_id();
        let b = g.allocate_id();
        let mut na = NodeInstance::new(a, "Box", [10.0, 20.0]);
        na.properties.insert("width", PortValue::Number(7.5));
        na.properties.insert("height", PortValue::Number(8.0));
        na.properties.insert("depth", PortValue::Number(9.0));
        let mut nb = NodeInstance::new(b, "Transform", [220.0, 30.0]);
        nb.properties.insert("ty", PortValue::Number(2.5));
        nb.properties.insert("sx", PortValue::Number(1.0));
        nb.properties.insert("sy", PortValue::Number(1.0));
        nb.properties.insert("sz", PortValue::Number(1.0));
        g.add_node(na).unwrap();
        g.add_node(nb).unwrap();
        g.connect(
            Edge {
                from: SocketId { node: a, name: "out" },
                to: SocketId { node: b, name: "input" },
            },
            &reg,
        ).unwrap();

        let json = graph_to_json_string(&g);
        let LoadResult { graph: g2, warnings } = graph_from_json_str(&json, &reg).unwrap();
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);
        assert_eq!(g2.node_count(), 2);
        assert_eq!(g2.edge_count(), 1);

        // Find the Box and verify its properties survived.
        let box_node = g2
            .nodes()
            .find(|n| n.type_id == "Box")
            .expect("Box not present after reload");
        match box_node.properties.get("width").unwrap() {
            PortValue::Number(w) => assert!((*w - 7.5).abs() < 1e-9),
            _ => panic!(),
        }
    }

    #[test]
    fn round_trip_extrude_preserves_color_and_matrix_properties() {
        let reg = registry();
        let mut g = Graph::new();
        let id = g.allocate_id();
        let mut node = NodeInstance::new(id, "Extrude", [0.0, 0.0]);
        node.properties.insert("height", PortValue::Number(8.5));
        node.properties.insert("bevel_radius", PortValue::Number(1.25));
        node.properties.insert("bevel_segments", PortValue::Number(12.0));
        node.properties
            .insert("color", PortValue::Color([0.25, 0.5, 0.75, 1.0]));
        let m: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            2.0, 3.0, 4.0, 1.0,
        ];
        node.properties.insert("matrix", PortValue::Matrix4x4(m));
        g.add_node(node).unwrap();

        let json = graph_to_json_string(&g);
        let LoadResult { graph: g2, warnings } = graph_from_json_str(&json, &reg).unwrap();
        assert!(warnings.is_empty(), "warnings: {:?}", warnings);

        let restored = g2.nodes().find(|n| n.type_id == "Extrude").unwrap();
        match restored.properties.get("color").unwrap() {
            PortValue::Color(c) => assert_eq!(*c, [0.25, 0.5, 0.75, 1.0]),
            _ => panic!("color did not round-trip as Color"),
        }
        match restored.properties.get("matrix").unwrap() {
            PortValue::Matrix4x4(mm) => assert_eq!(*mm, m),
            _ => panic!("matrix did not round-trip as Matrix4x4"),
        }
        match restored.properties.get("height").unwrap() {
            PortValue::Number(v) => assert!((v - 8.5).abs() < 1e-9),
            _ => panic!(),
        }
        match restored.properties.get("bevel_segments").unwrap() {
            PortValue::Number(v) => assert!((v - 12.0).abs() < 1e-9),
            _ => panic!(),
        }
    }

    #[test]
    fn unknown_node_type_is_skipped_with_warning() {
        let reg = registry();
        let json = r#"{
            "version": 1,
            "nodes": [
                {"id": 0, "type_id": "WidgetFromTheFuture", "position": [0,0], "properties": {}},
                {"id": 1, "type_id": "Box", "position": [10,10], "properties": {}}
            ],
            "edges": []
        }"#;
        let result = graph_from_json_str(json, &reg).unwrap();
        assert_eq!(result.graph.node_count(), 1);
        assert!(result.warnings.iter().any(|w| w.contains("WidgetFromTheFuture")));
    }
}
