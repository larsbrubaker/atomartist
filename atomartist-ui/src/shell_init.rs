//! Shared shell initialization — theme, fonts, and text-quality settings.
//!
//! The native (`demo-native`) and wasm (`demo-wasm`) shells are meant to be
//! the thinnest possible platform glue: a window or `<canvas>`, an event
//! pump, and a GPU surface. Everything that affects *what the user sees*
//! lives here so the two platforms render pixel-identically. When this was
//! duplicated in each shell the copies drifted — the wasm shell hardcoded
//! LCD/hinting off and skipped the gamma/weight recipe entirely, so its
//! text rasterized differently from native. Centralizing it makes that
//! class of divergence impossible.
//!
//! The only platform-specific input is the device scale (native: winit
//! `window.scale_factor()`; wasm: `devicePixelRatio`). It is passed in
//! explicitly rather than read from the global so the LCD/hinting DPI
//! decision is identical and order-independent across shells. Callers
//! should also register the same value via [`agg_gui::set_device_scale`]
//! so layout and hit-testing use the same ratio.

use std::sync::Arc;

use agg_gui::{
    text::Font,
    theme::{set_visuals, Visuals},
};

const DEFAULT_FONT_BYTES: &[u8] =
    include_bytes!("../../../agg-gui/agg-gui/assets/fonts/NotoSans-Regular.ttf");
const ICON_FONT_BYTES: &[u8] = include_bytes!("../assets/bootstrap-icons.ttf");

/// Install AtomArtist's light theme, system font (NotoSans with a Bootstrap
/// Icons fallback), and the full text-quality recipe. Call once at startup,
/// from both shells, so native and wasm produce identical pixels.
///
/// `device_scale` is the platform's physical-pixels-per-logical-pixel ratio.
/// LCD subpixel rendering and Y-axis hinting are enabled only at standard
/// DPI (`<= 1.25`) — above that they produce colour-fringe artifacts, so we
/// fall back to plain grayscale AA. Every other rasterizer parameter is
/// pinned to the same defaults the reference `truetype_test` demo uses.
pub fn install_theme_and_fonts(device_scale: f64) {
    // Light theme — AtomArtist is a CAD-style design tool where
    // high-contrast white backgrounds match user expectation.
    set_visuals(Visuals::light());

    let icon_font =
        Arc::new(Font::from_bytes(ICON_FONT_BYTES.to_vec()).expect("load Bootstrap Icons"));
    let font = Arc::new(
        Font::from_bytes(DEFAULT_FONT_BYTES.to_vec())
            .expect("load NotoSans-Regular")
            .with_fallback(icon_font),
    );
    // Make the font available to every widget via agg-gui's thread-local
    // system-font slot, so widgets fall back to it without an explicit
    // `ctx.set_font` call.
    agg_gui::font_settings::set_system_font(Some(font));

    let standard_dpi = device_scale <= 1.25;
    agg_gui::font_settings::set_font_size_scale(1.0);
    agg_gui::font_settings::set_lcd_enabled(standard_dpi);
    agg_gui::font_settings::set_hinting_enabled(standard_dpi);
    agg_gui::font_settings::set_gamma(1.0);
    agg_gui::font_settings::set_width(1.0);
    agg_gui::font_settings::set_interval(0.0);
    agg_gui::font_settings::set_faux_weight(0.0);
    agg_gui::font_settings::set_faux_italic(0.0);
    agg_gui::font_settings::set_primary_weight(1.0 / 3.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both shells route through this one function, so the LCD/hinting DPI
    /// gate is shared. Regression for the native-vs-wasm text mismatch: the
    /// wasm shell used to hardcode both off, making its text render
    /// differently from native on standard-DPI displays.
    #[test]
    fn standard_dpi_enables_lcd_and_hinting() {
        install_theme_and_fonts(1.0);
        assert!(agg_gui::font_settings::lcd_enabled());
        assert!(agg_gui::font_settings::hinting_enabled());
    }

    /// Above 1.25x, LCD subpixel + hinting are dropped to avoid colour
    /// fringing — and crucially both platforms make the *same* call, so a
    /// hi-DPI native window and a hi-DPI browser canvas still match.
    #[test]
    fn hi_dpi_disables_lcd_and_hinting() {
        install_theme_and_fonts(2.0);
        assert!(!agg_gui::font_settings::lcd_enabled());
        assert!(!agg_gui::font_settings::hinting_enabled());
    }
}
