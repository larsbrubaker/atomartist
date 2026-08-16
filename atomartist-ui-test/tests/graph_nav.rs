//! Node-canvas navigation cluster (design §5d step 6h-3) end-to-end
//! through the live widget tree: the home button frames the graph with an
//! animation, and the mode group re-binds the left mouse button.
//!
//! NodeDesigner equivalent: `views/partials/_graph_panel.ejs`
//! (`.graph-controls`) driving `static/js/node-editor/core/graph-manager.js`
//! (`resetView`, `setInteractionMode`).

use agg_gui::{MouseButton, Point};
use atomartist_ui::graph_nav::GraphNavCluster;
use atomartist_ui_test::TestHarness;

/// Absolute (root, Y-up) bounds of a widget, by type name.
///
/// `Widget::bounds()` is parent-relative — the canvas sits inside a
/// `Stack` inside a pane probe inside the splitter — so the inspector
/// snapshot, which accumulates the transform as it walks, is the only
/// honest source of screen coordinates here.
fn screen_bounds(h: &TestHarness, type_name: &str) -> agg_gui::Rect {
    h.snapshot()
        .into_iter()
        .find(|n| n.type_name == type_name)
        .unwrap_or_else(|| panic!("no {type_name} in the tree"))
        .screen_bounds
}

fn nav_bounds(h: &TestHarness) -> agg_gui::Rect {
    screen_bounds(h, "GraphNavCluster")
}

fn canvas_bounds(h: &TestHarness) -> agg_gui::Rect {
    screen_bounds(h, "NodeEditor")
}

/// Root-absolute Y-up point for a cluster-local one.
fn to_root(h: &TestHarness, local: Point) -> Point {
    let b = nav_bounds(h);
    Point::new(b.x + local.x, b.y + local.y)
}

fn pane_height(h: &TestHarness) -> f64 {
    nav_bounds(h).height
}

/// The cluster's `mode` property — the same channel the inspector reads.
fn nav_property(h: &TestHarness, key: &str) -> String {
    h.snapshot()
        .into_iter()
        .find(|n| n.type_name == "GraphNavCluster")
        .and_then(|n| {
            n.properties
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        })
        .unwrap_or_else(|| panic!("graph-nav has no `{key}` property"))
}

/// Run frames for `ms` of wall clock, which is how the fit animation
/// (500 ms, eased) advances — it is ticked from the editor's `layout`.
fn animate(h: &mut TestHarness, ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        h.frame();
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    h.frame();
}

/// Home frames every node: after the animation, each node's canvas
/// position maps inside the pane, and the view is no longer the absurd
/// one we started from.
#[test]
fn home_button_animates_a_fit_of_every_node() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    // Start somewhere hopeless so "it framed" is unambiguous.
    h.state()
        .apply_project_view(Some(&atomartist_lib::serialization::ProjectView {
            view_state: Some(atomartist_lib::serialization::CanvasView {
                scale: 2.5,
                offset: [-4000.0, 3000.0],
            }),
            ..Default::default()
        }));
    h.frame();

    let height = pane_height(&h);
    let home = to_root(&h, GraphNavCluster::home_center(height));
    h.click_local(home, MouseButton::Left);
    animate(&mut h, 700);

    let scale = *h.state().canvas_zoom.lock().unwrap();
    let offset = *h.state().canvas_pan.lock().unwrap();
    assert!(scale > 0.0 && scale != 2.5, "the fit changed the zoom");

    let pane = canvas_bounds(&h);
    let positions: Vec<[f64; 2]> = {
        let g = h.state().graph.lock().unwrap();
        g.nodes().map(|n| n.position).collect()
    };
    assert!(!positions.is_empty(), "starter graph has nodes");
    for p in positions {
        let x = p[0] * scale + offset[0];
        let y = p[1] * scale + offset[1];
        assert!(
            x >= 0.0 && x <= pane.width && y >= 0.0 && y <= pane.height,
            "node at {p:?} maps to ({x}, {y}) outside the {} × {} pane",
            pane.width,
            pane.height
        );
    }

    // The animation terminates: another 200 ms of frames move nothing.
    animate(&mut h, 200);
    assert_eq!(*h.state().canvas_zoom.lock().unwrap(), scale);
    assert_eq!(*h.state().canvas_pan.lock().unwrap(), offset);
}

/// Pan mode: a left-drag that starts *on a node* pans the canvas and
/// leaves the node where it was.
#[test]
fn pan_mode_pans_instead_of_dragging_the_node_under_the_pointer() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    let height = pane_height(&h);

    h.click_local(
        to_root(&h, GraphNavCluster::mode_center(height, 1)),
        MouseButton::Left,
    );
    h.frame();
    assert_eq!(nav_property(&h, "mode"), "pan");

    // A node far enough right that the cluster is nowhere near it.
    let (id, pos) = {
        let g = h.state().graph.lock().unwrap();
        let n = g
            .nodes()
            .find(|n| n.type_id.as_ref() == "Extrude")
            .expect("starter graph has an Extrude");
        (n.id, n.position)
    };
    let pane = canvas_bounds(&h);
    let press = Point::new(pane.x + pos[0] + 20.0, pane.y + pos[1] - 12.0);
    let pan_before = *h.state().canvas_pan.lock().unwrap();

    let (px, py) = h.to_screen(press);
    h.mouse_move(px, py);
    h.mouse_down(MouseButton::Left);
    h.mouse_move(px + 40.0, py - 25.0); // right and (Y-up) up
    h.mouse_up(MouseButton::Left);

    let pan_after = *h.state().canvas_pan.lock().unwrap();
    assert_eq!(
        pan_after,
        [pan_before[0] + 40.0, pan_before[1] + 25.0],
        "the drag panned the canvas"
    );
    let after = {
        let g = h.state().graph.lock().unwrap();
        g.get(id).expect("node still there").position
    };
    assert_eq!(after, pos, "pan mode must not drag the node under it");
}

/// Select mode is the default and still drags nodes — the mode group
/// changes nothing until the user asks.
#[test]
fn select_mode_is_the_default_and_still_drags_nodes() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    assert_eq!(nav_property(&h, "mode"), "select");

    let (id, pos) = {
        let g = h.state().graph.lock().unwrap();
        let n = g
            .nodes()
            .find(|n| n.type_id.as_ref() == "Extrude")
            .expect("starter graph has an Extrude");
        (n.id, n.position)
    };
    let pane = canvas_bounds(&h);
    let press = Point::new(pane.x + pos[0] + 20.0, pane.y + pos[1] - 12.0);
    let (px, py) = h.to_screen(press);
    h.mouse_move(px, py);
    h.mouse_down(MouseButton::Left);
    h.mouse_move(px + 30.0, py);
    h.mouse_up(MouseButton::Left);

    let after = {
        let g = h.state().graph.lock().unwrap();
        g.get(id).expect("node still there").position
    };
    assert!(
        (after[0] - pos[0] - 30.0).abs() < 1.0,
        "select mode still drags: {after:?} from {pos:?}"
    );
}

/// The cluster is invisible to the pointer everywhere but its buttons:
/// a click just below the row reaches the canvas and clears the
/// selection, exactly as it did before the cluster existed.
#[test]
fn clicks_outside_the_buttons_fall_through_to_the_canvas() {
    let mut h = TestHarness::with_starter_graph();
    h.frame();
    h.state()
        .set_selection(Some(atomartist_lib::graph::node::NodeId(99)));

    let pane = canvas_bounds(&h);
    // Same row as the cluster but far to the right, past every node —
    // empty canvas, and a click there must reach it.
    let point = Point::new(pane.x + pane.width - 60.0, pane.y + pane.height - 32.0);
    h.click_local(point, MouseButton::Left);
    assert_eq!(*h.state().selection.lock().unwrap(), None);
}
