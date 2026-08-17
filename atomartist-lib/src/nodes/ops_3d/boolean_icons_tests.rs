//! Schema-side tests for the Boolean node's operation row (plan §3.5).
//!
//! The row is MatterCAD's icon row: an `EnumIcons` editor whose variants
//! are [`OPERATIONS`] and whose icon ids are [`OPERATION_ICONS`]. The
//! artwork itself lives in the UI crate — what the *schema* owes is a
//! well-formed, aligned pair of lists, and a stored value that keeps
//! resolving (and migrating) exactly as it did when the row was a
//! button strip. Presentation must not be able to change semantics.

use crate::registry::{EditorKind, NodeDef};

use super::{BooleanNode, OPERATIONS, OPERATION_ICONS};

fn operation_editor() -> EditorKind {
    BooleanNode
        .properties()
        .into_iter()
        .find(|p| p.name.as_ref() == "operation")
        .expect("the Boolean node declares an operation property")
        .editor
}

#[test]
fn the_operation_row_is_an_icon_strip_naming_all_four_icons() {
    let editor = operation_editor();
    let variants = editor
        .enum_variants()
        .expect("the operation row is an enum row");
    let icons = editor
        .enum_icon_ids()
        .expect("the operation row is an icon row");

    assert_eq!(variants.len(), OPERATIONS.len());
    assert_eq!(icons.len(), OPERATIONS.len());
    for (i, want) in OPERATIONS.iter().enumerate() {
        assert_eq!(variants[i].as_ref(), *want);
        assert_eq!(
            icons[i].as_ref(),
            OPERATION_ICONS[i],
            "icon {i} is out of step with its variant"
        );
    }
}

/// Ids have to be unique and namespaced — the registry is process-wide,
/// so a bare "combine" would be a collision waiting to happen and a
/// duplicate would paint the same glyph twice.
#[test]
fn the_icon_ids_are_unique_and_namespaced() {
    for (i, id) in OPERATION_ICONS.iter().enumerate() {
        assert!(id.starts_with("boolean."), "{id} is not namespaced");
        for other in &OPERATION_ICONS[i + 1..] {
            assert_ne!(id, other, "duplicate icon id {id}");
        }
    }
}

/// The presentation switch must not touch how the value reads: an icon
/// row is still an enum row, so both the stored-name path and the legacy
/// numeric-index path resolve exactly as they did under `EnumButtons`.
#[test]
fn switching_to_icons_left_the_value_semantics_alone() {
    use crate::graph::node::PortValue;
    use crate::serialization::prop_migration::migrate_value;

    let prop = BooleanNode
        .properties()
        .into_iter()
        .find(|p| p.name.as_ref() == "operation")
        .expect("operation exists");

    // Legacy `Number` index → the variant at that index.
    let (v, warning) = migrate_value(&prop, PortValue::Number(2.0));
    assert!(matches!(&v, PortValue::StringVal(s) if s.as_str() == "Intersect"));
    assert!(warning.is_none());

    // A stored name is left alone.
    let (v, warning) = migrate_value(&prop, PortValue::StringVal("Subtract".to_string().into()));
    assert!(matches!(&v, PortValue::StringVal(s) if s.as_str() == "Subtract"));
    assert!(warning.is_none());

    // Out of range still falls back to the default and warns.
    let (v, warning) = migrate_value(&prop, PortValue::Number(9.0));
    assert!(matches!(&v, PortValue::StringVal(s) if s.as_str() == OPERATIONS[0]));
    assert!(warning.is_some(), "an unmappable index must be reported");
}
