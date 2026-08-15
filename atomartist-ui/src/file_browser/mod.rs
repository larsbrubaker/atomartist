//! The in-app file browser (`docs/file-browser-design.md`, step 6b).
//!
//! One browsing component with three faces — Open/Save modal, favorites
//! bar, drag-drop source — built on a widget-free core so navigation,
//! listing states, and stale-response handling can be tested without a
//! window or a GPU (design §6).
//!
//! Contents, in the order the design's §4 lists them:
//!
//! - [`model`] — [`BrowserModel`]: provider roots, current directory,
//!   [`Listing`] state, generation guards, selection, search filter. It
//!   drives [`atomartist_storage::StorageProvider::list`] jobs through the
//!   Phase 4 pump (`crate::storage_ops`) and owns no widgets.
//!
//! - [`thumbs`] — [`ThumbnailCache`]: the visibility-gated preview store
//!   behind the grid. Keyed `(uri, stamp, size, `[`CACHE_VERSION`]`)`,
//!   bounded by a byte budget with LRU eviction, and driven through the
//!   same Phase 4 pump. Also widget-free: it reports a [`ThumbState`] and
//!   leaves the glyph fallback to the widget.
//!
//! - [`widget`] — [`FileBrowser`]: the shared widget both faces embed. It
//!   paints a [`BrowserModel`] (sidebar, breadcrumbs, grid, search, and —
//!   in [`BrowserMode::Save`] — a name field), turns clicks into model
//!   calls, and runs the [`ThumbnailCache`]'s visibility round once per
//!   layout. Geometry and painting live beside it in `widget_geom` /
//!   `widget_paint` so no file approaches the 800-line cap.
//!
//! - [`modal`] — [`FileBrowserModalHost`] plus its
//!   [`FileBrowserModalHandle`]: the Open/Save dialog (step 6c-1). The
//!   host lives at the top of the app's root `Stack` and is empty until
//!   someone calls [`FileBrowserModalHandle::open`], which hands back the
//!   [`atomartist_storage::Job`] the pick settles into. `modal_panel`
//!   holds the chrome (title, OK / Cancel) so neither file grows past the
//!   line cap.
//!
//! - [`dialogs`] — [`ModalFileDialogs`]: the
//!   [`FileDialogProvider`](crate::top_menu_bar::FileDialogProvider) that
//!   answers every File-menu pick through that modal (step 6c-2). The web
//!   shell's only picker; a native shell uses it for anything `rfd`
//!   cannot address.
//!
//! The favorites bar (6d) embeds the widget the same way.

pub mod dialogs;
pub mod modal;
pub mod modal_panel;
pub mod model;
pub mod thumbs;
pub mod widget;
pub mod widget_geom;
mod widget_paint;

pub use dialogs::ModalFileDialogs;
pub use modal::{
    resolve_pick, FileBrowserModalHandle, FileBrowserModalHost, PANEL_SIZE, PROJECT_EXTENSION,
};
pub use modal_panel::{FileBrowserModal, ModalLayout};
pub use model::{BrowserModel, Crumb, Listing, ProviderRoot};
pub use thumbs::{
    can_have_thumbnail, ThumbKey, ThumbState, ThumbnailCache, ThumbnailImage, CACHE_VERSION,
    DEFAULT_BYTE_BUDGET, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_IN_FLIGHT, DEFAULT_THUMB_SIZE,
};
pub use widget::{BrowserMode, FileBrowser};
