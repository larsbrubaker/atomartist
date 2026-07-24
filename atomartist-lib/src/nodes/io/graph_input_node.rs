//! GraphInput — a *typed* declarative input port for a subgraph.
//!
//! When the host graph is wrapped as a `SubgraphNodeDef`, each
//! GraphInput contributes one input socket on the resulting subgraph
//! node, named by the `name` property and typed by the `port_type`
//! property (Geometry / Number / Boolean / String / Color). Mirrors
//! NodeDesigner's Group-Input concept: a subgraph parameter with a
//! declared type and a per-type default value.
//!
//! Standalone (non-subgraph) usage acts as a passthrough:
//!   - If `_injected` carries a value (written by
//!     `SubgraphNodeDef::evaluate` before running the cloned template),
//!     that value is emitted verbatim.
//!   - Otherwise the node emits the typed default matching `port_type`
//!     (Number → `default_number`, Boolean → `default_bool`, …). A
//!     Geometry port has no scalar default and emits `None`.
//!
//! Changing `port_type` retypes the `out` socket via
//! [`NodeDef::on_property_changed`]; the graph then disconnects any
//! noodle the retype left type-incompatible.

use std::sync::Arc;

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeProperties,
    NodeRegistry, PropDef, PropertyChangedCtx,
};
use crate::socket_types::SocketType;

pub struct GraphInputNode;

/// Default `port_type` — a fresh GraphInput is a Geometry passthrough,
/// matching the pre-typed behaviour and the standalone marker use.
const DEFAULT_PORT_TYPE: &str = "Geometry";

/// Opaque white default for the Color port — consistent with the
/// ColorConst / geometry-node neutral color.
const DEFAULT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Map a `port_type` string to the socket type its `out` socket carries.
/// Unknown / legacy strings fall back to Geometry3d.
fn socket_type_for(port_type: &str) -> SocketType {
    match port_type {
        "Number" => SocketType::Number,
        "Boolean" => SocketType::Bool,
        "String" => SocketType::StringVal,
        "Color" => SocketType::Color,
        // "Geometry" and any unknown / legacy value → Geometry.
        _ => SocketType::Geometry3d,
    }
}

/// Read the current `port_type` string from a property map, defaulting
/// to [`DEFAULT_PORT_TYPE`].
fn port_type_of(props: &NodeProperties) -> String {
    match props.get("port_type") {
        PortValue::StringVal(s) => s.as_str().to_string(),
        _ => DEFAULT_PORT_TYPE.to_string(),
    }
}

impl NodeDef for GraphInputNode {
    fn type_id(&self) -> &'static str { "GraphInput" }
    fn display_name(&self) -> &'static str { "Graph Input" }
    fn category(&self) -> &'static str { "I/O" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        // Seed the output socket type from the port_type seed default
        // ("Geometry" → Geometry3d). Changing port_type later retypes
        // it through `on_property_changed`.
        InstanceTemplate::builder(alloc)
            .output("out", socket_type_for(DEFAULT_PORT_TYPE))
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            // The parameter name is user-facing text, so opt into inline
            // canvas editing (StringSingleLine).
            PropDef::new("name", PortValue::StringVal(Arc::new("input".into())))
                .with_editor(EditorKind::StringSingleLine),
            // The declared port type — drives the `out` socket type and
            // which typed default is emitted / shown.
            PropDef::new(
                "port_type",
                PortValue::StringVal(Arc::new(DEFAULT_PORT_TYPE.into())),
            )
            .with_editor(EditorKind::EnumDropdown {
                variants: vec![
                    "Geometry".into(),
                    "Number".into(),
                    "Boolean".into(),
                    "String".into(),
                    "Color".into(),
                ],
            }),
            // Per-type default values. `row_visible` shows only the one
            // matching the current `port_type`.
            PropDef::new("default_number", PortValue::Number(0.0))
                .with_range(-10000.0, 10000.0),
            PropDef::new("default_bool", PortValue::Bool(false))
                .with_editor(EditorKind::Toggle),
            PropDef::new("default_string", PortValue::StringVal(Arc::new("".into())))
                .with_editor(EditorKind::StringSingleLine),
            PropDef::new("default_color", PortValue::Color(DEFAULT_COLOR))
                .with_editor(EditorKind::ColorPicker),
            // Set by SubgraphNodeDef::evaluate before running the
            // executor on the cloned template; standalone (non-subgraph)
            // usage leaves it `None` and the node emits the typed default.
            PropDef::new("_injected", PortValue::None).hidden(),
        ]
    }

    /// Show only the default-value row matching the current `port_type`.
    /// Geometry has no scalar default, so none of the `default_*` rows
    /// show for it.
    fn row_visible(&self, name: &str, props: &NodeProperties) -> bool {
        let port_type = port_type_of(props);
        match name {
            "default_number" => port_type == "Number",
            "default_bool" => port_type == "Boolean",
            "default_string" => port_type == "String",
            "default_color" => port_type == "Color",
            // `name`, `port_type`, `_injected`, advanced toggle, etc.
            // fall through to the declarative VisibleWhen rule.
            _ => self.default_row_visible(name, props),
        }
    }

    /// When `port_type` changes, retype the `out` socket to match. The
    /// graph revalidates this node's noodles after the hook returns and
    /// drops any left type-incompatible.
    fn on_property_changed(&self, ctx: &mut PropertyChangedCtx) {
        if ctx.property != "port_type" {
            return;
        }
        let ty = match ctx.property_value("port_type") {
            Some(PortValue::StringVal(s)) => socket_type_for(s.as_str()),
            _ => socket_type_for(DEFAULT_PORT_TYPE),
        };
        let out_uid = ctx
            .graph
            .get(ctx.this_node)
            .and_then(|n| n.output_by_name("out").map(|s| s.uid));
        if let Some(uid) = out_uid {
            let _ = ctx.graph.retype_socket(ctx.this_node, uid, ty);
        }
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        // An injected value (from the wrapping subgraph) always wins.
        let injected = ctx.properties.get("_injected").clone();
        let value = if injected != PortValue::None {
            injected
        } else {
            // No injection: emit the typed default for the port type.
            match port_type_of(ctx.properties).as_str() {
                "Number" => PortValue::Number(ctx.properties.number("default_number", 0.0)),
                "Boolean" => PortValue::Bool(ctx.properties.bool_("default_bool", false)),
                "String" => match ctx.properties.get("default_string") {
                    PortValue::StringVal(s) => PortValue::StringVal(s.clone()),
                    _ => PortValue::StringVal(Arc::new("".into())),
                },
                "Color" => PortValue::Color(ctx.properties.color("default_color", DEFAULT_COLOR)),
                // Geometry (and unknown / legacy) has no scalar default.
                _ => PortValue::None,
            }
        };
        let mut out = NodeOutputs::default();
        out.set("out", value);
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(GraphInputNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::graph::Noodle;
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::graph::undo_commands::ChangePropertyCmd;
    use crate::graph::Graph;
    use crate::registry::{NodeInputs, NodeProperties};
    use agg_gui::undo::UndoRedoCommand;
    use std::sync::Mutex;

    /// Evaluate a GraphInput with the given properties and return the
    /// value emitted on `out`.
    fn eval_out(props: &NodeProperties) -> PortValue {
        let def = GraphInputNode;
        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), def.type_id().to_string(), [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let inputs = NodeInputs::default();
        let ctx = EvalCtx { instance: &inst, properties: props, inputs: &inputs };
        def.evaluate(&ctx).unwrap().by_name.get("out").cloned().unwrap()
    }

    fn props_with(pairs: &[(&str, PortValue)]) -> NodeProperties {
        let mut p = NodeProperties::default();
        for (k, v) in pairs {
            p.insert(*k, v.clone());
        }
        p
    }

    fn str_val(s: &str) -> PortValue {
        PortValue::StringVal(Arc::new(s.into()))
    }

    #[test]
    fn instantiate_defaults_to_geometry_output() {
        let def = GraphInputNode;
        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        let out = tpl.outputs.iter().find(|s| s.name.as_ref() == "out").unwrap();
        assert_eq!(out.socket_type, SocketType::Geometry3d);
    }

    #[test]
    fn evaluate_emits_each_typed_default_when_uninjected() {
        // Geometry → None (no scalar default).
        let geo = props_with(&[("port_type", str_val("Geometry"))]);
        assert_eq!(eval_out(&geo), PortValue::None);

        // Number → default_number.
        let num = props_with(&[
            ("port_type", str_val("Number")),
            ("default_number", PortValue::Number(4.5)),
        ]);
        assert_eq!(eval_out(&num), PortValue::Number(4.5));

        // Boolean → default_bool.
        let boolean = props_with(&[
            ("port_type", str_val("Boolean")),
            ("default_bool", PortValue::Bool(true)),
        ]);
        assert_eq!(eval_out(&boolean), PortValue::Bool(true));

        // String → default_string.
        let string = props_with(&[
            ("port_type", str_val("String")),
            ("default_string", str_val("hello")),
        ]);
        assert_eq!(eval_out(&string), str_val("hello"));

        // Color → default_color.
        let color = props_with(&[
            ("port_type", str_val("Color")),
            ("default_color", PortValue::Color([0.1, 0.2, 0.3, 1.0])),
        ]);
        assert_eq!(eval_out(&color), PortValue::Color([0.1, 0.2, 0.3, 1.0]));
    }

    #[test]
    fn injected_value_wins_over_typed_default() {
        // Even with port_type = Number and a default set, an injected
        // value is emitted verbatim.
        let p = props_with(&[
            ("port_type", str_val("Number")),
            ("default_number", PortValue::Number(4.5)),
            ("_injected", PortValue::Number(99.0)),
        ]);
        assert_eq!(eval_out(&p), PortValue::Number(99.0));

        // Injected geometry-ish value survives even for a Geometry port.
        let g = props_with(&[
            ("port_type", str_val("Geometry")),
            ("_injected", PortValue::Number(7.0)),
        ]);
        assert_eq!(eval_out(&g), PortValue::Number(7.0));
    }

    #[test]
    fn row_visible_shows_only_matching_default() {
        let def = GraphInputNode;
        let cases = [
            ("Geometry", None),
            ("Number", Some("default_number")),
            ("Boolean", Some("default_bool")),
            ("String", Some("default_string")),
            ("Color", Some("default_color")),
        ];
        let all_defaults =
            ["default_number", "default_bool", "default_string", "default_color"];
        for (port_type, visible) in cases {
            let props = props_with(&[("port_type", str_val(port_type))]);
            for row in all_defaults {
                let expect = visible == Some(row);
                assert_eq!(
                    def.row_visible(row, &props),
                    expect,
                    "port_type={port_type}, row={row}",
                );
            }
            // `name` and `port_type` are always visible.
            assert!(def.row_visible("name", &props));
            assert!(def.row_visible("port_type", &props));
            // `_injected` stays hidden.
            assert!(!def.row_visible("_injected", &props));
        }
    }

    /// Changing `port_type` via the undoable command path retypes the
    /// `out` socket; undo restores it.
    #[test]
    fn port_type_change_retypes_out_and_undo_restores() {
        let mut reg = NodeRegistry::new();
        register(&mut reg);
        let reg = Arc::new(reg);
        let g = Arc::new(Mutex::new(Graph::new()));

        let (id, out_uid) = {
            let mut graph = g.lock().unwrap();
            let id = graph.add_new_node("GraphInput", [0.0, 0.0], &reg).unwrap();
            let out_uid = graph.get(id).unwrap().output_by_name("out").unwrap().uid;
            (id, out_uid)
        };

        let out_type = |graph: &Graph| {
            graph.get(id).unwrap().output_by_uid(out_uid).unwrap().socket_type
        };
        assert_eq!(out_type(&g.lock().unwrap()), SocketType::Geometry3d);

        let mut cmd = ChangePropertyCmd::new(
            g.clone(),
            id,
            "port_type",
            str_val("Number"),
        )
        .with_registry(reg.clone());
        cmd.do_it();
        assert_eq!(
            out_type(&g.lock().unwrap()),
            SocketType::Number,
            "port_type → Number retypes out socket",
        );

        cmd.undo_it();
        assert_eq!(
            out_type(&g.lock().unwrap()),
            SocketType::Geometry3d,
            "undo restores the Geometry3d out socket",
        );

        cmd.do_it();
        assert_eq!(out_type(&g.lock().unwrap()), SocketType::Number, "redo re-applies");
    }

    /// A retype that makes a connected noodle incompatible drops it, and
    /// undo restores the noodle (revalidation round-trip through the
    /// command path).
    #[test]
    fn port_type_change_disconnects_incompatible_noodle() {
        let mut reg = NodeRegistry::new();
        crate::nodes::register_all(&mut reg);
        let reg = Arc::new(reg);
        let g = Arc::new(Mutex::new(Graph::new()));

        // GraphInput.out(Geometry3d) → Output's wildcard slot? Instead
        // wire into a Transform input (Geometry3d) which becomes
        // incompatible when out retypes to Number.
        let (gin, xform, out_uid, noodle) = {
            let mut graph = g.lock().unwrap();
            let gin = graph.add_new_node("GraphInput", [0.0, 0.0], &reg).unwrap();
            let xform = graph.add_new_node("Transform", [200.0, 0.0], &reg).unwrap();
            let out_uid = graph.get(gin).unwrap().output_by_name("out").unwrap().uid;
            let in_uid = graph.get(xform).unwrap().input_by_name("input").unwrap().uid;
            let noodle = Noodle::new(gin, out_uid, xform, in_uid);
            graph.connect(noodle, &reg).unwrap();
            (gin, xform, out_uid, noodle)
        };
        let _ = xform;
        let _ = out_uid;
        assert_eq!(g.lock().unwrap().noodle_count(), 1);

        let mut cmd = ChangePropertyCmd::new(g.clone(), gin, "port_type", str_val("Number"))
            .with_registry(reg.clone());
        cmd.do_it();
        assert!(
            !g.lock().unwrap().noodles().contains(&noodle),
            "Geometry→Number retype drops the now-incompatible noodle",
        );

        cmd.undo_it();
        assert!(
            g.lock().unwrap().noodles().contains(&noodle),
            "undo restores the disconnected noodle",
        );
    }
}
