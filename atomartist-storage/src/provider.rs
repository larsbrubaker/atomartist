//! The `StorageProvider` plug-in seam and its data types.
//!
//! A provider is the only thing in AtomArtist allowed to know how bytes are
//! actually persisted — `std::fs`, OPFS, IndexedDB, or an HTTP API. Everything
//! above it speaks [`StorageUri`] and [`Job`].
//!
//! The trait is object-safe on purpose (`Arc<dyn StorageProvider>` lives in
//! the [`StorageRegistry`](crate::StorageRegistry)) and has no `async fn`, so
//! no executor is imposed on the native shell. See
//! `docs/storage-architecture-plan.md` §3.2.

use serde::{Deserialize, Serialize};

use crate::job::Job;
use crate::uri::StorageUri;

/// Bytes read out of storage.
pub type Blob = Vec<u8>;
/// Bytes handed to storage.
pub type Bytes = Vec<u8>;
/// Wall-clock milliseconds since the Unix epoch — the plan's `SystemTimeish`.
/// `SystemTime` is not available on `wasm32-unknown-unknown`, and a `u64` is
/// what every backend can produce and every UI needs.
pub type ModifiedMs = u64;

/// Opaque version handle for a stored object: an ETag, a generation number,
/// an mtime hash — whatever the backend can compare cheaply. Only equality is
/// meaningful; never parse or order stamps.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stamp(String);

impl Stamp {
    pub fn new(value: impl Into<String>) -> Self {
        Stamp(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Stamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a provider can and cannot do. The UI greys out affordances from this
/// rather than discovering limits through failed operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub writable: bool,
    pub can_list: bool,
    pub can_create_dir: bool,
    /// Supports [`Precondition::IfMatch`] — required for safe multi-device
    /// editing of the same project.
    pub versioned: bool,
    pub max_blob_bytes: Option<u64>,
    pub requires_auth: bool,
}

impl Default for Capabilities {
    /// A fully capable, unauthenticated, unversioned local store.
    fn default() -> Self {
        Capabilities {
            writable: true,
            can_list: true,
            can_create_dir: true,
            versioned: false,
            max_blob_bytes: None,
            requires_auth: false,
        }
    }
}

/// One item in a directory listing, or the result of [`StorageProvider::stat`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub uri: StorageUri,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified: Option<ModifiedMs>,
    pub stamp: Option<Stamp>,
}

impl Entry {
    /// Directory entry with no size or stamp.
    pub fn dir(uri: StorageUri) -> Entry {
        let name = uri.file_name().unwrap_or("/").to_string();
        Entry {
            uri,
            name,
            is_dir: true,
            size: None,
            modified: None,
            stamp: None,
        }
    }

    /// File entry, naming itself from the URI's last segment.
    pub fn file(uri: StorageUri, size: u64, stamp: Stamp) -> Entry {
        let name = uri.file_name().unwrap_or("").to_string();
        Entry {
            uri,
            name,
            is_dir: false,
            size: Some(size),
            modified: None,
            stamp: Some(stamp),
        }
    }
}

/// Guard applied to a write so concurrent editors cannot clobber each other.
///
/// A provider that reports `versioned: false` in its [`Capabilities`] and is
/// handed `IfMatch` or `IfAbsent` **must** fail with
/// [`StorageError::Unsupported`](crate::StorageError::Unsupported). Silently
/// ignoring a precondition turns a safety mechanism into a data-loss bug: the
/// caller believes the write was guarded when it was not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Precondition {
    /// Overwrite unconditionally.
    #[default]
    None,
    /// Write only if the stored stamp still equals this one. Violations fail
    /// with `Conflict { expected: Some(stamp), actual }`, where `actual` is
    /// `None` when nothing is stored *or* when the backend cannot report the
    /// current stamp.
    IfMatch(Stamp),
    /// Write only if nothing exists at the target ("save as new file").
    /// Violations fail with `Conflict { expected: None, actual }`.
    IfAbsent,
}

/// Placeholder for providers that would rather show the OS file dialog than
/// the in-app browser (native local storage). Fleshed out in Phase 6, when
/// the file-browser widget and the `Job<Option<StorageUri>>` dialog API land;
/// it exists now only so [`StorageProvider::native_picker`] has the shape the
/// plan calls for.
pub trait NativePicker: Send + Sync {
    /// Human-readable label for the button that opens the OS dialog.
    fn label(&self) -> &str;
}

/// The storage plug-in seam. One implementation per scheme.
pub trait StorageProvider: Send + Sync {
    /// URI scheme this provider owns, e.g. `"file"` or `"browser"`.
    fn scheme(&self) -> &str;

    /// Name shown in the provider sidebar — "This PC", "This Browser".
    fn display_name(&self) -> &str;

    fn capabilities(&self) -> Capabilities;

    /// Entries directly inside `dir`, in provider order. Each URI appears at
    /// most once — a path is either a file or a directory, never both.
    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>>;

    fn read(&self, at: &StorageUri) -> Job<Blob>;

    /// Store `bytes`, returning the new [`Stamp`]. Fails with
    /// [`StorageError::Conflict`](crate::StorageError::Conflict) when `pre`
    /// is not satisfied.
    ///
    /// If any ancestor of `at` is an existing *file*, the write must fail
    /// with [`StorageError::Io`](crate::StorageError::Io) and change nothing
    /// — a file may not become a directory by implication.
    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp>;

    fn delete(&self, at: &StorageUri) -> Job<()>;

    /// Metadata for `at`, or `Ok(None)` when it does not exist. Note the
    /// distinction from [`read`](Self::read), which fails with `NotFound`.
    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>>;

    /// Create `at` and any missing ancestors. Idempotent when the directory
    /// already exists; fails with [`StorageError::Io`](crate::StorageError::Io)
    /// when `at` or any ancestor is an existing file.
    fn create_dir(&self, at: &StorageUri) -> Job<()>;

    /// Providers that prefer the OS picker answer `Some`; cloud providers
    /// return `None` and get the in-app browser.
    fn native_picker(&self) -> Option<&dyn NativePicker> {
        None
    }
}
