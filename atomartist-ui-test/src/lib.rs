//! UI test harness for AtomArtist.
//!
//! Builds the real production widget tree (`atomartist_ui::build_app`)
//! around a fresh `AppState` without ever opening a window or touching the
//! GPU. Tests drive the tree via synthetic event helpers that wrap
//! `agg-gui`'s `App::on_mouse_*` / `on_key_*`, then assert on the live
//! `AppState` (the same object the production widgets mutate) and on the
//! widget tree itself via agg-gui's reflection plumbing
//! (`find_widget_by_id`, `find_widget_by_type`).
//!
//! Why this shape:
//!   - **Real production code** — we test the actual widgets, not a
//!     mock-up. The CLAUDE.md mandate is "tests must test actual
//!     production code, not copies."
//!   - **No GPU / no window** — `paint()` is never called, so the harness
//!     runs in any CI environment, on any platform, in microseconds per
//!     event.
//!   - **Re-layout per event** — bounds drift if you skip layout, so each
//!     event helper calls `app.layout(size)` afterward (same shape as
//!     agg-gui's reflection-driven inspector tests).
//!
//! Example:
//! ```ignore
//! use atomartist_ui_test::TestHarness;
//! use agg_gui::MouseButton;
//!
//! let mut h = TestHarness::with_starter_graph();
//! h.click(640.0, 360.0, MouseButton::Left);
//! assert!(h.state().selection.lock().unwrap().is_some());
//! ```

pub mod harness;

pub use harness::TestHarness;
