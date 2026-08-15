//! `BrowserProvider` — the browser-local backend for the `browser:` scheme.
//!
//! Module root. The work is split so that everything which does not need a
//! DOM can be compiled and tested on every platform:
//!
//! - [`paths`] — pure functions: URI → OPFS path segments, stamps, entry
//!   assembly, and the DOMException-name → [`StorageError`](crate::StorageError)
//!   table. No `web-sys`, so `cargo test` on the desktop covers it.
//! - `opfs` (wasm only) — the [`StorageProvider`](crate::StorageProvider)
//!   implementation itself, driving the asynchronous Origin Private File
//!   System through `web-sys` and resolving each [`Job`](crate::Job) from a
//!   `spawn_local` future.
//!
//! Counterpart to `local_fs.rs`: same contract, different persistence. The
//! browser has no worker here on purpose — `createSyncAccessHandle` is
//! worker-only, so the main thread uses the promise-based API and every
//! operation genuinely settles across frames, which is what the Phase 4 job
//! pump exists to absorb.

pub mod paths;

#[cfg(target_arch = "wasm32")]
mod opfs;

pub use paths::BROWSER_SCHEME;

#[cfg(target_arch = "wasm32")]
pub use opfs::BrowserProvider;
