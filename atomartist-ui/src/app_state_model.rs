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
//!
//! ## Sockets live on the instance
//!
//! Following the Stage 1 engine refactor, a node instance owns its
//! socket layout (`n.inputs`, `n.outputs`). The adapter reads sockets
//! from the instance — not from `NodeDef::instantiate` — so dynamic
//! nodes (Output, eventually Combine) project their per-instance
//! configuration into the canvas correctly. Noodles are referenced by
//! [`SocketUid`] in the graph, but the editor speaks names; the
//! adapter resolves names ↔ uids on each crossing.

use std::sync::Arc;

use agg_gui::Color;
use agg_gui_node_editor as ne;

use atomartist_lib::graph::graph::{Noodle, GraphError};
use atomartist_lib::graph::node::{NodeId as DomainNodeId, PortValue};
use atomartist_lib::graph::socket::SocketUid;
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

    fn property_value_to_ne(v: &PortValue) -> ne::PropertyValue {
        match v {
            PortValue::Number(n) => ne::PropertyValue::Number(*n),
            PortValue::Bool(b) => ne::PropertyValue::Bool(*b),
            PortValue::Color(c) => ne::PropertyValue::Color(*c),
            PortValue::StringVal(s) => ne::PropertyValue::Other {
                display: s.as_str().to_string(),
            },
            PortValue::Matrix4x4(m) => ne::PropertyValue::Other {
                // MatterCAD shows "Identity" when the transform is the
                // identity matrix; any other transform shows nothing
                // (the matrix popup carries the detail).
                display: if *m == atomartist_lib::graph::node::identity_matrix() {
                    "Identity".into()
                } else {
                    "Matrix".into()
                },
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
                let def = reg.get(&n.type_id)?;
                // Sockets live on the instance now — that's where dynamic
                // nodes track their per-instance configuration.
                let inputs: Vec<ne::SocketView> = n
                    .inputs
                    .iter()
                    .map(|s| ne::SocketView {
                        name: s.name.to_string(),
                        socket_type: Self::socket_type_to_id(s.socket_type),
                        display_label: s.display_label.as_ref().map(|l| l.to_string()),
                    })
                    .collect();
                let outputs: Vec<ne::SocketView> = n
                    .outputs
                    .iter()
                    .map(|s| ne::SocketView {
                        name: s.name.to_string(),
                        socket_type: Self::socket_type_to_id(s.socket_type),
                        display_label: s.display_label.as_ref().map(|l| l.to_string()),
                    })
                    .collect();
                // Row-by-row visibility runs through `NodeDef::row_visible`,
                // the Rust analogue of MatterCAD's
                // `IPropertyGridModifier.UpdateControls(change)`. The
                // default impl applies each `PropDef.visible_when`
                // against the live `advanced` toggle; nodes with
                // complex inter-property predicates (e.g. an Align
                // node where `XOffset` depends on both `XAlign` and
                // `XMode`) override the hook to express the full
                // logic. Build a `NodeProperties` snapshot once per
                // node, then ask the def whether each row is visible.
                let mut snapshot = atomartist_lib::registry::NodeProperties::default();
                for (k, v) in &n.properties {
                    snapshot.insert(k.clone(), v.clone());
                }
                let properties: Vec<ne::PropertyView> = def
                    .properties()
                    .into_iter()
                    .filter(|p| def.row_visible(&p.name, &snapshot))
                    .map(|p| {
                        let current = n
                            .properties
                            .get(&p.name)
                            .cloned()
                            .unwrap_or_else(|| p.default.clone());
                        ne::PropertyView {
                            name: p.name.to_string(),
                            display_label: p.label.as_ref().map(|l| l.to_string()),
                            current: Self::property_value_to_ne(&current),
                            min: p.min,
                            max: p.max,
                            bound_input: p.bound_input.as_ref().map(|s| s.to_string()),
                            editor: Self::editor_kind_to_ne(&p.editor),
                            // Forward the full schema-side editor so
                            // the per-kind row renderers (`paint_row`)
                            // can mount the right pill, toggle,
                            // swatch, etc.
                            editor_kind: Some(p.editor.clone()),
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

    fn noodles(&self) -> Vec<ne::NoodleView> {
        let g = self.state.graph.lock().unwrap();
        g.noodles()
            .iter()
            .filter_map(|noodle| {
                // Resolve uids back to socket names so the editor sees
                // noodles in name form (matching its SocketView output).
                let from_name = g
                    .get(noodle.from.node)
                    .and_then(|n| n.output_by_uid(noodle.from.socket))?
                    .name
                    .to_string();
                let to_name = g
                    .get(noodle.to.node)
                    .and_then(|n| n.input_by_uid(noodle.to.socket))?
                    .name
                    .to_string();
                Some(ne::NoodleView {
                    from_node: Self::to_ne(noodle.from.node),
                    from_socket: from_name,
                    to_node: Self::to_ne(noodle.to.node),
                    to_socket: to_name,
                })
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
        let id = {
            let mut g = self.state.graph.lock().unwrap();
            g.add_new_node(type_id, pos, &self.state.registry).ok()?
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

    fn try_add_noodle(
        &mut self,
        from_node: ne::NodeId,
        from_socket: &str,
        to_node: ne::NodeId,
        to_socket: &str,
    ) -> ne::NoodleResult {
        // Resolve name → uid on the live instance. Names may be dynamic
        // (Output's slot names follow source-output names) so we always
        // walk the instance's own socket list.
        let from_uid = self.lookup_output_uid(Self::from_ne(from_node), from_socket);
        let to_uid = self.lookup_input_uid(Self::from_ne(to_node), to_socket);
        let (from_uid, to_uid) = match (from_uid, to_uid) {
            (Some(a), Some(b)) => (a, b),
            _ => return ne::NoodleResult::Rejected,
        };
        let noodle = Noodle::new(Self::from_ne(from_node), from_uid, Self::from_ne(to_node), to_uid);
        let mut g = self.state.graph.lock().unwrap();
        let result = match g.connect(noodle, &self.state.registry) {
            Ok(()) => ne::NoodleResult::Connected,
            Err(GraphError::InputAlreadyConnected) => {
                // Replacement semantics: drop the existing noodle to the
                // input, then retry.
                let to_remove: Vec<Noodle> = g
                    .noodles()
                    .iter()
                    .filter(|n| n.to == noodle.to)
                    .copied()
                    .collect();
                for n in to_remove {
                    let _ = g.disconnect(&n, &self.state.registry);
                }
                if g.connect(noodle, &self.state.registry).is_ok() {
                    ne::NoodleResult::Replaced
                } else {
                    ne::NoodleResult::Rejected
                }
            }
            Err(_) => ne::NoodleResult::Rejected,
        };
        drop(g);
        if matches!(result, ne::NoodleResult::Connected | ne::NoodleResult::Replaced) {
            self.state.schedule_evaluate();
        }
        result
    }

    fn set_property(&mut self, id: ne::NodeId, name: &str, value: ne::PropertyValue) {
        let domain_id = Self::from_ne(id);
        let port_value = match value {
            ne::PropertyValue::Number(n) => PortValue::Number(n),
            ne::PropertyValue::Bool(b) => PortValue::Bool(b),
            ne::PropertyValue::Color(c) => PortValue::Color(c),
            ne::PropertyValue::Other { .. } => return,
        };
        {
            let mut g = self.state.graph.lock().unwrap();
            let _ = g.set_property(domain_id, Arc::<str>::from(name), port_value);
        }
        self.state.schedule_evaluate();
    }

    fn on_canvas_zoom_changed(&mut self, zoom: f64) {
        *self.state.canvas_zoom.lock().unwrap() = zoom;
    }

    fn on_primary_selection_changed(&mut self, id: Option<ne::NodeId>) {
        let domain = id.map(Self::from_ne);
        self.state.set_selection(domain);
        if let Some(nid) = domain {
            let g = self.state.graph.lock().unwrap();
            // Geometry detection now reads the instance's outputs
            // directly — same answer as before for static nodes, and
            // correctly reflects dynamic outputs for the new Output node.
            let has_geom = g
                .get(nid)
                .map(|n| n.outputs.iter().any(|s| s.socket_type == SocketType::Geometry3d))
                .unwrap_or(false);
            drop(g);
            if has_geom {
                self.state.set_display_node(Some(nid));
            }
        }
    }
}

impl AppStateModel {
    fn lookup_output_uid(&self, node: DomainNodeId, name: &str) -> Option<SocketUid> {
        let g = self.state.graph.lock().unwrap();
        g.get(node)?.output_by_name(name).map(|s| s.uid)
    }

    fn lookup_input_uid(&self, node: DomainNodeId, name: &str) -> Option<SocketUid> {
        let g = self.state.graph.lock().unwrap();
        g.get(node)?.input_by_name(name).map(|s| s.uid)
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
        {
            let mut g = state.graph.lock().unwrap();
            g.add_new_node("Box", [10.0, 20.0], &state.registry).unwrap();
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
            g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
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
            g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
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
            g.add_new_node("Extrude", [0.0, 0.0], &state.registry).unwrap()
        };
        let model = AppStateModel::new(state);
        let nodes = ne::NodeGraphModel::nodes(&model);
        let n = nodes.iter().find(|n| n.id.0 == id.0).unwrap();
        assert_eq!(n.outputs.len(), 1);
        assert_eq!(n.outputs[0].name, "Geometry");
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
        let height_input = n.inputs.iter().find(|s| s.name == "Height").unwrap();
        assert_eq!(height_input.display_label.as_deref(), Some("Height"));
    }

    #[test]
    fn extrude_color_property_round_trips_as_color_value() {
        let state = fixture();
        let _id = {
            let mut g = state.graph.lock().unwrap();
            g.add_new_node("Extrude", [0.0, 0.0], &state.registry).unwrap()
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
