//! `LocalFsProvider` — the native filesystem backend for the `file:` scheme.
//!
//! This module is **the one place in AtomArtist allowed to touch `std::fs`
//! for project storage** (`docs/storage-architecture-plan.md` §3.1). Every
//! operation maps a [`StorageUri`] to a `PathBuf` with
//! [`StorageUri::to_local_path`] and completes synchronously via
//! [`Job::from_result`], so the desktop path keeps the zero-latency feel the
//! plan asks for (§3.3). A worker pool arrives with the remote providers, not
//! here.
//!
//! Behaviour is deliberately identical to [`MemoryProvider`](crate::MemoryProvider)
//! from the conformance suite's point of view: writes create missing parent
//! directories, an existing *file* ancestor blocks nested writes and
//! `create_dir`, `delete` refuses non-empty directories, and `stat` reports
//! `Ok(None)` where `read` fails with `NotFound`.
//!
//! ## Known windows (v1, documented rather than closed)
//!
//! - **`IfMatch` is stat-then-write.** A local filesystem offers no
//!   compare-and-swap, so another process can replace the file between the
//!   check and the rename. The guard catches this app's stale editors and
//!   multi-device sync, not a hostile racer.
//! - **`IfAbsent` briefly publishes a zero-byte file.** The exclusive
//!   `create_new` that reserves the name lands before the bytes do, so a
//!   concurrent reader can see `Ok(vec![])` and `stat`/`list` can report a
//!   0-byte entry for the width of one write. A concurrent `IfAbsent` that
//!   loses the race therefore reports an `actual` stamp for that placeholder
//!   — a version that will never be observable again. Callers must treat a
//!   conflict's `actual` as a hint for the conflict dialog, never as a stamp
//!   to write against.
//!
//! Compiled out on `wasm32` — the browser gets `BrowserProvider` in Phase 5.

use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use crate::error::{StorageError, StorageResult};
use crate::job::Job;
use crate::provider::{
    Blob, Bytes, Capabilities, Entry, ModifiedMs, Precondition, Stamp, StorageProvider,
};
use crate::uri::{StorageUri, FILE_SCHEME};

/// Prefix for the same-directory scratch files used by the write-then-rename
/// dance. Listings hide them so a crashed write never shows up as a project.
const TEMP_PREFIX: &str = ".atomartist-tmp-";

/// Bounded number of mtime bumps attempted to force a fresh stamp; see
/// [`stamp_after_write`].
const MAX_STAMP_BUMPS: u32 = 8;

/// Distinguishes concurrent writes within one process. Combined with the
/// process id it makes temp names unique without a random source.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Native filesystem [`StorageProvider`].
pub struct LocalFsProvider {
    display_name: String,
}

impl Default for LocalFsProvider {
    fn default() -> Self {
        LocalFsProvider::new()
    }
}

impl LocalFsProvider {
    /// Provider labelled "This PC" — the sidebar entry the plan specifies.
    pub fn new() -> Self {
        LocalFsProvider::with_display_name("This PC")
    }

    /// Same provider under a caller-chosen label (a shell may want a
    /// localized or more specific name).
    pub fn with_display_name(display_name: impl Into<String>) -> Self {
        LocalFsProvider {
            display_name: display_name.into(),
        }
    }

    /// Resolve a URI to a native path, rejecting other schemes with
    /// [`StorageError::Unsupported`] and un-mappable `file:` URIs (a Windows
    /// drive letter seen on Unix) with a descriptive
    /// [`StorageError::Io`].
    fn path_for(&self, uri: &StorageUri) -> StorageResult<PathBuf> {
        if !uri.scheme().eq_ignore_ascii_case(FILE_SCHEME) {
            return Err(StorageError::Unsupported);
        }
        uri.to_local_path()
            .ok_or_else(|| StorageError::Io(format!("`{uri}` has no local filesystem path")))
    }

    fn do_list(&self, dir: &StorageUri) -> StorageResult<Vec<Entry>> {
        let path = self.path_for(dir)?;
        // Matches `MemoryProvider`: "there is no directory here" — whether the
        // path is missing or is a file — is `NotFound`.
        match fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(StorageError::NotFound),
            Err(err) if is_absent(&err) => return Err(StorageError::NotFound),
            Err(err) => return Err(io_error("list", &path, &err)),
        }

        let read_dir = fs::read_dir(&path).map_err(|err| io_error("list", &path, &err))?;
        let mut out = Vec::new();
        for child in read_dir {
            let child = child.map_err(|err| io_error("list", &path, &err))?;
            let name = child.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(TEMP_PREFIX) {
                continue;
            }
            let child_path = child.path();
            let Some(uri) = entry_uri(dir, &name) else {
                eprintln!("local_fs: skipping unrepresentable entry name `{name}` in `{dir}`");
                continue;
            };
            // `fs::metadata` follows symlinks, so a link to a directory lists
            // as a directory. A broken link (or a file that vanished between
            // `read_dir` and here) still gets an entry, with no metadata.
            match fs::metadata(&child_path) {
                Ok(meta) => out.push(entry_from(uri, &meta)),
                Err(_) => out.push(Entry {
                    name: display_name_of(&uri),
                    uri,
                    is_dir: false,
                    size: None,
                    modified: None,
                    stamp: None,
                }),
            }
        }
        Ok(out)
    }

    fn do_read(&self, at: &StorageUri) -> StorageResult<Blob> {
        let path = self.path_for(at)?;
        fs::read(&path).map_err(|err| {
            if is_absent(&err) {
                return StorageError::NotFound;
            }
            // Reading a directory fails differently on every platform
            // (`IsADirectory` on Linux, `PermissionDenied` on Windows).
            // `MemoryProvider` has no bytes stored under a directory key and
            // says `NotFound`; say the same, so the parity claim in the module
            // header holds. Checked only on the error path, so the happy path
            // still costs one syscall.
            if matches!(fs::metadata(&path), Ok(meta) if meta.is_dir()) {
                return StorageError::NotFound;
            }
            io_error("read", &path, &err)
        })
    }

    fn do_write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> StorageResult<Stamp> {
        let path = self.path_for(at)?;
        if let Some(meta) = optional_metadata(&path)? {
            if meta.is_dir() {
                return Err(StorageError::Io(format!(
                    "cannot write `{}`: it is a directory",
                    path.display()
                )));
            }
        }
        // A file may not become a directory by implication. Checked before the
        // precondition, and before anything is created, so a rejected write
        // leaves the tree exactly as it was.
        if let Some(blocking) = file_ancestor(&path)? {
            return Err(StorageError::Io(format!(
                "cannot write `{}`: ancestor `{}` is a file",
                path.display(),
                blocking.display()
            )));
        }

        // Re-stat immediately before the write (see the module header on the
        // `IfMatch` TOCTOU window). The modification time is kept alongside
        // the stamp so `stamp_after_write` can guarantee the new stamp is
        // strictly newer than this one.
        let previous = version_of(&path)?;
        let current = previous.as_ref().map(|v| v.stamp.clone());
        match &pre {
            Precondition::None => {}
            Precondition::IfAbsent => {
                if current.is_some() {
                    return Err(StorageError::Conflict {
                        expected: None,
                        actual: current,
                    });
                }
            }
            Precondition::IfMatch(expected) => {
                if current.as_ref() != Some(expected) {
                    return Err(StorageError::Conflict {
                        expected: Some(expected.clone()),
                        actual: current,
                    });
                }
            }
        }

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|err| io_error("create parent directory of", &path, &err))?;
            }
        }

        // `IfAbsent` gets a genuinely atomic exclusive create: `create_new`
        // reserves the name in one syscall, so two racing "save as new file"
        // attempts cannot both win. The reservation is a zero-length
        // placeholder that the rename below replaces with the real bytes.
        if matches!(pre, Precondition::IfAbsent) {
            match File::create_new(&path) {
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(StorageError::Conflict {
                        expected: None,
                        actual: stamp_of(&path)?,
                    });
                }
                Err(err) => return Err(io_error("create", &path, &err)),
            }
        }

        if let Err(err) = write_atomic(&path, &bytes) {
            if matches!(pre, Precondition::IfAbsent) {
                // Do not leave our placeholder behind claiming a name whose
                // contents never landed.
                let _ = fs::remove_file(&path);
            }
            return Err(err);
        }
        stamp_after_write(&path, previous.as_ref().and_then(|v| v.modified))
    }

    fn do_delete(&self, at: &StorageUri) -> StorageResult<()> {
        let path = self.path_for(at)?;
        let meta = optional_metadata(&path)?.ok_or(StorageError::NotFound)?;
        if meta.is_dir() {
            // `remove_dir` (not `remove_dir_all`): a non-empty directory
            // fails, matching `MemoryProvider`. Recursive deletion is a
            // destructive operation the UI must ask for explicitly.
            fs::remove_dir(&path).map_err(|err| io_error("delete directory", &path, &err))
        } else {
            fs::remove_file(&path).map_err(|err| {
                if is_absent(&err) {
                    StorageError::NotFound
                } else {
                    io_error("delete", &path, &err)
                }
            })
        }
    }

    fn do_stat(&self, at: &StorageUri) -> StorageResult<Option<Entry>> {
        let path = self.path_for(at)?;
        // The entry names the URI we were asked about — `path` is derived
        // from it, so re-deriving a URI from the path can only lose
        // information (and now has a failure mode).
        Ok(optional_metadata(&path)?.map(|meta| entry_from(at.clone(), &meta)))
    }

    fn do_create_dir(&self, at: &StorageUri) -> StorageResult<()> {
        let path = self.path_for(at)?;
        if let Some(meta) = optional_metadata(&path)? {
            if meta.is_dir() {
                return Ok(()); // idempotent
            }
            return Err(StorageError::Io(format!(
                "cannot create directory `{}`: it is a file",
                path.display()
            )));
        }
        if let Some(blocking) = file_ancestor(&path)? {
            return Err(StorageError::Io(format!(
                "cannot create directory `{}`: ancestor `{}` is a file",
                path.display(),
                blocking.display()
            )));
        }
        // `create_dir_all`, so a deep path works in one call exactly as it
        // does in `MemoryProvider`.
        fs::create_dir_all(&path).map_err(|err| io_error("create directory", &path, &err))
    }
}

impl StorageProvider for LocalFsProvider {
    fn scheme(&self) -> &str {
        FILE_SCHEME
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: true,
            can_list: true,
            can_create_dir: true,
            versioned: true,
            max_blob_bytes: None,
            requires_auth: false,
        }
    }

    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>> {
        Job::from_result(self.do_list(dir))
    }

    fn read(&self, at: &StorageUri) -> Job<Blob> {
        Job::from_result(self.do_read(at))
    }

    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp> {
        Job::from_result(self.do_write(at, bytes, pre))
    }

    fn delete(&self, at: &StorageUri) -> Job<()> {
        Job::from_result(self.do_delete(at))
    }

    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>> {
        Job::from_result(self.do_stat(at))
    }

    fn create_dir(&self, at: &StorageUri) -> Job<()> {
        Job::from_result(self.do_create_dir(at))
    }
}

/// True when an `io::Error` means "nothing is stored here". `NotADirectory`
/// counts: a path *below* an existing file cannot exist, and the caller asked
/// about the leaf, not the ancestor.
fn is_absent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Single choke point from `io::Error` to [`StorageError`].
///
/// `PermissionDenied` is lifted out of the generic `Io` bucket because
/// `error.rs` documents it as the UI's recovery signal ("the backend refused
/// this identity"), and a read-only file or a locked directory is exactly
/// that case on a local disk.
fn io_error(action: &str, path: &Path, err: &io::Error) -> StorageError {
    if err.kind() == io::ErrorKind::PermissionDenied {
        return StorageError::PermissionDenied;
    }
    StorageError::Io(format!("failed to {action} `{}`: {err}", path.display()))
}

/// Entry name for a URI, with the root spelled `/` — the same fallback
/// `MemoryProvider` uses, applied everywhere an `Entry` is built.
fn display_name_of(uri: &StorageUri) -> String {
    uri.file_name().unwrap_or("/").to_string()
}

/// URI for a `read_dir` entry named `name` inside `dir`, or `None` when that
/// name has no URI form.
///
/// The child URI is derived from the (already valid) directory URI rather
/// than re-derived from the OS path: `from_local_path` refuses UNC and
/// verbatim paths that could never have produced `dir` in the first place.
///
/// Fallible because a *file name* is not a URI segment: on Unix and macOS a
/// backslash is an ordinary byte, so a file may legitimately be called
/// `a\..\b`, which the URI layer reads as traversal segments and refuses.
/// Such an entry is skipped by the caller — one unrepresentable name must
/// not abort the whole listing.
fn entry_uri(dir: &StorageUri, name: &str) -> Option<StorageUri> {
    dir.try_join(name).ok()
}

/// `Ok(None)` when nothing is stored at `path`, `Err` only for real failures
/// (permissions, a dead network share).
fn optional_metadata(path: &Path) -> StorageResult<Option<Metadata>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(err) if is_absent(&err) => Ok(None),
        Err(err) => Err(io_error("stat", path, &err)),
    }
}

/// Nearest existing ancestor of `path` that is a *file*, if any. Walking
/// stops at the first ancestor that exists: if it is a directory, nothing
/// above it can block.
fn file_ancestor(path: &Path) -> StorageResult<Option<PathBuf>> {
    let mut cursor = path.parent();
    while let Some(ancestor) = cursor {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        if let Some(meta) = optional_metadata(ancestor)? {
            if meta.is_dir() {
                return Ok(None);
            }
            return Ok(Some(ancestor.to_path_buf()));
        }
        cursor = ancestor.parent();
    }
    Ok(None)
}

fn modified_ms(meta: &Metadata) -> Option<ModifiedMs> {
    let modified = meta.modified().ok()?;
    let since_epoch = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(since_epoch.as_millis() as u64)
}

/// Version handle for a stored file: modification time in milliseconds plus
/// length.
///
/// **Known limitation.** Filesystem mtime is coarse (Windows in particular
/// updates it in ~15 ms ticks), so an *external* program that rewrites a file
/// with the same length inside one tick produces a stamp collision and a
/// stale `IfMatch` would wrongly succeed. Writes made through this provider
/// are immune: [`stamp_after_write`] forces the stamp to change. Good enough
/// for v1; a content hash or a sidecar generation counter is the upgrade.
fn stamp_from(meta: &Metadata) -> Stamp {
    match modified_ms(meta) {
        Some(ms) => Stamp::new(format!("m{ms}-l{}", meta.len())),
        // Pre-epoch or unsupported mtime: fall back to length alone rather
        // than inventing a value that would change on every stat.
        None => Stamp::new(format!("m?-l{}", meta.len())),
    }
}

/// What one stat tells us about an existing file: both halves are needed
/// before a write — the stamp for the precondition, the modification time to
/// keep the *next* stamp monotone.
struct FileVersion {
    stamp: Stamp,
    modified: Option<ModifiedMs>,
}

fn version_of(path: &Path) -> StorageResult<Option<FileVersion>> {
    Ok(optional_metadata(path)?
        .filter(|meta| !meta.is_dir())
        .map(|meta| FileVersion {
            stamp: stamp_from(&meta),
            modified: modified_ms(&meta),
        }))
}

fn stamp_of(path: &Path) -> StorageResult<Option<Stamp>> {
    Ok(version_of(path)?.map(|version| version.stamp))
}

fn entry_from(uri: StorageUri, meta: &Metadata) -> Entry {
    let name = display_name_of(&uri);
    let is_dir = meta.is_dir();
    Entry {
        uri,
        name,
        is_dir,
        size: if is_dir { None } else { Some(meta.len()) },
        modified: modified_ms(meta),
        stamp: if is_dir { None } else { Some(stamp_from(meta)) },
    }
}

/// Stamp of the file just written, guaranteed *strictly newer* than the
/// modification time observed before the write.
///
/// Equality alone is not enough. Stamps are `mtime`+`len`, both of which can
/// repeat: with mtime ticks of ~15 ms, write A can land at `m100`, write B be
/// bumped to `m101`, and write C then land at `m100` again — different from
/// B, identical to A, so a stale `IfMatch(A)` would wrongly succeed. Forcing
/// the recorded mtime to exceed the previous one makes stamps monotone per
/// file, which rules that out.
///
/// Best effort by design: if the mtime cannot be read or set (a read-only
/// file, a share that ignores `utimes`), the stamp we have is returned rather
/// than failing a write whose bytes already landed. The caller's save
/// succeeded; only the collision guard is weakened.
fn stamp_after_write(path: &Path, previous_ms: Option<ModifiedMs>) -> StorageResult<Stamp> {
    let mut meta = metadata_after_write(path)?;
    for _ in 0..MAX_STAMP_BUMPS {
        let (Some(previous), Some(current)) = (previous_ms, modified_ms(&meta)) else {
            break;
        };
        if current > previous {
            break;
        }
        if set_modified_ms(path, previous + 1).is_err() {
            break;
        }
        match metadata_after_write(path) {
            Ok(fresh) => meta = fresh,
            Err(_) => break,
        }
    }
    Ok(stamp_from(&meta))
}

fn metadata_after_write(path: &Path) -> StorageResult<Metadata> {
    optional_metadata(path)?
        .ok_or_else(|| StorageError::Io(format!("`{}` vanished after write", path.display())))
}

/// Set a file's modification time to `ms` after the Unix epoch.
fn set_modified_ms(path: &Path, ms: ModifiedMs) -> io::Result<()> {
    let file = File::options().write(true).open(path)?;
    let target = UNIX_EPOCH
        .checked_add(Duration::from_millis(ms))
        .ok_or_else(|| io::Error::other("modification time out of range"))?;
    file.set_modified(target)
}

/// Write `bytes` to a scratch file in the same directory, then rename it over
/// `path`. A crash mid-write leaves the old contents intact instead of a
/// truncated project file, and on success no scratch file survives.
fn write_atomic(path: &Path, bytes: &[u8]) -> StorageResult<()> {
    write_atomic_with(path, bytes, fill, rename)
}

/// `fs::rename` at a concrete `&Path` signature, so it can be passed where a
/// higher-ranked `Fn(&Path, &Path)` is expected.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// The body of [`write_atomic`], with the two filesystem primitives injected
/// so tests can drive the failure branches — the paths that decide whether a
/// user keeps their bytes — without needing a full disk or a locked file.
fn write_atomic_with<F, R>(path: &Path, bytes: &[u8], fill: F, rename: R) -> StorageResult<()>
where
    F: Fn(&Path, &[u8]) -> io::Result<()>,
    R: Fn(&Path, &Path) -> io::Result<()>,
{
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| StorageError::Io(format!("`{}` has no parent directory", path.display())))?;
    let temp = dir.join(temp_name());

    if let Err(err) = fill(&temp, bytes) {
        // Nothing of value here: the target still holds the previous contents.
        let _ = fs::remove_file(&temp);
        return Err(io_error("write", &temp, &err));
    }

    let Err(first) = rename(&temp, path) else {
        return Ok(());
    };
    // `std::fs::rename` replaces an existing file on both Unix (`rename(2)`)
    // and Windows (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`), so a
    // failure is *not* routine. Only the Windows shape of "the target is open
    // in another process" is worth a remove-then-rename retry; retrying
    // anything else (a full disk, a cross-device temp, a denied ACL) would
    // delete the user's existing file for nothing.
    if !is_sharing_violation(&first) {
        let _ = fs::remove_file(&temp);
        return Err(io_error("replace", path, &first));
    }
    let _ = fs::remove_file(path);
    if let Err(second) = rename(&temp, path) {
        // The temp file now holds the only copy of the new bytes, and the
        // retry has already removed the old ones. Keep it and name it, so the
        // user can recover by hand.
        return Err(StorageError::Io(format!(
            "failed to replace `{}`: {first} (retry: {second}); \
             the new contents are preserved at `{}`",
            path.display(),
            temp.display()
        )));
    }
    Ok(())
}

/// Windows reports "another process has this file open" as `PermissionDenied`
/// (`ERROR_ACCESS_DENIED`) or, when the target is being deleted,
/// `AlreadyExists`. These are the only kinds worth a second attempt.
fn is_sharing_violation(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::AlreadyExists
    )
}

fn fill(temp: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(temp)?;
    file.write_all(bytes)?;
    // Flush to the device before the rename, so a power loss cannot leave the
    // renamed name pointing at unwritten blocks.
    file.sync_all()
}

fn temp_name() -> String {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{TEMP_PREFIX}{}-{n}", std::process::id())
}

#[cfg(test)]
mod tests;
