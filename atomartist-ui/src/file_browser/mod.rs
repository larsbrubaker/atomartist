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
//! The thumbnail cache, the shared widget, and the modal host join it in
//! the following steps.

pub mod model;

pub use model::{BrowserModel, Crumb, Listing, ProviderRoot};
