//! Custom-painted circular icon button — the building block for the
//! tumble-cube navigation ring and the bottom row of viewport
//! controls.
//!
//! Visually mirrors MatterCAD's circular `ThemedRadioIconButton` /
//! `ThemedIconButton` widgets: transparent idle background, subtle
//! transparent hover/press tint, and the checked-radio treatment from
//! `ThemedRadioIconButton.OnDraw`: three translucent accent arcs
//! around the button rather than a solid blue fill.
//!
//! Implements [`agg_gui::Widget`] directly — no agg-gui `Button`
//! delegation because rectangular Button paint doesn't yield the
//! pill-perfect circle MatterCAD's reference uses.
//!
//! Hit-testing is confined to the inscribed circle (not the bounding
//! square) so the button doesn't gobble clicks on the corner pixels
//! that visually belong to the HUD bay background.

use std::f64::consts::TAU;
use std::rc::Rc;

use agg_gui::{
    theme::current_visuals, Color, DrawCtx, Event, EventResult, HAnchor, MouseButton, Point, Rect,
    Size, VAnchor, Widget, WidgetBase,
};

use crate::icons::{paint_icon, IconKind};
use crate::mattercad_icons::{scaled_tinted_rgba_arc_to_size, MatterCadIcon};

const MATTERCAD_ICON_SCALE: f64 = 1.2;
const MATTERCAD_ICON_BASE_PX: f64 = 16.0;
const MATTERCAD_CHEVRON_BASE_PX: f64 = 12.0;

/// A circular button rendering an icon from [`IconKind`].  See the
/// module-level docstring for visual / interaction semantics.
pub struct CircularIconButton {
    bounds: Rect,
    base: WidgetBase,
    icon: IconKind,
    /// When set, this MatterCAD PNG icon is blitted in place of the
    /// hand-drawn vector `icon`. The blit is recoloured to the theme
    /// text colour the same way MatterCAD's
    /// `WhiteToAlpha_GreyToColor` filter does.
    image_icon: Option<MatterCadIcon>,
    /// Optional secondary icon (overlaid lower-right) — used by
    /// dropdown buttons that want a small "▼" chevron over the main
    /// glyph.
    overlay_icon: Option<IconKind>,
    /// MatterCAD image used for the overlay slot, mutually-exclusive
    /// with `overlay_icon`.
    overlay_image: Option<MatterCadIcon>,
    on_click: Option<Box<dyn FnMut()>>,
    active_fn: Option<Rc<dyn Fn() -> bool>>,
    enabled_fn: Option<Rc<dyn Fn() -> bool>>,
    hovered: bool,
    pressed: bool,
    /// Backing storage for `Widget::children_mut` — leaf widget, so
    /// this is always empty. Stored on the struct rather than via a
    /// thread-local so the borrow is `'self`-lifetimed.
    children_storage: Vec<Box<dyn Widget>>,
}

impl CircularIconButton {
    pub fn new(icon: IconKind) -> Self {
        Self {
            bounds: Rect::default(),
            base: WidgetBase::new()
                .with_h_anchor(HAnchor::FIT)
                .with_v_anchor(VAnchor::FIT)
                .with_min_size(Size::new(20.0, 20.0))
                .with_max_size(Size::new(36.0, 36.0)),
            icon,
            image_icon: None,
            overlay_icon: None,
            overlay_image: None,
            on_click: None,
            active_fn: None,
            enabled_fn: None,
            hovered: false,
            pressed: false,
            children_storage: Vec::new(),
        }
    }

    pub fn on_click(mut self, cb: impl FnMut() + 'static) -> Self {
        self.on_click = Some(Box::new(cb));
        self
    }

    /// Bind the "active" state to a live predicate. Active buttons
    /// paint MatterCAD's checked-radio treatment: subtle fill plus
    /// three translucent accent arc segments around the circle.
    pub fn with_active_fn(mut self, f: impl Fn() -> bool + 'static) -> Self {
        self.active_fn = Some(Rc::new(f));
        self
    }

    pub fn with_enabled_fn(mut self, f: impl Fn() -> bool + 'static) -> Self {
        self.enabled_fn = Some(Rc::new(f));
        self
    }

    pub fn with_overlay(mut self, icon: IconKind) -> Self {
        self.overlay_icon = Some(icon);
        self
    }

    /// Use one of MatterCAD's bundled PNG icons in the main slot.
    /// The icon is tinted to `theme.text_color` so it composes
    /// against the button background the same way MatterCAD's
    /// `WhiteToAlpha_GreyToColor` filter produces themed icons.
    pub fn with_image_icon(mut self, image: MatterCadIcon) -> Self {
        self.image_icon = Some(image);
        self
    }

    /// Use a MatterCAD PNG as the overlay (small lower-right
    /// dropdown chevron).
    pub fn with_overlay_image(mut self, image: MatterCadIcon) -> Self {
        self.overlay_image = Some(image);
        self
    }

    fn enabled(&self) -> bool {
        self.enabled_fn.as_ref().map(|f| f()).unwrap_or(true)
    }

    fn active(&self) -> bool {
        self.active_fn.as_ref().map(|f| f()).unwrap_or(false)
    }

    /// Widget-local centre — events arrive with `(0, 0)` at the
    /// widget's bottom-left, so the circle's centre is at
    /// `(width/2, height/2)` regardless of where the widget sits
    /// inside its parent.
    fn circle_center(&self) -> (f64, f64) {
        (self.bounds.width * 0.5, self.bounds.height * 0.5)
    }

    fn circle_radius(&self) -> f64 {
        self.bounds.width.min(self.bounds.height) * 0.5
    }

    fn point_in_circle(&self, p: Point) -> bool {
        let (cx, cy) = self.circle_center();
        let r = self.circle_radius();
        let dx = p.x - cx;
        let dy = p.y - cy;
        dx * dx + dy * dy <= r * r
    }

    fn background_color(&self, active: bool, enabled: bool) -> Color {
        let v = current_visuals();
        if !enabled {
            return Color::rgba(0.0, 0.0, 0.0, 0.0);
        }
        match (active, self.hovered, self.pressed) {
            // Checked radio buttons in MatterCAD set BackgroundColor
            // to `MinimalShade`, then overlay accent arcs in OnDraw.
            // In the viewport HUD, keep this transparent enough that
            // only the arc treatment reads as selected.
            (true, _, true) => v.widget_bg_hovered.with_alpha(0.35),
            (true, true, _) => v.widget_bg_hovered.with_alpha(0.25),
            (true, false, false) => Color::rgba(0.0, 0.0, 0.0, 0.0),
            // ThemedButton: HoverColor = SlightShade,
            // MouseDownColor = MinimalShade.  Approximate that with
            // transparent overlays; idle is fully transparent.
            (false, _, true) => v.widget_bg_hovered.with_alpha(0.35),
            (false, true, false) => v.widget_bg_hovered.with_alpha(0.22),
            (false, false, false) => Color::rgba(0.0, 0.0, 0.0, 0.0),
        }
    }

    fn icon_color(&self, _active: bool) -> Color {
        let v = current_visuals();
        // MatterCAD recolors the source icon to theme.TextColor, then
        // draws selection as accent arcs around it. The icon itself
        // does not flip to white when checked.
        v.text_color
    }

    fn fill_circle(&self, ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64, color: Color) {
        ctx.set_fill_color(color);
        let steps = 24;
        ctx.begin_path();
        for i in 0..=steps {
            let a = (i as f64 / steps as f64) * TAU;
            let x = cx + r * a.cos();
            let y = cy + r * a.sin();
            if i == 0 {
                ctx.move_to(x, y);
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.fill();
    }

    fn stroke_circle(&self, ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64, color: Color) {
        ctx.set_stroke_color(color);
        ctx.set_line_width(1.0);
        let steps = 24;
        ctx.begin_path();
        for i in 0..=steps {
            let a = (i as f64 / steps as f64) * TAU;
            let x = cx + r * a.cos();
            let y = cy + r * a.sin();
            if i == 0 {
                ctx.move_to(x, y);
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.stroke();
    }

    /// Port of `ThemedRadioIconButton.OnDraw`: if the button is a full
    /// circle and checked, draw three 90-degree accent arcs around the
    /// perimeter. C# uses `stroke = 4 * DeviceScale` and
    /// `theme.PrimaryAccentColor.WithAlpha(100)`.
    fn paint_checked_arcs(&self, ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
        let v = current_visuals();
        let accent = Color::rgba(v.accent.r, v.accent.g, v.accent.b, 100.0 / 255.0);
        let stroke = 4.0;
        let arc_r = (r - stroke * 0.5).max(1.0);
        let quarter = TAU / 4.0;
        for start_ratio in [1.0 / 3.0 + 0.75, 2.0 / 3.0 + 0.75, 1.0 + 0.75] {
            let start = TAU * start_ratio - quarter * 0.5;
            let end = TAU * start_ratio + quarter * 0.5;
            ctx.set_stroke_color(accent);
            ctx.set_line_width(stroke);
            ctx.begin_path();
            let steps = 12;
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let a = start + (end - start) * t;
                let x = cx + arc_r * a.cos();
                let y = cy + arc_r * a.sin();
                if i == 0 {
                    ctx.move_to(x, y);
                } else {
                    ctx.line_to(x, y);
                }
            }
            ctx.stroke();
        }
    }
}

impl Widget for CircularIconButton {
    fn type_name(&self) -> &'static str { "CircularIconButton" }
    fn bounds(&self) -> Rect { self.bounds }
    fn set_bounds(&mut self, b: Rect) { self.bounds = b; }
    fn children(&self) -> &[Box<dyn Widget>] { &[] }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Widget>> {
        // Leaf widget — no children. agg-gui's `children_mut` expects
        // a mutable slot; we keep a tiny empty vec on the widget so
        // the borrow is safe.
        &mut self.children_storage
    }
    fn h_anchor(&self) -> HAnchor { self.base.h_anchor }
    fn v_anchor(&self) -> VAnchor { self.base.v_anchor }
    fn min_size(&self) -> Size { self.base.min_size }
    fn max_size(&self) -> Size { self.base.max_size }
    fn widget_base(&self) -> Option<&WidgetBase> { Some(&self.base) }

    fn layout(&mut self, available: Size) -> Size {
        // Pick the largest square that fits in the available area but
        // is also within our min/max box.
        let w = available
            .width
            .clamp(self.base.min_size.width, self.base.max_size.width);
        let h = available
            .height
            .clamp(self.base.min_size.height, self.base.max_size.height);
        let side = w.min(h);
        self.bounds = Rect::new(0.0, 0.0, side, side);
        Size::new(side, side)
    }

    fn paint(&mut self, ctx: &mut dyn DrawCtx) {
        let w = self.bounds.width;
        let h = self.bounds.height;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let cx = w * 0.5;
        let cy = h * 0.5;
        let r = w.min(h) * 0.5 - 0.5; // leave 0.5 px for the outline

        let enabled = self.enabled();
        let active = self.active();
        let bg = self.background_color(active, enabled);
        if bg.a > 0.0 {
            self.fill_circle(ctx, cx, cy, r, bg);
        }

        if active {
            self.paint_checked_arcs(ctx, cx, cy, r);
        }

        // MatterCAD's HUD icon buttons do not draw an idle white
        // circular background. Only draw a faint outline when hovered
        // or pressed so the hover remains visible but transparent.
        if self.hovered || self.pressed {
            let v = current_visuals();
            self.stroke_circle(ctx, cx, cy, r, v.widget_stroke.with_alpha(0.35));
        }

        // Main icon, slightly inset. PNG-based icons are blitted at
        // the canonical MatterCAD size (16-px source, scaled to fit
        // ~70% of the button diameter); fall back to the hand-drawn
        // vector glyph when no PNG has been set.
        let icon_color = self.icon_color(active);
        if let Some(img) = self.image_icon {
            paint_image_icon(
                ctx,
                img,
                icon_color,
                cx,
                cy,
                MATTERCAD_ICON_BASE_PX * MATTERCAD_ICON_SCALE,
            );
        } else {
            paint_icon(self.icon, ctx, cx, cy, r, icon_color);
        }

        // Optional overlay (lower-right). Either a vector chevron
        // or a small MatterCAD PNG.
        if let Some(overlay) = self.overlay_image {
            let ox = cx + r * 0.45;
            let oy = cy - r * 0.45;
            paint_image_icon(
                ctx,
                overlay,
                icon_color,
                ox,
                oy,
                MATTERCAD_CHEVRON_BASE_PX * MATTERCAD_ICON_SCALE,
            );
        } else if let Some(overlay) = self.overlay_icon {
            let or = r * 0.4;
            let ox = cx + r * 0.45;
            let oy = cy - r * 0.45;
            paint_icon(overlay, ctx, ox, oy, or, icon_color);
        }
    }

    fn on_event(&mut self, event: &Event) -> EventResult {
        if !self.enabled() {
            return EventResult::Ignored;
        }
        match event {
            Event::MouseDown { pos, button, .. } if *button == MouseButton::Left => {
                if self.point_in_circle(*pos) {
                    self.pressed = true;
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            Event::MouseUp { pos, button, .. } if *button == MouseButton::Left => {
                let was_pressed = self.pressed;
                self.pressed = false;
                if was_pressed && self.point_in_circle(*pos) {
                    if let Some(cb) = self.on_click.as_mut() {
                        cb();
                    }
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            Event::MouseMove { pos } => {
                let inside = self.point_in_circle(*pos);
                if inside != self.hovered {
                    self.hovered = inside;
                }
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    }
}

/// Blit a recoloured MatterCAD PNG icon centred on `(cx, cy)`.  The
/// PNG is decoded once per process, recoloured to `color`, and then
/// drawn at `target_size × target_size` logical pixels via the
/// software-friendly `draw_image_rgba` blit.
fn paint_image_icon(
    ctx: &mut dyn DrawCtx,
    icon: MatterCadIcon,
    color: Color,
    cx: f64,
    cy: f64,
    target_size: f64,
) {
    let rgb = [
        (color.r * 255.0).clamp(0.0, 255.0) as u8,
        (color.g * 255.0).clamp(0.0, 255.0) as u8,
        (color.b * 255.0).clamp(0.0, 255.0) as u8,
    ];
    let px = target_size.round().max(1.0) as u32;
    let (rgba, w, h) = scaled_tinted_rgba_arc_to_size(icon, rgb, px, px);
    // The caller supplies the centre; the icon buffer is already
    // scaled using the spline-family CPU scaler and is drawn 1:1.
    ctx.draw_image_rgba_arc(
        &rgba,
        w,
        h,
        cx - w as f64 * 0.5,
        cy - h as f64 * 0.5,
        w as f64,
        h as f64,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn button_constructs_and_lays_out_square() {
        let b = CircularIconButton::new(IconKind::Home);
        let mut b2 = b;
        let s = b2.layout(Size::new(40.0, 40.0));
        assert_eq!(s.width, s.height);
        assert!(s.width >= 20.0 && s.width <= 36.0);
    }

    #[test]
    fn click_inside_circle_fires_callback() {
        let counter = Arc::new(Mutex::new(0));
        let c = counter.clone();
        let mut b = CircularIconButton::new(IconKind::Home).on_click(move || {
            *c.lock().unwrap() += 1;
        });
        b.layout(Size::new(30.0, 30.0));
        let p = Point::new(15.0, 15.0);
        b.on_event(&Event::MouseDown {
            pos: p,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        b.on_event(&Event::MouseUp {
            pos: p,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[test]
    fn click_outside_circle_is_ignored() {
        let counter = Arc::new(Mutex::new(0));
        let c = counter.clone();
        let mut b = CircularIconButton::new(IconKind::Home).on_click(move || {
            *c.lock().unwrap() += 1;
        });
        b.layout(Size::new(30.0, 30.0));
        // Far corner — outside the inscribed circle of a 30x30 square.
        let p = Point::new(1.0, 1.0);
        b.on_event(&Event::MouseDown {
            pos: p,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        b.on_event(&Event::MouseUp {
            pos: p,
            button: MouseButton::Left,
            modifiers: agg_gui::Modifiers::default(),
        });
        assert_eq!(*counter.lock().unwrap(), 0);
    }
}
