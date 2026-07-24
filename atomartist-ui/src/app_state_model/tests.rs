//! Unit tests for [`super::AppStateModel`] — the adapter between
//! AtomArtist's `AppState` and the `agg-gui-node-editor` `NodeGraphModel`
//! trait. Split out of `app_state_model.rs` to keep that file under the
//! project's 800-line cap. Undo round-trip coverage lives separately in
//! `atomartist-ui/tests/undo_round_trip.rs`. Component drill-in coverage
//! lives in the `drill` submodule so this file stays under the cap.

use super::*;
use atomartist_lib::nodes;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::Graph;

mod drill;

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
fn selecting_unconnected_geometry_node_does_not_pin_viewport_display() {
    // Product spec: nothing renders in the 3-D viewport unless it is
    // wired into the Output node. Selecting a bare primitive sitting on
    // the canvas must NOT pin it as the viewport's display target — the
    // old preview-on-selection behaviour let an unconnected body appear
    // in the viewport, violating the "only Output renders" rule.
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("Box", [0.0, 0.0], &state.registry).unwrap()
    };
    let mut model = AppStateModel::new(state);
    ne::NodeGraphModel::on_primary_selection_changed(&mut model, Some(ne::NodeId(id.0)));
    assert_eq!(
        *model.state.display_node.lock().unwrap(),
        None,
        "selecting an unconnected geometry node must not pin it as the viewport display",
    );
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
fn enum_editor_string_property_stays_display_only_not_editable_text() {
    // A StringVal property whose schema editor is an enum
    // (EnumDropdown / EnumButtons / EnumTabs) must NOT surface as an
    // inline-editable `Text` value — free-text entry would let the
    // user type a value outside the enum's variant set. It must
    // round-trip as a display-only `Other` instead. Mirrors the
    // AlignAxis fixture in atomartist-lib/src/registry.rs.
    use atomartist_lib::graph::socket::SocketUidAlloc;
    use atomartist_lib::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, PropDef,
    };

    struct EnumStringNode;
    impl NodeDef for EnumStringNode {
        fn type_id(&self) -> &'static str {
            "EnumStringTest"
        }
        fn category(&self) -> &'static str {
            "Test"
        }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc).build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
        fn properties(&self) -> Vec<PropDef> {
            vec![
                PropDef::new("mode", PortValue::StringVal(Arc::new("A".into())))
                    .with_editor(EditorKind::EnumDropdown {
                        variants: vec!["A".into(), "B".into(), "C".into()],
                    }),
            ]
        }
    }

    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    reg.register(EnumStringNode);
    let state = AppState::new(Graph::new(), reg);
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("EnumStringTest", [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let model = AppStateModel::new(state);
    let nodes = ne::NodeGraphModel::nodes(&model);
    let n = nodes.iter().find(|n| n.id.0 == id.0).unwrap();
    let mode = n.properties.iter().find(|p| p.name == "mode").unwrap();
    match &mode.current {
        ne::PropertyValue::Other { display } => assert_eq!(display, "A"),
        other => panic!(
            "enum-backed string must stay display-only Other, got {:?}",
            other
        ),
    }
    assert!(
        !mode.current.is_editable_inline(),
        "enum-backed string must NOT be inline-editable on the canvas",
    );
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

#[test]
fn string_property_surfaces_editable_text_and_round_trips_with_undo() {
    // StringConst's `value` uses `EditorKind::StringSingleLine`, so
    // it must reach the canvas as an inline-editable `Text` value
    // (not a display-only `Other`), and `set_property` must commit
    // a `StringVal` through the same undoable ChangePropertyCmd path
    // that Number / Bool / Color use.
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("StringConst", [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let mut model = AppStateModel::new(state);

    // 1. Snapshot surfaces an editable Text value.
    let nodes = ne::NodeGraphModel::nodes(&model);
    let n = nodes.iter().find(|n| n.id.0 == id.0).unwrap();
    let value = n.properties.iter().find(|p| p.name == "value").unwrap();
    match &value.current {
        ne::PropertyValue::Text(s) => assert_eq!(s, ""),
        other => panic!("expected editable Text, got {:?}", other),
    }
    assert!(
        value.current.is_editable_inline(),
        "string value must be inline-editable on the canvas",
    );

    // 2. Editing commits a StringVal into the graph.
    ne::NodeGraphModel::set_property(
        &mut model,
        ne::NodeId(id.0),
        "value",
        ne::PropertyValue::Text("hello".to_string()),
    );
    assert_eq!(
        model.state.undo.lock().unwrap().undo_name(),
        Some("Change Property"),
    );
    {
        let g = model.state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("value") {
            Some(PortValue::StringVal(s)) => assert_eq!(s.as_str(), "hello"),
            other => panic!("value not committed as StringVal: {:?}", other),
        }
    }

    // 3. Undo restores the previous (empty) string.
    model.state.undo.lock().unwrap().undo();
    {
        let g = model.state.graph.lock().unwrap();
        match g.get(id).unwrap().properties.get("value") {
            Some(PortValue::StringVal(s)) => assert_eq!(
                s.as_str(),
                "",
                "undo must restore the pre-edit string",
            ),
            other => panic!("expected StringVal after undo, got {:?}", other),
        }
    }
}

#[test]
fn property_change_fires_on_property_changed_hook_and_reverts_on_undo() {
    // A property edit made through the production `set_property` path
    // must fire the type's `on_property_changed` hook — that only
    // happens because the adapter now attaches the registry to
    // `ChangePropertyCmd` (`.with_registry`). Register a node whose
    // output socket retypes when its `as_geom` property flips, drive
    // the edit through `AppStateModel::set_property`, and assert the
    // socket retyped; undo must re-fire the hook and revert it.
    use atomartist_lib::graph::socket::SocketUidAlloc;
    use atomartist_lib::registry::{
        EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, PropDef,
        PropertyChangedCtx,
    };

    struct RetypeNode;
    impl NodeDef for RetypeNode {
        fn type_id(&self) -> &'static str {
            "RetypeHookTest"
        }
        fn category(&self) -> &'static str {
            "Test"
        }
        fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
            InstanceTemplate::builder(alloc)
                .output("out", SocketType::Number)
                .property("as_geom", PortValue::Bool(false))
                .build()
        }
        fn evaluate(&self, _ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
            Ok(NodeOutputs::default())
        }
        fn properties(&self) -> Vec<PropDef> {
            vec![PropDef::new("as_geom", PortValue::Bool(false))]
        }
        fn on_property_changed(&self, ctx: &mut PropertyChangedCtx) {
            if ctx.property != "as_geom" {
                return;
            }
            let ty = match ctx.property_value("as_geom") {
                Some(PortValue::Bool(true)) => SocketType::Geometry3d,
                _ => SocketType::Number,
            };
            let out_uid = ctx
                .graph
                .get(ctx.this_node)
                .and_then(|n| n.outputs.first().map(|s| s.uid));
            if let Some(uid) = out_uid {
                let _ = ctx.graph.retype_socket(ctx.this_node, uid, ty);
            }
        }
    }

    let mut reg = NodeRegistry::new();
    nodes::register_all(&mut reg);
    reg.register(RetypeNode);
    let state = AppState::new(Graph::new(), reg);
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("RetypeHookTest", [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let mut model = AppStateModel::new(state);

    let out_ty = |m: &AppStateModel| {
        m.state.graph.lock().unwrap().get(id).unwrap().outputs[0].socket_type
    };
    assert_eq!(out_ty(&model), SocketType::Number);

    // Production path: fires the hook only if the registry is attached.
    ne::NodeGraphModel::set_property(
        &mut model,
        ne::NodeId(id.0),
        "as_geom",
        ne::PropertyValue::Bool(true),
    );
    assert_eq!(
        out_ty(&model),
        SocketType::Geometry3d,
        "on_property_changed must fire through the production set_property path",
    );

    // Undo re-fires the hook, restoring the Number output type.
    model.state.undo.lock().unwrap().undo();
    assert_eq!(
        out_ty(&model),
        SocketType::Number,
        "undo must re-fire the hook and revert the socket type",
    );
}

#[test]
fn number_const_value_slider_range_follows_min_max_props() {
    // NumberConst's `value` PropertyView must carry the slider range
    // resolved from the instance's live `min`/`max` props (via
    // `editor_override` -> `numeric_range`), so editing the bounds
    // through the production path updates the value row's range.
    let state = fixture();
    let id = {
        let mut g = state.graph.lock().unwrap();
        g.add_new_node("NumberConst", [0.0, 0.0], &state.registry)
            .unwrap()
    };
    let mut model = AppStateModel::new(state);
    ne::NodeGraphModel::set_property(
        &mut model,
        ne::NodeId(id.0),
        "min",
        ne::PropertyValue::Number(-3.0),
    );
    ne::NodeGraphModel::set_property(
        &mut model,
        ne::NodeId(id.0),
        "max",
        ne::PropertyValue::Number(12.0),
    );

    let nodes = ne::NodeGraphModel::nodes(&model);
    let n = nodes.iter().find(|n| n.id.0 == id.0).unwrap();
    let value = n.properties.iter().find(|p| p.name == "value").unwrap();
    assert_eq!(
        value.min,
        Some(-3.0),
        "value slider min must follow the instance's min prop",
    );
    assert_eq!(
        value.max,
        Some(12.0),
        "value slider max must follow the instance's max prop",
    );
}
