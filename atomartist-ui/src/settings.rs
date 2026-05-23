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
//! main_window_x=120
//! main_window_y=80
//! main_window_w=1280
//! main_window_h=720
//! main_window_maximized=false
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

use std::path::{Path, PathBuf};

use atomartist_renderer::RenderStyle;

/// Persisted geometry + maximized flag for the host OS window the
/// app paints into. Coordinates are **physical pixels** in the OS's
/// virtual-desktop space (positive Y down, top-left origin) so that
/// the position survives DPI changes when the user moves the
/// window between monitors with different scale factors. `width`
/// or `height` ≤ 0 means "no usable saved geometry, fall back to
/// the launch defaults" — same convention as `DebugWindowState`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MainWindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl MainWindowState {
    /// `true` if the stored geometry is usable. Zero-area means we
    /// have never persisted real bounds, so the shell should keep
    /// its first-launch defaults.
    pub fn has_valid_geometry(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// Decide whether the saved window placement still overlaps at
    /// least one of the currently-attached monitors by enough area
    /// for the user to drag the window. Monitors are passed as
    /// `(x, y, width, height)` physical-pixel rects (same shape
    /// `winit::monitor::MonitorHandle` reports). Requires at least
    /// a `MIN_OVERLAP_WIDTH` × `MIN_OVERLAP_HEIGHT` patch of the
    /// title-bar strip (top edge of the window) to be visible —
    /// enough to grab and drag the window back.
    pub fn fits_on_monitors<I, M>(&self, monitors: I) -> bool
    where
        I: IntoIterator<Item = M>,
        M: Into<(i32, i32, u32, u32)>,
    {
        if !self.has_valid_geometry() {
            return false;
        }
        // Restrict the visibility check to the title-bar strip
        // (top 40 physical pixels of the window). If that strip
        // is reachable on some monitor, the user can drag the rest
        // of the window onto the screen.
        let strip_w = self.width as i32;
        let strip_h = MIN_OVERLAP_HEIGHT.min(self.height as i32);
        let strip = (self.x, self.y, strip_w, strip_h);
        monitors
            .into_iter()
            .map(Into::into)
            .any(|m| rects_overlap_at_least(strip, m, MIN_OVERLAP_WIDTH, MIN_OVERLAP_HEIGHT))
    }
}

impl Default for MainWindowState {
    fn default() -> Self {
        // Sentinel size of zero so first-launch is detected as
        // "no saved bounds" and the shell uses its built-in
        // initial window size. Position is irrelevant when
        // `has_valid_geometry` returns false.
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            maximized: false,
        }
    }
}

/// Minimum on-screen overlap (physical pixels) of the window's
/// title-bar strip required for [`MainWindowState::fits_on_monitors`]
/// to call a saved placement "still reachable".
pub const MIN_OVERLAP_WIDTH: i32 = 120;
pub const MIN_OVERLAP_HEIGHT: i32 = 40;

/// Geometry of an axis-aligned overlap test on two screen-space
/// rectangles in `(x, y, w, h)` form. Returns `true` when the
/// intersection covers at least `min_w` × `min_h` pixels.
fn rects_overlap_at_least(
    a: (i32, i32, i32, i32),
    b: (i32, i32, u32, u32),
    min_w: i32,
    min_h: i32,
) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let bw = bw as i32;
    let bh = bh as i32;
    let ix0 = ax.max(bx);
    let iy0 = ay.max(by);
    let ix1 = (ax + aw).min(bx + bw);
    let iy1 = (ay + ah).min(by + bh);
    (ix1 - ix0) >= min_w && (iy1 - iy0) >= min_h
}

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
#[derive(Clone, Debug, PartialEq)]
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
    /// OS window placement (position, size, maximized). Persists
    /// across launches; the shell validates the position against
    /// the current monitor layout before applying so a window that
    /// was last on a now-disconnected monitor falls back to default
    /// placement instead of opening invisibly.
    pub main_window: MainWindowState,
    /// Visibility + bounds for the View → Debug floating windows.
    /// These are owned by the widget tree (not `AppState`) so the
    /// shell composes them in via [`crate::debug_windows::DebugWindowHandles`]
    /// before writing to disk.
    pub debug_windows: DebugWindowsState,
    /// Absolute path to the last project file the user opened or
    /// saved (typically `.atmr`, occasionally `.json` for legacy
    /// saves). The shell auto-reopens this on launch so the user
    /// resumes where they left off; `None` means there's nothing
    /// to reopen and the starter graph stays loaded.
    pub last_project_path: Option<PathBuf>,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            perspective: true,
            turntable: true,
            show_bed: true,
            render_style: RenderStyle::default(),
            snap_amount: 1.0,
            main_window: MainWindowState::default(),
            debug_windows: DebugWindowsState::default(),
            last_project_path: None,
        }
    }
}

impl UiSettings {
    /// Render to the on-disk text format.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(512);
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
        write_main_window(&mut out, &self.main_window);
        write_debug_window(&mut out, "inspector", &self.debug_windows.inspector);
        write_debug_window(&mut out, "performance", &self.debug_windows.performance);
        if let Some(p) = self.last_project_path.as_ref() {
            // `to_string_lossy` is good enough — projects with
            // non-UTF-8 paths (rare) round-trip in their lossy form,
            // which is no worse than the alternative of dropping the
            // line silently. The auto-reopen path will fall back to
            // the starter graph if the on-disk file no longer
            // exists, so a corrupted path here can't break startup.
            out.push_str(&format!("last_project_path={}\n", p.to_string_lossy()));
        }
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
                "last_project_path" => {
                    // Empty value clears the slot — same as missing
                    // line. Anything non-empty is taken at face
                    // value; the shell rechecks existence on
                    // startup before trying to open it.
                    if value.is_empty() {
                        out.last_project_path = None;
                    } else {
                        out.last_project_path = Some(PathBuf::from(value));
                    }
                }
                _ => {
                    if apply_main_window_kv(&mut out.main_window, key, value) {
                        continue;
                    }
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

fn write_main_window(out: &mut String, state: &MainWindowState) {
    out.push_str(&format!("main_window_x={}\n", state.x));
    out.push_str(&format!("main_window_y={}\n", state.y));
    out.push_str(&format!("main_window_w={}\n", state.width));
    out.push_str(&format!("main_window_h={}\n", state.height));
    out.push_str(&format!("main_window_maximized={}\n", state.maximized));
}

/// Returns `true` if the key was a `main_window_*` field (regardless
/// of whether the value parsed cleanly). Lets the dispatch in
/// `from_text` fall through to `apply_debug_window_kv` for everything
/// else without the two parsers stepping on each other.
fn apply_main_window_kv(state: &mut MainWindowState, key: &str, value: &str) -> bool {
    let Some(suffix) = key.strip_prefix("main_window_") else {
        return false;
    };
    match suffix {
        "x" => {
            if let Ok(n) = value.parse::<i32>() {
                state.x = n;
            }
        }
        "y" => {
            if let Ok(n) = value.parse::<i32>() {
                state.y = n;
            }
        }
        "w" => {
            if let Ok(n) = value.parse::<u32>() {
                state.width = n;
            }
        }
        "h" => {
            if let Ok(n) = value.parse::<u32>() {
                state.height = n;
            }
        }
        "maximized" => {
            if let Some(b) = parse_bool(value) {
                state.maximized = b;
            }
        }
        _ => {}
    }
    true
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
            main_window: MainWindowState {
                x: 250,
                y: 180,
                width: 1600,
                height: 900,
                maximized: true,
            },
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
            last_project_path: Some(PathBuf::from("C:/users/bob/projects/widget.atmr")),
        };
        let parsed = UiSettings::from_text(&s.to_text());
        assert_eq!(s, parsed);
    }

    #[test]
    fn last_project_path_round_trips_when_present_and_absent() {
        let mut s = UiSettings::default();
        // Absent: no `last_project_path=` line is emitted.
        let text = s.to_text();
        assert!(!text.contains("last_project_path="));
        let parsed = UiSettings::from_text(&text);
        assert_eq!(parsed.last_project_path, None);

        // Present: serialized and parsed back through.
        s.last_project_path = Some(PathBuf::from("/tmp/atomartist/test.atmr"));
        let parsed = UiSettings::from_text(&s.to_text());
        assert_eq!(parsed.last_project_path, s.last_project_path);

        // Empty value explicitly clears the slot.
        let parsed = UiSettings::from_text("last_project_path=\n");
        assert_eq!(parsed.last_project_path, None);
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
        assert_eq!(s.main_window, MainWindowState::default());
    }

    #[test]
    fn default_main_window_state_is_not_geometry_valid() {
        // Sentinel zero-size means "no saved geometry" so the
        // shell falls back to its built-in launch defaults.
        assert!(!MainWindowState::default().has_valid_geometry());
    }

    #[test]
    fn main_window_fits_when_fully_inside_a_monitor() {
        let win = MainWindowState {
            x: 200,
            y: 200,
            width: 1280,
            height: 720,
            maximized: false,
        };
        // Single 1920×1080 monitor at origin.
        let monitors = vec![(0, 0, 1920u32, 1080u32)];
        assert!(win.fits_on_monitors(monitors));
    }

    #[test]
    fn main_window_does_not_fit_when_completely_off_screen() {
        // Window saved on a 1920×1080 second monitor that has been
        // detached. The remaining primary monitor at (0, 0) sees
        // nothing of the window.
        let win = MainWindowState {
            x: 2500,
            y: 400,
            width: 1280,
            height: 720,
            maximized: false,
        };
        let monitors = vec![(0, 0, 1920u32, 1080u32)];
        assert!(!win.fits_on_monitors(monitors));
    }

    #[test]
    fn main_window_fits_when_title_bar_just_pokes_onto_a_monitor() {
        // Window is mostly off the right edge but the title bar
        // strip still overlaps the monitor by more than the
        // minimum-drag width — user can still reach it.
        let win = MainWindowState {
            x: 1700, // monitor goes to x = 1920, so 220px visible
            y: 100,
            width: 800,
            height: 600,
            maximized: false,
        };
        let monitors = vec![(0, 0, 1920u32, 1080u32)];
        assert!(win.fits_on_monitors(monitors));
    }

    #[test]
    fn main_window_does_not_fit_when_only_a_sliver_is_visible() {
        // 50 px of overlap is less than `MIN_OVERLAP_WIDTH` (120 px)
        // — not enough to grab and drag.
        let win = MainWindowState {
            x: 1870, // 50 px until the 1920 right edge
            y: 100,
            width: 800,
            height: 600,
            maximized: false,
        };
        let monitors = vec![(0, 0, 1920u32, 1080u32)];
        assert!(!win.fits_on_monitors(monitors));
    }

    #[test]
    fn main_window_fits_on_secondary_monitor_when_primary_does_not() {
        // Two monitors side by side; the saved position is on the
        // secondary. `fits_on_monitors` must accept overlap with
        // *any* monitor, not just the first one.
        let win = MainWindowState {
            x: 2400,
            y: 200,
            width: 1280,
            height: 720,
            maximized: false,
        };
        let monitors = vec![
            (0, 0, 1920u32, 1080u32),     // primary
            (1920, 0, 1920u32, 1080u32),  // secondary on the right
        ];
        assert!(win.fits_on_monitors(monitors));
    }

    #[test]
    fn invalid_geometry_never_fits() {
        // Zero-size geometry is the sentinel for "no saved bounds"
        // and must always be reported as unfit so callers fall back
        // to first-launch placement.
        let win = MainWindowState::default();
        let monitors = vec![(0, 0, 1920u32, 1080u32)];
        assert!(!win.fits_on_monitors(monitors));
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
