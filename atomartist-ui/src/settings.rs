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
//! Project *locations* inside these settings (`last_project_path`,
//! `recent_projects`) are [`StorageUri`]s and are read / written through
//! a storage provider like every other project reference. The settings
//! file **itself** is shell-owned application configuration, not project
//! storage, so [`UiSettings::read_from_file`] /
//! [`UiSettings::write_to_file`] keep using `Path` + `std::fs` — this
//! module is on the documented allowlist in
//! `atomartist-lib/tests/no_fs_outside_provider.rs`.
//!
//! `read_from_str` is forgiving: unknown keys are skipped, missing
//! keys default to the documented "first launch" defaults (matching
//! `UiSettings::default`), and malformed values fall back to the
//! default for that field. The intent is that an old or
//! hand-edited file never blocks app startup.

use std::path::Path;
use std::str::FromStr;

use agg_gui::theme::{AccentColor, ThemePreference};
use atomartist_renderer::RenderStyle;
use atomartist_storage::StorageUri;

use crate::file_browser::favorites::{Favorite, Favorites};

/// Persisted geometry + maximized flag for the host OS window the
/// app paints into. Coordinates are **physical pixels** in the OS's
/// virtual-desktop space (positive Y down, top-left origin) so that
/// the position survives DPI changes when the user moves the
/// window between monitors with different scale factors. `width`
/// or `height` ≤ 0 means "no usable saved geometry, fall back to
/// the launch defaults" — same convention as `DebugWindowState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    /// Decide what the platform shell should do with these saved
    /// bounds at startup, given the currently-attached monitor list.
    ///
    /// The returned [`WindowPlacement`] captures every decision the
    /// shell needs to make:
    ///
    /// * Whether to use the saved position + size, fall back to
    ///   defaults, or keep the saved size at a recentered position
    ///   (when the saved one is off-screen now).
    /// * Whether to call `set_maximized(true)` afterward — applied
    ///   independently of position validity, so a user who closed
    ///   the app maximized comes back to a maximized window even
    ///   when the un-maximized position needed adjustment.
    ///
    /// Splitting the decision out of the shell keeps it testable
    /// without spinning up a real OS window — the same `monitors`
    /// shape `winit::Window::available_monitors` returns can be
    /// hand-rolled in unit tests.
    pub fn placement<I, M>(self, monitors: I) -> WindowPlacement
    where
        I: IntoIterator<Item = M>,
        M: Into<(i32, i32, u32, u32)>,
    {
        if !self.has_valid_geometry() {
            // Sentinel "no saved geometry" — the shell uses its
            // built-in defaults. We still propagate the maximized
            // flag in case somebody hand-edited the settings file
            // to ask for "maximized at default size".
            return WindowPlacement::Default {
                maximized: self.maximized,
            };
        }
        if self.fits_on_monitors(monitors) {
            return WindowPlacement::Restore { bounds: self };
        }
        WindowPlacement::Recenter {
            width: self.width,
            height: self.height,
            maximized: self.maximized,
        }
    }
}

/// Plan the host shell follows at startup to position the OS window.
/// Built by [`MainWindowState::placement`] from the saved bounds and
/// the live monitor list; `main.rs` consumes it without making any
/// validity decisions of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowPlacement {
    /// No usable saved bounds — the shell uses its first-launch
    /// defaults (e.g., 1280×720 centred by the OS). The maximized
    /// flag is still honoured: the user can come back to a
    /// maximized window even on the very first run if their
    /// settings file says so.
    Default { maximized: bool },
    /// Saved bounds are usable as-is; restore the position, size,
    /// and maximized flag verbatim.
    Restore { bounds: MainWindowState },
    /// Saved bounds had a usable size but a position that no longer
    /// overlaps any attached monitor. The shell should keep the
    /// saved size and maximized flag, but pick a new centred
    /// position on the primary monitor.
    Recenter {
        width: u32,
        height: u32,
        maximized: bool,
    },
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
    /// Location of the last project the user opened or saved — always
    /// an `.atmr` archive, the only project format. A [`StorageUri`],
    /// so a project restored on launch can live on disk, in browser
    /// storage, or on a remote provider. The shell auto-reopens this so
    /// the user resumes where they left off; `None` means there's
    /// nothing to reopen and the starter graph stays loaded.
    pub last_project_path: Option<StorageUri>,
    /// User's chosen theme (light / dark / system). Persisted so the
    /// View → Theme selection survives across runs.
    pub theme: ThemePreference,
    /// User's chosen accent swatch. Persisted so the View → Color
    /// selection survives across runs.
    pub accent_color: AccentColor,
    /// Most-recently-used project files, newest first. Rendered as
    /// the File → Open Recent submenu; capped at
    /// [`MAX_RECENT_PROJECTS`] on both write and read so a hand-
    /// edited file can't grow the menu unboundedly.
    pub recent_projects: Vec<StorageUri>,
    /// Pinned entries for the left favorites rail, plus the
    /// "have we ever seeded?" flag. See
    /// [`crate::file_browser::favorites`] — an empty list with the
    /// flag set is a row the user deliberately cleared, and stays
    /// empty across launches.
    pub favorites: Favorites,
    /// Whether the favorites bar is expanded into its full panel. The
    /// bar starts collapsed on a fresh install, like both ancestors'.
    pub favorites_bar_expanded: bool,
    /// Width the favorites bar opens to, in logical pixels. Kept across
    /// a snap-closed collapse (design §2), and clamped on parse so a
    /// hand-edited file can't hide the node canvas behind the bar.
    pub favorites_bar_width: f32,
}

/// Upper bound on the persisted / displayed recent-projects list.
pub const MAX_RECENT_PROJECTS: usize = 10;

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
            // Light is AtomArtist's first-launch default (CAD-style
            // white background); the shell's `set_visuals` call at
            // startup matches.
            theme: ThemePreference::Light,
            accent_color: AccentColor::default(),
            recent_projects: Vec::new(),
            // Empty and *not* seeded: the first run that has a node
            // registry in hand calls `seed_defaults_once`.
            favorites: Favorites::default(),
            favorites_bar_expanded: false,
            favorites_bar_width: crate::favorites_bar::DEFAULT_EXPANDED_W,
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
        out.push_str(&format!("theme={}\n", self.theme.key()));
        out.push_str(&format!("accent_color={}\n", self.accent_color.key()));
        if let Some(uri) = self.last_project_path.as_ref() {
            // A URI renders as `scheme:///path`, which round-trips
            // exactly through `StorageUri::from_str`. The auto-reopen
            // path asks the provider whether the project still exists
            // before loading, so a stale entry can't break startup.
            out.push_str(&format!("last_project_path={}\n", uri));
        }
        for (i, uri) in self
            .recent_projects
            .iter()
            .take(MAX_RECENT_PROJECTS)
            .enumerate()
        {
            // Indexed keys keep the format line-per-value; the parser
            // sorts by index so a hand-shuffled file still round-trips
            // in a deterministic order.
            //
            // TODO(unescaped): a `StorageUri` may contain a newline or
            // edge whitespace (the type validates the scheme and
            // rejects traversal, not file-name characters), so this
            // write can split one entry across two physical lines —
            // the same hole the favorites encoding closes with
            // [`crate::file_browser::favorites::escape_field`] /
            // `unescape_field`. Fixing it here is a separate change
            // (it needs a format migration for existing files).
            out.push_str(&format!("recent_project_{}={}\n", i, uri));
        }
        // Favorites bar geometry. Both are plain scalars; the parser
        // clamps the width, so an out-of-range value costs the user their
        // size and nothing else.
        out.push_str(&format!(
            "favorites_bar_expanded={}\n",
            self.favorites_bar_expanded
        ));
        out.push_str(&format!(
            "favorites_bar_width={}\n",
            self.favorites_bar_width
        ));
        // The seed flag is written unconditionally (even when false, even
        // with an empty row): it is what tells the next launch whether an
        // empty row means "user cleared it" or "never seeded".
        out.push_str(&format!("favorites_seeded={}\n", self.favorites.seeded()));
        for (i, fav) in self.favorites.list().iter().enumerate() {
            out.push_str(&format!("favorite_{}={}\n", i, fav.to_field()));
        }
        out
    }

    /// Parse from the text format. Missing / malformed fields fall
    /// back to `UiSettings::default()`.
    pub fn from_text(text: &str) -> Self {
        let mut out = Self::default();
        // Collected out-of-line so shuffled / duplicated indices in a
        // hand-edited file still produce a stable, capped list.
        let mut recent: Vec<(usize, StorageUri)> = Vec::new();
        // Same treatment for the favorites row: collected by index so
        // a hand-shuffled file still loads in a deterministic order.
        let mut favorites: Vec<(usize, Favorite)> = Vec::new();
        let mut favorites_seeded = false;
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
                "theme" => {
                    if let Some(t) = ThemePreference::from_key(value) {
                        out.theme = t;
                    }
                }
                "accent_color" => {
                    if let Some(a) = AccentColor::from_key(value) {
                        out.accent_color = a;
                    }
                }
                "last_project_path" => {
                    // Empty value clears the slot — same as missing
                    // line. A value that is not a valid `StorageUri`
                    // is *discarded*: pre-release there is no path-
                    // style form to migrate, so a stale settings file
                    // simply loses the entry (plan §3.1). The shell
                    // rechecks existence on startup before opening.
                    out.last_project_path = StorageUri::from_str(value).ok();
                }
                "favorites_seeded" => {
                    if let Some(b) = parse_bool(value) {
                        favorites_seeded = b;
                    }
                }
                "favorites_bar_expanded" => {
                    if let Some(b) = parse_bool(value) {
                        out.favorites_bar_expanded = b;
                    }
                }
                "favorites_bar_width" => {
                    // Clamped, not rejected: a stale or hand-edited width
                    // (0, NaN, 10_000) should still open a usable bar
                    // rather than one the user cannot see or cannot get
                    // out from behind.
                    if let Ok(f) = value.parse::<f32>() {
                        out.favorites_bar_width = crate::favorites_bar::clamp_stored_width(f);
                    }
                }
                _ => {
                    if let Some(idx) = key.strip_prefix("recent_project_") {
                        if let Ok(i) = idx.parse::<usize>() {
                            // Same rule: unparseable entries vanish
                            // rather than poisoning the recent list.
                            if let Ok(uri) = StorageUri::from_str(value) {
                                recent.push((i, uri));
                            }
                        }
                        continue;
                    }
                    if let Some(idx) = key.strip_prefix("favorite_") {
                        if let Ok(i) = idx.parse::<usize>() {
                            // Malformed / unknown-kind entries vanish
                            // rather than loading half-formed.
                            if let Some(fav) = Favorite::from_field(value) {
                                favorites.push((i, fav));
                            }
                        }
                        continue;
                    }
                    if apply_main_window_kv(&mut out.main_window, key, value) {
                        continue;
                    }
                    apply_debug_window_kv(&mut out.debug_windows, key, value);
                }
            }
        }
        recent.sort_by_key(|(i, _)| *i);
        out.recent_projects = recent.into_iter().map(|(_, uri)| uri).collect();
        out.recent_projects.dedup();
        out.recent_projects.truncate(MAX_RECENT_PROJECTS);
        favorites.sort_by_key(|(i, _)| *i);
        // `from_parts` re-applies the dedupe + cap rules, so a hand-
        // edited file can't produce a row `add` would have refused.
        out.favorites = Favorites::from_parts(
            favorites.into_iter().map(|(_, f)| f).collect(),
            favorites_seeded,
        );
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
        RenderStyle::Outlines => "Outlines",
        RenderStyle::NonManifold => "NonManifold",
        RenderStyle::Polygons => "Polygons",
        RenderStyle::Overhang => "Overhang",
    }
}

fn render_style_from_token(s: &str) -> Option<RenderStyle> {
    match s {
        "Shaded" | "shaded" => Some(RenderStyle::Shaded),
        "Outlines" | "outlines" | "Outline" | "OutlineOnly" => Some(RenderStyle::Outlines),
        "NonManifold" | "nonmanifold" | "Non-Manifold" => Some(RenderStyle::NonManifold),
        "Polygons" | "polygons" => Some(RenderStyle::Polygons),
        "Overhang" | "overhang" | "Overhangs" => Some(RenderStyle::Overhang),
        // The deprecated software `Wireframe` mode maps to Polygons —
        // its full-edge overlay is the closest surviving view. Keeps
        // settings files written before the render-modes port loading.
        "Wireframe" | "wireframe" => Some(RenderStyle::Polygons),
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

// Tests live in `settings_tests.rs` so this file stays under the
// 800-line cap enforced by `atomartist-lib::tests::file_line_count`.
#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
