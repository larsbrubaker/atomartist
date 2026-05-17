//! Hand-painted vector icons for the viewport overlay buttons.
//!
//! Every icon is drawn into a small circle of radius `r` centred on
//! `(cx, cy)` via `DrawCtx` primitives — no SVG asset pipeline, no
//! Font Awesome dependency. The icons are stylised silhouettes that
//! visually echo MatterCAD's PNG icons for the corresponding controls
//! without trying to be pixel-identical.
//!
//! Each painter:
//! - Strokes / fills with `color` (caller picks a theme-appropriate
//!   foreground colour: `text_color` for idle, white for active).
//! - Uses a base stroke width of `r * 0.16` so detail scales with the
//!   button size.
//! - Stays within `(cx ± r * 0.65, cy ± r * 0.65)` so the icon doesn't
//!   collide with the button's outline.

use std::f64::consts::{PI, TAU};

use agg_gui::{Color, DrawCtx};

/// Identity of the icon to paint. Kept as a flat enum so the
/// `CircularIconButton` widget can store a `Copy` value and dispatch
/// inside `paint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconKind {
    /// Reset-view: house outline.
    Home,
    /// Fit-to-selection: four inward-pointing corner brackets.
    Fit,
    /// Pointer arrow.
    Select,
    /// Rotate: 3/4 circular arrow.
    Rotate,
    /// Pan: 4-way arrows.
    Pan,
    /// Zoom: magnifier with diagonal handle.
    Zoom,
    /// Turntable: arrow looping around a horizontal disc.
    Turn,
    /// Perspective: 3 lines converging to a vanishing point.
    Persp,
    /// Print-bed / floor-grid: three horizontal lines.
    Bed,
    /// Shaded sphere — half-lit circle.
    Shade,
    /// Three dots on a horizontal line (snap intervals).
    Snap,
    /// Small downward chevron, used as a "this opens a dropdown"
    /// affordance overlaid on another icon.
    DropdownChevron,
}

/// Paint the requested icon at `(cx, cy)` with effective radius `r`.
///
/// The caller is expected to have already filled the circular button
/// background; this function only draws the glyph on top.
pub fn paint_icon(kind: IconKind, ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64, color: Color) {
    ctx.set_stroke_color(color);
    ctx.set_fill_color(color);
    let lw = (r * 0.16).max(1.0);
    ctx.set_line_width(lw);

    match kind {
        IconKind::Home => paint_home(ctx, cx, cy, r),
        IconKind::Fit => paint_fit(ctx, cx, cy, r),
        IconKind::Select => paint_select(ctx, cx, cy, r),
        IconKind::Rotate => paint_rotate(ctx, cx, cy, r, lw),
        IconKind::Pan => paint_pan(ctx, cx, cy, r),
        IconKind::Zoom => paint_zoom(ctx, cx, cy, r),
        IconKind::Turn => paint_turn(ctx, cx, cy, r, lw),
        IconKind::Persp => paint_persp(ctx, cx, cy, r),
        IconKind::Bed => paint_bed(ctx, cx, cy, r),
        IconKind::Shade => paint_shade(ctx, cx, cy, r),
        IconKind::Snap => paint_snap(ctx, cx, cy, r),
        IconKind::DropdownChevron => paint_dropdown_chevron(ctx, cx, cy, r),
    }
}

// ---------------------------------------------------------------------------
// Individual icon painters. Coordinates use the standard math convention
// (X to the right, Y up); agg-gui's DrawCtx is Y-up so calling x += δ
// shifts right and y += δ shifts up.
// ---------------------------------------------------------------------------

fn paint_home(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // House silhouette: triangular roof on top of a rectangular body.
    let s = r * 0.55;
    // Body rectangle.
    let body_y_top = cy - s * 0.1;
    let body_y_bot = cy - s * 0.9;
    ctx.begin_path();
    ctx.move_to(cx - s * 0.8, body_y_bot);
    ctx.line_to(cx + s * 0.8, body_y_bot);
    ctx.line_to(cx + s * 0.8, body_y_top);
    ctx.line_to(cx - s * 0.8, body_y_top);
    ctx.line_to(cx - s * 0.8, body_y_bot);
    ctx.stroke();

    // Roof triangle.
    ctx.begin_path();
    ctx.move_to(cx - s * 1.0, body_y_top);
    ctx.line_to(cx, cy + s * 0.85);
    ctx.line_to(cx + s * 1.0, body_y_top);
    ctx.stroke();
}

fn paint_fit(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Four corner brackets, each forming an "L" pointing toward
    // (cx, cy) from one corner of a square inscribed in the button.
    let s = r * 0.65;
    let inset = s * 0.4;
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let ox = cx + sx * s;
        let oy = cy + sy * s;
        ctx.begin_path();
        ctx.move_to(ox - sx * inset, oy);
        ctx.line_to(ox, oy);
        ctx.line_to(ox, oy - sy * inset);
        ctx.stroke();
    }
}

fn paint_select(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Classic arrow cursor: pointed tip at top-left, body sweeping to
    // bottom-right with a notched tail.
    let s = r * 0.65;
    let tip_x = cx - s * 0.5;
    let tip_y = cy + s * 0.65;
    ctx.begin_path();
    ctx.move_to(tip_x, tip_y);
    ctx.line_to(tip_x + s * 0.95, tip_y - s * 0.3);
    ctx.line_to(tip_x + s * 0.55, tip_y - s * 0.45);
    ctx.line_to(tip_x + s * 0.80, tip_y - s * 1.05);
    ctx.line_to(tip_x + s * 0.50, tip_y - s * 1.15);
    ctx.line_to(tip_x + s * 0.25, tip_y - s * 0.55);
    ctx.line_to(tip_x, tip_y - s * 0.95);
    ctx.line_to(tip_x, tip_y);
    ctx.fill();
}

fn paint_rotate(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64, lw: f64) {
    // 3/4 circle arc + arrowhead at the end of the arc.
    let radius = r * 0.55;
    let steps = 24;
    let a0 = -PI * 0.25;
    let a1 = a0 + PI * 1.5;
    ctx.begin_path();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let a = a0 + (a1 - a0) * t;
        let x = cx + radius * a.cos();
        let y = cy + radius * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();

    // Arrowhead at the arc end pointing along the tangent direction.
    let tip_x = cx + radius * a1.cos();
    let tip_y = cy + radius * a1.sin();
    // Tangent at angle a1 is the perpendicular to the radial vector
    // (rotated +90° CCW so the head points "forward" along the arc's
    // sweep direction).
    let tangent_x = -a1.sin();
    let tangent_y = a1.cos();
    let head_back = lw * 2.0;
    let head_width = lw * 1.4;
    let bx = tip_x - tangent_x * head_back;
    let by = tip_y - tangent_y * head_back;
    let perp_x = -tangent_y;
    let perp_y = tangent_x;
    ctx.begin_path();
    ctx.move_to(tip_x, tip_y);
    ctx.line_to(bx + perp_x * head_width, by + perp_y * head_width);
    ctx.line_to(bx - perp_x * head_width, by - perp_y * head_width);
    ctx.line_to(tip_x, tip_y);
    ctx.fill();
}

fn paint_pan(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Cross of 4 arrows (up, down, left, right).
    let s = r * 0.7;
    let head = r * 0.18;
    // Horizontal axis.
    ctx.begin_path();
    ctx.move_to(cx - s, cy);
    ctx.line_to(cx + s, cy);
    ctx.stroke();
    // Vertical axis.
    ctx.begin_path();
    ctx.move_to(cx, cy - s);
    ctx.line_to(cx, cy + s);
    ctx.stroke();
    // Arrowheads.
    let arrows = [
        (cx + s, cy, (-head, head), (-head, -head)), // right
        (cx - s, cy, (head, head), (head, -head)),   // left
        (cx, cy + s, (head, -head), (-head, -head)), // up
        (cx, cy - s, (head, head), (-head, head)),   // down
    ];
    for (tx, ty, b1, b2) in arrows {
        ctx.begin_path();
        ctx.move_to(tx, ty);
        ctx.line_to(tx + b1.0, ty + b1.1);
        ctx.line_to(tx + b2.0, ty + b2.1);
        ctx.line_to(tx, ty);
        ctx.fill();
    }
}

fn paint_zoom(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Magnifying glass: circle in upper-left, handle going to bottom-right.
    let glass_r = r * 0.42;
    let gx = cx - r * 0.15;
    let gy = cy + r * 0.18;
    let steps = 18;
    ctx.begin_path();
    for i in 0..=steps {
        let a = (i as f64 / steps as f64) * TAU;
        let x = gx + glass_r * a.cos();
        let y = gy + glass_r * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
    // Handle.
    let h0_x = gx + glass_r * 0.70710678;
    let h0_y = gy - glass_r * 0.70710678;
    let h1_x = h0_x + r * 0.45;
    let h1_y = h0_y - r * 0.45;
    ctx.begin_path();
    ctx.move_to(h0_x, h0_y);
    ctx.line_to(h1_x, h1_y);
    ctx.stroke();
}

fn paint_turn(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64, lw: f64) {
    // Turntable: flat ellipse with an arrow looping over it.
    let rx = r * 0.55;
    let ry = r * 0.18;
    let cy_disc = cy - r * 0.25;
    let steps = 24;
    ctx.begin_path();
    for i in 0..=steps {
        let a = (i as f64 / steps as f64) * TAU;
        let x = cx + rx * a.cos();
        let y = cy_disc + ry * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
    // Arc above the disc forming a partial loop.
    let loop_r = r * 0.42;
    let cy_loop = cy + r * 0.05;
    let a0 = PI * 0.2;
    let a1 = PI - PI * 0.2;
    ctx.begin_path();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let a = a0 + (a1 - a0) * t;
        let x = cx + loop_r * a.cos();
        let y = cy_loop + loop_r * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
    // Arrowhead at left end of the loop, pointing down.
    let tip_x = cx + loop_r * a1.cos();
    let tip_y = cy_loop + loop_r * a1.sin();
    let head = lw * 1.6;
    ctx.begin_path();
    ctx.move_to(tip_x, tip_y);
    ctx.line_to(tip_x - head, tip_y + head);
    ctx.line_to(tip_x - head * 0.2, tip_y + head * 1.2);
    ctx.line_to(tip_x, tip_y);
    ctx.fill();
}

fn paint_persp(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Three lines converging to a vanishing point (right edge of icon).
    let vp_x = cx + r * 0.6;
    let vp_y = cy;
    let s = r * 0.65;
    for fy in [-1.0, 0.0, 1.0] {
        ctx.begin_path();
        ctx.move_to(cx - s, cy + fy * s);
        ctx.line_to(vp_x, vp_y);
        ctx.stroke();
    }
    // Small dot at vanishing point.
    ctx.begin_path();
    ctx.move_to(vp_x + 1.0, vp_y);
    ctx.line_to(vp_x - 1.0, vp_y);
    ctx.stroke();
}

fn paint_bed(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // 3 horizontal lines of decreasing width — a stack of bed layers
    // / a grid in perspective.
    let s = r * 0.7;
    let lengths = [1.0_f64, 0.78, 0.55];
    let ys = [cy - s * 0.45, cy - s * 0.05, cy + s * 0.35];
    for (len, y) in lengths.iter().zip(ys.iter()) {
        let half = s * len;
        ctx.begin_path();
        ctx.move_to(cx - half, *y);
        ctx.line_to(cx + half, *y);
        ctx.stroke();
    }
}

fn paint_shade(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Outline circle plus a half-disc "shadow" on the left side.
    let sphere_r = r * 0.55;
    let steps = 24;
    // Outline.
    ctx.begin_path();
    for i in 0..=steps {
        let a = (i as f64 / steps as f64) * TAU;
        let x = cx + sphere_r * a.cos();
        let y = cy + sphere_r * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    ctx.stroke();
    // Shadow crescent: arc from +π/2 to -π/2 along the left semicircle.
    ctx.begin_path();
    let half_steps = 12;
    for i in 0..=half_steps {
        let t = i as f64 / half_steps as f64;
        let a = PI * 0.5 + t * PI;
        let x = cx + sphere_r * a.cos();
        let y = cy + sphere_r * a.sin();
        if i == 0 {
            ctx.move_to(x, y);
        } else {
            ctx.line_to(x, y);
        }
    }
    // Close with a narrower arc back across so it forms a crescent.
    for i in 0..=half_steps {
        let t = i as f64 / half_steps as f64;
        let a = -PI * 0.5 + t * PI;
        let x = cx + sphere_r * 0.2 * a.cos();
        let y = cy + sphere_r * a.sin();
        ctx.line_to(x, y);
    }
    ctx.fill();
}

fn paint_snap(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Three filled dots on a horizontal line.
    let s = r * 0.6;
    ctx.begin_path();
    ctx.move_to(cx - s, cy);
    ctx.line_to(cx + s, cy);
    ctx.stroke();

    let dot_r = (r * 0.16).max(2.0);
    let steps = 12;
    for fx in [-0.7_f64, 0.0, 0.7] {
        let dx = cx + fx * s;
        ctx.begin_path();
        for i in 0..=steps {
            let a = (i as f64 / steps as f64) * TAU;
            let x = dx + dot_r * a.cos();
            let y = cy + dot_r * a.sin();
            if i == 0 {
                ctx.move_to(x, y);
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.fill();
    }
}

fn paint_dropdown_chevron(ctx: &mut dyn DrawCtx, cx: f64, cy: f64, r: f64) {
    // Small downward chevron — used as an overlay affordance.
    let s = r * 0.45;
    ctx.begin_path();
    ctx.move_to(cx - s, cy + s * 0.4);
    ctx.line_to(cx, cy - s * 0.4);
    ctx.line_to(cx + s, cy + s * 0.4);
    ctx.stroke();
}

#[cfg(test)]
mod tests {
    use super::*;
    use agg_gui::{framebuffer::Framebuffer, GfxCtx};

    fn paint_icon_to_buffer(kind: IconKind) -> Framebuffer {
        let mut fb = Framebuffer::new(64, 64);
        {
            let mut g = GfxCtx::new(&mut fb);
            paint_icon(kind, &mut g, 32.0, 32.0, 24.0, Color::rgba(0.2, 0.2, 0.2, 1.0));
        }
        fb
    }

    #[test]
    fn every_icon_writes_some_pixels() {
        for kind in [
            IconKind::Home,
            IconKind::Fit,
            IconKind::Select,
            IconKind::Rotate,
            IconKind::Pan,
            IconKind::Zoom,
            IconKind::Turn,
            IconKind::Persp,
            IconKind::Bed,
            IconKind::Shade,
            IconKind::Snap,
            IconKind::DropdownChevron,
        ] {
            let fb = paint_icon_to_buffer(kind);
            let painted = fb.pixels().chunks_exact(4).filter(|p| p[3] > 0).count();
            assert!(
                painted > 5,
                "icon {:?} produced no visible pixels",
                kind
            );
        }
    }
}
