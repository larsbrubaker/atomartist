//! Pure logic behind `BrowserProvider`, free of any browser API.
//!
//! Everything here is ordinary Rust over strings and numbers — URI to OPFS
//! path segments, stamp derivation, [`Entry`] assembly, and the mapping from
//! a JavaScript `DOMException.name` onto [`StorageError`]. Keeping it in its
//! own module means the parts most likely to be wrong (path handling, error
//! classification) are unit-tested by `cargo test` on the desktop, where no
//! browser exists; only the plumbing in `browser/opfs.rs` needs a real
//! browser to exercise.

// The only *caller* of these helpers is `browser/opfs.rs`, which exists on
// wasm alone. They are still compiled — and unit-tested — on native, which
// is the whole point of the split, so dead-code is expected there.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use crate::error::{StorageError, StorageResult};
use crate::provider::{Entry, ModifiedMs, Stamp};
use crate::uri::StorageUri;

/// URI scheme owned by the browser-local provider.
pub const BROWSER_SCHEME: &str = "browser";

/// OPFS path segments for `uri`, empty for the provider root.
///
/// Rejects a URI belonging to another provider with
/// [`StorageError::Unsupported`], exactly as `MemoryProvider::key` and
/// `LocalFsProvider::path_for` do. No traversal check is needed: a
/// [`StorageUri`] cannot hold a `.` or `..` segment (see `uri.rs`), which is
/// what lets a rooted provider like this one trust the path it is given.
pub fn segments(uri: &StorageUri) -> StorageResult<Vec<&str>> {
    if !uri.scheme().eq_ignore_ascii_case(BROWSER_SCHEME) {
        return Err(StorageError::Unsupported);
    }
    Ok(uri.path().split('/').filter(|s| !s.is_empty()).collect())
}

/// Directory segments of `segments` (everything but the last).
pub fn parent_segments<'a>(segments: &'a [&'a str]) -> &'a [&'a str] {
    match segments.len() {
        0 => &[],
        n => &segments[..n - 1],
    }
}

/// Leaf name of `segments`, or `None` at the root.
pub fn leaf<'a>(segments: &[&'a str]) -> Option<&'a str> {
    segments.last().copied()
}

/// Version handle for an OPFS file: `lastModified` in milliseconds plus
/// length, the same shape `LocalFsProvider` uses.
///
/// **OPFS has no ETag and no generation counter**, so this is the strongest
/// handle the platform offers — and it is weak: two same-length writes
/// inside one millisecond produce the same stamp. That is exactly why
/// `BrowserProvider` reports `versioned: false` and refuses
/// [`Precondition`](crate::Precondition)s it cannot honour, rather than
/// pretending to a compare-and-swap it does not have.
pub fn stamp_for(last_modified_ms: f64, size: u64) -> Stamp {
    match modified_ms(last_modified_ms) {
        Some(ms) => Stamp::new(format!("m{ms}-l{size}")),
        None => Stamp::new(format!("m?-l{size}")),
    }
}

/// `File.lastModified` (wall-clock milliseconds, a JS `double`) as the
/// plan's `SystemTimeish`. Negative (pre-epoch), NaN, and infinite values
/// have no `u64` form and read as "unknown".
pub fn modified_ms(last_modified_ms: f64) -> Option<ModifiedMs> {
    if !last_modified_ms.is_finite() || last_modified_ms < 0.0 {
        return None;
    }
    Some(last_modified_ms as ModifiedMs)
}

/// `Blob.size` (also a JS `double`) as a byte count.
pub fn size_bytes(size: f64) -> u64 {
    if !size.is_finite() || size < 0.0 {
        return 0;
    }
    size as u64
}

/// Entry for a stored file, named from the URI's last segment.
pub fn file_entry(uri: StorageUri, size: f64, last_modified_ms: f64) -> Entry {
    let size = size_bytes(size);
    let name = entry_name(&uri);
    Entry {
        uri,
        name,
        is_dir: false,
        size: Some(size),
        modified: modified_ms(last_modified_ms),
        stamp: Some(stamp_for(last_modified_ms, size)),
    }
}

/// Entry for a directory. OPFS exposes no size or timestamp for one, so
/// both stay `None` — as they do for `MemoryProvider` and `LocalFsProvider`.
pub fn dir_entry(uri: StorageUri) -> Entry {
    let name = entry_name(&uri);
    Entry {
        uri,
        name,
        is_dir: true,
        size: None,
        modified: None,
        stamp: None,
    }
}

/// Entry name for a URI, with the root spelled `/` — the fallback every
/// provider uses.
fn entry_name(uri: &StorageUri) -> String {
    uri.file_name().unwrap_or("/").to_string()
}

/// Whether a `DOMException.name` means "nothing is stored here".
///
/// `TypeMismatchError` counts: it is what OPFS throws when the name exists
/// but is the other kind (a `getFileHandle` on a directory, or any lookup
/// *below* a file). The caller asked about the leaf, and no leaf exists —
/// the same reading `LocalFsProvider` gives `NotADirectory`.
pub fn is_absent(name: &str) -> bool {
    matches!(name, "NotFoundError" | "TypeMismatchError")
}

/// Map a JavaScript exception onto the provider-agnostic error the UI
/// reasons about. `action` describes what was attempted ("read", "write
/// to") and `at` names the URI, so the `Io` payload reads as a sentence.
///
/// | `DOMException.name` | maps to | why |
/// |---|---|---|
/// | `NotFoundError` | `NotFound` | the entry does not exist |
/// | `TypeMismatchError` | `Io` | a file is in the way of a directory (callers that mean "absent" check [`is_absent`] first) |
/// | `NotAllowedError`, `SecurityError` | `PermissionDenied` | the origin may not use storage — `error.rs` names this the UI's recovery signal |
/// | `QuotaExceededError` | `Io` | out of browser storage; no dedicated variant, and the message is what the user needs |
/// | `InvalidModificationError` | `Io` | e.g. removing a non-empty directory without `recursive` |
/// | `NoModificationAllowedError` | `Io` | the file is locked by another handle |
/// | `AbortError` | `Io` | a stream aborted; **not** `Cancelled`, which is reserved for `Job::cancel` |
/// | anything else | `Io` | carries the name and message through |
pub fn error_for(name: &str, message: &str, action: &str, at: &StorageUri) -> StorageError {
    match name {
        "NotFoundError" => StorageError::NotFound,
        "NotAllowedError" | "SecurityError" => StorageError::PermissionDenied,
        "QuotaExceededError" => StorageError::Io(format!(
            "failed to {action} `{at}`: browser storage quota exceeded"
        )),
        _ => {
            let detail = if message.is_empty() { name } else { message };
            StorageError::Io(format!("failed to {action} `{at}`: {detail}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(path: &str) -> StorageUri {
        StorageUri::new(BROWSER_SCHEME, path)
    }

    #[test]
    fn segments_split_the_path_and_the_root_is_empty() {
        assert_eq!(segments(&uri("/")).unwrap(), Vec::<&str>::new());
        assert_eq!(
            segments(&uri("/projects/bracket.atmr")).unwrap(),
            vec!["projects", "bracket.atmr"]
        );
        // Normalization already collapsed the repeats, but the filter keeps
        // an empty segment from ever reaching `getDirectoryHandle("")`.
        assert_eq!(segments(&uri("/a//b")).unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn a_foreign_scheme_is_unsupported() {
        let foreign: StorageUri = "file:///C:/x.atmr".parse().unwrap();
        assert_eq!(segments(&foreign), Err(StorageError::Unsupported));
    }

    #[test]
    fn parents_and_leaf_describe_the_target() {
        let at = uri("/projects/bracket.atmr");
        let segs = segments(&at).unwrap();
        assert_eq!(parent_segments(&segs), ["projects"]);
        assert_eq!(leaf(&segs), Some("bracket.atmr"));

        let root = uri("/");
        let segs = segments(&root).unwrap();
        assert!(parent_segments(&segs).is_empty());
        assert_eq!(leaf(&segs), None);
    }

    #[test]
    fn stamps_combine_modification_time_and_length() {
        assert_eq!(stamp_for(1_700_000_000_000.0, 42).as_str(), "m1700000000000-l42");
        // Two writes of the same length inside one millisecond collide —
        // the documented reason this provider is `versioned: false`.
        assert_eq!(stamp_for(5.0, 3), stamp_for(5.0, 3));
        assert_ne!(stamp_for(5.0, 3), stamp_for(6.0, 3));
        assert_ne!(stamp_for(5.0, 3), stamp_for(5.0, 4));
    }

    #[test]
    fn nonsense_metadata_reads_as_unknown_rather_than_panicking() {
        assert_eq!(modified_ms(f64::NAN), None);
        assert_eq!(modified_ms(-1.0), None);
        assert_eq!(modified_ms(f64::INFINITY), None);
        assert_eq!(stamp_for(f64::NAN, 7).as_str(), "m?-l7");
        assert_eq!(size_bytes(f64::NAN), 0);
        assert_eq!(size_bytes(-3.0), 0);
        assert_eq!(size_bytes(12.0), 12);
    }

    #[test]
    fn file_entries_carry_size_time_and_stamp() {
        let at = uri("/projects/bracket.atmr");
        let entry = file_entry(at.clone(), 9.0, 1_000.0);
        assert_eq!(entry.uri, at);
        assert_eq!(entry.name, "bracket.atmr");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, Some(9));
        assert_eq!(entry.modified, Some(1_000));
        assert_eq!(entry.stamp, Some(stamp_for(1_000.0, 9)));
    }

    #[test]
    fn directory_entries_report_no_metadata() {
        let entry = dir_entry(uri("/projects"));
        assert!(entry.is_dir);
        assert_eq!(entry.name, "projects");
        assert_eq!(entry.size, None);
        assert_eq!(entry.modified, None);
        assert_eq!(entry.stamp, None);

        assert_eq!(dir_entry(uri("/")).name, "/");
    }

    #[test]
    fn dom_exception_names_map_onto_storage_errors() {
        let at = uri("/projects/bracket.atmr");
        assert_eq!(
            error_for("NotFoundError", "no such entry", "read", &at),
            StorageError::NotFound
        );
        assert_eq!(
            error_for("NotAllowedError", "", "write to", &at),
            StorageError::PermissionDenied
        );
        assert_eq!(
            error_for("SecurityError", "", "read", &at),
            StorageError::PermissionDenied
        );
        match error_for("QuotaExceededError", "", "write to", &at) {
            StorageError::Io(msg) => assert!(msg.contains("quota"), "{msg}"),
            other => panic!("expected Io, got {other:?}"),
        }
        for name in [
            "TypeMismatchError",
            "InvalidModificationError",
            "NoModificationAllowedError",
            "AbortError",
            "SomethingBrandNewError",
        ] {
            match error_for(name, "detail here", "write to", &at) {
                StorageError::Io(msg) => {
                    assert!(msg.contains("detail here"), "{msg}");
                    assert!(msg.contains("bracket.atmr"), "{msg}");
                }
                other => panic!("expected Io for {name}, got {other:?}"),
            }
        }
        // With no message, the exception name is what the user sees.
        match error_for("WeirdError", "", "list", &at) {
            StorageError::Io(msg) => assert!(msg.contains("WeirdError"), "{msg}"),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn absence_covers_the_wrong_kind_of_entry() {
        assert!(is_absent("NotFoundError"));
        assert!(is_absent("TypeMismatchError"));
        assert!(!is_absent("NotAllowedError"));
        assert!(!is_absent("QuotaExceededError"));
    }
}
