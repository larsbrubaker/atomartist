//! The Boolean node's operation icons, end to end through the shell's
//! shared startup (plan step B-2b, docs/boolean-node-plan.md §3.5).
//!
//! No NodeDesigner ancestor — the icon family comes from MatterCAD
//! (`StaticData/Icons/{combine,subtract,intersect,subtract_and_replace}
//! .svg`), rendered by MatterCAD's `BooleanObject3D` as a 16×16 icon row.
//!
//! What this file pins is the *seam*: the schema names icons by id, the
//! shells register the artwork at startup, and the two halves are wired
//! by different crates. Either half can be green on its own while the
//! row still paints text.
//!
//! The registration these tests observe is the one the **shells**
//! perform: the harness calls `atomartist_ui::install_theme_and_fonts`,
//! the same startup function `demo-native` and `demo-wasm` call, and
//! nothing here registers an icon itself. Deleting the registration
//! from `shell_init` fails both tests below — verified, and the reason
//! the harness stopped installing its own font: a harness that
//! re-implements startup can only test itself.

use agg_gui::vector_icon;
use atomartist_lib::nodes::ops_3d::boolean_node::{OPERATIONS, OPERATION_ICONS};
use atomartist_ui_test::TestHarness;

/// Booting the app registers the artwork: every id the Boolean node's
/// schema names resolves, with geometry in it.
#[test]
fn booting_the_app_registers_every_boolean_operation_icon() {
    let _h = TestHarness::new();
    for id in OPERATION_ICONS {
        let art = vector_icon::icon(id)
            .unwrap_or_else(|| panic!("no artwork registered for {id} after startup"));
        assert!(
            art.point_count() > 20,
            "{id} registered but has almost no geometry"
        );
    }
}

/// …and the live property row the canvas reads asks for exactly those
/// ids, in variant order. A mismatch here is the failure mode the id
/// indirection invites: both sides fine, the join wrong, and the row
/// silently falls back to truncated text.
#[test]
fn the_live_operation_row_names_registered_icons_in_variant_order() {
    let h = TestHarness::new();
    let def = h
        .state()
        .registry
        .get("Boolean")
        .expect("the Boolean node type is registered");
    let editor = def
        .properties()
        .into_iter()
        .find(|p| p.name.as_ref() == "operation")
        .expect("the Boolean node has an operation row")
        .editor;

    let variants = editor.enum_variants().expect("operation is an enum row");
    let icons = editor.enum_icon_ids().expect("operation is an icon row");
    assert_eq!(variants.len(), OPERATIONS.len());
    assert_eq!(icons.len(), variants.len());
    for (i, id) in icons.iter().enumerate() {
        assert_eq!(variants[i].as_ref(), OPERATIONS[i]);
        assert!(
            vector_icon::icon(id).is_some(),
            "the row asks for '{}' (variant '{}') but nothing is registered under it",
            id,
            variants[i]
        );
    }
}
