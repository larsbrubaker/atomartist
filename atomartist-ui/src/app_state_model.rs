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

use atomartist_lib::graph::graph::Noodle;
use atomartist_lib::graph::node::{NodeId as DomainNodeId, PortValue};
use atomartist_lib::graph::socket::SocketUid;
use atomartist_lib::graph::undo_commands::{
    AddNodeCmd, BatchCmd, ChangePropertyCmd, ConnectCmd, DisconnectCmd,
    MoveNodeCmd, RemoveNodeCmd,
};
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

    /// A string maps to the canvas's inline-editable `Text` variant
    /// only when its schema editor is *explicitly* the single-line
    /// string editor. This is an allowlist, not a denylist: enum-backed
    /// strings (`EnumDropdown` / `EnumButtons` / `EnumTabs`) also carry
    /// a `PortValue::StringVal`, and surfacing those as free-text would
    /// let the user type a value outside the enum's variant set. Read-
    /// only, multi-line, and the default `Display` editor likewise stay
    /// display-only (`Other`). Node authors opt a string into inline
    /// editing by declaring `EditorKind::StringSingleLine`.
    fn string_is_inline_editable(editor: &EditorKind) -> bool {
        matches!(editor, EditorKind::StringSingleLine)
    }

    fn property_value_to_ne(v: &PortValue, editor: &EditorKind) -> ne::PropertyValue {
        match v {
            PortValue::Number(n) => ne::PropertyValue::Number(*n),
            PortValue::Bool(b) => ne::PropertyValue::Bool(*b),
            PortValue::Color(c) => ne::PropertyValue::Color(*c),
            PortValue::StringVal(s) if Self::string_is_inline_editable(editor) => {
                ne::PropertyValue::Text(s.as_str().to_string())
            }
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
        let ag = self.state.active_graph();
        let g = ag.lock().unwrap();
        let reg = &self.state.registry;
        g.nodes()
            .filter_map(|n| {
                let def = reg.get(&n.type_id)?;
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
                // Sockets whose bound property is currently filtered
                // out should disappear too — otherwise the canvas
                // renders them as bare input rows at the top of the
                // node body. Compute that set up-front and use it to
                // prune the input list before SocketView translation.
                let hidden_sockets: std::collections::HashSet<String> = def
                    .properties()
                    .into_iter()
                    .filter(|p| !def.row_visible(&p.name, &snapshot))
                    .filter_map(|p| p.bound_input.as_ref().map(|s| s.to_string()))
                    .collect();
                let inputs: Vec<ne::SocketView> = n
                    .inputs
                    .iter()
                    .filter(|s| !hidden_sockets.contains(s.name.as_ref()))
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
                        // Let the node vary a property's editor with its
                        // live values (e.g. NumberConst's value slider
                        // tracking the instance's min/max/step). Falls
                        // back to the static schema editor when the node
                        // has no override for this property.
                        let editor = def
                            .editor_override(&p.name, &snapshot)
                            .unwrap_or_else(|| p.editor.clone());
                        let (min, max) = match editor.numeric_range() {
                            (Some(mn), Some(mx)) => (Some(mn), Some(mx)),
                            _ => (p.min, p.max),
                        };
                        ne::PropertyView {
                            name: p.name.to_string(),
                            display_label: p.label.as_ref().map(|l| l.to_string()),
                            current: Self::property_value_to_ne(&current, &editor),
                            min,
                            max,
                            bound_input: p.bound_input.as_ref().map(|s| s.to_string()),
                            editor: Self::editor_kind_to_ne(&editor),
                            // Forward the full schema-side editor so
                            // the per-kind row renderers (`paint_row`)
                            // can mount the right pill, toggle,
                            // swatch, etc.
                            editor_kind: Some(editor),
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
        let ag = self.state.active_graph();
        let g = ag.lock().unwrap();
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
            // Graph-interface / value-source nodes (NodeDesigner uses a
            // magenta family, #8b4c8b, for these).
            "Input" => Color::rgb(0.545, 0.298, 0.545),
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
        let domain_id = Self::from_ne(id);
        let undo = self.state.active_undo();
        // Drag coalescing: a single user drag fires this method many
        // times per second. Merge into the top-of-stack `MoveNodeCmd`
        // when the target matches so the whole drag is one undo step.
        let coalesced = undo.lock().unwrap().try_coalesce_last(|top| {
            if let Some(cmd) = top.as_any_mut().downcast_mut::<MoveNodeCmd>() {
                if cmd.id == domain_id {
                    cmd.extend_into(pos);
                    return true;
                }
            }
            false
        });
        if !coalesced {
            let cmd = MoveNodeCmd::new(self.state.active_graph(), domain_id, pos);
            undo.lock().unwrap().add_and_do(Box::new(cmd));
        }
    }

    fn add_node(&mut self, type_id: &str, pos: [f64; 2]) -> Option<ne::NodeId> {
        // Build the node off-graph via `add_new_node`, then pull it back
        // out so the AddNodeCmd owns the full NodeInstance for redo.
        // Wasteful but engine-side simpler than introducing a separate
        // build-without-insert API — revisit if profiling shows this
        // matters.
        let ag = self.state.active_graph();
        let (id, node) = {
            let mut g = ag.lock().unwrap();
            let id = g.add_new_node(type_id, pos, &self.state.registry).ok()?;
            let (node, _detached) = g.remove_node(id).ok()?;
            (id, node)
        };
        let cmd = AddNodeCmd::new(self.state.active_graph(), node);
        self.state.active_undo().lock().unwrap().add_and_do(Box::new(cmd));
        self.state.schedule_evaluate_after_edit();
        Some(Self::to_ne(id))
    }

    fn remove_node(&mut self, id: ne::NodeId) {
        let domain_id = Self::from_ne(id);
        let cmd = RemoveNodeCmd::new(self.state.active_graph(), domain_id);
        self.state.active_undo().lock().unwrap().add_and_do(Box::new(cmd));
        self.state.schedule_evaluate_after_edit();
    }

    /// Remove a whole selection as **one** undo step.
    ///
    /// agg-gui's `NodeEditor::delete_selection` — the single funnel for
    /// the Delete/Backspace key, the right-click menu's "Delete", and
    /// the Edit-menu `DeleteSelection` command — hands the entire
    /// selection here in one call. Grouping the per-node
    /// [`RemoveNodeCmd`]s into a [`BatchCmd`] is what makes a 5-node
    /// delete take one Ctrl+Z instead of five.
    ///
    /// NodeDesigner parity: a multi-delete is a batch named
    /// "Delete N Nodes"; a single-node delete stays a bare "Remove Node"
    /// so the undo menu keeps reading naturally.
    fn remove_nodes(&mut self, ids: &[ne::NodeId]) {
        match ids {
            [] => {}
            [only] => self.remove_node(*only),
            many => {
                let children: Vec<Box<dyn agg_gui::undo::UndoRedoCommand>> = many
                    .iter()
                    .map(|id| {
                        Box::new(RemoveNodeCmd::new(
                            self.state.active_graph(),
                            Self::from_ne(*id),
                        )) as Box<dyn agg_gui::undo::UndoRedoCommand>
                    })
                    .collect();
                let batch = BatchCmd::new(format!("Delete {} Nodes", many.len()), children);
                self.state.active_undo().lock().unwrap().add_and_do(Box::new(batch));
                self.state.schedule_evaluate_after_edit();
            }
        }
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
        let ag = self.state.active_graph();
        let undo = self.state.active_undo();
        // Dry-run the connect on a peek lock to figure out which path
        // (clean Connect / Replace-existing / Reject) we'll take —
        // without mutating. Pre-collect existing noodles to the same
        // input so the Replace branch knows what to capture for the
        // undo batch.
        let (existing_at_target, decision) = {
            let g = ag.lock().unwrap();
            let existing: Vec<Noodle> = g
                .noodles()
                .iter()
                .filter(|n| n.to == noodle.to)
                .copied()
                .collect();
            // Try a validate-only path — we'll actually perform via cmd
            // below. Read `validate_input_connection` via registry; if
            // the noodle would otherwise succeed we treat input-already
            // -connected as Replace.
            let result_kind = if existing.is_empty() {
                // We can't know for sure without calling connect, but
                // the bridge's `try_add_noodle` already filters
                // socket-direction / cycle / type at the widget
                // level for the most common rejections. Best-effort:
                // assume Connected, fall back to Rejected if the
                // command's do_it actually fails.
                ne::NoodleResult::Connected
            } else {
                ne::NoodleResult::Replaced
            };
            (existing, result_kind)
        };

        let result = match decision {
            ne::NoodleResult::Connected => {
                let cmd = ConnectCmd::new(
                    ag.clone(),
                    self.state.registry.clone(),
                    noodle,
                );
                undo.lock().unwrap().add_and_do(Box::new(cmd));
                // Check it actually landed (validation failures inside
                // ConnectCmd::do_it set succeeded=false; the noodle
                // count tells us cleanly).
                if ag.lock().unwrap().noodles().contains(&noodle) {
                    ne::NoodleResult::Connected
                } else {
                    // Pop the no-op cmd off the stack so users don't
                    // see a phantom undo step for a rejected connect.
                    undo.lock().unwrap().undo();
                    ne::NoodleResult::Rejected
                }
            }
            ne::NoodleResult::Replaced => {
                // Batch: disconnect each existing noodle to the target
                // input, then connect the new noodle. Single undo step
                // restores everything.
                let mut children: Vec<Box<dyn agg_gui::undo::UndoRedoCommand>> = Vec::new();
                for existing in &existing_at_target {
                    children.push(Box::new(DisconnectCmd::new(
                        ag.clone(),
                        self.state.registry.clone(),
                        *existing,
                    )));
                }
                children.push(Box::new(ConnectCmd::new(
                    ag.clone(),
                    self.state.registry.clone(),
                    noodle,
                )));
                let batch = BatchCmd::new("Replace Connection", children);
                undo.lock().unwrap().add_and_do(Box::new(batch));
                if ag.lock().unwrap().noodles().contains(&noodle) {
                    ne::NoodleResult::Replaced
                } else {
                    undo.lock().unwrap().undo();
                    ne::NoodleResult::Rejected
                }
            }
            _ => ne::NoodleResult::Rejected,
        };
        if matches!(result, ne::NoodleResult::Connected | ne::NoodleResult::Replaced) {
            self.state.schedule_evaluate_after_edit();
        }
        result
    }

    fn remove_noodle(
        &mut self,
        from_node: ne::NodeId,
        from_socket: &str,
        to_node: ne::NodeId,
        to_socket: &str,
    ) -> bool {
        let from_uid = match self.lookup_output_uid(Self::from_ne(from_node), from_socket) {
            Some(u) => u,
            None => return false,
        };
        let to_uid = match self.lookup_input_uid(Self::from_ne(to_node), to_socket) {
            Some(u) => u,
            None => return false,
        };
        let noodle = Noodle::new(
            Self::from_ne(from_node),
            from_uid,
            Self::from_ne(to_node),
            to_uid,
        );
        // Only push the command if the noodle actually exists — agg-gui
        // sometimes asks for removal of a noodle that's already gone
        // (multi-event drag races). A phantom undo step would be
        // surprising to the user.
        let ag = self.state.active_graph();
        if !ag.lock().unwrap().noodles().contains(&noodle) {
            return false;
        }
        let cmd = DisconnectCmd::new(
            ag,
            self.state.registry.clone(),
            noodle,
        );
        self.state.active_undo().lock().unwrap().add_and_do(Box::new(cmd));
        self.state.schedule_evaluate_after_edit();
        true
    }

    fn set_property(&mut self, id: ne::NodeId, name: &str, value: ne::PropertyValue) {
        let domain_id = Self::from_ne(id);
        let port_value = match value {
            ne::PropertyValue::Number(n) => PortValue::Number(n),
            ne::PropertyValue::Bool(b) => PortValue::Bool(b),
            ne::PropertyValue::Color(c) => PortValue::Color(c),
            ne::PropertyValue::Text(s) => PortValue::StringVal(Arc::new(s)),
            ne::PropertyValue::Other { .. } => return,
        };
        let name_arc: Arc<str> = Arc::<str>::from(name);
        let undo = self.state.active_undo();
        // Slider-coalescing — see MoveNodeCmd::extend_into for the
        // matching node-drag case. Pixel-rate property writes merge
        // into the top-of-stack ChangePropertyCmd as long as the
        // (id, name) tuple matches.
        let coalesced = {
            let name_for_pred = name_arc.clone();
            let value_for_pred = port_value.clone();
            undo.lock().unwrap().try_coalesce_last(|top| {
                if let Some(cmd) = top.as_any_mut().downcast_mut::<ChangePropertyCmd>() {
                    if cmd.id == domain_id && cmd.name == name_for_pred {
                        cmd.extend_into(value_for_pred.clone());
                        return true;
                    }
                }
                false
            })
        };
        if !coalesced {
            let cmd = ChangePropertyCmd::new(
                self.state.active_graph(),
                domain_id,
                name_arc,
                port_value,
            )
            .with_registry(self.state.registry.clone());
            undo.lock().unwrap().add_and_do(Box::new(cmd));
        }
        self.state.schedule_evaluate_after_edit();
    }

    fn on_canvas_zoom_changed(&mut self, zoom: f64) {
        *self.state.canvas_zoom.lock().unwrap() = zoom;
    }

    /// Mirror the canvas pan so widgets outside the editor (the
    /// favorites bar's drag-insert controller) can convert a pointer
    /// position into canvas space at the live pan / zoom.
    fn on_canvas_pan_changed(&mut self, pan: [f64; 2]) {
        *self.state.canvas_pan.lock().unwrap() = pan;
    }

    fn on_primary_selection_changed(&mut self, id: Option<ne::NodeId>) {
        let domain = id.map(Self::from_ne);
        self.state.set_selection(domain);
        // Product spec: nothing renders in the 3-D viewport unless it is
        // wired into the Output node, whose merged `__display__` geometry
        // is what the viewport shows. We deliberately do NOT pin the
        // selected node as the viewport display here — the previous
        // preview-on-selection behaviour made an unconnected primitive
        // appear in the viewport the moment it was clicked, violating that
        // rule. `display_node` remains available as an explicit,
        // programmatic override (the starter graph pins Output; tests pin
        // a specific node) but is no longer driven by canvas selection.
    }

    /// Double-click on a node title bar. Drill into the node when it's a
    /// component (a `SubgraphNodeDef`), returning `true` so the editor
    /// suppresses its default collapse toggle; return `false` for plain
    /// nodes so the collapse behaviour is preserved.
    ///
    /// Safe under the editor's model-mutex hold: `enter_component` locks
    /// only `AppState`'s graph / stack mutexes, never this model's.
    fn on_node_activated(&mut self, node: ne::NodeId) -> bool {
        self.state.enter_component(Self::from_ne(node))
    }
}

impl AppStateModel {
    fn lookup_output_uid(&self, node: DomainNodeId, name: &str) -> Option<SocketUid> {
        let ag = self.state.active_graph();
        let g = ag.lock().unwrap();
        g.get(node)?.output_by_name(name).map(|s| s.uid)
    }

    fn lookup_input_uid(&self, node: DomainNodeId, name: &str) -> Option<SocketUid> {
        let ag = self.state.active_graph();
        let g = ag.lock().unwrap();
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
mod tests;
