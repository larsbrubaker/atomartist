//! AtomArtist shared widget tree.
//!
//! Contains the platform-agnostic UI: node canvas widget, property panel,
//! menu bar / toolbar, application state, and the top-level layout that
//! `demo-native` (winit shell) and `demo-wasm` (wasm-bindgen shell) both
//! mount. No platform code lives here.

pub mod app_state;
pub mod app_state_model;
pub mod circular_dropdown;
pub mod circular_icon_button;
pub mod debug_windows;
pub mod icons;
pub mod mattercad_icons;
pub mod node_helpers;
pub mod settings;
pub mod status_bar;
pub mod top_level;
pub mod top_menu_bar;
pub mod viewport_overlay;

pub use app_state::AppState;
pub use app_state_model::{shared_model_for, AppStateModel};
pub use debug_windows::{build_debug_windows, DebugWindowHandles};
pub use node_helpers::add_node_with_defaults;
pub use settings::{DebugWindowState, DebugWindowsState, UiSettings};
pub use top_level::{build_app, fresh_state_with_builtins, fresh_state_with_starter_graph};
pub use viewport_overlay::{build_viewport_overlay, ViewportOverlay};

/// Phase 0 placeholder kept so `demo-native` / `demo-wasm` stubs continue to
/// compile while real wiring lands. Removed in Phase 6.
pub fn placeholder() {}
