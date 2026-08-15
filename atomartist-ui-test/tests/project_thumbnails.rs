//! Project-preview (`Metadata/thumbnail.png`) behaviour end-to-end
//! through `AppState`. Ancestor reference: NodeDesigner's
//! `capturePreviewImage` + `dfs-local-backend.ts` thumbnail read
//! (`MatterHackers/FDS/NodeDesigner/src/services/dfs-local-backend.ts`).
//!
//! The GPU capture itself is the shell's business and can't run
//! headless; what *is* testable — and what this file pins — is the slot
//! contract: whatever preview the shell parked in
//! `AppState::latest_thumbnail` is what the next project write embeds,
//! and a shell that never fills it produces preview-less files.

use atomartist_lib::serialization::read_thumbnail_from_bytes;
use atomartist_storage::StorageUri;
use atomartist_ui_test::{memory_uri, TestHarness};

fn read_stored(h: &TestHarness, uri: &StorageUri) -> Vec<u8> {
    let provider = h.storage().resolve(uri).expect("provider for test URI");
    provider
        .read(uri)
        .take()
        .expect("memory provider completes synchronously")
        .expect("stored project readable")
}

/// A tiny but genuine 4x3 PNG, produced by the same encoder the native
/// capture path uses — so this test also covers "the bytes we embed are
/// a real image".
fn sample_png() -> Vec<u8> {
    let rgb = vec![10u8, 20, 30].repeat(4 * 3);
    atomartist_ui::thumbnail::encode_rgb_png(&rgb, 4, 3).expect("encode sample png")
}

#[test]
fn save_embeds_the_thumbnail_slot_contents() {
    let h = TestHarness::with_starter_graph();
    let png = sample_png();
    h.state().set_thumbnail_png(png.clone());

    let uri = memory_uri("with_preview.atmr");
    h.state().save_project(&uri);
    h.pump_until_idle(4);

    let bytes = read_stored(&h, &uri);
    assert_eq!(read_thumbnail_from_bytes(&bytes), Some(png));
}

#[test]
fn save_without_a_captured_preview_writes_no_thumbnail() {
    // The headless case — no shell ever fills the slot, so the entry is
    // simply absent and the project is still perfectly valid.
    let h = TestHarness::with_starter_graph();
    assert!(h.state().thumbnail_png().is_none());

    let uri = memory_uri("no_preview.atmr");
    h.state().save_project(&uri);
    h.pump_until_idle(4);

    let bytes = read_stored(&h, &uri);
    assert!(read_thumbnail_from_bytes(&bytes).is_none());
}

#[test]
fn export_project_copy_carries_the_preview_too() {
    // File → Export → AtomArtist Project produces a full project copy,
    // so it must carry the same preview a Save would.
    let h = TestHarness::with_starter_graph();
    let png = sample_png();
    h.state().set_thumbnail_png(png.clone());

    let uri = memory_uri("exported_copy.atmr");
    h.state().export_project_copy_to_uri(&uri);
    h.pump_until_idle(4);

    let bytes = read_stored(&h, &uri);
    assert_eq!(read_thumbnail_from_bytes(&bytes), Some(png));
}

#[test]
fn new_project_clears_the_stale_preview() {
    // File → New throws the model away; the preview of the *old* model
    // must not ride along into the first save of the new one.
    let h = TestHarness::with_starter_graph();
    h.state().set_thumbnail_png(sample_png());

    h.state().new_empty_project();
    assert!(h.state().thumbnail_png().is_none());

    let uri = memory_uri("after_new.atmr");
    h.state().save_project(&uri);
    h.pump_until_idle(4);
    assert!(read_thumbnail_from_bytes(&read_stored(&h, &uri)).is_none());
}

#[test]
fn opening_a_project_clears_the_previous_projects_preview() {
    // Project B is saved back within seconds of being opened — before
    // the shell has captured a frame of it. Whatever A's preview was,
    // it must not be written into B's file and mislabel it forever.
    let h = TestHarness::with_starter_graph();
    let uri_b = memory_uri("project_b.atmr");
    h.state().save_project(&uri_b);
    h.pump_until_idle(4);

    h.state().set_thumbnail_png(sample_png());
    let uri_a = memory_uri("project_a.atmr");
    h.state().save_project(&uri_a);
    h.pump_until_idle(4);
    assert!(read_thumbnail_from_bytes(&read_stored(&h, &uri_a)).is_some());

    h.state().open_project(&uri_b);
    h.pump_until_idle(4);
    assert!(h.state().thumbnail_png().is_none());

    h.state().save_project(&uri_b);
    h.pump_until_idle(4);
    assert!(read_thumbnail_from_bytes(&read_stored(&h, &uri_b)).is_none());
}

#[test]
fn opening_a_project_with_a_preview_still_loads_the_graph() {
    // Readers ignore the extra entry: a project written with a preview
    // round-trips exactly like one without.
    let h = TestHarness::with_starter_graph();
    h.state().set_thumbnail_png(sample_png());
    let nodes_before = h.state().graph.lock().unwrap().nodes().count();

    let uri = memory_uri("preview_round_trip.atmr");
    h.state().save_project(&uri);
    h.pump_until_idle(4);

    h.state().new_empty_project();
    assert_eq!(h.state().graph.lock().unwrap().nodes().count(), 0);

    h.state().open_project(&uri);
    h.pump_until_idle(4);
    assert_eq!(
        h.state().graph.lock().unwrap().nodes().count(),
        nodes_before
    );
}

/// Regression: the preview must frame the *3-D viewport*, not the node
/// canvas below it — nor the favorites bar beside it (step 6f-1 docked
/// the bar on the viewport's own pane, so the crop's left edge is now a
/// real thing that can regress).
///
/// `Widget::bounds()` is parent-local — `Viewport3dWidget::layout` even
/// resets its own origin to (0, 0) — so deriving the crop from
/// `find_widget_by_id(...).bounds()` produced a rect anchored at the
/// window's bottom-left, i.e. exactly the node canvas + status bar.
/// The crop must come from *absolute* screen bounds instead.
#[test]
fn thumbnail_crop_frames_the_viewport_not_the_node_canvas() {
    let (w, h_px) = (1280u32, 720u32);
    let h = TestHarness::new().with_size(w, h_px);

    // Absolute (Y-up) placement of both panes, straight from agg-gui's
    // inspector walk — the tree's own answer, so the test doesn't
    // hard-code a splitter ratio.
    let screen_rect = |type_name: &str| {
        h.snapshot()
            .into_iter()
            .find(|n| n.type_name == type_name)
            .unwrap_or_else(|| panic!("{type_name} must be in the tree"))
            .screen_bounds
    };
    let viewport = screen_rect("Viewport3dWidget");
    let canvas = screen_rect("NodeEditor");
    // Sanity: the default layout really does stack viewport over canvas.
    assert!(
        viewport.y >= canvas.y + canvas.height,
        "viewport should sit above the node canvas: {viewport:?} vs {canvas:?}"
    );

    // Framebuffer rows (top-down) each pane occupies.
    let fb_rows = |r: agg_gui::Rect| {
        let top = h_px as f64 - (r.y + r.height);
        (top, top + r.height)
    };
    let (vp_top, vp_bottom) = fb_rows(viewport);
    let (canvas_top, canvas_bottom) = fb_rows(canvas);

    let crop = atomartist_ui::viewport_framebuffer_crop(h.app().root(), w, h_px)
        .expect("viewport is on screen, so the capture path yields a crop");
    let region = atomartist_ui::thumbnail_source_region(crop);

    // The favorites bar shares the viewport's pane, on its left. Columns
    // are the same in both spaces (only rows flip), so its right edge is
    // the leftmost column the preview may sample.
    let bar = screen_rect("FavoritesBar");
    assert!(
        bar.x + bar.width <= viewport.x + 0.5,
        "the bar docks left of the viewport: {bar:?} vs {viewport:?}"
    );

    for (label, r) in [("crop", crop), ("source region", region)] {
        let (top, bottom) = (r.y as f64, (r.y + r.h) as f64);
        assert!(
            top >= vp_top - 1.0 && bottom <= vp_bottom + 1.0,
            "{label} {r:?} must lie inside the viewport's framebuffer rows \
             {vp_top}..{vp_bottom}"
        );
        assert!(
            bottom <= canvas_top || top >= canvas_bottom,
            "{label} {r:?} must not overlap the node canvas's rows \
             {canvas_top}..{canvas_bottom}"
        );
        let (left, right) = (r.x as f64, (r.x + r.w) as f64);
        assert!(
            left >= bar.x + bar.width - 1.0,
            "{label} {r:?} must start right of the favorites bar \
             (ends at x={})",
            bar.x + bar.width
        );
        assert!(
            left >= viewport.x - 1.0 && right <= viewport.x + viewport.width + 1.0,
            "{label} {r:?} must lie inside the viewport's columns \
             {}..{}",
            viewport.x,
            viewport.x + viewport.width
        );
    }
}
