//! Unit tests for [`crate::graph_nav`] — geometry, hit regions, tooltips
//! and the commands each button queues. No font, no frame, no GPU: the
//! cluster is laid out directly and driven with synthetic events.
//!
//! The end-to-end "click home and the canvas frames the graph" path lives
//! in `atomartist-ui-test/tests/graph_nav.rs`.

use super::*;
use agg_gui::Modifiers;

const PANE: Size = Size {
    width: 900.0,
    height: 300.0,
};

fn cluster() -> (GraphNavCluster, NodeEditorHandle) {
    let handle = NodeEditorHandle::new();
    let mut c = GraphNavCluster::new(handle.clone());
    c.layout(PANE);
    (c, handle)
}

fn click(c: &mut GraphNavCluster, pos: Point) {
    c.on_event(&Event::MouseDown {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    c.on_event(&Event::MouseUp {
        pos,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
}

/// ND's insets are top-down; ours are the same distance from the pane's
/// TOP edge in Y-up coords.
#[test]
fn buttons_sit_twelve_pixels_below_the_panes_top_edge() {
    let (c, _) = cluster();
    let home = c.home_rect();
    assert_eq!(home.x, 12.0);
    assert_eq!(home.width, BUTTON);
    assert_eq!(
        home.y + home.height,
        PANE.height - TOP_INSET,
        "top edge of the button is 12 px below the pane's top"
    );
    // The mode group starts at ND's 64 px and the segments abut.
    assert_eq!(c.mode_rect(0).x, 64.0);
    assert_eq!(c.mode_rect(1).x, 64.0 + BUTTON);
    assert_eq!(c.mode_rect(2).x, 64.0 + 2.0 * BUTTON);
    assert_eq!(c.mode_rect(0).y, home.y, "one row");
}

/// Everything that is not a button falls through to the canvas below.
#[test]
fn only_the_buttons_take_the_pointer() {
    let (c, _) = cluster();
    assert!(c.hit_test(GraphNavCluster::home_center(PANE.height)));
    assert!(c.hit_test(GraphNavCluster::mode_center(PANE.height, 2)));
    // The gap between the two groups is canvas.
    assert!(!c.hit_test(Point::new(58.0, PANE.height - TOP_INSET - 20.0)));
    // So is the middle of the pane.
    assert!(!c.hit_test(Point::new(450.0, 150.0)));
    // …and so is everything below the row.
    assert!(!c.hit_test(Point::new(20.0, PANE.height - TOP_INSET - BUTTON - 4.0)));
}

/// Home queues the animated fit; it never touches the mode.
#[test]
fn home_queues_fit_to_content() {
    let (mut c, handle) = cluster();
    click(&mut c, GraphNavCluster::home_center(PANE.height));
    assert_eq!(handle.take(), vec![NodeEditorCommand::FitToContent]);
    assert_eq!(c.properties()[0].1, "select", "mode is untouched");
}

/// Segments switch the mode, and re-clicking the active one is a no-op
/// (no redundant command reaches the editor).
#[test]
fn mode_segments_switch_and_deduplicate() {
    let (mut c, handle) = cluster();
    assert_eq!(c.properties()[0].1, "select", "Select is the default");

    click(&mut c, GraphNavCluster::mode_center(PANE.height, 1));
    assert_eq!(
        handle.take(),
        vec![NodeEditorCommand::SetInteractionMode(InteractionMode::Pan)]
    );
    assert_eq!(c.properties()[0].1, "pan");

    click(&mut c, GraphNavCluster::mode_center(PANE.height, 1));
    assert!(handle.take().is_empty(), "already in pan mode");

    click(&mut c, GraphNavCluster::mode_center(PANE.height, 2));
    assert_eq!(
        handle.take(),
        vec![NodeEditorCommand::SetInteractionMode(InteractionMode::Zoom)]
    );
    assert_eq!(c.properties()[0].1, "zoom");
}

/// A press that slides off its button before release does nothing.
#[test]
fn a_press_that_slides_off_does_not_fire() {
    let (mut c, handle) = cluster();
    c.on_event(&Event::MouseDown {
        pos: GraphNavCluster::home_center(PANE.height),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    c.on_event(&Event::MouseUp {
        pos: Point::new(450.0, 150.0),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    assert!(handle.take().is_empty());
}

/// Tooltips are ND's `title` strings, published only while the pointer is
/// over the matching button — the cluster is one widget, so a constant
/// string would offer the wrong help everywhere.
#[test]
fn tooltips_track_the_hovered_button() {
    let (mut c, _) = cluster();
    assert_eq!(c.tooltip_text(), None);

    c.on_event(&Event::MouseMove {
        pos: GraphNavCluster::home_center(PANE.height),
    });
    assert_eq!(c.tooltip_text(), Some("Reset graph view"));

    for (index, expected) in [
        (0, "Select Mode"),
        (1, "Pan Mode (middle click)"),
        (2, "Zoom Mode (scroll wheel)"),
    ] {
        c.on_event(&Event::MouseMove {
            pos: GraphNavCluster::mode_center(PANE.height, index),
        });
        assert_eq!(c.tooltip_text(), Some(expected));
    }

    // agg-gui delivers a (-1, -1) move to the widget the pointer left,
    // which no rectangle contains — that clears the hover latch.
    c.on_event(&Event::MouseMove {
        pos: Point::new(-1.0, -1.0),
    });
    assert_eq!(c.tooltip_text(), None);
    assert_eq!(c.properties()[1].1, "none");
}

/// The cluster re-pins to the top when the splitter resizes the pane.
#[test]
fn the_row_follows_the_panes_top_edge_on_resize() {
    let (mut c, _) = cluster();
    c.layout(Size::new(900.0, 500.0));
    assert_eq!(c.home_rect().y + BUTTON, 500.0 - TOP_INSET);
    assert!(c.hit_test(GraphNavCluster::home_center(500.0)));
}
