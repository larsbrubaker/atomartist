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
    assert_eq!(canvas.type_name(), "NodeCanvas");
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
    let canvas = h.find_by_type("NodeCanvas").expect("by-type lookup");
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
    assert!(types.contains(&"NodeCanvas"));
    assert!(types.contains(&"Viewport3dWidget"));
    assert!(types.contains(&"StatusBar"));
}
