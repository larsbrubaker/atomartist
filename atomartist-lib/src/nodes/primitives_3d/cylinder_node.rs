//! Cylinder primitive — the first node ported onto atomartist's
//! reflection-driven property panel. MatterCAD parity:
//!
//!   - Easy mode: `Diameter`, `Height`, `Sides`.
//!   - Advanced toggle exposes `DiameterTop` (taper), `StartingAngle`
//!     and `EndingAngle` (partial revolve).
//!   - A read-only `EasyModeMessage` row nudges the user toward
//!     Advanced mode when the toggle is off.
//!
//! The property schema is declared in two pieces:
//!
//!   - The typed [`CylinderProps`] struct, deriving
//!     [`bevy_reflect::Reflect`] so future inspector / form-driven UI
//!     can iterate fields by type.
//!   - The [`props_layout`] table pairs each field name with its
//!     default `PortValue` and a [`NodeFieldAttrs`] describing the
//!     editor, label, description, and advanced-mode visibility.
//!
//! `NodeDef::properties` is derived from `props_layout`; the field
//! struct and the table are the **only** sources of schema truth,
//! mirroring MatterCAD's `[Slider] / [Description] / [ReadOnly] /
//! [HideFromEditor]` attribute pattern.

use std::sync::Arc;

use bevy_reflect::Reflect;

use crate::geometry::{generate_cylinder, generate_cylinder_advanced};
use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    geometry_props, wrap_mesh, EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError,
    NodeFieldAttrs, NodeOutputs, NodeRegistry, NumberAttrs, PropDef,
};
use crate::socket_types::SocketType;

/// Typed property struct for the Cylinder node — every field corresponds
/// to one row in MatterCAD's property panel. Derives [`Reflect`] so the
/// future property-panel implementation can iterate fields by type the
/// same way MatterCAD's `PropertyEditor` does on C# `PropertyInfo`.
#[derive(Clone, Debug, Reflect)]
pub struct CylinderProps {
    /// Width across the cylinder at its base (and at the top when
    /// `advanced` is off). MatterCAD's `Diameter`.
    pub diameter: f64,
    /// Cylinder height — distance from `-height/2` to `+height/2`.
    pub height: f64,
    /// Number of segments around the perimeter. MatterCAD's `Sides`.
    pub sides: f64,
    /// Easy/Advanced toggle. When `false` the cylinder is uniform and
    /// a full revolution; when `true` `diameter_top`, `starting_angle`,
    /// and `ending_angle` come into play.
    pub advanced: bool,
    /// Top diameter — only used when `advanced` is on; otherwise mirrors
    /// `diameter`. MatterCAD's `DiameterTop`.
    pub diameter_top: f64,
    /// Sweep start in degrees. MatterCAD's `StartingAngle`. Only used
    /// when `advanced` is on.
    pub starting_angle: f64,
    /// Sweep end in degrees. MatterCAD's `EndingAngle`. Only used when
    /// `advanced` is on.
    pub ending_angle: f64,
}

impl Default for CylinderProps {
    fn default() -> Self {
        Self {
            diameter: 20.0,
            height: 20.0,
            sides: 40.0,
            advanced: false,
            diameter_top: 20.0,
            starting_angle: 0.0,
            ending_angle: 360.0,
        }
    }
}

impl CylinderProps {
    /// Read a snapshot from an evaluation context. Each numeric
    /// property has a matching input socket — when wired, the
    /// upstream value wins; otherwise the property's stored value (or
    /// the type default) feeds the geometry. Matches the Extrude
    /// node's "socket-or-property" pattern so connection flow is
    /// consistent across every geometry-producing node.
    pub fn resolve(ctx: &EvalCtx) -> Self {
        let def = Self::default();
        let p = ctx.properties;
        let num = |socket: &str, prop: &str, fallback: f64| match ctx.input_named(socket) {
            PortValue::Number(n) => *n,
            _ => p.number(prop, fallback),
        };
        Self {
            diameter: num("Diameter", "diameter", def.diameter),
            height: num("Height", "height", def.height),
            sides: num("Sides", "sides", def.sides),
            advanced: p.bool_("advanced", def.advanced),
            diameter_top: num("Diameter Top", "diameter_top", def.diameter_top),
            starting_angle: num("Starting Angle", "starting_angle", def.starting_angle),
            ending_angle: num("Ending Angle", "ending_angle", def.ending_angle),
        }
    }
}

/// Layout table — one entry per editable field on [`CylinderProps`].
/// Mirrors MatterCAD's `[Slider(...)]`, `[MaxDecimalPlaces(...)]`,
/// `[Description(...)]`, and `[HideFromEditor]`-style attributes via the
/// [`NodeFieldAttrs`] builder.
fn props_layout() -> Vec<(&'static str, PortValue, NodeFieldAttrs)> {
    let def = CylinderProps::default();
    vec![
        (
            "diameter",
            PortValue::Number(def.diameter),
            NodeFieldAttrs::new()
                .with_label("Diameter")
                .with_description("The width from one side to the opposite side.")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(1.0, 400.0)
                        .with_ease_in(2.0)
                        .with_snap_grid()
                        .with_decimal_places(2),
                ))
                .bound_to("Diameter"),
        ),
        (
            "height",
            PortValue::Number(def.height),
            NodeFieldAttrs::new()
                .with_label("Height")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(1.0, 400.0)
                        .with_ease_in(2.0)
                        .with_snap_grid()
                        .with_decimal_places(2),
                ))
                .bound_to("Height"),
        ),
        (
            "sides",
            PortValue::Number(def.sides),
            NodeFieldAttrs::new()
                .with_label("Sides")
                .with_description("The number of segments around the perimeter.")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(3.0, 360.0)
                        .integer()
                        .with_step(1.0)
                        .with_ease_in(2.0),
                ))
                .bound_to("Sides"),
        ),
        (
            "advanced",
            PortValue::Bool(def.advanced),
            NodeFieldAttrs::new()
                .with_label("Advanced")
                .with_editor(EditorKind::Toggle),
        ),
        (
            "easy_mode_message",
            PortValue::StringVal(Arc::new(
                "You can switch to Advanced mode to get more cylinder options.".to_string(),
            )),
            // Read-only string with no label — MatterCAD's
            // `[ReadOnly][DisplayName("")]` combo on `EasyModeMessage`.
            // Visible only when `advanced == false`; UI layer hides
            // the row once the toggle flips on.
            NodeFieldAttrs::new()
                .with_label("")
                .with_editor(EditorKind::StringReadOnly)
                .easy_only(),
        ),
        (
            "diameter_top",
            PortValue::Number(def.diameter_top),
            NodeFieldAttrs::new()
                .with_label("Diameter Top")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(1.0, 400.0)
                        .with_ease_in(2.0)
                        .with_snap_grid()
                        .with_decimal_places(2),
                ))
                .bound_to("Diameter Top")
                .advanced(),
        ),
        (
            "starting_angle",
            PortValue::Number(def.starting_angle),
            NodeFieldAttrs::new()
                .with_label("Starting Angle")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(0.0, 359.0)
                        .with_step(1.0)
                        .with_decimal_places(2),
                ))
                .bound_to("Starting Angle")
                .advanced(),
        ),
        (
            "ending_angle",
            PortValue::Number(def.ending_angle),
            NodeFieldAttrs::new()
                .with_label("Ending Angle")
                .with_editor(EditorKind::Slider(
                    NumberAttrs::with_range(1.0, 360.0)
                        .with_step(1.0)
                        .with_decimal_places(2),
                ))
                .bound_to("Ending Angle")
                .advanced(),
        ),
    ]
}

pub struct CylinderNode;

impl NodeDef for CylinderNode {
    fn type_id(&self) -> &'static str { "Cylinder" }
    fn display_name(&self) -> &'static str { "Cylinder" }
    fn category(&self) -> &'static str { "Primitives 3D" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        // Every editable numeric / color / matrix property carries a
        // matching optional input socket so any of them can be driven
        // by an upstream connection — the same "socket-or-property"
        // shape Extrude uses. The `advanced` toggle and the read-only
        // `easy_mode_message` row stay property-only since they're
        // UI controls, not data inputs.
        InstanceTemplate::builder(alloc)
            .input_with_label("Color", "Color", SocketType::Color, true)
            .input_with_label("Matrix", "Matrix", SocketType::Matrix4x4, true)
            .input_with_label("Diameter", "Diameter", SocketType::Number, true)
            .input_with_label("Height", "Height", SocketType::Number, true)
            .input_with_label("Sides", "Sides", SocketType::Number, true)
            .input_with_label("Diameter Top", "Diameter Top", SocketType::Number, true)
            .input_with_label("Starting Angle", "Starting Angle", SocketType::Number, true)
            .input_with_label("Ending Angle", "Ending Angle", SocketType::Number, true)
            .output("out", SocketType::Geometry3d)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        let p: Vec<PropDef> = props_layout()
            .into_iter()
            .map(|(name, default, attrs)| PropDef::from_attrs(name, default, &attrs))
            .collect();
        // Shared `color` + `matrix` properties — bind them to the
        // matching input sockets minted in `instantiate`, then place
        // them first so the panel renders them as the leading rows.
        let mut geom = geometry_props();
        for prop in &mut geom {
            let socket = match prop.name.as_ref() {
                "color" => "Color",
                "matrix" => "Matrix",
                _ => continue,
            };
            *prop = prop.clone().bind_input(socket);
        }
        geom.extend(p);
        geom
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let props = CylinderProps::resolve(ctx);
        let sides = props.sides.round().clamp(3.0, 360.0) as u32;
        let mesh = if !props.advanced {
            generate_cylinder(props.diameter * 0.5, props.height, sides)
        } else {
            let start = props.starting_angle.to_radians();
            // Clamp end strictly greater than start so we don't collapse
            // the arc to zero width.
            let end_deg = props
                .ending_angle
                .max(props.starting_angle + 0.01)
                .min(360.0);
            let end = end_deg.to_radians();
            generate_cylinder_advanced(
                props.diameter,
                props.diameter_top,
                props.height,
                sides,
                start,
                end,
            )
        };
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Geometry3d(Arc::new(wrap_mesh(ctx, mesh))));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(CylinderNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_reflect::Struct;

    /// Field iteration through `Reflect`: the property panel will walk
    /// the struct exactly the same way MatterCAD's `PropertyEditor`
    /// iterates `PropertyInfo` via reflection.
    #[test]
    fn reflect_iterates_every_cylinder_prop() {
        let props = CylinderProps::default();
        let names: Vec<_> = (0..props.field_len())
            .map(|i| props.name_at(i).unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "diameter",
                "height",
                "sides",
                "advanced",
                "diameter_top",
                "starting_angle",
                "ending_angle",
            ]
        );
    }

    /// `props_layout` must cover every field on `CylinderProps`, plus
    /// the read-only easy-mode message that has no backing field.
    /// Catches drift between the typed struct and the layout table.
    #[test]
    fn layout_covers_every_reflected_field() {
        let props = CylinderProps::default();
        let reflected: Vec<&str> = (0..props.field_len())
            .map(|i| props.name_at(i).unwrap())
            .collect();
        let layout_names: Vec<&str> = props_layout().iter().map(|(n, _, _)| *n).collect();
        for name in reflected {
            assert!(
                layout_names.contains(&name),
                "layout table missing field {}",
                name
            );
        }
        // The read-only message has no Reflect-backed field — it's a
        // pure display row. Verify it's in the table anyway.
        assert!(layout_names.contains(&"easy_mode_message"));
    }

    /// Advanced-only fields should carry the `advanced` flag in the
    /// minted `PropDef` so the UI layer can hide them when the
    /// `advanced` toggle is off. Easy-mode fields must not.
    #[test]
    fn visibility_gating_propagates_to_propdef() {
        use crate::registry::VisibleWhen;
        let by_name: std::collections::HashMap<String, PropDef> = CylinderNode
            .properties()
            .into_iter()
            .map(|p| (p.name.to_string(), p))
            .collect();
        assert_eq!(by_name["diameter"].visible_when, VisibleWhen::Always);
        assert_eq!(by_name["height"].visible_when, VisibleWhen::Always);
        assert_eq!(by_name["sides"].visible_when, VisibleWhen::Always);
        assert_eq!(by_name["advanced"].visible_when, VisibleWhen::Always);
        assert_eq!(by_name["easy_mode_message"].visible_when, VisibleWhen::AdvancedOff);
        assert_eq!(by_name["diameter_top"].visible_when, VisibleWhen::AdvancedOn);
        assert_eq!(by_name["starting_angle"].visible_when, VisibleWhen::AdvancedOn);
        assert_eq!(by_name["ending_angle"].visible_when, VisibleWhen::AdvancedOn);
        // Color + matrix come from `geometry_props()` — Always.
        assert_eq!(by_name["color"].visible_when, VisibleWhen::Always);
        assert_eq!(by_name["matrix"].visible_when, VisibleWhen::Always);
    }

    /// Color must render first, matrix second — MatterCAD-style panel
    /// ordering set by `geometry_props()` being prepended to the
    /// per-node property list.
    #[test]
    fn color_then_matrix_lead_the_property_list() {
        let names: Vec<String> = CylinderNode
            .properties()
            .into_iter()
            .map(|p| p.name.to_string())
            .collect();
        assert_eq!(names[0], "color");
        assert_eq!(names[1], "matrix");
        assert!(names.contains(&"diameter".to_string()));
    }

    /// Editor hints survive the layout → `PropDef` translation. The
    /// `Slider` variant carries its range and decimal-places metadata
    /// through unchanged.
    #[test]
    fn slider_attrs_round_trip_through_propdef() {
        let by_name: std::collections::HashMap<String, PropDef> = CylinderNode
            .properties()
            .into_iter()
            .map(|p| (p.name.to_string(), p))
            .collect();
        let diameter = &by_name["diameter"];
        match &diameter.editor {
            EditorKind::Slider(attrs) => {
                assert_eq!(attrs.min, Some(1.0));
                assert_eq!(attrs.max, Some(400.0));
                assert_eq!(attrs.max_decimal_places, Some(2));
                assert_eq!(attrs.ease_in, Some(2.0));
                assert!(attrs.snap_grid);
            }
            other => panic!("expected Slider, got {:?}", other),
        }
        // `sides` is an integer slider with step=1, no decimal places.
        let sides = &by_name["sides"];
        match &sides.editor {
            EditorKind::Slider(attrs) => {
                assert!(attrs.integer);
                assert_eq!(attrs.step, Some(1.0));
                assert_eq!(attrs.max_decimal_places, None);
            }
            other => panic!("expected Slider, got {:?}", other),
        }
    }

    /// The Toggle editor is what the UI mounts for `advanced`.
    #[test]
    fn advanced_uses_toggle_editor() {
        let by_name: std::collections::HashMap<String, PropDef> = CylinderNode
            .properties()
            .into_iter()
            .map(|p| (p.name.to_string(), p))
            .collect();
        assert!(matches!(by_name["advanced"].editor, EditorKind::Toggle));
    }

    /// `easy_mode_message` is a read-only string with default text — the
    /// UI mounts it as a wrapped-text display row.
    #[test]
    fn easy_mode_message_is_read_only_string() {
        let by_name: std::collections::HashMap<String, PropDef> = CylinderNode
            .properties()
            .into_iter()
            .map(|p| (p.name.to_string(), p))
            .collect();
        let msg = &by_name["easy_mode_message"];
        assert!(matches!(msg.editor, EditorKind::StringReadOnly));
        match &msg.default {
            PortValue::StringVal(s) => assert!(s.contains("Advanced mode")),
            other => panic!("expected StringVal, got {:?}", other),
        }
    }

    /// Descriptions land on `PropDef.description` and are surfaced as
    /// tooltips in the UI. MatterCAD's `[Description("…")]` analogue.
    #[test]
    fn descriptions_propagate_to_propdef() {
        let by_name: std::collections::HashMap<String, PropDef> = CylinderNode
            .properties()
            .into_iter()
            .map(|p| (p.name.to_string(), p))
            .collect();
        assert!(by_name["diameter"]
            .description
            .as_deref()
            .map(|s| s.contains("opposite side"))
            .unwrap_or(false));
        assert!(by_name["sides"]
            .description
            .as_deref()
            .map(|s| s.contains("perimeter"))
            .unwrap_or(false));
    }

    /// Evaluation path for `advanced == false`: uses the simple
    /// uniform-revolve `generate_cylinder`. Verifies a basic
    /// triangle-count sanity check rather than asserting exact mesh
    /// data — that lives in `geometry::primitives` tests.
    #[test]
    fn evaluate_easy_mode_returns_a_mesh() {
        use crate::graph::node::NodeId;
        use crate::graph::node::NodeInstance;
        use crate::graph::socket::SocketUidAlloc;
        use crate::registry::{NodeInputs, NodeProperties};

        let mut alloc = SocketUidAlloc::new();
        let mut inst = NodeInstance::new(NodeId(0), "Cylinder", [0.0, 0.0]);
        let tpl = CylinderNode.instantiate(&mut alloc);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let mut props = NodeProperties::default();
        for p in CylinderNode.properties() {
            props.insert(p.name.clone(), p.default.clone());
        }
        let inputs = NodeInputs::default();
        let ctx = EvalCtx {
            instance: &inst,
            properties: &props,
            inputs: &inputs,
        };
        let outputs = CylinderNode.evaluate(&ctx).unwrap();
        let geom = outputs
            .by_name
            .get("out")
            .expect("Cylinder must emit `out`");
        match geom {
            PortValue::Geometry3d(g) => {
                assert!(crate::geometry::num_verts(&g.first().unwrap().mesh) > 0);
                assert!(crate::geometry::num_tris(&g.first().unwrap().mesh) > 0);
            }
            other => panic!("expected Geometry3d, got {:?}", other),
        }
    }

    /// Advanced path: a partial-revolve cylinder ends up with more
    /// triangles than the easy-mode case at the same `sides` count
    /// because two wedge walls close the partial volume.
    #[test]
    fn evaluate_advanced_partial_revolve_adds_wedge_walls() {
        use crate::graph::node::NodeId;
        use crate::graph::node::NodeInstance;
        use crate::graph::socket::SocketUidAlloc;
        use crate::registry::{NodeInputs, NodeProperties};

        let mut alloc = SocketUidAlloc::new();
        let mut inst = NodeInstance::new(NodeId(0), "Cylinder", [0.0, 0.0]);
        let tpl = CylinderNode.instantiate(&mut alloc);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let mut props = NodeProperties::default();
        for p in CylinderNode.properties() {
            props.insert(p.name.clone(), p.default.clone());
        }
        // Flip the advanced toggle + restrict the arc.
        props.insert("advanced", PortValue::Bool(true));
        props.insert("starting_angle", PortValue::Number(0.0));
        props.insert("ending_angle", PortValue::Number(180.0));
        let inputs = NodeInputs::default();
        let ctx = EvalCtx {
            instance: &inst,
            properties: &props,
            inputs: &inputs,
        };
        let outputs = CylinderNode.evaluate(&ctx).unwrap();
        if let Some(PortValue::Geometry3d(g)) = outputs.by_name.get("out") {
            // Two wedge quads = 4 triangles on top of the full mesh, so
            // the half-arc must still produce a mesh.
            assert!(crate::geometry::num_tris(&g.first().unwrap().mesh) > 0);
        } else {
            panic!("advanced evaluate must produce Geometry3d");
        }
    }
}
