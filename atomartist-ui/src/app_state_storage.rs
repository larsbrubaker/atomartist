//! Storage access helpers shared by [`crate::app_state_files`].
//!
//! Every project read / write in the UI layer funnels through here: the
//! [`StorageUri`] is resolved against the [`AppState`](crate::AppState)'s
//! [`StorageRegistry`], the provider is asked for the bytes, and the
//! resulting [`Job`] is driven to completion.
//!
//! **Phase 3 caveat — synchronous providers only.** The plan
//! (`docs/storage-architecture-plan.md` §3.3) puts a job pump in the frame
//! loop; that lands in Phase 4. Until then these call sites are
//! synchronous, and [`await_job`] supports exactly one class of provider:
//! one that has already settled its job by the time it returns (`Job::ready`
//! / `Job::from_result`), which is what `LocalFsProvider` and
//! `MemoryProvider` do.
//!
//! A genuinely asynchronous provider — anything built on `spawn_blocking`
//! or `spawn_local` — will *not* work here, and will most likely be
//! **cancelled before its worker has a chance to run**: the bounded poll
//! loop is a tight spin with no yield, so it exhausts its budget in
//! microseconds and then cancels the job. That is deliberate. The
//! alternative (block the UI thread waiting on the network) is worse, and
//! failing fast with a clear message keeps the gap visible until Phase 4's
//! pump removes the limitation for good.

use std::sync::Arc;

use atomartist_storage::{Job, Precondition, StorageProvider, StorageRegistry, StorageUri};

/// How many times [`await_job`] samples a job before giving up. Any
/// synchronous provider settles on the first poll; the loop exists purely
/// so a mistakenly-registered async provider fails loudly rather than
/// hanging the UI thread.
const MAX_POLLS: u32 = 1_000;

/// Provider owning `uri`'s scheme, or a user-readable error naming the
/// scheme that nothing is registered for.
pub(crate) fn provider_for(
    registry: &StorageRegistry,
    uri: &StorageUri,
) -> Result<Arc<dyn StorageProvider>, String> {
    registry
        .resolve(uri)
        .ok_or_else(|| format!("no storage provider for scheme `{}`", uri.scheme()))
}

/// Drive a job to completion. See the module docs for why this is a
/// bounded spin rather than a wait.
pub(crate) fn await_job<T>(job: Job<T>) -> Result<T, String> {
    for _ in 0..MAX_POLLS {
        if job.poll().is_settled() {
            return match job.take() {
                Some(Ok(value)) => Ok(value),
                Some(Err(err)) => Err(err.to_string()),
                // `take` only returns `None` for pending / already-taken,
                // and this job handle is not shared with anyone else.
                None => Err("storage job produced no result".to_string()),
            };
        }
    }
    job.cancel();
    Err("storage backend did not complete synchronously".to_string())
}

/// Read every byte stored at `uri`.
pub(crate) fn read_bytes(registry: &StorageRegistry, uri: &StorageUri) -> Result<Vec<u8>, String> {
    let provider = provider_for(registry, uri)?;
    await_job(provider.read(uri))
}

/// Store `bytes` at `uri`, overwriting unconditionally. Preconditions
/// (conflict detection) arrive with the remote providers in Phase 8.
pub(crate) fn write_bytes(
    registry: &StorageRegistry,
    uri: &StorageUri,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let provider = provider_for(registry, uri)?;
    await_job(provider.write(uri, bytes, Precondition::None)).map(|_stamp| ())
}

/// Whether something is stored at `uri`. An unresolvable scheme or a
/// failed `stat` both read as "not there" — the caller's recovery
/// (dropping a stale recent entry, falling back to the starter graph) is
/// the same either way.
pub fn uri_exists(registry: &StorageRegistry, uri: &StorageUri) -> bool {
    let Ok(provider) = provider_for(registry, uri) else {
        return false;
    };
    matches!(await_job(provider.stat(uri)), Ok(Some(_)))
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
    use atomartist_storage::{MemoryProvider, StorageError};
    use std::path::Path;

    fn registry_with_memory() -> StorageRegistry {
        let mut registry = StorageRegistry::new();
        registry
            .register(Arc::new(MemoryProvider::new("mem", "Memory")))
            .expect("fresh registry");
        registry
    }

    #[test]
    fn round_trips_bytes_through_a_provider() {
        let registry = registry_with_memory();
        let uri: StorageUri = "mem:///projects/a.atmr".parse().unwrap();
        write_bytes(&registry, &uri, b"hello".to_vec()).unwrap();
        assert!(uri_exists(&registry, &uri));
        assert_eq!(read_bytes(&registry, &uri).unwrap(), b"hello");
    }

    #[test]
    fn unregistered_scheme_names_itself_in_the_error() {
        let registry = registry_with_memory();
        let uri: StorageUri = "nope:///a.atmr".parse().unwrap();
        let err = read_bytes(&registry, &uri).unwrap_err();
        assert!(err.contains("nope"), "error should name the scheme: {err}");
        assert!(!uri_exists(&registry, &uri));
    }

    /// A never-settling job must return an error rather than spinning
    /// forever — the frame-loop pump (Phase 4) is what makes real async
    /// providers usable.
    #[test]
    fn a_job_that_never_settles_reports_an_error() {
        let (job, completer) = Job::<u32>::pending();
        let err = await_job(job).unwrap_err();
        assert_eq!(err, "storage backend did not complete synchronously");
        drop(completer);
    }

    #[test]
    fn a_failed_job_surfaces_the_storage_error_text() {
        let err = await_job(Job::<u32>::failed(StorageError::NotFound)).unwrap_err();
        assert_eq!(err, StorageError::NotFound.to_string());
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
