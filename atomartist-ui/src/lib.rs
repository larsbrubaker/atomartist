//! AtomArtist shared widget tree.
//!
//! Contains the platform-agnostic UI: node canvas widget, property panel,
//! menu bar / toolbar, application state, and the top-level layout that
//! `demo-native` (winit shell) and `demo-wasm` (wasm-bindgen shell) both
//! mount. No platform code lives here.

pub mod app_state;
pub mod app_state_drill;
pub mod app_state_files;
pub mod app_state_files_import;
pub mod app_state_model;
pub mod app_state_storage;
pub mod breadcrumb_bar;
pub mod circular_dropdown;
pub mod circular_icon_button;
pub mod debug_windows;
pub mod fa;
pub mod favorites_bar;
pub mod favorites_bar_geom;
pub mod favorites_bar_host;
mod favorites_bar_paint;
pub mod file_browser;
pub mod first_paint;
pub mod floating_overlay;
pub mod icons;
pub mod mattercad_icons;
pub mod menu_actions;
pub mod node_helpers;
pub mod settings;
pub mod shell_init;
pub mod status_bar;
pub mod storage_ops;
pub mod thumbnail;
pub mod top_level;
pub mod top_menu_bar;
pub mod viewport_overlay;

pub use app_state::AppState;
pub use app_state_files::MeshExportFormat;
pub use app_state_model::{shared_model_for, AppStateModel};
pub use app_state_storage::{display_uri, uri_file_stem};
pub use debug_windows::{build_debug_windows, DebugWindowHandles};
pub use favorites_bar::{FavoritesBar, BAR_ID as FAVORITES_BAR_ID, EMBEDDED_BROWSER_ID};
pub use first_paint::FirstPaintGate;
pub use floating_overlay::{FloatingOverlayHandle, FloatingOverlayHost};
pub use node_helpers::add_node_with_defaults;
pub use settings::{
    DebugWindowState, DebugWindowsState, MainWindowState, UiSettings, WindowPlacement,
};
pub use shell_init::install_theme_and_fonts;
pub use storage_ops::{JobOp, Notice, NoticeLevel, PendingOp};
pub use thumbnail::{
    framebuffer_crop_from_widget_rect, thumbnail_png_from_exact_rgba, thumbnail_png_from_rgba,
    thumbnail_png_from_rgba_region, thumbnail_source_region, viewport_framebuffer_crop, CropRect,
    THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, VIEWPORT_WIDGET_ID,
};
pub use top_level::{
    build_app, fresh_state_with_builtins, fresh_state_with_builtins_and_storage,
    fresh_state_with_starter_graph, fresh_state_with_starter_graph_and_storage,
};
pub use viewport_overlay::{build_viewport_overlay, ViewportOverlay};

/// Phase 0 placeholder kept so `demo-native` / `demo-wasm` stubs continue to
/// compile while real wiring lands. Removed in Phase 6.
pub fn placeholder() {}
