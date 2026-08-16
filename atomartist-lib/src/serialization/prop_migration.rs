//! Property-value migration applied when a saved graph is loaded.
//!
//! A node type's schema evolves; the JSON on disk does not. This module
//! is the one place where a stored [`PortValue`] whose variant no longer
//! matches the schema's declared default is coerced into the current
//! shape, so [`load_graph`](super::graph_json::load_graph) stays a
//! straight decode and every node type gets the same treatment.
//!
//! ## Today's single rule: numeric enums → variant names
//!
//! Every choice-valued property in this codebase started life as a
//! `Number` index (the Boolean node's `operation` was 0 = union,
//! 1 = difference, 2 = intersection) before [`ParamSet::enum_`] gave us
//! real enums serialized **by name** — MatterCAD's convention
//! (`BooleanObject3D.cs:57-60`: "serialized by name, so the order of the
//! members is free to change but their spellings are not").
//!
//! So: a property whose schema editor is an enum, carrying a stored
//! `Number`, resolves to the variant at that index. The mapping is
//! correct precisely because the enum's declaration order preserves the
//! old numeric encoding — node authors adding an enum to replace a
//! numeric property must keep the legacy values at their old indices.
//! An out-of-range index falls back to the schema default rather than
//! guessing.
//!
//! Anything else is left exactly as the file had it: a value this module
//! doesn't understand is more useful preserved (the "extra properties"
//! path in `load_graph` keeps unknown keys for the same reason) than
//! silently replaced.
//!
//! ## Falling back is reported, not swallowed
//!
//! Two cases end at the schema default rather than at the user's stored
//! value: an index outside the variant list, and a name that is not a
//! variant. Both mean a saved choice was lost, so both return a warning
//! for `load_graph` to surface on the same channel it uses for unknown
//! node types. Silence here would look exactly like "the file always
//! said Combine".
//!
//! Name matching is **exact** — no trimming, no case folding. The names
//! are written by this program from a fixed list, never typed by a user,
//! so a mismatch means the variant was renamed or the file was
//! hand-edited; both deserve the warning rather than a guess that
//! quietly picks a neighbour.
//!
//! [`ParamSet::enum_`]: crate::registry::ParamSet::enum_

use crate::graph::node::PortValue;
use crate::registry::{enum_variant_for_index, EditorKind, PropDef};

/// Coerce one loaded property value into the shape `prop` declares.
///
/// Returns the value to store plus an optional warning describing a
/// stored choice that could not be honoured. `value` comes back
/// untouched (and warning-free) when no migration applies.
pub fn migrate_value(prop: &PropDef, value: PortValue) -> (PortValue, Option<String>) {
    let variants = match &prop.editor {
        EditorKind::EnumDropdown { variants }
        | EditorKind::EnumButtons { variants }
        | EditorKind::EnumTabs { variants } => variants.as_slice(),
        _ => return (value, None),
    };
    match value {
        PortValue::Number(n) => match enum_variant_for_index(variants, n) {
            Some(v) => (PortValue::StringVal(std::sync::Arc::new(v.to_string())), None),
            // Out of range: the file's index means nothing in the
            // current variant list, so the schema default is the honest
            // answer — and the user hears about it.
            None => (
                prop.default.clone(),
                Some(format!(
                    "property '{}' had the out-of-range choice {} — reset to '{}'",
                    prop.name,
                    n,
                    default_label(prop),
                )),
            ),
        },
        PortValue::StringVal(ref s) if !variants.iter().any(|v| v.as_ref() == s.as_str()) => (
            prop.default.clone(),
            Some(format!(
                "property '{}' had the unknown choice '{}' — reset to '{}'",
                prop.name,
                s.as_str(),
                default_label(prop),
            )),
        ),
        other => (other, None),
    }
}

/// The schema default rendered for a message.
fn default_label(prop: &PropDef) -> String {
    match &prop.default {
        PortValue::StringVal(s) => s.as_str().to_string(),
        other => format!("{:?}", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn enum_prop() -> PropDef {
        PropDef::new(
            "operation",
            PortValue::StringVal(Arc::new("Combine".to_string())),
        )
        .with_editor(EditorKind::EnumButtons {
            variants: vec![
                Arc::from("Combine"),
                Arc::from("Subtract"),
                Arc::from("Intersect"),
                Arc::from("Subtract & Replace"),
            ],
        })
    }

    /// The migrated value as a string, asserting there was no warning.
    fn quietly(p: &PropDef, v: PortValue) -> String {
        let (out, warning) = migrate_value(p, v);
        assert_eq!(warning, None, "unexpected warning");
        match out {
            PortValue::StringVal(s) => s.as_str().to_string(),
            other => panic!("expected StringVal, got {:?}", other),
        }
    }

    /// The migrated value as a string, plus the warning it must carry.
    fn with_warning(p: &PropDef, v: PortValue) -> (String, String) {
        let (out, warning) = migrate_value(p, v);
        let w = warning.expect("a lost choice must be reported");
        match out {
            PortValue::StringVal(s) => (s.as_str().to_string(), w),
            other => panic!("expected StringVal, got {:?}", other),
        }
    }

    #[test]
    fn legacy_number_maps_to_the_variant_at_that_index() {
        let p = enum_prop();
        assert_eq!(quietly(&p, PortValue::Number(0.0)), "Combine");
        assert_eq!(quietly(&p, PortValue::Number(1.0)), "Subtract");
        assert_eq!(quietly(&p, PortValue::Number(2.0)), "Intersect");
    }

    /// Indices arrive as `f64`, and a number spinner could have left one
    /// a hair off a whole value. Rounding is half-away-from-zero (`f64::
    /// round`), and the small negative band down to -0.5 rounds to 0
    /// rather than being refused — a stored `-0.4` was always index 0.
    #[test]
    fn fractional_indices_round_to_the_nearest_variant() {
        let p = enum_prop();
        assert_eq!(quietly(&p, PortValue::Number(0.5)), "Subtract");
        assert_eq!(quietly(&p, PortValue::Number(0.4)), "Combine");
        assert_eq!(quietly(&p, PortValue::Number(1.5)), "Intersect");
        assert_eq!(quietly(&p, PortValue::Number(-0.4)), "Combine");
    }

    #[test]
    fn out_of_range_number_falls_back_and_warns() {
        let p = enum_prop();
        let (value, warning) = with_warning(&p, PortValue::Number(9.0));
        assert_eq!(value, "Combine");
        assert!(
            warning.contains("operation") && warning.contains('9'),
            "warning must name the property and the lost choice: {}",
            warning
        );
        assert_eq!(with_warning(&p, PortValue::Number(-1.0)).0, "Combine");
        assert_eq!(with_warning(&p, PortValue::Number(f64::NAN)).0, "Combine");
    }

    #[test]
    fn a_stored_variant_name_is_left_alone() {
        let p = enum_prop();
        let v = PortValue::StringVal(Arc::new("Subtract & Replace".to_string()));
        assert_eq!(quietly(&p, v), "Subtract & Replace");
    }

    /// Names match exactly — see the module docs on why nothing is
    /// trimmed or case-folded.
    #[test]
    fn an_unknown_name_falls_back_and_warns() {
        let p = enum_prop();
        let (value, warning) = with_warning(&p, PortValue::StringVal(Arc::new("Frob".into())));
        assert_eq!(value, "Combine");
        assert!(
            warning.contains("Frob"),
            "warning must quote the unknown choice: {}",
            warning
        );
        // Near-misses are unknown names too, deliberately.
        assert_eq!(
            with_warning(&p, PortValue::StringVal(Arc::new("subtract".into()))).0,
            "Combine"
        );
        assert_eq!(
            with_warning(&p, PortValue::StringVal(Arc::new(" Subtract".into()))).0,
            "Combine"
        );
    }

    #[test]
    fn non_enum_properties_are_untouched() {
        let p = PropDef::new("width", PortValue::Number(1.0)).with_range(0.0, 10.0);
        match migrate_value(&p, PortValue::Number(4.0)) {
            (PortValue::Number(n), None) => assert!((n - 4.0).abs() < 1e-9),
            other => panic!("width was migrated to {:?}", other),
        }
    }
}
