//! `AppStateModel` — adapts AtomArtist's [`AppState`] to the
//! [`agg_gui_node_editor::NodeGraphModel`] trait so the generic
//! `NodeEditor` widget from the agg-gui-node-editor crate can drive
//! AtomArtist's graph.
//!
//! The adapter is the *only* place inside atomartist-ui that crosses
//! the boundary between agg-gui's domain-agnostic node-editor types
//! and AtomArtist's domain-specific `Graph` / `NodeRegistry` /
//! `PortValue` types.  Anything new that wants to live inside the
//! canvas widget tree should plug in here, not in the widget itself.

use std::sync::Arc;

use agg_gui::Color;
use agg_gui_node_editor as ne;

use atomartist_lib::graph::graph::{Edge, GraphError};
use atomartist_lib::graph::node::{NodeId as DomainNodeId, PortValue, SocketId};
use atomartist_lib::registry::EditorKind;
use atomartist_lib::SocketType;

use crate::app_state::AppState;

/// Adapter wrapping a clone of `AppState`. Construct one, hand the
/// `Arc<Mutex<...>>` to `NodeEditor::new`, and the rest of AtomArtist's
/// machinery (graph mutex, registry, evaluator, selection mirror)
/// stays untouched.
pub struct AppStateModel {
    pub state: AppState,
}

impl AppStateModel {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Convert AtomArtist's `NodeId` to the editor's opaque id.
    fn to_ne(id: DomainNodeId) -> ne::NodeId {
        ne::NodeId(id.0)
    }
    /// Convert the editor's opaque id back to AtomArtist's `NodeId`.
    fn from_ne(id: ne::NodeId) -> DomainNodeId {
        DomainNodeId(id.0)
    }

    /// Stable u32 mapping for `SocketType` so the editor's
    /// `SocketTypeId` round-trips deterministically.  The exact
    /// numbers are private — only equality matters.
    fn socket_type_to_id(t: SocketType) -> ne::SocketTypeId {
        ne::SocketTypeId(match t {
            SocketType::None => 0,
            SocketType::Number => 1,
            SocketType::Bool => 2,
            SocketType::StringVal => 3,
            SocketType::Color => 4,
            SocketType::Matrix4x4 => 5,
            SocketType::Path2d => 6,
            SocketType::Geometry3d => 7,
        })
    }
    fn id_to_socket_type(id: ne::SocketTypeId) -> SocketType {
        match id.0 {
            1 => SocketType::Number,
            2 => SocketType::Bool,
            3 => SocketType::StringVal,
            4 => SocketType::Color,
            5 => SocketType::Matrix4x4,
            6 => SocketType::Path2d,
            7 => SocketType::Geometry3d,
            _ => SocketType::None,
        }
    }

    /// Map AtomArtist's schema-side `EditorKind` to the canvas's
    /// `EditorHint`.  The canvas only understands hints it can act on
    /// (today: colour-wheel popup); other variants forward as `None`
    /// and the canvas falls back to its default inline behaviour.
    fn editor_kind_to_ne(k: &EditorKind) -> Option<ne::EditorHint> {
        match k {
            EditorKind::ColorPicker => Some(ne::EditorHint::Color),
            _ => None,
        }
    }

    /// Map a `PortValue` to an editor-side `PropertyValue`. Numbers,
    /// bools, and colors get rich inline visuals; matrices / paths /
    /// meshes round-trip through `Other` and rely on host-side
    /// editors.
    fn property_value_to_ne(v: &PortValue) -> ne::PropertyValue {
        match v {
            PortValue::Number(n) => ne::PropertyValue::Number(*n),
            PortValue::Bool(b) => ne::PropertyValue::Bool(*b),
            PortValue::StringVal(s) => ne::PropertyValue::Other {
                display: s.as_str().to_string(),
            },
            PortValue::Color(c) => ne::PropertyValue::Color(*c),
            PortValue::Matrix4x4(_) => ne::PropertyValue::Other {
                display: "Matrix".into(),
            },
            PortValue::Path2d(_) => ne::PropertyValue::Other {
                display: "Path2d".into(),
            },
            PortValue::Geometry3d(_) => ne::PropertyValue::Other {
                display: "Geometry".into(),
            },
            PortValue::None => ne::PropertyValue::Other { display: "—".into() },
        }
    }
}

impl ne::NodeGraphModel for AppStateModel {
    fn nodes(&self) -> Vec<ne::NodeView> {
        let g = self.state.graph.lock().unwrap();
        let reg = &self.state.registry;
        g.nodes()
            .filter_map(|n| {
                let def = reg.get(n.type_id)?;
                let inputs: Vec<ne::SocketView> = def
                    .input_sockets()
                    .into_iter()
                    .map(|s| ne::SocketView {
                        name: s.name.into(),
                        socket_type: Self::socket_type_to_id(s.socket_type),
                        display_label: s.display_label.map(Into::into),
                    })
                    .collect();
                let outputs: Vec<ne::SocketView> = def
                    .output_sockets()
                    .into_iter()
                    .map(|s| ne::SocketView {
                        name: s.name.into(),
                        socket_type: Self::socket_type_to_id(s.socket_type),
                        display_label: s.display_label.map(Into::into),
                    })
                    .collect();
                let properties: Vec<ne::PropertyView> = def
                    .properties()
                    .into_iter()
                    .map(|p| {
                        let current = n
                            .properties
                            .get(p.name)
                            .cloned()
                            .unwrap_or_else(|| p.default.clone());
                        ne::PropertyView {
                            name: p.name.into(),
                            display_label: p.label.map(Into::into),
                            current: Self::property_value_to_ne(&current),
                            min: p.min,
                            max: p.max,
                            bound_input: p.bound_input.map(Into::into),
                            editor: Self::editor_kind_to_ne(&p.editor),
                        }
                    })
                    .collect();
                Some(ne::NodeView {
                    id: Self::to_ne(n.id),
                    type_id: def.type_id().into(),
                    display_name: def.display_name().into(),
                    category: def.category().into(),
                    position: n.position,
                    inputs,
                    outputs,
                    properties,
                })
            })
            .collect()
    }

    fn edges(&self) -> Vec<ne::EdgeView> {
        let g = self.state.graph.lock().unwrap();
        g.edges()
            .iter()
            .map(|e| ne::EdgeView {
                from_node: Self::to_ne(e.from.node),
                from_socket: e.from.name.into(),
                to_node: Self::to_ne(e.to.node),
                to_socket: e.to.name.into(),
            })
            .collect()
    }

    fn node_types_by_category(&self) -> Vec<(String, Vec<ne::NodeTypeView>)> {
        self.state
            .registry
            .by_category()
            .into_iter()
            .map(|(cat, defs)| {
                let types = defs
                    .into_iter()
                    .map(|d| ne::NodeTypeView {
                        type_id: d.type_id().into(),
                        display_name: d.display_name().into(),
                        category: d.category().into(),
                    })
                    .collect();
                (cat.into(), types)
            })
            .collect()
    }

    fn socket_color(&self, ty: ne::SocketTypeId) -> Color {
        let [r, g, b, a] = Self::id_to_socket_type(ty).display_color_rgba();
        Color::rgba(
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        )
    }

    fn category_color(&self, category: &str, fallback: Color) -> Color {
        // Matches the previous AtomArtist palette in canvas_draw.rs.
        match category {
            "Primitives 2D" | "Operations 2D" => Color::rgb(0.30, 0.50, 0.86),
            "Primitives 3D" => Color::rgb(0.20, 0.55, 0.90),
            "Operations 3D" => Color::rgb(0.42, 0.66, 0.32),
            "Mesh" => Color::rgb(0.85, 0.55, 0.22),
            "Math" => Color::rgb(0.50, 0.50, 0.55),
            "Output" => Color::rgb(0.62, 0.36, 0.78),
            _ => fallback,
        }
    }

    fn sockets_compatible(&self, out_ty: ne::SocketTypeId, in_ty: ne::SocketTypeId) -> bool {
        Self::id_to_socket_type(out_ty)
            .is_compatible_with(Self::id_to_socket_type(in_ty))
    }

    fn primary_selection(&self) -> Option<ne::NodeId> {
        self.state.selection.lock().ok()?.map(Self::to_ne)
    }

    fn set_node_position(&mut self, id: ne::NodeId, pos: [f64; 2]) {
        let mut g = self.state.graph.lock().unwrap();
        let _ = g.set_position(Self::from_ne(id), pos);
    }

    fn add_node(&mut self, type_id: &str, pos: [f64; 2]) -> Option<ne::NodeId> {
        // Re-look-up the static `&'static str` from the registry — we
        // can't mint one from a runtime &str without leaking memory.
        let static_id = self
            .state
            .registry
            .iter()
            .map(|d| d.type_id())
            .find(|s| *s == type_id)?;
        let id = {
            let mut g = self.state.graph.lock().unwrap();
            crate::node_helpers::add_node_with_defaults(&mut g, &self.state.registry, static_id, pos)?
        };
        self.state.schedule_evaluate();
        Some(Self::to_ne(id))
    }

    fn remove_node(&mut self, id: ne::NodeId) {
        let domain_id = Self::from_ne(id);
        {
            let mut g = self.state.graph.lock().unwrap();
            let _ = g.remove_node(domain_id);
        }
        self.state.schedule_evaluate();
    }

    fn try_add_edge(
        &mut self,
        from_node: ne::NodeId,
        from_socket: &str,
        to_node: ne::NodeId,
        to_socket: &str,
    ) -> ne::EdgeResult {
        // Resolve `&str` socket names back to `&'static str` via the
        // registry. The graph stores these as `&'static str` so we
        // need to look them up rather than minting new strings.
        let from_static = self.lookup_socket_name(Self::from_ne(from_node), from_socket, true);
        let to_static = self.lookup_socket_name(Self::from_ne(to_node), to_socket, false);
        let (from_static, to_static) = match (from_static, to_static) {
            (Some(a), Some(b)) => (a, b),
            _ => return ne::EdgeResult::Rejected,
        };
        let edge = Edge {
            from: SocketId { node: Self::from_ne(from_node), name: from_static },
            to: SocketId { node: Self::from_ne(to_node), name: to_static },
        };
        let mut g = self.state.graph.lock().unwrap();
        let result = match g.connect(edge.clone(), &self.state.registry) {
            Ok(()) => ne::EdgeResult::Connected,
            Err(GraphError::InputAlreadyConnected) => {
                // Replacement semantics: drop the existing edge to the
                // input, then retry.
                let to_remove: Vec<Edge> = g
                    .edges()
                    .iter()
                    .filter(|e| e.to == edge.to)
                    .cloned()
                    .collect();
                for e in to_remove {
                    let _ = g.disconnect(&e);
                }
                if g.connect(edge, &self.state.registry).is_ok() {
                    ne::EdgeResult::Replaced
                } else {
                    ne::EdgeResult::Rejected
                }
            }
            Err(_) => ne::EdgeResult::Rejected,
        };
        drop(g);
        if matches!(result, ne::EdgeResult::Connected | ne::EdgeResult::Replaced) {
            self.state.schedule_evaluate();
        }
        result
    }

    fn set_property(&mut self, id: ne::NodeId, name: &str, value: ne::PropertyValue) {
        let domain_id = Self::from_ne(id);
        let static_name = match self.lookup_property_name(domain_id, name) {
            Some(n) => n,
            None => return,
        };
        let port_value = match value {
            ne::PropertyValue::Number(n) => PortValue::Number(n),
            ne::PropertyValue::Bool(b) => PortValue::Bool(b),
            ne::PropertyValue::Color(c) => PortValue::Color(c),
            ne::PropertyValue::Other { .. } => return,
        };
        {
            let mut g = self.state.graph.lock().unwrap();
            let _ = g.set_property(domain_id, static_name, port_value);
        }
        self.state.schedule_evaluate();
    }

    fn on_canvas_zoom_changed(&mut self, zoom: f64) {
        *self.state.canvas_zoom.lock().unwrap() = zoom;
    }

    fn on_primary_selection_changed(&mut self, id: Option<ne::NodeId>) {
        let domain = id.map(Self::from_ne);
        // Mirror to AppState::selection so the 3-D viewport (and any
        // other widget that reads it) stays in sync.
        self.state.set_selection(domain);
        // If the newly-selected node has a Geometry3d output, also
        // surface it as the display node so the viewport renders that
        // node's mesh on the next evaluate.
        if let Some(nid) = domain {
            let g = self.state.graph.lock().unwrap();
            let has_geom = g
                .get(nid)
                .and_then(|n| self.state.registry.get(n.type_id))
                .map(|def| {
                    def.output_sockets()
                        .iter()
                        .any(|s| s.socket_type == SocketType::Geometry3d)
                })
                .unwrap_or(false);
            drop(g);
            if has_geom {
                self.state.set_display_node(Some(nid));
            }
        }
    }
}

impl AppStateModel {
    /// Resolve a runtime `&str` socket name back to its `&'static str`
    /// in the node's registered def.  `is_output` selects whether we
    /// scan the input or output socket list. Returns `None` when the
    /// node or socket isn't found — the caller treats that as a
    /// rejected mutation.
    fn lookup_socket_name(
        &self,
        node: DomainNodeId,
        name: &str,
        is_output: bool,
    ) -> Option<&'static str> {
        let g = self.state.graph.lock().unwrap();
        let n = g.get(node)?;
        let def = self.state.registry.get(n.type_id)?;
        let sockets = if is_output {
            def.output_sockets()
        } else {
            def.input_sockets()
        };
        sockets.into_iter().find(|s| s.name == name).map(|s| s.name)
    }

    fn lookup_property_name(&self, node: DomainNodeId, name: &str) -> Option<&'static str> {
        let g = self.state.graph.lock().unwrap();
        let n = g.get(node)?;
        let def = self.state.registry.get(n.type_id)?;
        def.properties().into_iter().find(|p| p.name == name).map(|p| p.name)
    }
}

/// Convenience: build an `Arc<Mutex<AppStateModel>>` ready to hand to
/// `agg_gui_node_editor::NodeEditor::new`.  Used by `top_level::build_app`.
pub fn shared_model_for(state: AppState) -> ne::SharedModel {
    use std::sync::Mutex;
    Arc::new(Mutex::new(AppStateModel::new(state)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomartist_lib::nodes;
    use atomartist_lib::registry::NodeRegistry;
    use atomartist_lib::Graph;

    fn fixture() -> AppState {
        let mut reg = NodeRegistry::new();
        nodes::register_all(&mut reg);
        AppState::new(Graph::new(), reg)
    }

    #[test]
    fn nodes_view_round_trips_position_and_type() {
        let state = fixture();
        // Insert a Box node at a known position.
        {
            let mut g = state.graph.lock().unwrap();
            crate::node_helpers::add_node_with_defaults(&mut g, &state.registry, "Box", [10.0, 20.0])
                .unwrap();
        }
        let model = AppStateModel::new(state);
        let nodes = ne::NodeGraphModel::nodes(&model);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].type_id, "Box");
        assert_eq!(nodes[0].position, [10.0, 20.0]);
    }

    #[test]
    fn add_node_inserts_through_adapter() {
        let state = fixture();
        let mut model = AppStateModel::new(state);
        let id = ne::NodeGraphModel::add_node(&mut model, "Box", [50.0, 60.0]);
        assert!(id.is_some());
        let g = model.state.graph.lock().unwrap();
        assert_eq!(g.nodes().count(), 1);
    }

    #[test]
    fn property_set_through_adapter_writes_graph() {
        let state = fixture();
        let id = {
            let mut g = state.graph.lock().unwrap();
            crate::node_helpers::add_node_with_defaults(&mut g, &state.registry, "Box", [0.0, 0.0])
                .unwrap()
        };
        let mut model = AppStateModel::new(state);
        ne::NodeGraphModel::set_property(
            &mut model,
            ne::NodeId(id.0),
            "width",
            ne::PropertyValue::Number(7.5),
        );
        let g = model.state.graph.lock().unwrap();
        let n = g.get(id).unwrap();
        match n.properties.get("width") {
            Some(PortValue::Number(v)) => assert!((v - 7.5).abs() < 1e-9),
            _ => panic!("width property not updated"),
        }
    }

    #[test]
    fn primary_selection_change_mirrors_to_app_state() {
        let state = fixture();
        let id = {
            let mut g = state.graph.lock().unwrap();
            crate::node_helpers::add_node_with_defaults(&mut g, &state.registry, "Box", [0.0, 0.0])
                .unwrap()
        };
        let mut model = AppStateModel::new(state);
        ne::NodeGraphModel::on_primary_selection_changed(&mut model, Some(ne::NodeId(id.0)));
        assert_eq!(*model.state.selection.lock().unwrap(), Some(id));
    }

    #[test]
    fn extrude_view_pairs_inputs_with_bound_properties() {
        let state = fixture();
        let id = {
            let mut g = state.graph.lock().unwrap();
            crate::node_helpers::add_node_with_defaults(&mut g, &state.registry, "Extrude", [0.0, 0.0])
                .unwrap()
        };
        let model = AppStateModel::new(state);
        let nodes = ne::NodeGraphModel::nodes(&model);
        let n = nodes.iter().find(|n| n.id.0 == id.0).unwrap();
        // Outputs come first in NodeDesigner-style layout.
        assert_eq!(n.outputs.len(), 1);
        assert_eq!(n.outputs[0].name, "Geometry");
        // Each optional input must be paired with a bound property.
        let optional_input_names: Vec<&str> = vec![
            "Height",
            "Radius",
            "Segments",
            "Bottom Radius",
            "Bottom Segments",
            "Color",
            "Matrix",
        ];
        for name in optional_input_names {
            let matched = n
                .properties
                .iter()
                .any(|p| p.bound_input.as_deref() == Some(name));
            assert!(matched, "no property bound to input '{}'", name);
        }
        // Sockets carry display labels.
        let height_input = n.inputs.iter().find(|s| s.name == "Height").unwrap();
        assert_eq!(height_input.display_label.as_deref(), Some("Height"));
    }

    #[test]
    fn extrude_color_property_round_trips_as_color_value() {
        let state = fixture();
        let _id = {
            let mut g = state.graph.lock().unwrap();
            crate::node_helpers::add_node_with_defaults(&mut g, &state.registry, "Extrude", [0.0, 0.0])
                .unwrap()
        };
        let model = AppStateModel::new(state);
        let nodes = ne::NodeGraphModel::nodes(&model);
        let n = &nodes[0];
        let color = n.properties.iter().find(|p| p.name == "color").unwrap();
        match &color.current {
            ne::PropertyValue::Color(c) => assert_eq!(*c, [1.0, 1.0, 1.0, 1.0]),
            other => panic!("expected Color, got {:?}", other),
        }
    }
}
