//! NumberConst — emits a constant Number value. Useful as a single source
//! of truth driving multiple Box / Cylinder / Transform inputs.
//!
//! Part of the `Input` category (see `super`), alongside the Boolean,
//! String, and Color constant nodes.
//!
//! Mirrors NodeDesigner's `basic/const` Number node: the `value` prop is
//! rendered as a slider whose range follows the instance's live
//! `min`/`max`/`step` properties (via [`NodeDef::editor_override`]), and
//! `evaluate` clamps `value` into `[min, max]`.
//!
//! **Not migrated to the declarative `ParamSet` schema.** NumberConst is
//! the single sanctioned `EditorKind::Slider` in the app (see
//! `tests/number_editor_policy.rs`) and its `value` editor is *per
//! instance* — the slider's live min/max/step are pulled from sibling
//! properties via `editor_override`. The static `ParamSet` schema is per
//! node *type* and mints an input socket per param; neither fits a
//! socket-less live-slider node, so NumberConst keeps its hand-written
//! `properties()` / `evaluate()` trio.

use crate::graph::node::PortValue;
use crate::graph::socket::SocketUidAlloc;
use crate::registry::{
    EditorKind, EvalCtx, InstanceTemplate, NodeDef, NodeError, NodeOutputs, NodeProperties,
    NodeRegistry, NumberAttrs, PropDef,
};
use crate::socket_types::SocketType;

pub struct NumberConstNode;

impl NumberConstNode {
    /// NodeDesigner defaults for the value slider's bounds/step.
    const DEFAULT_MIN: f64 = -50.0;
    const DEFAULT_MAX: f64 = 50.0;
    const DEFAULT_STEP: f64 = 0.1;
}

impl NodeDef for NumberConstNode {
    fn type_id(&self) -> &'static str { "NumberConst" }
    fn display_name(&self) -> &'static str { "Number" }
    fn category(&self) -> &'static str { "Input" }

    fn instantiate(&self, alloc: &mut SocketUidAlloc) -> InstanceTemplate {
        InstanceTemplate::builder(alloc)
            .output("out", SocketType::Number)
            .build()
    }

    fn properties(&self) -> Vec<PropDef> {
        vec![
            // `value` renders as a slider. The static range here is the
            // default; `editor_override` swaps in the instance's live
            // min/max/step so the slider tracks the configured bounds.
            PropDef::new("value", PortValue::Number(1.0)).with_editor(EditorKind::Slider(
                NumberAttrs {
                    min: Some(Self::DEFAULT_MIN),
                    max: Some(Self::DEFAULT_MAX),
                    step: Some(Self::DEFAULT_STEP),
                    ..Default::default()
                },
            )),
            PropDef::new("min", PortValue::Number(Self::DEFAULT_MIN))
                .with_range(-500.0, 500.0)
                .with_editor(EditorKind::NumberDrag(NumberAttrs {
                    min: Some(-500.0),
                    max: Some(500.0),
                    step: Some(1.0),
                    ..Default::default()
                }))
                .advanced(),
            PropDef::new("max", PortValue::Number(Self::DEFAULT_MAX))
                .with_range(-500.0, 500.0)
                .with_editor(EditorKind::NumberDrag(NumberAttrs {
                    min: Some(-500.0),
                    max: Some(500.0),
                    step: Some(1.0),
                    ..Default::default()
                }))
                .advanced(),
            PropDef::new("step", PortValue::Number(Self::DEFAULT_STEP))
                .with_range(0.001, 10.0)
                .with_editor(EditorKind::NumberDrag(NumberAttrs {
                    min: Some(0.001),
                    max: Some(10.0),
                    step: Some(0.001),
                    ..Default::default()
                }))
                .advanced(),
        ]
    }

    /// The `value` slider follows the instance's live min/max/step.
    fn editor_override(&self, prop: &str, props: &NodeProperties) -> Option<EditorKind> {
        if prop != "value" {
            return None;
        }
        let min = props.number("min", Self::DEFAULT_MIN);
        let max = props.number("max", Self::DEFAULT_MAX);
        let step = props.number("step", Self::DEFAULT_STEP);
        // Sort the bounds so an inverted min/max never hands the slider
        // widget an inverted range — consistent with `evaluate`'s sorted
        // clamp.
        let (min, max) = if min <= max { (min, max) } else { (max, min) };
        Some(EditorKind::Slider(NumberAttrs {
            min: Some(min),
            max: Some(max),
            step: Some(step),
            ..Default::default()
        }))
    }

    fn evaluate(&self, ctx: &EvalCtx) -> Result<NodeOutputs, NodeError> {
        let v = ctx.properties.number("value", 1.0);
        let min = ctx.properties.number("min", Self::DEFAULT_MIN);
        let max = ctx.properties.number("max", Self::DEFAULT_MAX);
        // Guard against an inverted range: clamp to the sorted bounds so
        // the output is always well-defined even if min > max.
        let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
        // Use `max().min()` rather than `f64::clamp`, which panics when a
        // bound is NaN (it asserts `lo <= hi`). This form degrades
        // gracefully: a NaN bound is treated as "no constraint" and the
        // value passes through the non-NaN side.
        let clamped = v.max(lo).min(hi);
        let mut out = NodeOutputs::default();
        out.set("out", PortValue::Number(clamped));
        Ok(out)
    }
}

pub fn register(reg: &mut NodeRegistry) {
    reg.register(NumberConstNode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::{NodeId, NodeInstance};
    use crate::registry::{NodeInputs, NodeProperties};

    /// Build an `EvalCtx`-ready instance + evaluate with the given props.
    fn eval_with(props: NodeProperties) -> f64 {
        let def = NumberConstNode;
        let mut alloc = SocketUidAlloc::new();
        let tpl = def.instantiate(&mut alloc);
        let mut inst = NodeInstance::new(NodeId(1), def.type_id().to_string(), [0.0, 0.0]);
        inst.inputs = tpl.inputs;
        inst.outputs = tpl.outputs;
        let inputs = NodeInputs::default();
        let ctx = EvalCtx { instance: &inst, properties: &props, inputs: &inputs };
        match def.evaluate(&ctx).unwrap().by_name.get("out").unwrap() {
            PortValue::Number(n) => *n,
            other => panic!("expected Number, got {other:?}"),
        }
    }

    fn props(pairs: &[(&str, f64)]) -> NodeProperties {
        let mut p = NodeProperties::default();
        for (k, v) in pairs {
            p.insert(*k, PortValue::Number(*v));
        }
        p
    }

    #[test]
    fn registers_in_input_category() {
        let mut reg = NodeRegistry::new();
        register(&mut reg);
        let def = reg.get("NumberConst").expect("NumberConst registered");
        assert_eq!(def.category(), "Input");
    }

    #[test]
    fn in_range_value_passes_through() {
        assert_eq!(eval_with(props(&[("value", 7.0)])), 7.0);
    }

    #[test]
    fn value_above_max_clamps_to_max() {
        let v = eval_with(props(&[("value", 999.0), ("min", -50.0), ("max", 50.0)]));
        assert_eq!(v, 50.0);
    }

    #[test]
    fn value_below_min_clamps_to_min() {
        let v = eval_with(props(&[("value", -999.0), ("min", -50.0), ("max", 50.0)]));
        assert_eq!(v, -50.0);
    }

    #[test]
    fn inverted_min_max_clamps_to_sorted_bounds() {
        // min > max: bounds are sorted, so value clamps into [max, min].
        let v = eval_with(props(&[("value", 100.0), ("min", 50.0), ("max", -50.0)]));
        assert_eq!(v, 50.0);
        let v = eval_with(props(&[("value", -100.0), ("min", 50.0), ("max", -50.0)]));
        assert_eq!(v, -50.0);
    }

    #[test]
    fn nan_bound_degrades_without_panic() {
        // A NaN min/max must not panic. `f64::clamp` asserts `lo <= hi`
        // and panics when a bound is NaN; the `max().min()` form used by
        // `evaluate` instead degrades gracefully — the call completes and
        // yields a finite result clamped by the surviving (non-NaN) bound.
        let v = eval_with(props(&[("value", 7.0), ("min", f64::NAN), ("max", 50.0)]));
        assert!(v.is_finite(), "NaN min must not produce NaN/panic, got {v}");
        assert_eq!(v, 50.0);
        let v = eval_with(props(&[("value", 7.0), ("min", -50.0), ("max", f64::NAN)]));
        assert!(v.is_finite(), "NaN max must not produce NaN/panic, got {v}");
        assert_eq!(v, -50.0);
    }

    #[test]
    fn editor_override_reflects_live_attrs() {
        let def = NumberConstNode;
        let p = props(&[("min", -3.0), ("max", 12.0), ("step", 0.25)]);
        match def.editor_override("value", &p) {
            Some(EditorKind::Slider(attrs)) => {
                assert_eq!(attrs.min, Some(-3.0));
                assert_eq!(attrs.max, Some(12.0));
                assert_eq!(attrs.step, Some(0.25));
            }
            other => panic!("expected Slider override, got {other:?}"),
        }
        // Non-value props do not get an override.
        assert!(def.editor_override("min", &p).is_none());

        // Inverted min/max is sorted before reaching the slider so the
        // widget never sees a reversed range.
        let inv = props(&[("min", 12.0), ("max", -3.0), ("step", 0.25)]);
        match def.editor_override("value", &inv) {
            Some(EditorKind::Slider(attrs)) => {
                assert_eq!(attrs.min, Some(-3.0));
                assert_eq!(attrs.max, Some(12.0));
            }
            other => panic!("expected Slider override, got {other:?}"),
        }
    }
}
