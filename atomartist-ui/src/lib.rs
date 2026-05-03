//! AtomArtist shared widget tree.
//!
//! Contains the platform-agnostic UI: node canvas widget, property panel,
//! menu bar / toolbar, application state, and the top-level layout that
//! `demo-native` (winit shell) and `demo-wasm` (wasm-bindgen shell) both
//! mount. No platform code lives here.
//!
//! Phase 0 stub. Real implementation begins in Phase 3.

/// Crate-level placeholder so `cargo check` has something to type-check.
/// Removed when Phase 3 adds the canvas_widget + app_state modules.
pub fn placeholder() {}
