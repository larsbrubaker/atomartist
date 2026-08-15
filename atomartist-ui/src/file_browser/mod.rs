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
//! The shared widget and the modal host join them in the following steps.

pub mod model;
pub mod thumbs;

pub use model::{BrowserModel, Crumb, Listing, ProviderRoot};
pub use thumbs::{
    can_have_thumbnail, ThumbKey, ThumbState, ThumbnailCache, ThumbnailImage, CACHE_VERSION,
    DEFAULT_BYTE_BUDGET, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_IN_FLIGHT, DEFAULT_THUMB_SIZE,
};
