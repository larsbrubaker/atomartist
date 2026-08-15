//! AtomArtist pluggable storage.
//!
//! This crate owns the seam between "the app has some bytes for a project"
//! and "those bytes live somewhere": a local disk, browser storage (OPFS /
//! IndexedDB), or a remote service. Nothing above this crate knows which.
//!
//! The pieces, in dependency order:
//!
//! - [`StorageUri`] (`uri.rs`) — the identity of a project or asset,
//!   replacing `PathBuf` everywhere outside a provider.
//! - [`Job`] (`job.rs`) — a pollable slot holding work in flight, so no
//!   async runtime is imposed and wasm (which cannot block) uses the same
//!   call sites as native. [`set_completion_hook`] (`completion_hook.rs`)
//!   is how a job settling on a worker thread wakes a host that sleeps
//!   between frames instead of polling.
//! - [`StorageProvider`] (`provider.rs`) — the object-safe plug-in trait,
//!   plus [`Capabilities`], [`Entry`], [`Precondition`], and [`Stamp`].
//! - [`StorageRegistry`] (`registry.rs`) — scheme -> provider lookup, in the
//!   spirit of `atomartist-lib`'s node registry.
//! - [`MemoryProvider`] (`memory.rs`) and [`FlakyProvider`] (`flaky.rs`) —
//!   the in-process reference backend and its fault-injecting wrapper.
//! - `LocalFsProvider` (`local_fs.rs`, native only) — the `file:` scheme, and
//!   the one place in the app allowed to touch `std::fs` for storage.
//! - `BrowserProvider` (`browser/`, wasm only) — the `browser:` scheme,
//!   persisting to the Origin Private File System. Its pure path / error
//!   logic (`browser/paths.rs`) compiles everywhere and is unit-tested
//!   natively; only the OPFS plumbing needs a browser.
//! - [`conformance`] — the suite every provider (including third-party ones)
//!   must pass.
//!
//! Deliberately absent everywhere else: `std::fs`, HTTP, and any GUI
//! dependency. `local_fs.rs` (Phase 3) and the browser provider (Phase 5) are
//! the only places allowed to touch platform IO.
//!
//! See `docs/storage-architecture-plan.md` for the full design.

mod browser;
mod completion_hook;
pub mod conformance;
mod error;
mod flaky;
mod job;
#[cfg(not(target_arch = "wasm32"))]
mod local_fs;
mod memory;
mod provider;
mod registry;
mod uri;

pub use browser::BROWSER_SCHEME;
pub use completion_hook::{clear_completion_hook, set_completion_hook};
pub use error::{StorageError, StorageResult};
pub use flaky::{FlakyConfig, FlakyProvider};
pub use job::{Job, JobCompleter, JobState};
pub use memory::MemoryProvider;
pub use provider::{
    Blob, Bytes, Capabilities, Entry, ModifiedMs, NativePicker, Precondition, Stamp,
    StorageProvider,
};
pub use registry::{DuplicateScheme, StorageRegistry};
pub use uri::{StorageUri, UriParseError, FILE_SCHEME};

#[cfg(not(target_arch = "wasm32"))]
pub use job::spawn_blocking;
#[cfg(target_arch = "wasm32")]
pub use browser::BrowserProvider;
#[cfg(target_arch = "wasm32")]
pub use job::spawn_local;
#[cfg(not(target_arch = "wasm32"))]
pub use local_fs::LocalFsProvider;
