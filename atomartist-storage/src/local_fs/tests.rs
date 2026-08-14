//! Internals-only tests for `LocalFsProvider`.
//!
//! The behavioural suite lives in `tests/local_fs.rs` and drives the public
//! `StorageProvider` API; this file covers the two things that decide whether
//! a user keeps their bytes and cannot be provoked through that API on a
//! healthy disk: the atomic-write failure branches, and stamp monotonicity
//! under a coarse clock. Both need the module's private seams.

use super::*;

/// Scratch directory unique to this process and test, removed on drop.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "atomartist-localfs-unit-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        Scratch { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// File names present, temp droppings included.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.dir)
            .expect("read scratch")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Undo any read-only bit a test set, or the removal fails.
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    let mut perms = meta.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = fs::set_permissions(entry.path(), perms);
                }
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn failing_rename(kind: io::ErrorKind) -> impl Fn(&Path, &Path) -> io::Result<()> {
    move |_, _| Err(io::Error::new(kind, "injected rename failure"))
}

/// A rename failure that is *not* the Windows "file is open elsewhere"
/// shape (here: a full disk) must leave the existing file untouched. The
/// earlier code removed the target before every retry, so a failed save
/// destroyed the last good copy.
#[test]
fn a_non_retryable_rename_failure_keeps_the_existing_file() {
    let scratch = Scratch::new("rename-fatal");
    let target = scratch.path("project.atmr");
    fs::write(&target, b"old").expect("seed");

    let err = write_atomic_with(
        &target,
        b"new",
        fill,
        failing_rename(io::ErrorKind::StorageFull),
    )
    .expect_err("the injected failure must surface");
    assert!(matches!(err, StorageError::Io(_)), "{err:?}");

    assert_eq!(
        fs::read(&target).expect("the old file must survive"),
        b"old".to_vec()
    );
    assert_eq!(
        scratch.names(),
        vec!["project.atmr".to_string()],
        "the scratch file must be cleaned up"
    );
}

/// When the retry path is taken and also fails, the target is already
/// gone — so the temp file holding the new bytes must be kept, and named
/// in the error, rather than deleted along with it.
#[test]
fn a_failed_retry_preserves_the_new_bytes_and_says_where() {
    let scratch = Scratch::new("rename-retry");
    let target = scratch.path("project.atmr");
    fs::write(&target, b"old").expect("seed");

    let err = write_atomic_with(
        &target,
        b"new",
        fill,
        failing_rename(io::ErrorKind::PermissionDenied),
    )
    .expect_err("the injected failure must surface");
    let StorageError::Io(message) = err else {
        panic!("expected Io, got {err:?}");
    };

    let leftovers = scratch.names();
    let temp = leftovers
        .iter()
        .find(|name| name.starts_with(TEMP_PREFIX))
        .expect("the new bytes must be preserved in the temp file");
    assert!(
        message.contains(temp.as_str()),
        "the error must name the recoverable file: {message}"
    );
    assert_eq!(
        fs::read(scratch.path(temp)).expect("temp readable"),
        b"new".to_vec()
    );
}

/// A failure while filling the scratch file leaves neither a dropping nor
/// a damaged target.
#[test]
fn a_fill_failure_cleans_up_and_keeps_the_existing_file() {
    let scratch = Scratch::new("fill");
    let target = scratch.path("project.atmr");
    fs::write(&target, b"old").expect("seed");

    let err = write_atomic_with(
        &target,
        b"new",
        |temp: &Path, _: &[u8]| {
            // Partially written, like a disk filling up mid-save.
            fs::write(temp, b"tor")?;
            Err(io::Error::other("injected fill failure"))
        },
        rename,
    )
    .expect_err("the injected failure must surface");
    assert!(matches!(err, StorageError::Io(_)), "{err:?}");

    assert_eq!(fs::read(&target).expect("old file"), b"old".to_vec());
    assert_eq!(scratch.names(), vec!["project.atmr".to_string()]);
}

/// Stamps must be monotone per file, not merely different from the
/// immediately preceding one: with a coarse clock, write C can land on
/// the very mtime write A used (A=m100, B bumped to m101, C=m100), and a
/// stale `IfMatch(A)` would then wrongly succeed.
#[test]
fn a_stamp_never_reuses_an_earlier_modification_time() {
    let scratch = Scratch::new("monotone");
    let path = scratch.path("a.bin");
    fs::write(&path, b"abc").expect("seed");
    // Simulate "the filesystem handed this write an older tick".
    set_modified_ms(&path, 1_000).expect("set mtime");

    let stamp = stamp_after_write(&path, Some(2_000)).expect("stamp");
    assert_eq!(stamp.as_str(), "m2001-l3", "must exceed the previous mtime");

    // And the file really carries it, so a later `stat` agrees.
    let restated = stamp_of(&path).expect("stat").expect("present");
    assert_eq!(restated, stamp);
}

/// Bumping the mtime is best effort: a file whose timestamp cannot be
/// changed must still report a stamp, because the bytes already landed.
/// Failing here would show "save failed" over a file that saved fine.
#[test]
fn an_unbumpable_file_still_yields_a_stamp() {
    let scratch = Scratch::new("readonly");
    let path = scratch.path("a.bin");
    fs::write(&path, b"abc").expect("seed");
    set_modified_ms(&path, 1_000).expect("set mtime");

    let mut perms = fs::metadata(&path).expect("stat").permissions();
    perms.set_readonly(true);
    fs::set_permissions(&path, perms).expect("set read-only");

    // A previous mtime far in the future forces every bump attempt to run.
    let stamp = stamp_after_write(&path, Some(9_000_000_000_000))
        .expect("a write whose bytes landed must not fail");
    assert!(stamp.as_str().ends_with("-l3"), "{stamp}");
}

/// `PermissionDenied` is the UI's "the backend refused you" signal, so it
/// must not be flattened into the generic `Io` bucket. Everything else
/// keeps the path in its message for the status bar.
#[test]
fn permission_denied_is_mapped_out_of_the_io_bucket() {
    let path = Path::new("some/project.atmr");
    assert_eq!(
        io_error(
            "read",
            path,
            &io::Error::new(io::ErrorKind::PermissionDenied, "denied")
        ),
        StorageError::PermissionDenied
    );
    match io_error(
        "read",
        path,
        &io::Error::new(io::ErrorKind::StorageFull, "full"),
    ) {
        StorageError::Io(message) => assert!(message.contains("project.atmr"), "{message}"),
        other => panic!("expected Io, got {other:?}"),
    }
}

#[test]
fn only_the_windows_sharing_shapes_are_retried() {
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::AlreadyExists,
    ] {
        assert!(is_sharing_violation(&io::Error::new(kind, "x")), "{kind:?}");
    }
    for kind in [
        io::ErrorKind::StorageFull,
        io::ErrorKind::NotFound,
        io::ErrorKind::CrossesDevices,
        io::ErrorKind::Other,
    ] {
        assert!(
            !is_sharing_violation(&io::Error::new(kind, "x")),
            "{kind:?}"
        );
    }
}
