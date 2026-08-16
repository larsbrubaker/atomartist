//! The persistent 72 px icon strip inside
//! [`FavoritesBar`](crate::favorites_bar::FavoritesBar)
//! (`docs/file-browser-design.md` §5b, step 6f-1).
//!
//! NodeDesigner's parts bar keeps its icon strip on screen at all times —
//! expanding the bar grows the *browser panel* beside it, never the
//! strip. This module owns everything about that strip that is not
//! rectangle arithmetic (which lives in
//! [`crate::favorites_bar_geom`]): resolving the favourites into
//! displayable items, turning an item into a drag payload, and painting
//! them.
//!
//! Split out of `favorites_bar.rs` so that file stays assembly + event
//! routing and both stay well under the 800-line cap.
//!
//! # Icons
//!
//! A `NodeType` favourite's 44 × 44 slot carries a render of the
//! primitive's *own* generator, evaluated with the node type's defaults
//! ([`crate::node_icons`], step 6f-2). Those renders are deferred:
//! [`pump_icons`] resolves at most one per frame, so the strip appears
//! with labels and Font Awesome glyphs and the pictures fill in over the
//! next few frames. The glyph is never a placeholder to be *waited* for
//! — it is the permanent fallback for anything that does not render
//! (project favourites, dead entries, non-geometry types).
//!
//! Coordinates are widget-local and **Y-up**; items were stacked downward
//! from the strip's top by `favorites_bar_geom::compute`.

use std::sync::Arc;

use agg_gui::{theme::current_visuals, DrawCtx, Rect};
use atomartist_lib::registry::NodeDef;
use atomartist_storage::StorageUri;

use crate::app_state::AppState;
use crate::drag_insert::DragPayload;
use crate::favorites_bar_geom::{self as geom, LABEL_SIZE};
use crate::file_browser::favorites::{FavoriteKind, FavoriteResolution};
use crate::mesh_raster::IconImage;

/// Glyph size inside the 44 × 44 slot — the fallback for a favourite
/// with no rendered icon.
const SLOT_GLYPH_SIZE: f64 = 22.0;

/// Inset of the rendered icon inside the 44 × 44 slot, in logical
/// pixels, so the render never touches the slot's edge (the ancestor's
/// `padding: 2px` on its icon `<img>`).
const SLOT_ICON_INSET: f64 = 2.0;

/// Side of the rendered icon in logical pixels.
const SLOT_ICON_SIDE: f64 = geom::ICON_SLOT - SLOT_ICON_INSET * 2.0;

/// Edge length, in **device** pixels, to rasterize a slot's icon at.
///
/// Deviation from the ancestor, and the reason this is not simply ND's
/// 96 px: NodeDesigner hands its 96 px PNG to the browser, which scales
/// it down to the slot with a linear filter. Ours renders at the slot's
/// own device-pixel size instead — see
/// [`crate::mesh_raster::device_pixel_size`], the one place that rule
/// lives now that the drag ghost wants it too.
pub(crate) fn icon_pixel_size() -> u32 {
    crate::mesh_raster::device_pixel_size(SLOT_ICON_SIDE)
}

/// One favourite as the strip needs it for a frame: resolved label,
/// glyph, and whether it is still live.
pub(crate) struct StripItem {
    pub(crate) kind: FavoriteKind,
    pub(crate) stable_key: String,
    pub(crate) label: String,
    pub(crate) glyph: char,
    /// A favourite whose node type / URI no longer resolves. Kept and
    /// greyed rather than pruned (design §2: the provider may come back).
    pub(crate) alive: bool,
    /// The rendered primitive, once [`pump_icons`] has produced one.
    /// `None` means "paint the glyph" — either not rendered *yet*, or
    /// not renderable at all.
    pub(crate) icon: Option<IconImage>,
}

impl StripItem {
    /// What dragging this item out of the strip would insert. Dead
    /// favourites (unregistered type, unparsable URI) are not draggable —
    /// there is nothing to insert.
    pub(crate) fn payload(&self) -> Option<DragPayload> {
        if !self.alive {
            return None;
        }
        match self.kind {
            FavoriteKind::NodeType => Some(DragPayload::NodeType {
                type_id: self.stable_key.clone(),
                label: self.label.clone(),
                glyph: self.glyph,
            }),
            FavoriteKind::Project => Some(DragPayload::File {
                uri: self.stable_key.parse::<StorageUri>().ok()?,
                label: self.label.clone(),
                glyph: self.glyph,
            }),
        }
    }
}

/// The favourites, resolved against the live registry for this frame.
pub(crate) fn collect_items(state: &AppState) -> Vec<StripItem> {
    let favorites = state.favorites.lock().unwrap().clone();
    let size = icon_pixel_size();
    favorites
        .list()
        .iter()
        .map(|fav| {
            let (label, glyph, alive) = match fav.resolve(&state.registry) {
                FavoriteResolution::NodeType { def, display_name } => {
                    (display_name, node_type_glyph(def), true)
                }
                FavoriteResolution::Project { display_name, .. } => {
                    (display_name, crate::fa::FILE_NEW, true)
                }
                FavoriteResolution::Dead => (
                    fav.display_name.clone(),
                    match fav.kind {
                        FavoriteKind::NodeType => crate::fa::CUBE,
                        FavoriteKind::Project => crate::fa::FILE_NEW,
                    },
                    false,
                ),
            };
            // Only node types have a generator to render; a project
            // favourite's preview is its thumbnail, which is the
            // browser's business (design §5b: "project favorites keep
            // their thumbnail/glyph behaviour").
            let icon = (alive && fav.kind == FavoriteKind::NodeType)
                .then(|| crate::node_icons::icon(&fav.stable_key, size))
                .flatten();
            StripItem {
                kind: fav.kind,
                stable_key: fav.stable_key.clone(),
                label,
                glyph,
                alive,
                icon,
            }
        })
        .collect()
}

/// Render one still-missing icon, if any, and ask for the redraw that
/// puts it on screen.
///
/// Called at the *end* of the bar's paint, so the first render happens
/// after the strip has already been drawn with its glyphs, and exactly
/// once per frame (paint, unlike layout, is not run twice by a measuring
/// parent). One icon costs well under a millisecond — the seven seeded
/// primitives measure ≈5 ms in total at 96 px — so the palette fills in
/// over a handful of frames without ever showing up in one (design §5b).
/// Once every favourite is resolved this asks for no further redraws, so
/// a reactive host goes idle again.
pub(crate) fn pump_icons(state: &AppState, items: &[StripItem]) {
    let wanted: Vec<&str> = items
        .iter()
        .filter(|item| item.alive && item.kind == FavoriteKind::NodeType && item.icon.is_none())
        .map(|item| item.stable_key.as_str())
        .collect();
    if crate::node_icons::render_next(&state.registry, &wanted, icon_pixel_size()) {
        agg_gui::animation::request_draw();
    }
}

/// Palette glyph for a node type — the same category icon the Add Node
/// menu shows, so a favourite reads as the thing it adds.
fn node_type_glyph(def: &Arc<dyn NodeDef>) -> char {
    crate::top_menu_bar::category_icon(def.category()).unwrap_or(crate::fa::CUBE)
}

/// Paint the strip's background, its (scrolled) items, and the pin
/// affordance.
///
/// `items_viewport` is the scroll region: items are clipped to it, so a
/// partly scrolled item is cut off exactly where the bar stops
/// hit-testing it, and the anchored pin below is never painted over.
pub(crate) fn paint_strip(
    ctx: &mut dyn DrawCtx,
    strip: Rect,
    items_viewport: Rect,
    items: &[(Rect, &StripItem)],
    pin: Option<Rect>,
) {
    let visuals = current_visuals();
    // The strip reads as its own column of chrome next to the panel.
    ctx.set_fill_color(visuals.panel_fill);
    ctx.begin_path();
    ctx.rect(strip.x, strip.y, strip.width, strip.height);
    ctx.fill();

    ctx.save();
    ctx.clip_rect(
        items_viewport.x,
        items_viewport.y,
        items_viewport.width,
        items_viewport.height,
    );
    for (rect, item) in items {
        paint_item(ctx, *rect, item);
    }
    ctx.restore();

    if let Some(pin) = pin {
        let slot = geom::icon_slot(pin);
        ctx.set_fill_color(visuals.text_dim);
        ctx.set_font_size(SLOT_GLYPH_SIZE);
        ctx.fill_text(
            &crate::fa::PLUS_CIRCLE.to_string(),
            slot.x + (slot.width - SLOT_GLYPH_SIZE) * 0.5,
            slot.y + (slot.height - SLOT_GLYPH_SIZE) * 0.5,
        );
        paint_label(ctx, pin, "Pin", visuals.text_dim);
    }
}

fn paint_item(ctx: &mut dyn DrawCtx, item: Rect, info: &StripItem) {
    let visuals = current_visuals();
    let color = if info.alive {
        visuals.text_color
    } else {
        visuals.text_dim
    };
    // The slot: a faint tile so the icon reads as a button even before
    // 6f-2 renders the real primitive into it.
    let slot = geom::icon_slot(item);
    ctx.set_fill_color(visuals.text_color.with_alpha(0.06));
    ctx.begin_path();
    ctx.rect(slot.x, slot.y, slot.width, slot.height);
    ctx.fill();

    let blitted = info
        .icon
        .as_ref()
        .is_some_and(|icon| paint_icon(ctx, slot, icon));
    if !blitted {
        ctx.set_fill_color(color);
        ctx.set_font_size(SLOT_GLYPH_SIZE);
        // `slot` is already clamped to the item by `icon_slot`, so
        // both axes centre on its own size.
        ctx.fill_text(
            &info.glyph.to_string(),
            slot.x + (slot.width - SLOT_GLYPH_SIZE) * 0.5,
            slot.y + (slot.height - SLOT_GLYPH_SIZE) * 0.5,
        );
    }
    paint_label(ctx, item, &info.label, color);
}

/// Blit a rendered icon, square and centred, into the slot. Returns
/// `false` when nothing was drawn, which is the caller's cue to paint
/// the glyph instead.
///
/// The buffer is straight RGBA8 in top-down row order, which is what
/// `draw_image_rgba_arc` takes; the `Arc` is passed through so the wgpu
/// backend can key its texture cache on the allocation and re-use the
/// upload across frames. Backends without a real blit (agg-gui's
/// `gl_renderer` implements neither image entry point — the default is a
/// no-op) answer `has_image_blit()` with `false` and get the glyph
/// rather than an empty slot; `Label` gates its own cache the same way.
fn paint_icon(ctx: &mut dyn DrawCtx, slot: Rect, icon: &IconImage) -> bool {
    let side = (slot.width.min(slot.height) - SLOT_ICON_INSET * 2.0).max(0.0);
    if side <= 0.0 || icon.width == 0 || icon.height == 0 || !ctx.has_image_blit() {
        return false;
    }
    ctx.draw_image_rgba_arc(
        &icon.rgba,
        icon.width,
        icon.height,
        slot.x + (slot.width - side) * 0.5,
        slot.y + (slot.height - side) * 0.5,
        side,
        side,
    );
    true
}

fn paint_label(ctx: &mut dyn DrawCtx, item: Rect, label: &str, color: agg_gui::Color) {
    let box_ = geom::label_box(item);
    ctx.set_fill_color(color);
    ctx.set_font_size(LABEL_SIZE);
    // Shared with the browser's chrome and cards — same measuring, one
    // copy (`widget_geom::elide`, beside the `measure` it is built on).
    let text =
        crate::file_browser::widget_geom::elide(label, (box_.width - 4.0).max(0.0), LABEL_SIZE);
    let w = crate::file_browser::widget_geom::measure(&text, LABEL_SIZE);
    ctx.fill_text(
        &text,
        box_.x + (box_.width - w) * 0.5,
        box_.y + (box_.height - LABEL_SIZE) * 0.5,
    );
}
