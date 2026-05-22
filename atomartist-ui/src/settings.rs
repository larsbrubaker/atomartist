//! Persistent HUD settings (perspective / turntable / bed-visible /
//! render-mode / snap-amount) and floating debug-window layout
//! (Inspector, Performance graph).
//!
//! The settings live in a single text file on disk so we don't pull
//! `serde` into the crate just for a few scalar fields. The format is
//! one `key=value` per line, comments allowed with a leading `#`:
//!
//! ```text
//! # AtomArtist UI settings
//! perspective=true
//! turntable=true
//! show_bed=true
//! render_style=Shaded
//! snap_amount=1.0
//! inspector_open=false
//! inspector_x=60
//! inspector_y=60
//! inspector_w=420
//! inspector_h=520
//! performance_open=false
//! performance_x=60
//! performance_y=620
//! performance_w=360
//! performance_h=160
//! ```
//!
//! `read_from_str` is forgiving: unknown keys are skipped, missing
//! keys default to the documented "first launch" defaults (matching
//! `UiSettings::default`), and malformed values fall back to the
//! default for that field. The intent is that an old or
//! hand-edited file never blocks app startup.

use std::path::Path;

use atomartist_renderer::RenderStyle;

/// Persisted geometry + visibility for one floating debug window
/// (Inspector or Performance). Bounds are agg-gui Y-up screen
/// coordinates (origin bottom-left) — see `agg_gui::Rect`. `width`
/// or `height` ≤ 0 means "use the default window placement".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugWindowState {
    pub open: bool,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl DebugWindowState {
    /// `true` if the stored bounds are usable. We treat zero-area or
    /// negative-area bounds as "ignored" so the window falls back to
    /// its hard-coded default placement on next startup.
    pub fn has_valid_bounds(&self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }
}

/// Layout state for every floating debug window AtomArtist ships.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugWindowsState {
    pub inspector: DebugWindowState,
    pub performance: DebugWindowState,
}

impl Default for DebugWindowsState {
    fn default() -> Self {
        // Defaults chosen so the Inspector and Performance windows
        // open in the bottom-left corner without overlapping each
        // other and without covering the 3-D viewport in the centre.
        Self {
            inspector: DebugWindowState {
                open: false,
                x: 60.0,
                y: 60.0,
                width: 420.0,
                height: 520.0,
            },
            performance: DebugWindowState {
                open: false,
                x: 60.0,
                y: 620.0,
                width: 360.0,
                height: 160.0,
            },
        }
    }
}

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
    /// Visibility + bounds for the View → Debug floating windows.
    /// These are owned by the widget tree (not `AppState`) so the
    /// shell composes them in via [`crate::debug_windows::DebugWindowHandles`]
    /// before writing to disk.
    pub debug_windows: DebugWindowsState,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            perspective: true,
            turntable: true,
            show_bed: true,
            render_style: RenderStyle::default(),
            snap_amount: 1.0,
            debug_windows: DebugWindowsState::default(),
        }
    }
}

impl UiSettings {
    /// Render to the on-disk text format.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(320);
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
        write_debug_window(&mut out, "inspector", &self.debug_windows.inspector);
        write_debug_window(&mut out, "performance", &self.debug_windows.performance);
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
                _ => {
                    apply_debug_window_kv(&mut out.debug_windows, key, value);
                }
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

fn write_debug_window(out: &mut String, prefix: &str, state: &DebugWindowState) {
    out.push_str(&format!("{prefix}_open={}\n", state.open));
    out.push_str(&format!("{prefix}_x={}\n", state.x));
    out.push_str(&format!("{prefix}_y={}\n", state.y));
    out.push_str(&format!("{prefix}_w={}\n", state.width));
    out.push_str(&format!("{prefix}_h={}\n", state.height));
}

/// Dispatch a single `key=value` line into the matching field on
/// `DebugWindowsState`. Unknown prefixes / suffixes are silently
/// ignored so old config files keep loading.
fn apply_debug_window_kv(state: &mut DebugWindowsState, key: &str, value: &str) {
    let Some((prefix, suffix)) = key.split_once('_') else {
        return;
    };
    let target: &mut DebugWindowState = match prefix {
        "inspector" => &mut state.inspector,
        "performance" => &mut state.performance,
        _ => return,
    };
    match suffix {
        "open" => {
            if let Some(b) = parse_bool(value) {
                target.open = b;
            }
        }
        "x" => {
            if let Ok(f) = value.parse::<f64>() {
                if f.is_finite() {
                    target.x = f;
                }
            }
        }
        "y" => {
            if let Ok(f) = value.parse::<f64>() {
                if f.is_finite() {
                    target.y = f;
                }
            }
        }
        "w" => {
            if let Ok(f) = value.parse::<f64>() {
                if f.is_finite() && f > 0.0 {
                    target.width = f;
                }
            }
        }
        "h" => {
            if let Ok(f) = value.parse::<f64>() {
                if f.is_finite() && f > 0.0 {
                    target.height = f;
                }
            }
        }
        _ => {}
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
            debug_windows: DebugWindowsState {
                inspector: DebugWindowState {
                    open: true,
                    x: 100.0,
                    y: 200.0,
                    width: 480.0,
                    height: 600.0,
                },
                performance: DebugWindowState {
                    open: true,
                    x: 800.0,
                    y: 100.0,
                    width: 320.0,
                    height: 140.0,
                },
            },
        };
        let parsed = UiSettings::from_text(&s.to_text());
        assert_eq!(s, parsed);
    }

    #[test]
    fn missing_debug_windows_block_defaults_cleanly() {
        // Older config files predate the View → Debug windows
        // section; loading them must not corrupt the rest of the
        // settings and must surface the documented defaults for
        // the new fields.
        let text = "\
            perspective=false\n\
            turntable=false\n\
        ";
        let s = UiSettings::from_text(text);
        assert!(!s.perspective);
        assert!(!s.turntable);
        assert_eq!(s.debug_windows, DebugWindowsState::default());
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
