//! Unit tests for the declarative parameter schema ([`super::ParamSet`] /
//! [`super::ParamReader`]). Split out of `params.rs` to keep that module
//! under the 800-line guardrail.

use super::*;
use crate::graph::node::{identity_matrix, NodeId, NodeInstance};
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{InstanceTemplate, NodeInputs, NodeProperties};
use crate::socket_types::SocketType;

fn sample() -> ParamSet {
    ParamSet::new()
        .number("width", "Width", 4.0, 0.1..=100.0)
        .number_stepped("segments", "Segments", 8.0, 1.0..=64.0, 1.0)
        .integer()
        .bool_("uniform", "Uniform", false)
        .color("color", "Color", [1.0, 1.0, 1.0, 1.0])
        .matrix("matrix", "Matrix", identity_matrix())
}

/// Build a bare EvalCtx fixture: the ParamSet mints the sockets, the
/// named inputs/properties seed the resolution chain.
fn fixture(
    params: &ParamSet,
    named_inputs: &[(&str, PortValue)],
    named_props: &[(&str, PortValue)],
) -> (NodeInstance, NodeInputs, NodeProperties) {
    let mut alloc = SocketUidAlloc::new();
    let tpl = params.mint_sockets(InstanceTemplate::builder(&mut alloc)).build();
    let mut inst = NodeInstance::new(NodeId(1), "Sample", [0.0, 0.0]);
    inst.inputs = tpl.inputs;
    inst.outputs = tpl.outputs;
    let mut inputs = NodeInputs::default();
    for (name, value) in named_inputs {
        let uid = inst.input_by_name(name).unwrap().uid;
        inputs.insert(uid, value.clone());
    }
    let mut props = NodeProperties::default();
    for (name, value) in named_props {
        props.insert(*name, value.clone());
    }
    (inst, inputs, props)
}

#[test]
fn mint_sockets_preserves_order_and_types() {
    let ps = sample();
    let mut alloc = SocketUidAlloc::new();
    let tpl = ps.mint_sockets(InstanceTemplate::builder(&mut alloc)).build();
    let names: Vec<&str> = tpl.inputs.iter().map(|s| s.name.as_ref()).collect();
    assert_eq!(names, vec!["width", "segments", "uniform", "color", "matrix"]);
    let labels: Vec<&str> = tpl
        .inputs
        .iter()
        .map(|s| s.display_label.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(labels, vec!["Width", "Segments", "Uniform", "Color", "Matrix"]);
    assert_eq!(tpl.inputs[0].socket_type, SocketType::Number);
    assert_eq!(tpl.inputs[2].socket_type, SocketType::Bool);
    assert_eq!(tpl.inputs[3].socket_type, SocketType::Color);
    assert_eq!(tpl.inputs[4].socket_type, SocketType::Matrix4x4);
    assert!(tpl.inputs.iter().all(|s| s.optional));
}

#[test]
fn prop_defs_editor_defaults_by_type() {
    let props = sample().prop_defs();
    let width = props.iter().find(|p| p.name.as_ref() == "width").unwrap();
    assert!(matches!(width.editor, EditorKind::NumberDrag(_)));
    assert_eq!(width.min, Some(0.1));
    assert_eq!(width.max, Some(100.0));
    assert_eq!(width.bound_input.as_deref(), Some("width"));

    let segments = props.iter().find(|p| p.name.as_ref() == "segments").unwrap();
    let attrs = segments.editor.number_attrs().unwrap();
    assert!(attrs.integer);
    assert_eq!(attrs.step, Some(1.0));

    let uniform = props.iter().find(|p| p.name.as_ref() == "uniform").unwrap();
    assert!(matches!(uniform.editor, EditorKind::Toggle));

    let color = props.iter().find(|p| p.name.as_ref() == "color").unwrap();
    assert!(matches!(color.editor, EditorKind::ColorPicker));

    let matrix = props.iter().find(|p| p.name.as_ref() == "matrix").unwrap();
    assert!(matches!(matrix.editor, EditorKind::Matrix));
}

#[test]
fn socket_and_prop_names_cohere() {
    let ps = sample();
    let mut alloc = SocketUidAlloc::new();
    let tpl = ps.mint_sockets(InstanceTemplate::builder(&mut alloc)).build();
    let props = ps.prop_defs();
    // Every optional socket has a prop bound to it, and every bound
    // prop names a real socket — the same invariant the registry-wide
    // sweep enforces.
    for s in tpl.inputs.iter().filter(|s| s.optional) {
        assert!(
            props
                .iter()
                .any(|p| p.bound_input.as_deref() == Some(s.name.as_ref())),
            "socket '{}' has no bound prop",
            s.name
        );
    }
    for p in props.iter().filter_map(|p| p.bound_input.as_deref()) {
        assert!(tpl.inputs.iter().any(|s| s.name.as_ref() == p));
    }
}

#[test]
fn reader_socket_wins_over_property() {
    let ps = sample();
    let (inst, inputs, props) = fixture(
        &ps,
        &[("width", PortValue::Number(9.0))],
        &[("width", PortValue::Number(3.0))],
    );
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let r = ps.reader(&ctx);
    assert_eq!(r.number("width"), 9.0);
}

#[test]
fn reader_property_wins_when_socket_unwired() {
    let ps = sample();
    let (inst, inputs, props) =
        fixture(&ps, &[], &[("width", PortValue::Number(3.0))]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let r = ps.reader(&ctx);
    assert_eq!(r.number("width"), 3.0);
}

#[test]
fn reader_falls_back_to_default() {
    let ps = sample();
    let (inst, inputs, props) = fixture(&ps, &[], &[]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    let r = ps.reader(&ctx);
    assert_eq!(r.number("width"), 4.0);
    assert_eq!(r.number("segments"), 8.0);
    assert!(!r.bool_("uniform"));
    assert_eq!(r.color("color"), [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(r.matrix("matrix"), identity_matrix());
}

#[test]
fn no_socket_param_has_no_socket_and_no_bind_input() {
    let ps = ParamSet::new()
        .number("width", "Width", 4.0, 0.1..=100.0)
        .number("hidden", "Hidden", 1.0, 0.0..=2.0)
        .no_socket();
    let mut alloc = SocketUidAlloc::new();
    let tpl = ps.mint_sockets(InstanceTemplate::builder(&mut alloc)).build();
    let names: Vec<&str> = tpl.inputs.iter().map(|s| s.name.as_ref()).collect();
    assert_eq!(names, vec!["width"], "no_socket param must not mint a socket");
    let props = ps.prop_defs();
    let hidden = props.iter().find(|p| p.name.as_ref() == "hidden").unwrap();
    assert!(hidden.bound_input.is_none(), "no_socket prop must not bind an input");
    // And its reader still resolves property-then-default.
    let (inst, inputs, props) =
        fixture(&ps, &[], &[("hidden", PortValue::Number(1.5))]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    assert_eq!(ps.reader(&ctx).number("hidden"), 1.5);
}

#[test]
fn enum_param_mints_no_socket_and_validates_variants() {
    let ps = ParamSet::new().enum_(
        "op",
        "Operation",
        "Union",
        &["Union", "Difference", "Intersection"],
    );
    // No socket for an enum param.
    let mut alloc = SocketUidAlloc::new();
    let tpl = ps.mint_sockets(InstanceTemplate::builder(&mut alloc)).build();
    assert!(tpl.inputs.is_empty(), "enum param must not mint a socket");
    // PropDef carries the EnumDropdown editor and no bound input.
    let props = ps.prop_defs();
    assert_eq!(props.len(), 1);
    assert!(props[0].bound_input.is_none());
    assert!(matches!(props[0].editor, EditorKind::EnumDropdown { .. }));

    // A legal stored value resolves to itself.
    let (inst, inputs, p) = fixture(
        &ps,
        &[],
        &[("op", PortValue::StringVal(Arc::new("Difference".into())))],
    );
    let ctx = EvalCtx { instance: &inst, properties: &p, inputs: &inputs };
    assert_eq!(ps.reader(&ctx).enum_("op"), "Difference");

    // An illegal / legacy stored value falls back to the default.
    let (inst, inputs, p) = fixture(
        &ps,
        &[],
        &[("op", PortValue::StringVal(Arc::new("Bogus".into())))],
    );
    let ctx = EvalCtx { instance: &inst, properties: &p, inputs: &inputs };
    assert_eq!(ps.reader(&ctx).enum_("op"), "Union");

    // Missing property also resolves to the default.
    let (inst, inputs, p) = fixture(&ps, &[], &[]);
    let ctx = EvalCtx { instance: &inst, properties: &p, inputs: &inputs };
    assert_eq!(ps.reader(&ctx).enum_("op"), "Union");
}

#[test]
fn geometry_and_op_preseeds_declare_color_matrix_with_expected_defaults() {
    use crate::geometry::{DEFAULT_GEOMETRY_COLOR, INHERIT_COLOR};
    // Both preseeds lead with color then matrix on capitalized sockets.
    for ps in [ParamSet::geometry(), ParamSet::op()] {
        let mut alloc = SocketUidAlloc::new();
        let tpl = ps.mint_sockets(InstanceTemplate::builder(&mut alloc)).build();
        let names: Vec<&str> = tpl.inputs.iter().map(|s| s.name.as_ref()).collect();
        assert_eq!(names, vec!["Color", "Matrix"]);
        let props = ps.prop_defs();
        assert_eq!(props[0].name.as_ref(), "color");
        assert_eq!(props[0].bound_input.as_deref(), Some("Color"));
        assert_eq!(props[1].name.as_ref(), "matrix");
        assert_eq!(props[1].bound_input.as_deref(), Some("Matrix"));
    }
    // The color default differs: geometry = opaque, op = inherit.
    let (inst, inputs, props) = fixture(&ParamSet::geometry(), &[], &[]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    assert_eq!(ParamSet::geometry().reader(&ctx).color("color"), DEFAULT_GEOMETRY_COLOR);
    let (inst, inputs, props) = fixture(&ParamSet::op(), &[], &[]);
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    assert_eq!(ParamSet::op().reader(&ctx).color("color"), INHERIT_COLOR);
}

#[test]
fn number_unbounded_has_no_range() {
    let ps = ParamSet::new().number_unbounded("tx", "Translate X", 0.0);
    let props = ps.prop_defs();
    assert_eq!(props[0].min, None);
    assert_eq!(props[0].max, None);
    let attrs = props[0].editor.number_attrs().unwrap();
    assert_eq!(attrs.min, None);
    assert_eq!(attrs.max, None);
}

#[test]
fn socket_named_override_renames_socket_and_bind_input() {
    let ps = ParamSet::new()
        .number("width", "Width", 4.0, 0.1..=100.0)
        .socket_named("Width");
    let mut alloc = SocketUidAlloc::new();
    let tpl = ps.mint_sockets(InstanceTemplate::builder(&mut alloc)).build();
    assert_eq!(tpl.inputs[0].name.as_ref(), "Width");
    let props = ps.prop_defs();
    assert_eq!(props[0].name.as_ref(), "width");
    assert_eq!(props[0].bound_input.as_deref(), Some("Width"));
    // Reader reads the renamed socket, else the property keyed by name.
    let (inst, inputs, props) = fixture(
        &ps,
        &[("Width", PortValue::Number(12.0))],
        &[("width", PortValue::Number(3.0))],
    );
    let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
    assert_eq!(ps.reader(&ctx).number("width"), 12.0);
}
