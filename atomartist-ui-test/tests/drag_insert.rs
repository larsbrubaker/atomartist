//! Drag-drop insert from the favorites bar into the scene, driven
//! through the real widget tree (`docs/file-browser-design.md` §1.3, §2
//! drop-pipeline rows, §6 — step 6e).
//!
//! Ancestors: NodeDesigner's
//! `static/js/node-editor/ui/parts-bar-drag.js` (threshold → ghost →
//! insert on canvas-enter → remove on leave) and MatterCAD's
//! `ViewDragDropHandler` (single undo on commit). Neither ancestor
//! shipped tests for the gesture, so the assertions here come from the
//! behaviour the design records.
//!
//! Every gesture is a real `on_mouse_*` stream through `build_app`, which
//! is the point: the controller shares the event path with the canvas, so
//! a passing test here means the desktop app behaves the same way.
//!
//! Coordinates: `find_widget_screen_rect` reports screen-absolute **Y-up**
//! rects; `to_screen` flips them for the event helpers.
//!
//! Since step 6f-1 the bar is docked in the *3-D viewport* pane, so every
//! drag here crosses the splitter on its way to the node canvas — the
//! controller works from the canvas rectangle the bar publishes, not from
//! its own width.

use std::sync::Arc;

use agg_gui::widget::find_widget_screen_rect;
use agg_gui::{Key, MouseButton, Point, Rect, Size};
use atomartist_storage::{MemoryProvider, StorageProvider, StorageRegistry, StorageUri};
use atomartist_ui::favorites_bar::{BAR_ID, EMBEDDED_BROWSER_ID};
use atomartist_ui::favorites_bar_geom::{self as bar_geom, HANDLE_W};
use atomartist_ui::file_browser::widget_geom::{self as geom, BrowserLayout};
use atomartist_ui::file_browser::{BrowserMode, Favorite, SEED_NODE_TYPES};
use atomartist_ui::{fresh_state_with_starter_graph_and_storage, DRAG_GHOST_ID};
use atomartist_ui_test::TestHarness;

const SCHEME: &str = "mem";

fn uri(path: &str) -> StorageUri {
    StorageUri::new(SCHEME, path)
}

fn memory_registry() -> Arc<StorageRegistry> {
    let mut registry = StorageRegistry::new();
    registry
        .register(Arc::new(MemoryProvider::new(SCHEME, "Test Memory")) as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    Arc::new(registry)
}

// ── Geometry helpers (same shape as tests/favorites_bar.rs) ─────────────

fn rect_of(h: &TestHarness, id: &str) -> Rect {
    find_widget_screen_rect(h.app().root(), id)
        .unwrap_or_else(|| panic!("no visible widget with id `{id}`"))
}

fn prop(h: &TestHarness, id: &str, key: &str) -> String {
    h.find_by_id(id)
        .unwrap_or_else(|| panic!("no widget with id `{id}`"))
        .properties()
        .into_iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("`{id}` exposes no `{key}` property"))
}

fn expanded(h: &TestHarness) -> bool {
    prop(h, BAR_ID, "expanded") == "true"
}

fn dragging(h: &TestHarness) -> bool {
    prop(h, BAR_ID, "dragging") == "true"
}

fn item_rect(h: &TestHarness, index: usize) -> Rect {
    let bar = rect_of(h, BAR_ID);
    let count: usize = prop(h, BAR_ID, "favorites").parse().unwrap();
    let scroll: f64 = prop(h, BAR_ID, "scroll").parse().unwrap();
    let layout = bar_geom::compute(
        Size::new(bar.width, bar.height),
        expanded(h),
        count,
        h.state().current_file.lock().unwrap().is_some(),
        scroll,
    );
    let local = *layout
        .items
        .get(index)
        .unwrap_or_else(|| panic!("the strip has no item {index} in {bar:?}"));
    assert!(
        bar_geom::item_visible(local, layout.items_viewport),
        "item {index} is scrolled out of view; scroll it in before dragging it"
    );
    Rect::new(bar.x + local.x, bar.y + local.y, local.width, local.height)
}

/// Wheel `notches` over the strip (negative = reveal favourites further
/// down the palette).
fn wheel_over_strip(h: &mut TestHarness, notches: f64) {
    let bar = rect_of(h, BAR_ID);
    let over = Point::new(bar.x + 20.0, bar.y + bar.height * 0.5);
    let (x, y) = h.to_screen(over);
    h.mouse_move(x, y);
    h.scroll(notches);
}

fn item_center(h: &TestHarness, index: usize) -> Point {
    let r = item_rect(h, index);
    Point::new(r.x + r.width * 0.5, r.y + r.height * 0.5)
}

fn handle_center(h: &TestHarness) -> Point {
    let bar = rect_of(h, BAR_ID);
    Point::new(bar.x + bar.width - HANDLE_W * 0.5, bar.y + bar.height * 0.5)
}

fn embedded_cell(h: &TestHarness, index: usize) -> Point {
    let browser = rect_of(h, EMBEDDED_BROWSER_ID);
    let layout = BrowserLayout::compute(
        Size::new(browser.width, browser.height),
        BrowserMode::Embedded,
    );
    let count: usize = prop(h, EMBEDDED_BROWSER_ID, "entries").parse().unwrap();
    let geo = geom::grid_geometry(layout.grid, count);
    let local = geom::cell_rect(layout.grid, &geo, index, 0.0).center();
    Point::new(browser.x + local.x, browser.y + local.y)
}

/// A point well inside the node canvas — deliberately in its lower-right
/// quadrant, clear of the starter graph's row of nodes.
fn canvas_point(h: &TestHarness) -> Point {
    let c = rect_of(h, "node-canvas");
    Point::new(c.x + c.width * 0.7, c.y + c.height * 0.3)
}

fn node_count(h: &TestHarness) -> usize {
    h.state().graph.lock().unwrap().node_count()
}

/// Press at `from` (Y-up world coords) and move through `via`, without
/// releasing. Returns the harness for chaining.
fn press_and_move(h: &mut TestHarness, from: Point, via: &[Point]) {
    let (x0, y0) = h.to_screen(from);
    h.mouse_move(x0, y0);
    h.mouse_down(MouseButton::Left);
    for p in via {
        let (x, y) = h.to_screen(*p);
        h.mouse_move(x, y);
    }
}

fn release_at(h: &mut TestHarness, at: Point) {
    let (x, y) = h.to_screen(at);
    h.mouse_move(x, y);
    h.mouse_up(MouseButton::Left);
}

/// A point just above the press, still over the bar: enough travel to
/// pass the 4 px threshold without leaving the bar.
fn nudged(p: Point) -> Point {
    Point::new(p.x, p.y - 12.0)
}

/// Write a project into the memory provider so the browser has something
/// to list and drag. Returns its node count.
fn seed_project(storage: &Arc<StorageRegistry>, at: &StorageUri) -> usize {
    let seed = fresh_state_with_starter_graph_and_storage(storage.clone());
    {
        let mut graph = seed.graph.lock().unwrap();
        graph
            .add_new_node("Box", [400.0, 400.0], &seed.registry)
            .expect("Box is a built-in");
    }
    seed.save_project(at);
    for _ in 0..8 {
        if !seed.pump_storage() {
            break;
        }
    }
    let count = seed.graph.lock().unwrap().node_count();
    count
}

// ── Click behaviour must not regress ────────────────────────────────────

/// A press released in place on a strip item is still a click: it inserts
/// nothing and never reports a drag. (The seeded favourites are node
/// types, whose click is deliberately inert — see `favorites_bar`.)
#[test]
fn sub_threshold_click_on_a_strip_item_inserts_nothing() {
    let mut h = TestHarness::with_starter_graph();
    let before = node_count(&h);
    let row = item_center(&h, 0);

    // Move by less than the 4 px threshold before releasing.
    press_and_move(&mut h, row, &[Point::new(row.x + 2.0, row.y + 1.0)]);
    assert!(!dragging(&h), "2 px is a click, not a drag");
    release_at(&mut h, Point::new(row.x + 2.0, row.y + 1.0));

    assert_eq!(node_count(&h), before, "a click must not insert a node");
    assert!(!dragging(&h));
    assert!(
        h.find_by_id(DRAG_GHOST_ID).is_none(),
        "no ghost for a click"
    );
}

/// The handle's own click (toggle) and drag (resize) are untouched by the
/// new gesture — the two share the bar's event handler.
#[test]
fn sub_threshold_click_on_the_handle_still_toggles() {
    let mut h = TestHarness::with_starter_graph();
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    assert!(expanded(&h), "the handle still toggles the panel open");
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    assert!(!expanded(&h));
}

/// A click on a browser entry still selects it (the browser's click
/// behaviour runs on the press; the drag is only a candidate).
#[test]
fn sub_threshold_click_in_the_browser_still_selects() {
    let storage = memory_registry();
    seed_project(&storage, &uri("/bracket.atmr"));
    let state = fresh_state_with_starter_graph_and_storage(storage);
    let mut h = TestHarness::with_app_state(state);

    *h.state().favorites_bar_width.lock().unwrap() = 420.0;
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.pump_until_idle(8);
    h.frame();
    assert_eq!(prop(&h, EMBEDDED_BROWSER_ID, "entries"), "1");

    let cell = embedded_cell(&h, 0);
    h.click_local(cell, MouseButton::Left);
    assert_eq!(
        prop(&h, EMBEDDED_BROWSER_ID, "selected"),
        "bracket.atmr",
        "a plain click still selects the entry"
    );
    assert!(!dragging(&h));
}

// ── Ghost + insert on canvas-enter ──────────────────────────────────────

/// Past the threshold, outside the canvas: the bar reports a drag and the
/// ghost is floating in the app's overlay slot.
#[test]
fn dragging_a_favorite_past_the_threshold_raises_the_ghost() {
    let mut h = TestHarness::with_starter_graph();
    let before = node_count(&h);
    let row = item_center(&h, 0);
    press_and_move(&mut h, row, &[nudged(row)]);

    assert!(dragging(&h), "the bar reflects the drag for the inspector");
    assert!(
        h.find_by_id(DRAG_GHOST_ID).is_some(),
        "a ghost follows the cursor while outside the canvas"
    );
    assert_eq!(node_count(&h), before, "nothing inserted yet");
}

/// The deliverable: dragging a node-type favourite onto the canvas and
/// releasing leaves exactly one new node, at the cursor, evaluated — and
/// one undo entry that removes the whole gesture.
#[test]
fn dropping_a_node_type_favorite_adds_one_node_and_one_undo_step() {
    let mut h = TestHarness::with_starter_graph();
    let before = node_count(&h);
    let row = item_center(&h, 0);
    let drop = canvas_point(&h);

    press_and_move(&mut h, row, &[nudged(row), drop]);
    // Insert-on-enter: the node exists before the release.
    assert_eq!(node_count(&h), before + 1, "crossing in inserts the node");
    assert!(
        h.find_by_id(DRAG_GHOST_ID).is_none(),
        "the ghost gives way to the real node"
    );
    release_at(&mut h, drop);

    assert_eq!(node_count(&h), before + 1);
    let expected_type = SEED_NODE_TYPES[0];
    let canvas = rect_of(&h, "node-canvas");
    {
        let graph = h.state().graph.lock().unwrap();
        let node = graph
            .nodes()
            .find(|n| n.type_id.as_ref() == expected_type)
            .expect("the dragged type landed in the graph");
        // Pan 0 / zoom 1: canvas-space == canvas-widget-local.
        assert!((node.position[0] - (drop.x - canvas.x)).abs() < 1.0);
        assert!((node.position[1] - (drop.y - canvas.y)).abs() < 1.0);
    }
    h.evaluate_now();

    // One Ctrl+Z takes the whole gesture back.
    let undo = h.state().active_undo();
    assert!(undo.lock().unwrap().can_undo(), "the drop is undoable");
    undo.lock().unwrap().undo();
    assert_eq!(node_count(&h), before, "one undo removes the dropped node");
    assert!(
        !undo.lock().unwrap().can_undo(),
        "the gesture pushed exactly one undo entry"
    );
}

/// Dragging in and back out again restores the graph and re-raises the
/// ghost; releasing outside inserts nothing and leaves the undo stack
/// empty.
#[test]
fn dragging_in_and_back_out_leaves_nothing_behind() {
    let mut h = TestHarness::with_starter_graph();
    let before = node_count(&h);
    let row = item_center(&h, 0);
    let drop = canvas_point(&h);

    press_and_move(&mut h, row, &[nudged(row), drop, nudged(row)]);
    assert_eq!(node_count(&h), before, "leaving removes the carried node");
    assert!(
        h.find_by_id(DRAG_GHOST_ID).is_some(),
        "and the ghost comes back"
    );

    release_at(&mut h, nudged(row));
    assert_eq!(node_count(&h), before);
    assert!(
        !h.state().active_undo().lock().unwrap().can_undo(),
        "a cancelled gesture must not touch the undo stack"
    );
    assert!(!dragging(&h));
    assert!(h.find_by_id(DRAG_GHOST_ID).is_none(), "ghost is dropped");
}

/// Escape mid-drag cancels: the carried node goes away and the ghost with
/// it, even though the button is still down.
#[test]
fn escape_cancels_a_drag_in_flight() {
    let mut h = TestHarness::with_starter_graph();
    let before = node_count(&h);
    let row = item_center(&h, 0);
    let drop = canvas_point(&h);

    press_and_move(&mut h, row, &[nudged(row), drop]);
    assert_eq!(node_count(&h), before + 1);

    h.key_down(Key::Escape);
    assert_eq!(node_count(&h), before, "Escape removes the carried node");
    assert!(!dragging(&h));
    assert!(h.find_by_id(DRAG_GHOST_ID).is_none());

    // The release that follows must not resurrect anything.
    release_at(&mut h, drop);
    assert_eq!(node_count(&h), before);
    assert!(!h.state().active_undo().lock().unwrap().can_undo());
}

/// Base-position snapshot: many moves inside the canvas leave the node
/// exactly under the final cursor position, with no accumulated drift.
#[test]
fn wiggling_over_the_canvas_never_drifts() {
    let mut h = TestHarness::with_starter_graph();
    let row = item_center(&h, 0);
    let start = canvas_point(&h);

    press_and_move(&mut h, row, &[nudged(row), start]);
    let wiggle: Vec<Point> = (0..60)
        .map(|i| {
            Point::new(
                start.x + ((i % 9) as f64) * 4.0,
                start.y - ((i % 7) as f64) * 3.0,
            )
        })
        .collect();
    for p in &wiggle {
        let (x, y) = h.to_screen(*p);
        h.mouse_move(x, y);
    }
    let last = Point::new(start.x + 37.0, start.y - 21.0);
    release_at(&mut h, last);

    let canvas = rect_of(&h, "node-canvas");
    let graph = h.state().graph.lock().unwrap();
    let node = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == SEED_NODE_TYPES[0])
        .expect("the dragged node is in the graph");
    assert!(
        (node.position[0] - (last.x - canvas.x)).abs() < 1.0
            && (node.position[1] - (last.y - canvas.y)).abs() < 1.0,
        "node at {:?} should sit under the cursor {:?}",
        node.position,
        last
    );
}

// ── File payloads: same import path as an OS file drop ──────────────────

/// A pinned project dragged out of the strip imports into the current
/// scene — the same `import_dropped_file` call the OS file-drop handler
/// makes.
#[test]
fn dragging_a_project_favorite_imports_it() {
    let storage = memory_registry();
    let saved = uri("/bracket.atmr");
    seed_project(&storage, &saved);
    let state = fresh_state_with_starter_graph_and_storage(storage);
    state
        .favorites
        .lock()
        .unwrap()
        .add(Favorite::project(&saved));
    let mut h = TestHarness::with_app_state(state);
    h.frame();

    let before = node_count(&h);
    let count: usize = prop(&h, BAR_ID, "favorites").parse().unwrap();
    // `add` appends, so the pinned project is the last item — and at
    // 1280×720 the eight favourites (seeded palette + this project) do
    // not all fit, so scroll it into view first. That the drag then
    // works is the point: hit-testing and drag sources honour the
    // scroll offset.
    wheel_over_strip(&mut h, -8.0);
    let row = item_center(&h, count - 1);
    let drop = canvas_point(&h);

    press_and_move(&mut h, row, &[nudged(row), drop]);
    assert_eq!(
        node_count(&h),
        before,
        "a file payload is not carried live — it lands on release"
    );
    assert!(h.find_by_id(DRAG_GHOST_ID).is_some());
    release_at(&mut h, drop);
    h.pump_until_idle(8);

    assert!(
        node_count(&h) > before,
        "the dropped project merged into the scene"
    );
    assert_eq!(
        h.state().current_file.lock().unwrap().clone(),
        None,
        "an import is not an open — Save must not retarget"
    );
}

/// …and the same holds for an entry dragged out of the embedded browser
/// grid, the third drag surface.
#[test]
fn dragging_a_browser_entry_imports_it() {
    let storage = memory_registry();
    seed_project(&storage, &uri("/bracket.atmr"));
    let state = fresh_state_with_starter_graph_and_storage(storage);
    let mut h = TestHarness::with_app_state(state);

    *h.state().favorites_bar_width.lock().unwrap() = 420.0;
    let handle = handle_center(&h);
    h.click_local(handle, MouseButton::Left);
    h.pump_until_idle(8);
    h.frame();
    assert_eq!(prop(&h, EMBEDDED_BROWSER_ID, "entries"), "1");

    let before = node_count(&h);
    let cell = embedded_cell(&h, 0);
    let drop = canvas_point(&h);
    press_and_move(&mut h, cell, &[nudged(cell), drop]);
    assert!(
        h.find_by_id(DRAG_GHOST_ID).is_some(),
        "the browser feeds the same controller"
    );
    release_at(&mut h, drop);
    h.pump_until_idle(8);

    assert!(node_count(&h) > before, "the entry imported into the scene");
}

/// Pressing the bar's resize handle mid-drag takes the mouse capture
/// away from the gesture — the release will never come back to it — so
/// the press must end the drag rather than orphan the node it carried.
#[test]
fn starting_a_handle_drag_mid_gesture_gives_the_carried_node_back() {
    let mut h = TestHarness::with_starter_graph();
    let before = node_count(&h);
    let row = item_center(&h, 0);
    let drop = canvas_point(&h);

    press_and_move(&mut h, row, &[nudged(row), drop]);
    assert_eq!(node_count(&h), before + 1, "a node is being carried");

    // Second press, on the handle: agg-gui re-targets its single
    // capture slot here.
    let handle = handle_center(&h);
    let (hx, hy) = h.to_screen(handle);
    h.mouse_move(hx, hy);
    h.mouse_down(MouseButton::Left);

    assert_eq!(node_count(&h), before, "the carried node is handed back");
    assert!(
        !h.state().active_undo().lock().unwrap().can_undo(),
        "an abandoned gesture must not touch the undo stack"
    );
    assert!(h.find_by_id(DRAG_GHOST_ID).is_none(), "and no ghost leaks");
    h.mouse_up(MouseButton::Left);
}

// ── Step 6f-4: dropping on the 3-D bed ──────────────────────────────────

/// A point in the middle of the 3-D viewport — the bed, the second drop
/// target since 6f-4.
fn viewport_point(h: &TestHarness) -> Point {
    let v = rect_of(h, "viewport-3d");
    Point::new(v.x + v.width * 0.5, v.y + v.height * 0.5)
}

/// The deliverable of step 6f-4: dragging a palette favourite onto the
/// **bed** inserts it, places it left of the Output node, wires it in,
/// and undoes as one step.
#[test]
fn dropping_a_node_type_on_the_bed_places_and_wires_it() {
    let mut h = TestHarness::with_starter_graph();
    let before_nodes = node_count(&h);
    let before_noodles = h.state().graph.lock().unwrap().noodle_count();
    let row = item_center(&h, 0);
    let drop = viewport_point(&h);

    press_and_move(&mut h, row, &[nudged(row), drop]);
    // v1 ghosts over the bed — nothing is carried live there.
    assert!(
        h.find_by_id(DRAG_GHOST_ID).is_some(),
        "the ghost stays up over the bed"
    );
    assert_eq!(
        node_count(&h),
        before_nodes,
        "nothing inserted before release"
    );
    release_at(&mut h, drop);

    assert_eq!(node_count(&h), before_nodes + 1);
    {
        let graph = h.state().graph.lock().unwrap();
        let node = graph
            .nodes()
            .find(|n| n.type_id.as_ref() == SEED_NODE_TYPES[0])
            .expect("the dragged type landed in the graph");
        let output = graph
            .nodes()
            .find(|n| n.type_id.as_ref() == "Output")
            .expect("the starter graph has an Output");
        assert!(
            graph
                .noodles()
                .iter()
                .any(|n| n.from.node == node.id && n.to.node == output.id),
            "the bed drop auto-wires into the Output"
        );
        assert!(
            node.position[0] < output.position[0],
            "and is placed left of it, got {:?}",
            node.position
        );
    }
    h.evaluate_now();

    let undo = h.state().active_undo();
    assert!(undo.lock().unwrap().can_undo());
    undo.lock().unwrap().undo();
    assert_eq!(node_count(&h), before_nodes);
    assert_eq!(
        h.state().graph.lock().unwrap().noodle_count(),
        before_noodles,
        "insert and wire undo together"
    );
    assert!(
        !undo.lock().unwrap().can_undo(),
        "the gesture pushed exactly one undo entry"
    );
}

/// The bar and its handle share the viewport's pane but are not the bed:
/// releasing over them cancels, as it always has.
#[test]
fn releasing_over_the_bar_handle_is_not_a_bed_drop() {
    let mut h = TestHarness::with_starter_graph();
    let before = node_count(&h);
    let row = item_center(&h, 0);
    let handle = handle_center(&h);

    let bed = viewport_point(&h);
    press_and_move(&mut h, row, &[nudged(row), bed, handle]);
    release_at(&mut h, handle);

    assert_eq!(
        node_count(&h),
        before,
        "the bar's chrome is not a drop target"
    );
    assert!(!h.state().active_undo().lock().unwrap().can_undo());
    assert!(h.find_by_id(DRAG_GHOST_ID).is_none());
}

/// A canvas drop keeps the position the user picked (the placement
/// helper is only for insertions the user did not position) and now
/// wires into the Output as well.
#[test]
fn a_canvas_drop_keeps_its_position_and_gains_a_wire() {
    let mut h = TestHarness::with_starter_graph();
    let before_noodles = h.state().graph.lock().unwrap().noodle_count();
    let row = item_center(&h, 0);
    let drop = canvas_point(&h);

    press_and_move(&mut h, row, &[nudged(row), drop]);
    release_at(&mut h, drop);

    let canvas = rect_of(&h, "node-canvas");
    let graph = h.state().graph.lock().unwrap();
    let node = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == SEED_NODE_TYPES[0])
        .expect("the dragged node is in the graph");
    assert!(
        (node.position[0] - (drop.x - canvas.x)).abs() < 1.0
            && (node.position[1] - (drop.y - canvas.y)).abs() < 1.0,
        "the drop position the user chose must stand, got {:?}",
        node.position
    );
    let output = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == "Output")
        .expect("the starter graph has an Output");
    assert!(
        graph
            .noodles()
            .iter()
            .any(|n| n.from.node == node.id && n.to.node == output.id),
        "and it is wired into the Output"
    );
    assert_eq!(graph.noodle_count(), before_noodles + 1);
}

/// The drop position is computed from the canvas's *live* pan and zoom,
/// both of which the editor publishes through the node-editor model
/// hooks. This drives real pan / zoom input first, then drags a
/// favourite in and checks the node landed under the cursor in the
/// panned, zoomed canvas.
#[test]
fn drop_position_follows_the_canvas_pan_and_zoom() {
    let mut h = TestHarness::with_starter_graph();
    let canvas = rect_of(&h, "node-canvas");
    let anchor = Point::new(
        canvas.x + canvas.width * 0.5,
        canvas.y + canvas.height * 0.5,
    );

    // Pan with a middle drag, then zoom in at the cursor.
    let (ax, ay) = h.to_screen(anchor);
    h.mouse_move(ax, ay);
    h.mouse_down(MouseButton::Middle);
    h.mouse_move(ax + 70.0, ay - 40.0);
    h.mouse_up(MouseButton::Middle);
    h.scroll(1.0);

    let pan = *h.state().canvas_pan.lock().unwrap();
    let zoom = *h.state().canvas_zoom.lock().unwrap();
    assert_ne!(pan, [0.0, 0.0], "the middle drag panned the canvas");
    assert_ne!(zoom, 1.0, "the wheel zoomed the canvas");

    let before = node_count(&h);
    let row = item_center(&h, 0);
    let drop = canvas_point(&h);
    press_and_move(&mut h, row, &[nudged(row), drop]);
    release_at(&mut h, drop);
    assert_eq!(node_count(&h), before + 1);

    // Same arithmetic the editor's own `local_to_canvas` does, fed with
    // the production pan / zoom rather than hand-set values.
    let expected = [
        (drop.x - canvas.x - pan[0]) / zoom,
        (drop.y - canvas.y - pan[1]) / zoom,
    ];
    let graph = h.state().graph.lock().unwrap();
    let node = graph
        .nodes()
        .find(|n| n.type_id.as_ref() == SEED_NODE_TYPES[0])
        .expect("the dragged node is in the graph");
    assert!(
        (node.position[0] - expected[0]).abs() < 1.0
            && (node.position[1] - expected[1]).abs() < 1.0,
        "node at {:?} should be at {:?} for pan {:?} zoom {}",
        node.position,
        expected,
        pan,
        zoom
    );
}
