//! AtomArtist shared widget tree.
//!
//! Contains the platform-agnostic UI: node canvas widget, property panel,
//! menu bar / toolbar, application state, and the top-level layout that
//! `demo-native` (winit shell) and `demo-wasm` (wasm-bindgen shell) both
//! mount. No platform code lives here.

pub mod app_state;
pub mod canvas_draw;
pub mod canvas_widget;
pub mod top_level;

pub use app_state::AppState;
pub use canvas_widget::{add_node_with_defaults, NodeCanvas};
pub use top_level::{build_app, fresh_state_with_builtins};

/// Phase 0 placeholder kept so `demo-native` / `demo-wasm` stubs continue to
/// compile while real wiring lands. Removed in Phase 6.
pub fn placeholder() {}
