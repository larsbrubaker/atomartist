//! Widget-behavior tests.
//!
//! Equivalents of the following NodeDesigner suites — each verifies
//! a specific widget's externally-visible behaviour against the live
//! production widget tree:
//!
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/editable-slider-widget.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/inline-edit-manager.test.ts`
//! - `MatterHackers/FDS/NodeDesigner/tests/unit/standalone-node-renderer.test.ts`

use atomartist_ui_test::TestHarness;

#[test]
fn canvas_widget_has_stable_id() {
    let h = TestHarness::new();
    let canvas = h.find_by_id("node-canvas").expect("canvas widget");
    // Type name now reflects the generic agg-gui-node-editor widget;
    // the id stays stable so existing test selectors still hit it.
    assert_eq!(canvas.type_name(), "NodeEditor");
}

#[test]
fn viewport_widget_has_stable_id_and_type() {
    let h = TestHarness::new();
    let viewport = h.find_by_id("viewport-3d").expect("viewport widget");
    assert_eq!(viewport.type_name(), "Viewport3dWidget");
}

#[test]
fn status_bar_has_stable_id() {
    let h = TestHarness::new();
    let status = h.find_by_id("status-bar").expect("status bar widget");
    assert_eq!(status.type_name(), "StatusBar");
}

#[test]
fn find_by_type_locates_canvas() {
    let h = TestHarness::new();
    let canvas = h.find_by_type("NodeEditor").expect("by-type lookup");
    assert!(canvas.id() == Some("node-canvas"));
}

#[test]
fn snapshot_includes_all_three_atomartist_widgets() {
    // Inspector tree should expose at least the three primary AtomArtist
    // widgets so any future port of NodeDesigner's standalone-node-renderer
    // tests has reflection-discoverable surface area.
    let h = TestHarness::new();
    let snap = h.snapshot();
    let types: Vec<&str> = snap.iter().map(|n| n.type_name).collect();
    assert!(types.contains(&"NodeEditor"));
    assert!(types.contains(&"Viewport3dWidget"));
    assert!(types.contains(&"StatusBar"));
}

#[test]
fn top_menu_bar_hugs_top_with_no_padding_strip() {
    // Regression for the wasted-space-above-menu UI report: the top chrome
    // row used to be 36 px tall while the menu bar inside it was 26 px,
    // leaving a 10 px gray strip pinned to the top of the window. The
    // fix locks the row to `MENU_BAR_H` so the menu fills the whole row
    // — traditional Windows-style placement against the top edge.
    let h = TestHarness::new();
    let bar = h.find_by_type("MenuBar").expect("menu bar widget");
    let bar_h = bar.bounds().height;
    assert!(
        (bar_h - agg_gui::widgets::menu::MENU_BAR_H).abs() < 0.5,
        "menu bar height = {bar_h} (expected MENU_BAR_H = {})",
        agg_gui::widgets::menu::MENU_BAR_H,
    );

    // The menu bar is the first child of the top FlexRow; that row sits
    // at the top of the column. Walk to it through reflection and
    // assert the row's height matches the menu bar's so there is no
    // dead chrome strip above the menu items.
    let row = h.find_by_type("FlexRow").expect("top FlexRow widget");
    assert!(
        (row.bounds().height - bar_h).abs() < 0.5,
        "top row height ({}) should equal menu bar height ({bar_h})",
        row.bounds().height,
    );
}
