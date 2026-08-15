//! `BrowserProvider` — OPFS-backed storage for the `browser:` scheme.
//!
//! The wasm counterpart to `local_fs.rs`: the one place in AtomArtist
//! allowed to touch the Origin Private File System. A URI maps to OPFS path
//! segments (`browser:///projects/bracket.atmr` → `["projects",
//! "bracket.atmr"]`) and every operation runs as a future on the browser
//! event loop, resolving its [`Job`] from `spawn_local`. Nothing here
//! blocks, because on the main thread nothing can.
//!
//! Deliberately the promise-based API only: `createSyncAccessHandle` is
//! worker-only, and adding a worker would mean shipping a second wasm
//! module and a message protocol for no gain at project-file sizes. Jobs
//! therefore settle across frames — the Phase 4 job pump is what makes that
//! invisible to call sites.
//!
//! Behaviour matches [`MemoryProvider`](crate::MemoryProvider) as seen by
//! the conformance suite: writes create missing parent directories, an
//! existing *file* ancestor blocks nested writes and `create_dir`, `delete`
//! refuses non-empty directories, and `stat` reports `Ok(None)` where
//! `read` fails with `NotFound`.
//!
//! ## Known windows (v1, documented rather than closed)
//!
//! - **Unversioned.** OPFS exposes no ETag or generation counter, only
//!   `File.lastModified` + `size`. That is a real stamp for change
//!   *detection* but not a compare-and-swap handle, so
//!   [`Capabilities::versioned`](crate::Capabilities) is `false` and any
//!   [`Precondition`] other than `None` is refused with
//!   [`StorageError::Unsupported`] rather than checked with a race in it.
//! - **A write is not atomic.** `createWritable` truncates the existing
//!   file before the new bytes land (OPFS's default is a swap file in
//!   Chrome but the spec does not promise one), so a tab closed mid-write
//!   can leave a short file. There is no rename primitive to build the
//!   `local_fs` write-then-rename dance out of.
//! - **Cancellation is requested, never observed.** `spawn_local` hands the
//!   future no [`JobCompleter`](crate::JobCompleter), so nothing here can
//!   check `is_cancelled`. Cancelling a write settles the *job* as
//!   [`StorageError::Cancelled`] while the OPFS operation runs on to
//!   `close()` and replaces the file anyway. This matches `spawn_blocking`
//!   on native (a cancelled `std::fs::write` also finishes), but a caller
//!   must not read "Cancelled" as "nothing was stored". Closing it means
//!   threading the completer into every operation and checking it between
//!   awaits — worth doing when an operation is long enough for a user to
//!   want out of it, which a project-file write is not.
//! - **Bytes are copied to the JS heap before every write.** A
//!   `Uint8Array` *view* over wasm linear memory detaches when the heap
//!   grows, and the writable stream may consume the chunk in a later
//!   microtask; `do_write` therefore builds an owned JS buffer. That is one
//!   extra copy of the file per save, paid deliberately.

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    File, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions,
    FileSystemGetFileOptions, FileSystemHandle, FileSystemHandleKind,
    FileSystemWritableFileStream,
};

use crate::browser::paths::{self, BROWSER_SCHEME};
use crate::error::{StorageError, StorageResult};
use crate::job::{spawn_local, Job};
use crate::provider::{
    Blob, Bytes, Capabilities, Entry, Precondition, Stamp, StorageProvider,
};
use crate::uri::StorageUri;

/// Browser-local [`StorageProvider`], persisting to OPFS.
pub struct BrowserProvider {
    display_name: String,
}

impl Default for BrowserProvider {
    fn default() -> Self {
        BrowserProvider::new()
    }
}

impl BrowserProvider {
    /// Provider labelled "This Browser" — the sidebar entry the plan
    /// specifies (`docs/storage-architecture-plan.md` §7).
    pub fn new() -> Self {
        BrowserProvider::with_display_name("This Browser")
    }

    pub fn with_display_name(display_name: impl Into<String>) -> Self {
        BrowserProvider {
            display_name: display_name.into(),
        }
    }

    /// Root URI of this provider, the natural starting point for listings.
    pub fn root(&self) -> StorageUri {
        StorageUri::new(BROWSER_SCHEME, "/")
    }
}

impl StorageProvider for BrowserProvider {
    fn scheme(&self) -> &str {
        BROWSER_SCHEME
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: true,
            can_list: true,
            can_create_dir: true,
            // See the module header: `lastModified` + size is not a
            // version handle, and pretending otherwise would turn a safety
            // mechanism into a data-loss bug.
            versioned: false,
            max_blob_bytes: None,
            requires_auth: false,
        }
    }

    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>> {
        let dir = dir.clone();
        spawn_local(async move { do_list(dir).await })
    }

    fn read(&self, at: &StorageUri) -> Job<Blob> {
        let at = at.clone();
        spawn_local(async move { do_read(at).await })
    }

    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp> {
        let at = at.clone();
        spawn_local(async move { do_write(at, bytes, pre).await })
    }

    fn delete(&self, at: &StorageUri) -> Job<()> {
        let at = at.clone();
        spawn_local(async move { do_delete(at).await })
    }

    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>> {
        let at = at.clone();
        spawn_local(async move { do_stat(at).await })
    }

    fn create_dir(&self, at: &StorageUri) -> Job<()> {
        let at = at.clone();
        spawn_local(async move { do_create_dir(at).await })
    }
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

async fn do_list(dir: StorageUri) -> StorageResult<Vec<Entry>> {
    let segments = paths::segments(&dir)?;
    let handle = directory(&dir, &segments, false, "list").await?;

    let mut out = Vec::new();
    let iterator = handle.values();
    loop {
        let next = iterator
            .next()
            .map_err(|err| failure(err).into_error("list", &dir))?;
        let step = JsFuture::from(next)
            .await
            .map_err(|err| failure(err).into_error("list", &dir))?
            .unchecked_into::<js_sys::IteratorNext>();
        if step.done() {
            break;
        }
        let child = step.value().unchecked_into::<FileSystemHandle>();
        let name = child.name();
        // The name comes from a JS API, not from our own code: it can hold
        // anything OPFS accepts, including a backslash, which the URI layer
        // reads as a path separator. `try_join` refuses what it cannot
        // represent and the entry is skipped — one odd name must not abort
        // the whole listing.
        let Ok(uri) = dir.try_join(&name) else {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "browser storage: skipping unrepresentable entry name `{name}` in `{dir}`"
            )));
            continue;
        };
        // `FileSystemHandleKind` is `#[non_exhaustive]`: a kind we do not
        // know about is not silently treated as a file (which would mean
        // calling `getFile()` on it and failing the whole listing).
        match child.kind() {
            FileSystemHandleKind::Directory => out.push(paths::dir_entry(uri)),
            FileSystemHandleKind::File => {
                let file = file_of(&child.unchecked_into::<FileSystemFileHandle>(), &uri, "list")
                    .await?;
                out.push(paths::file_entry(uri, file.size(), file.last_modified()));
            }
            other => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "browser storage: skipping entry `{name}` in `{dir}` of unknown kind {other:?}"
                )));
            }
        }
    }
    Ok(out)
}

async fn do_read(at: StorageUri) -> StorageResult<Blob> {
    let segments = paths::segments(&at)?;
    let Some(name) = paths::leaf(&segments) else {
        // The root is a directory; `MemoryProvider` has no bytes there.
        return Err(StorageError::NotFound);
    };
    let parent = directory(&at, paths::parent_segments(&segments), false, "read").await?;

    let handle = JsFuture::from(parent.get_file_handle(name))
        .await
        .map_err(|err| {
            let failure = failure(err);
            // Reading a directory is `NotFound`, matching every other
            // provider (see `LocalFsProvider::do_read`).
            if failure.is_absent() {
                StorageError::NotFound
            } else {
                failure.into_error("read", &at)
            }
        })?
        .unchecked_into::<FileSystemFileHandle>();

    let file = file_of(&handle, &at, "read").await?;
    let buffer = JsFuture::from(file.array_buffer())
        .await
        .map_err(|err| failure(err).into_error("read", &at))?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

async fn do_write(at: StorageUri, bytes: Bytes, pre: Precondition) -> StorageResult<Stamp> {
    let segments = paths::segments(&at)?;
    let Some(name) = paths::leaf(&segments) else {
        return Err(StorageError::Io(format!(
            "cannot write `{at}`: it is the provider root"
        )));
    };
    // Refused before anything is touched: an unversioned provider handed a
    // precondition must fail, never silently ignore it.
    if pre != Precondition::None {
        return Err(StorageError::Unsupported);
    }

    // `create: true` per segment, so a save into a fresh browser works
    // without a mkdir step — the same "materialize missing ancestors"
    // behaviour `MemoryProvider` and `LocalFsProvider` have. A segment that
    // is an existing *file* fails here with `TypeMismatchError`, which maps
    // to `Io`: a file may not become a directory by implication.
    let parent = directory(&at, paths::parent_segments(&segments), true, "write to").await?;

    let options = FileSystemGetFileOptions::new();
    options.set_create(true);
    let handle = JsFuture::from(parent.get_file_handle_with_options(name, &options))
        .await
        .map_err(|err| failure(err).into_error("write to", &at))?
        .unchecked_into::<FileSystemFileHandle>();

    let writable = JsFuture::from(handle.create_writable())
        .await
        .map_err(|err| failure(err).into_error("write to", &at))?
        .unchecked_into::<FileSystemWritableFileStream>();
    // The bytes are copied onto the JS heap *before* the promise is created.
    // `write_with_u8_array` would hand `write()` a `Uint8Array` view over
    // wasm linear memory, which detaches the moment the wasm heap grows —
    // and the stream is allowed to copy the chunk in a later microtask, with
    // allocating Rust code (`JsFuture::from`, the error path) running in
    // between. A detached view is a thrown error or, worse, wrong bytes on
    // disk. `js_sys::Uint8Array::from` allocates a JS-owned buffer instead.
    let chunk = js_sys::Uint8Array::from(&bytes[..]);
    let write = writable
        .write_with_buffer_source(&chunk)
        .map_err(|err| failure(err).into_error("write to", &at))?;
    JsFuture::from(write)
        .await
        .map_err(|err| failure(err).into_error("write to", &at))?;
    // The bytes are only durable once the stream closes.
    JsFuture::from(writable.close())
        .await
        .map_err(|err| failure(err).into_error("write to", &at))?;

    let file = file_of(&handle, &at, "write to").await?;
    Ok(paths::stamp_for(
        file.last_modified(),
        paths::size_bytes(file.size()),
    ))
}

async fn do_delete(at: StorageUri) -> StorageResult<()> {
    let segments = paths::segments(&at)?;
    let Some(name) = paths::leaf(&segments) else {
        return Err(StorageError::Io(format!(
            "cannot delete `{at}`: it is the provider root"
        )));
    };
    let parent = directory(&at, paths::parent_segments(&segments), false, "delete").await?;
    // No `recursive`: a non-empty directory fails with
    // `InvalidModificationError` → `Io`, matching `MemoryProvider`.
    // Recursive deletion is destructive enough that the UI must ask.
    JsFuture::from(parent.remove_entry(name))
        .await
        .map_err(|err| failure(err).into_error("delete", &at))?;
    Ok(())
}

async fn do_stat(at: StorageUri) -> StorageResult<Option<Entry>> {
    let segments = paths::segments(&at)?;
    let Some(name) = paths::leaf(&segments) else {
        // The root always exists.
        return Ok(Some(paths::dir_entry(at)));
    };
    let parent = match directory(&at, paths::parent_segments(&segments), false, "stat").await {
        Ok(parent) => parent,
        // Nothing above it exists (or an ancestor is a file), so nothing is
        // stored here — `Ok(None)`, not an error.
        Err(StorageError::NotFound) => return Ok(None),
        Err(err) => return Err(err),
    };

    match JsFuture::from(parent.get_file_handle(name)).await {
        Ok(handle) => {
            let handle = handle.unchecked_into::<FileSystemFileHandle>();
            let file = file_of(&handle, &at, "stat").await?;
            Ok(Some(paths::file_entry(
                at,
                file.size(),
                file.last_modified(),
            )))
        }
        Err(err) => {
            let file_failure = failure(err);
            if !file_failure.is_absent() {
                return Err(file_failure.into_error("stat", &at));
            }
            // Absent *as a file* — it may still be a directory.
            match JsFuture::from(parent.get_directory_handle(name)).await {
                Ok(_) => Ok(Some(paths::dir_entry(at))),
                Err(err) => {
                    let dir_failure = failure(err);
                    if dir_failure.is_absent() {
                        Ok(None)
                    } else {
                        Err(dir_failure.into_error("stat", &at))
                    }
                }
            }
        }
    }
}

async fn do_create_dir(at: StorageUri) -> StorageResult<()> {
    let segments = paths::segments(&at)?;
    // Idempotent, and deep paths are created in one call: `directory` walks
    // every segment with `create: true`.
    directory(&at, &segments, true, "create directory").await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// OPFS plumbing
// ---------------------------------------------------------------------------

/// Handle for the directory named by `segments`, relative to the origin's
/// private root.
///
/// With `create` false a missing (or wrong-kind) segment yields
/// [`StorageError::NotFound`], which is what "there is no directory here"
/// means to every other provider. With `create` true, missing segments are
/// created and a segment that is a *file* fails with `Io`.
async fn directory(
    at: &StorageUri,
    segments: &[&str],
    create: bool,
    action: &str,
) -> StorageResult<FileSystemDirectoryHandle> {
    let mut handle = opfs_root(at, action).await?;
    for segment in segments {
        let promise = if create {
            let options = FileSystemGetDirectoryOptions::new();
            options.set_create(true);
            handle.get_directory_handle_with_options(segment, &options)
        } else {
            handle.get_directory_handle(segment)
        };
        handle = match JsFuture::from(promise).await {
            Ok(child) => child.unchecked_into::<FileSystemDirectoryHandle>(),
            Err(err) => {
                let failure = failure(err);
                if !create && failure.is_absent() {
                    return Err(StorageError::NotFound);
                }
                return Err(failure.into_error(action, at));
            }
        };
    }
    Ok(handle)
}

/// `navigator.storage.getDirectory()` — the origin's private root.
async fn opfs_root(at: &StorageUri, action: &str) -> StorageResult<FileSystemDirectoryHandle> {
    let navigator = web_sys::window()
        .map(|window| window.navigator())
        .ok_or_else(|| {
            StorageError::Io(format!(
                "failed to {action} `{at}`: no browser window to reach storage through"
            ))
        })?;
    let root = JsFuture::from(navigator.storage().get_directory())
        .await
        .map_err(|err| failure(err).into_error(action, at))?;
    Ok(root.unchecked_into::<FileSystemDirectoryHandle>())
}

/// Snapshot of a file handle: OPFS reports size and `lastModified` only
/// through a `File`, so metadata costs one more await.
async fn file_of(
    handle: &FileSystemFileHandle,
    at: &StorageUri,
    action: &str,
) -> StorageResult<File> {
    let file = JsFuture::from(handle.get_file())
        .await
        .map_err(|err| failure(err).into_error(action, at))?;
    Ok(file.unchecked_into::<File>())
}

/// A rejected promise, reduced to the two fields that classify it.
struct JsFailure {
    name: String,
    message: String,
}

impl JsFailure {
    fn is_absent(&self) -> bool {
        paths::is_absent(&self.name)
    }

    fn into_error(self, action: &str, at: &StorageUri) -> StorageError {
        paths::error_for(&self.name, &self.message, action, at)
    }
}

/// Read `name` / `message` off a thrown value. Both are looked up
/// reflectively rather than by casting to `DomException`, because a
/// rejection can be any JS value at all — a string, a plain object, or
/// `undefined` — and losing the error entirely would be worse than
/// reporting it as a generic `Io`.
fn failure(err: JsValue) -> JsFailure {
    let field = |key: &str| {
        js_sys::Reflect::get(&err, &JsValue::from_str(key))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default()
    };
    let name = field("name");
    let mut message = field("message");
    if name.is_empty() && message.is_empty() {
        // Not an Error-shaped value; stringify whatever it is.
        message = match js_sys::JSON::stringify(&err) {
            Ok(text) => String::from(text),
            Err(_) => "unknown error".to_string(),
        };
    }
    JsFailure { name, message }
}
