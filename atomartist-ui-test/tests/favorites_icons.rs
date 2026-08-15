//! Rendered primitive icons in the favourites strip
//! (`docs/file-browser-design.md` §5b, step 6f-2). Ancestor:
//! NodeDesigner's `static/js/node-editor/ui/parts-bar-icons.js`, which
//! renders each palette entry's real generator offscreen and fills the
//! slots in after the bar is already on screen.
//!
//! **One test on purpose.** The icon cache
//! ([`atomartist_ui::node_icons`]) is process-wide, and the whole point
//! of the step is an ordered sequence — glyphs first, then one icon per
//! frame. A second test in this binary would race the cache and make the
//! "nothing rendered yet" assertion meaningless, so the whole sequence
//! lives in a single function.

use atomartist_ui::favorites_bar::BAR_ID;
use atomartist_ui::file_browser::{Favorite, SEED_NODE_TYPES};
use atomartist_ui_test::{memory_uri, TestHarness};

fn prop(h: &TestHarness, key: &str) -> String {
    h.find_by_id(BAR_ID)
        .expect("the favorites bar is in the tree")
        .properties()
        .into_iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("the bar exposes no `{key}` property"))
}

fn icons(h: &TestHarness) -> usize {
    prop(h, "icons").parse().expect("icons is a count")
}

fn favorites(h: &TestHarness) -> usize {
    prop(h, "favorites").parse().expect("favorites is a count")
}

/// The strip appears with glyphs, fills its icons in one frame at a
/// time, stops once every node-type favourite has one, and paints
/// without blowing up on the software backend.
#[test]
fn icons_are_deferred_then_fill_in_one_frame_at_a_time() {
    let mut h = TestHarness::with_starter_graph();
    // A pinned project rides along: it has no generator, so it must keep
    // its glyph however many frames go by (design §5b — project
    // favourites keep their thumbnail/glyph behaviour).
    h.state()
        .favorites
        .lock()
        .unwrap()
        .add(Favorite::project(&memory_uri("pinned.atmr")));
    h.frame();
    let total = favorites(&h);
    assert_eq!(
        total,
        SEED_NODE_TYPES.len() + 1,
        "the seeded palette plus the pinned project"
    );

    // Nothing is rendered until the strip has been painted at least
    // once: its first appearance costs only text.
    assert_eq!(
        icons(&h),
        0,
        "no icon may be rendered before the strip's first paint"
    );
    // …and it paints fine in that state (glyph fallback everywhere).
    h.paint_once();

    // One per painted frame, no more, until the palette is covered.
    let mut seen = 0;
    for frame in 0..SEED_NODE_TYPES.len() * 3 {
        h.frame();
        h.paint_once();
        let now = icons(&h);
        assert!(
            now <= seen + 1,
            "frame {frame} rendered {} icons at once; the pump is one-at-a-time",
            now - seen
        );
        seen = now;
        if seen == SEED_NODE_TYPES.len() {
            break;
        }
    }
    assert_eq!(
        seen,
        SEED_NODE_TYPES.len(),
        "every seeded primitive should end up with a rendered icon"
    );

    // The pinned project never gets one, and the pump goes quiet rather
    // than re-rendering forever.
    h.frame();
    assert_eq!(
        icons(&h),
        SEED_NODE_TYPES.len(),
        "a project favourite has no generator to render"
    );

    // Blitting the real buffers into the 44 px slots must not panic or
    // run off the framebuffer.
    h.paint_once();

    // …and with the cache full that paint asked for no further redraw,
    // so a `RunMode::Reactive` host goes idle instead of spinning a
    // frame per second forever. Clearing first isolates *this* paint:
    // the frames above legitimately requested draws.
    agg_gui::animation::clear_draw_request();
    h.paint_once();
    assert!(
        !agg_gui::animation::wants_draw(),
        "a fully-cached strip must not keep requesting redraws"
    );
}
