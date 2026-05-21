//! Persistent HUD settings (perspective / turntable / bed-visible /
//! render-mode / snap-amount).
//!
//! The settings live in a single text file on disk so we don't pull
//! `serde` into the crate just for five scalar fields. The format is
//! one `key=value` per line, comments allowed with a leading `#`:
//!
//! ```text
//! # AtomArtist UI settings
//! perspective=true
//! turntable=true
//! show_bed=true
//! render_style=Shaded
//! snap_amount=1.0
//! ```
//!
//! `read_from_str` is forgiving: unknown keys are skipped, missing
//! keys default to the documented "first launch" defaults (matching
//! `UiSettings::default`), and malformed values fall back to the
//! default for that field. The intent is that an old or
//! hand-edited file never blocks app startup.

use std::path::Path;

use atomartist_renderer::RenderStyle;

/// Snapshot of the HUD widget states that should survive across
/// runs of the app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiSettings {
    /// Whether perspective projection is enabled (vs orthographic).
    pub perspective: bool,
    /// Whether turntable orbit mode is enabled (vs trackball).
    pub turntable: bool,
    /// Whether the floor grid is drawn.
    pub show_bed: bool,
    /// Which render style is selected.
    pub render_style: RenderStyle,
    /// Active snap distance. `0.0` means snapping is off (matches
    /// the GridOptionsPanel "Off" entry).
    pub snap_amount: f64,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            perspective: true,
            turntable: true,
            show_bed: true,
            render_style: RenderStyle::default(),
            snap_amount: 1.0,
        }
    }
}

impl UiSettings {
    /// Render to the on-disk text format.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(160);
        out.push_str("# AtomArtist UI settings\n");
        out.push_str(&format!("perspective={}\n", self.perspective));
        out.push_str(&format!("turntable={}\n", self.turntable));
        out.push_str(&format!("show_bed={}\n", self.show_bed));
        out.push_str(&format!(
            "render_style={}\n",
            render_style_to_token(self.render_style)
        ));
        // Use the default `{}` formatting so simple values like `1`
        // round-trip cleanly without a trailing ".0".
        out.push_str(&format!("snap_amount={}\n", self.snap_amount));
        out
    }

    /// Parse from the text format. Missing / malformed fields fall
    /// back to `UiSettings::default()`.
    pub fn from_text(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "perspective" => {
                    if let Some(b) = parse_bool(value) {
                        out.perspective = b;
                    }
                }
                "turntable" => {
                    if let Some(b) = parse_bool(value) {
                        out.turntable = b;
                    }
                }
                "show_bed" => {
                    if let Some(b) = parse_bool(value) {
                        out.show_bed = b;
                    }
                }
                "render_style" => {
                    if let Some(s) = render_style_from_token(value) {
                        out.render_style = s;
                    }
                }
                "snap_amount" => {
                    if let Ok(f) = value.parse::<f64>() {
                        // Negative snap amounts are nonsensical;
                        // treat them as 0 (snap off).
                        out.snap_amount = if f.is_finite() && f >= 0.0 { f } else { 0.0 };
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Read settings from a file, falling back to `default()` if
    /// the file is missing or unreadable.
    pub fn read_from_file(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_text(&s),
            Err(_) => Self::default(),
        }
    }

    /// Write settings to a file, creating parent directories as
    /// needed. Returns the IO error on failure so callers can log
    /// it; we never want a settings-save failure to crash the app.
    pub fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_text())
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn render_style_to_token(s: RenderStyle) -> &'static str {
    match s {
        RenderStyle::Shaded => "Shaded",
        RenderStyle::OutlineOnly => "Outlines",
        RenderStyle::Wireframe => "Wireframe",
    }
}

fn render_style_from_token(s: &str) -> Option<RenderStyle> {
    // Tolerate the obvious variants — both MatterCAD's "Outlines"
    // label and the Rust enum name "OutlineOnly".
    match s {
        "Shaded" | "shaded" => Some(RenderStyle::Shaded),
        "Outlines" | "outlines" | "Outline" | "OutlineOnly" => Some(RenderStyle::OutlineOnly),
        "Wireframe" | "wireframe" | "Polygons" | "polygons" => Some(RenderStyle::Wireframe),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_text() {
        let s = UiSettings::default();
        let text = s.to_text();
        let parsed = UiSettings::from_text(&text);
        assert_eq!(s, parsed);
    }

    #[test]
    fn non_default_values_round_trip() {
        let s = UiSettings {
            perspective: false,
            turntable: false,
            show_bed: false,
            render_style: RenderStyle::Wireframe,
            snap_amount: 0.25,
        };
        let parsed = UiSettings::from_text(&s.to_text());
        assert_eq!(s, parsed);
    }

    #[test]
    fn unknown_lines_and_comments_are_tolerated() {
        let text = "\
            # comment line\n\
            future_setting=42\n\
            perspective=false\n\
            ; not a comment marker we recognise, but it has no =\n\
            \n\
            turntable=on\n\
        ";
        let s = UiSettings::from_text(text);
        assert!(!s.perspective);
        assert!(s.turntable);
        // Everything else is defaulted.
        let d = UiSettings::default();
        assert_eq!(s.show_bed, d.show_bed);
        assert_eq!(s.render_style, d.render_style);
        assert_eq!(s.snap_amount, d.snap_amount);
    }

    #[test]
    fn malformed_values_fall_back_to_defaults() {
        let text = "\
            perspective=maybe\n\
            snap_amount=NotANumber\n\
            render_style=ChunkyShader\n\
        ";
        let s = UiSettings::from_text(text);
        let d = UiSettings::default();
        assert_eq!(s.perspective, d.perspective);
        assert_eq!(s.snap_amount, d.snap_amount);
        assert_eq!(s.render_style, d.render_style);
    }
}
