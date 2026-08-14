//! Storage access helpers shared by [`crate::app_state_files`] and
//! [`crate::app_state_files_import`].
//!
//! Every project read / write in the UI layer funnels through here: the
//! [`StorageUri`] is resolved against the [`AppState`](crate::AppState)'s
//! [`StorageRegistry`] and the provider is asked for a [`Job`].
//!
//! **The job is where this module stops.** Nothing here waits for a
//! result — Phase 4c moved every call site onto
//! [`AppState::submit_op`](crate::AppState::submit_op), so the job goes
//! straight into the frame pump ([`crate::storage_ops`]) and a
//! continuation applies its outcome whenever it lands. A genuinely
//! asynchronous provider (Phase 5's OPFS backend, Phase 8's HTTP one)
//! therefore works at every call site; the local providers still settle
//! inline, so `submit_op` applies their continuations immediately and the
//! desktop path keeps its synchronous feel.
//!
//! An unresolvable scheme is reported as a *failed job* rather than a
//! separate error channel, so the call site has exactly one failure path
//! to handle.

use atomartist_storage::{
    Blob, Entry, Job, Precondition, Stamp, StorageError, StorageRegistry, StorageUri,
};

/// A job that fails immediately because no provider claims `uri`'s
/// scheme — a build without the provider that wrote a recent entry, for
/// instance.
fn no_provider<T>(uri: &StorageUri) -> Job<T> {
    Job::failed(StorageError::Io(format!(
        "no storage provider for scheme `{}`",
        uri.scheme()
    )))
}

/// Job reading every byte stored at `uri`.
pub(crate) fn read_job(registry: &StorageRegistry, uri: &StorageUri) -> Job<Blob> {
    match registry.resolve(uri) {
        Some(provider) => provider.read(uri),
        None => no_provider(uri),
    }
}

/// Job storing `bytes` at `uri`, overwriting unconditionally.
/// Preconditions (conflict detection) arrive with the remote providers in
/// Phase 8.
pub(crate) fn write_job(
    registry: &StorageRegistry,
    uri: &StorageUri,
    bytes: Vec<u8>,
) -> Job<Stamp> {
    match registry.resolve(uri) {
        Some(provider) => provider.write(uri, bytes, Precondition::None),
        None => no_provider(uri),
    }
}

/// Job answering "is anything stored at `uri`?". `Ok(None)` and any
/// failure both mean "you cannot open this" — callers (the recent list,
/// auto-reopen) recover the same way either way.
pub(crate) fn stat_job(registry: &StorageRegistry, uri: &StorageUri) -> Job<Option<Entry>> {
    match registry.resolve(uri) {
        Some(provider) => provider.stat(uri),
        None => no_provider(uri),
    }
}

/// Short name for a storage operation's status-bar label — the URI's last
/// segment, falling back to the whole URI for a segment-less location.
///
/// Deliberately *not* [`display_uri`]: the status bar has room for
/// `Opening bracket.atmr`, not for a full Windows path.
pub(crate) fn uri_label(uri: &StorageUri) -> String {
    uri.file_name()
        .map(|n| n.to_string())
        .unwrap_or_else(|| uri.to_string())
}

/// Lower-cased extension of the URI's last path segment, without the dot
/// (`""` when the name has none). The app dispatches import formats on
/// this, so it matches `Path::extension` — including the dotfile rule:
/// a leading dot starts the *name*, not an extension, so `.atmr` has no
/// extension at all.
pub(crate) fn uri_extension(uri: &StorageUri) -> String {
    uri.file_name()
        .and_then(split_stem_ext)
        .and_then(|(_stem, ext)| ext)
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Name of the URI's last path segment with any extension removed — the
/// `Path::file_stem` equivalent used to seed export file names. Same
/// dotfile rule: the stem of `.atmr` is `.atmr`.
pub fn uri_file_stem(uri: &StorageUri) -> Option<String> {
    uri.file_name()
        .and_then(split_stem_ext)
        .map(|(stem, _ext)| stem.to_string())
}

/// `Path::file_stem` / `Path::extension` semantics on a bare file name:
/// split at the *last* dot, except that a name which is nothing but a
/// leading dot plus text (`.atmr`) is all stem and no extension.
///
/// The leading dot is skipped by *character*, not by byte: a name like
/// `Ölkanne.stl` starts with a two-byte code point, and slicing at byte
/// index 1 would land mid-character and panic. File names come straight
/// from user drops and pickers, so non-ASCII is ordinary input.
fn split_stem_ext(name: &str) -> Option<(&str, Option<&str>)> {
    let first = name.chars().next()?;
    let after_first = first.len_utf8();
    match name[after_first..].rfind('.') {
        Some(cut) => {
            let cut = cut + after_first;
            Some((&name[..cut], Some(&name[cut + 1..])))
        }
        None => Some((name, None)),
    }
}

/// How a URI should be shown to a user in an error message or prompt.
///
/// A `file:` URI reads back as the native path the user recognises
/// (`C:\Users\lars\bracket.atmr`), not the URI form we store it in
/// (`file:///C:/Users/lars/bracket.atmr`). Every other scheme has no
/// friendlier rendering, so it shows as the URI.
pub fn display_uri(uri: &StorageUri) -> String {
    match uri.to_local_path() {
        Some(path) => path.display().to_string(),
        None => uri.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomartist_storage::MemoryProvider;
    use std::path::Path;
    use std::sync::Arc;

    fn registry_with_memory() -> StorageRegistry {
        let mut registry = StorageRegistry::new();
        registry
            .register(Arc::new(MemoryProvider::new("mem", "Memory")))
            .expect("fresh registry");
        registry
    }

    /// Settled-job helper: the local providers under test resolve before
    /// they return, which is exactly the case `submit_op` applies inline.
    fn settled<T>(job: Job<T>) -> Result<T, atomartist_storage::StorageError> {
        assert!(job.poll().is_settled(), "local providers settle inline");
        job.take().expect("a settled job yields its result")
    }

    #[test]
    fn round_trips_bytes_through_a_provider() {
        let registry = registry_with_memory();
        let uri: StorageUri = "mem:///projects/a.atmr".parse().unwrap();
        settled(write_job(&registry, &uri, b"hello".to_vec())).unwrap();
        assert!(settled(stat_job(&registry, &uri)).unwrap().is_some());
        assert_eq!(settled(read_job(&registry, &uri)).unwrap(), b"hello");
    }

    /// The error a user sees when the URI's scheme belongs to a provider
    /// this build does not register has to name that scheme.
    #[test]
    fn unregistered_scheme_names_itself_in_the_error() {
        let registry = registry_with_memory();
        let uri: StorageUri = "nope:///a.atmr".parse().unwrap();
        let err = settled(read_job(&registry, &uri)).unwrap_err().to_string();
        assert!(err.contains("nope"), "error should name the scheme: {err}");
        // `stat` fails the same way, which is what prunes the entry from
        // the recent list.
        assert!(settled(stat_job(&registry, &uri)).is_err());
    }

    /// The status-bar label is the file name, not the whole URI.
    #[test]
    fn labels_use_the_last_segment() {
        let uri: StorageUri = "mem:///projects/bracket.atmr".parse().unwrap();
        assert_eq!(uri_label(&uri), "bracket.atmr");
    }

    #[test]
    fn extension_and_stem_come_from_the_last_segment() {
        let uri: StorageUri = "mem:///a/b/model.STL".parse().unwrap();
        assert_eq!(uri_extension(&uri), "stl");
        assert_eq!(uri_file_stem(&uri).as_deref(), Some("model"));

        let no_ext: StorageUri = "mem:///a/plain".parse().unwrap();
        assert_eq!(uri_extension(&no_ext), "");
        assert_eq!(uri_file_stem(&no_ext).as_deref(), Some("plain"));

        let two_dots: StorageUri = "mem:///a/archive.tar.gz".parse().unwrap();
        assert_eq!(uri_extension(&two_dots), "gz");
        assert_eq!(uri_file_stem(&two_dots).as_deref(), Some("archive.tar"));
    }

    /// These helpers claim `Path` parity, so they must follow `Path`'s
    /// dotfile rule: a leading dot begins the name, so `.atmr` is a stem
    /// with no extension — not an extensionless-named `.atmr` file.
    #[test]
    fn dotfiles_follow_path_semantics() {
        let dotfile: StorageUri = "mem:///project/.atmr".parse().unwrap();
        assert_eq!(Path::new(".atmr").extension(), None);
        assert_eq!(uri_extension(&dotfile), "");
        assert_eq!(
            Path::new(".atmr").file_stem().and_then(|s| s.to_str()),
            Some(".atmr")
        );
        assert_eq!(uri_file_stem(&dotfile).as_deref(), Some(".atmr"));

        let dotted: StorageUri = "mem:///project/.config.json".parse().unwrap();
        assert_eq!(uri_extension(&dotted), "json");
        assert_eq!(uri_file_stem(&dotted).as_deref(), Some(".config"));
    }

    /// Reproduces a panic: the dotfile rule skipped the first *byte*
    /// rather than the first character, so slicing at index 1 landed
    /// inside a multi-byte code point and blew up on any name starting
    /// with a non-ASCII letter — reachable from a plain file drop of
    /// `Ölkanne.stl`, and from the export-name suggestion with such a
    /// project open.
    #[test]
    fn non_ascii_names_do_not_panic_and_match_path_semantics() {
        for name in [
            "Ölkanne.stl",     // 2-byte first char
            "日本.stl",         // 3-byte first char
            "😀.stl",           // 4-byte first char
            ".Ödot",           // dotfile whose second char is multi-byte
            ".Ödot.atmr",      // dotfile with a real extension
            "Ö",               // no extension at all
            "Ölkanne.tar.gz",  // multiple dots
        ] {
            let uri = StorageUri::new("mem", &format!("/project/{name}"));
            let as_path = Path::new(name);

            let expected_ext = as_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            assert_eq!(
                uri_extension(&uri),
                expected_ext,
                "extension of `{name}` must match Path"
            );

            let expected_stem = as_path.file_stem().and_then(|s| s.to_str());
            assert_eq!(
                uri_file_stem(&uri).as_deref(),
                expected_stem,
                "stem of `{name}` must match Path"
            );
        }
    }

    /// Users see the path they picked, not the URI we store it as.
    #[test]
    fn file_uris_display_as_native_paths() {
        let non_local: StorageUri = "mem:///projects/a.atmr".parse().unwrap();
        assert_eq!(display_uri(&non_local), "mem:///projects/a.atmr");

        let local = StorageUri::from_local_path(Path::new("/home/lars/a.atmr"))
            .expect("posix path has a URI form");
        assert_eq!(
            display_uri(&local),
            Path::new("/home/lars/a.atmr").display().to_string()
        );
    }
}
