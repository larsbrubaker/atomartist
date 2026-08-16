//! Painting for [`crate::favorites_bar::FavoritesBar`].
//!
//! Split out of `favorites_bar.rs` (assembly + events) so neither file
//! approaches the 800-line cap. Everything here reads the
//! [`BarLayout`](crate::favorites_bar_geom::BarLayout) that
//! `FavoritesBar::layout` already computed — no measuring against the
//! model, no favourite resolution, no storage calls. The strip's own
//! items are painted by [`crate::favorites_strip`].
//!
//! Coordinates are widget-local and **Y-up**: `rect.y + rect.height` is a
//! rectangle's top edge.
//!
//! The handle carries a chevron pointing the direction its click will
//! move things (ND's affordance): **right** while collapsed ("this opens
//! the panel"), **left** while expanded ("this closes it").
//!
//! # The grip is a tab, not a lane (6h-2)
//!
//! ND's handle (`static/css/node-designer-parts-bar.css:24-56`) is a
//! 16 × 56 button rounded on the side it protrudes from
//! (`border-radius: 6px 0 0 6px` for a right-docked bar; mirrored here
//! because AtomArtist docks left), carrying a 1 px border and the strip's
//! own background, and turning the accent colour with a white chevron on
//! hover. Nothing else is drawn at the bar's outer edge: the strip's
//! background is the boundary. We used to stroke a full-height rule
//! there too, which read as a dead column beside the strip — it is gone.
//! The strip ⇄ panel separator stays, because the ancestor has one
//! (`border-left` on `.nd-parts-bar-favorites`).

use agg_gui::{theme::current_visuals, Color, DrawCtx, Rect, Widget};

use crate::favorites_bar::FavoritesBar;
use crate::favorites_bar_geom as geom;

/// Chevron size in the handle grip.
const CHEVRON_SIZE: f64 = 10.0;
/// Corner radius on the grip's protruding side (ND's `border-radius`).
const GRIP_RADIUS: f64 = 6.0;

pub(crate) fn paint_bar(bar: &mut FavoritesBar, ctx: &mut dyn DrawCtx) {
    let bounds = bar.bounds();
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let visuals = current_visuals();
    if let Some(font) = agg_gui::font_settings::current_system_font() {
        ctx.set_font(font);
    }
    let layout = bar.layout_rects().clone();

    ctx.save();
    ctx.clip_rect(0.0, 0.0, bounds.width, bounds.height);

    // Bar background (the panel's own chrome paints over it).
    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rect(0.0, 0.0, bounds.width, bounds.height);
    ctx.fill();

    // Only the items inside the scroll viewport are drawn, and the strip
    // clips them, so a half-scrolled item is cut exactly where the
    // hit-test stops accepting it.
    let items: Vec<_> = layout
        .items
        .iter()
        .enumerate()
        .filter(|(_, rect)| geom::item_visible(**rect, layout.items_viewport))
        .filter_map(|(index, rect)| bar.strip_item(index).map(|info| (*rect, info)))
        .collect();
    crate::favorites_strip::paint_strip(
        ctx,
        layout.strip,
        layout.items_viewport,
        &items,
        layout.pin,
    );

    // Separator between the browser panel and the strip, when the panel
    // is showing (6g-2: the panel is *outboard* of the strip). The bar's
    // outer edge gets no rule at all — see the module docs.
    if layout.panel.is_some() {
        ctx.set_stroke_color(visuals.separator);
        ctx.set_line_width(1.0);
        ctx.begin_path();
        ctx.move_to(layout.strip.x + 0.5, layout.strip.y);
        ctx.line_to(layout.strip.x + 0.5, layout.strip.y + layout.strip.height);
        ctx.stroke();
    }

    paint_grip(bar, ctx, layout.handle);

    ctx.restore();

    // Last thing in the frame: render *one* still-missing primitive
    // icon (step 6f-2). Doing it here rather than in `layout` is what
    // makes the fill-in "deferred past first paint" — the strip has
    // already been drawn with its glyphs by the time the first render
    // runs, and paint happens exactly once per frame, so the palette
    // fills in at a steady one icon per frame.
    crate::favorites_strip::pump_icons(bar.app_state(), bar.strip_items());
}

/// The handle: ND's small protruding tab, and nothing else.
///
/// Idle it wears the strip's own fill with a hairline border, so it reads
/// as a tab pressed out of the strip's edge rather than a lane of chrome.
/// Hovered (or mid-resize) it fills with the app's accent colour and the
/// chevron goes white — the ancestor's `rgba(92, 152, 217, 0.85)` /
/// `#fff` rule, expressed through the theme so it follows the user's
/// chosen accent instead of hard-coding a blue.
fn paint_grip(bar: &FavoritesBar, ctx: &mut dyn DrawCtx, grip: Rect) {
    if grip.width <= 0.0 || grip.height <= 0.0 {
        return;
    }
    let visuals = current_visuals();
    let hot = bar.handle_hovered();

    // Half-pixel inset so the 1 px border lands on the pixel grid.
    let r = Rect::new(
        grip.x + 0.5,
        grip.y + 0.5,
        (grip.width - 1.0).max(0.0),
        (grip.height - 1.0).max(0.0),
    );
    ctx.begin_path();
    tab_path(ctx, r, GRIP_RADIUS.min(r.width).min(r.height * 0.5));
    ctx.set_fill_color(if hot {
        visuals.accent
    } else {
        visuals.panel_fill
    });
    ctx.set_stroke_color(if hot {
        visuals.accent
    } else {
        visuals.separator
    });
    ctx.set_line_width(1.0);
    ctx.fill_and_stroke();

    let chevron = if bar.expanded() {
        crate::fa::CHEVRON_LEFT
    } else {
        crate::fa::CHEVRON_RIGHT
    };
    ctx.set_fill_color(if hot {
        Color::white()
    } else {
        visuals.text_dim
    });
    ctx.set_font_size(CHEVRON_SIZE);
    ctx.fill_text(
        &chevron.to_string(),
        grip.x + (grip.width - CHEVRON_SIZE) * 0.5,
        grip.y + (grip.height - CHEVRON_SIZE) * 0.5,
    );
}

/// Add a rectangle rounded on its **right** side only — ND's
/// `border-radius: 6px 0 0 6px`, mirrored because AtomArtist docks the
/// bar left, so the tab protrudes toward the 3-D viewport. `DrawCtx` has
/// no per-corner radii, hence the explicit path.
///
/// Y-up: `y` is the bottom edge, `y + height` the top.
fn tab_path(ctx: &mut dyn DrawCtx, r: Rect, radius: f64) {
    let (x0, x1) = (r.x, r.x + r.width);
    let (y0, y1) = (r.y, r.y + r.height);
    ctx.move_to(x0, y0);
    ctx.line_to(x1 - radius, y0);
    ctx.quad_to(x1, y0, x1, y0 + radius);
    ctx.line_to(x1, y1 - radius);
    ctx.quad_to(x1, y1, x1 - radius, y1);
    ctx.line_to(x0, y1);
    ctx.close_path();
}
