//! Unit tests for [`crate::settings`].
//!
//! Split out of `settings.rs` to keep both files comfortably under the
//! 800-line cap enforced by `atomartist-lib::tests::file_line_count`.
//! The module is re-attached via `#[path]` from `settings.rs`, so the
//! tests still see the parent module's private items via `use super::*`.

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
        theme: ThemePreference::Dark,
        accent_color: AccentColor::Purple,
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
        (0, 0, 1920u32, 1080u32),    // primary
        (1920, 0, 1920u32, 1080u32), // secondary on the right
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
fn placement_default_when_no_saved_geometry() {
    // First launch: zero-size sentinel → shell uses defaults.
    // Maximized flag still propagates so somebody who hand-edits
    // the settings to request maximized gets it.
    let monitors = vec![(0, 0, 1920u32, 1080u32)];
    let win = MainWindowState::default();
    assert_eq!(
        win.placement(monitors.clone()),
        WindowPlacement::Default { maximized: false }
    );

    let win = MainWindowState {
        maximized: true,
        ..MainWindowState::default()
    };
    assert_eq!(
        win.placement(monitors),
        WindowPlacement::Default { maximized: true }
    );
}

#[test]
fn placement_restore_when_fully_on_screen() {
    let win = MainWindowState {
        x: 200,
        y: 200,
        width: 1280,
        height: 720,
        maximized: false,
    };
    let monitors = vec![(0, 0, 1920u32, 1080u32)];
    assert_eq!(
        win.placement(monitors),
        WindowPlacement::Restore { bounds: win }
    );
}

#[test]
fn placement_preserves_maximized_when_position_needs_recenter() {
    // Regression: this is the exact scenario the user hit after
    // closing the app while maximized on Windows. The OS-reported
    // outer position (-8, -8) for a maximized window fails the
    // `fits_on_monitors` overlap check (only 32 px of the title
    // strip lands on-screen, less than `MIN_OVERLAP_HEIGHT` of
    // 40 px), so we recenter — but the maximized flag must
    // survive into the `Recenter` arm.
    let win = MainWindowState {
        x: -8,
        y: -8,
        width: 1280,
        height: 720,
        maximized: true,
    };
    let monitors = vec![(0, 0, 1920u32, 1080u32)];
    assert_eq!(
        win.placement(monitors),
        WindowPlacement::Recenter {
            width: 1280,
            height: 720,
            maximized: true,
        }
    );
}

#[test]
fn placement_recenter_keeps_size_when_monitor_disconnected() {
    // Window saved on a now-detached secondary monitor — keep the
    // user's size preference, just pick a new on-screen position.
    let win = MainWindowState {
        x: 2500,
        y: 400,
        width: 1600,
        height: 900,
        maximized: false,
    };
    let monitors = vec![(0, 0, 1920u32, 1080u32)];
    assert_eq!(
        win.placement(monitors),
        WindowPlacement::Recenter {
            width: 1600,
            height: 900,
            maximized: false,
        }
    );
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
