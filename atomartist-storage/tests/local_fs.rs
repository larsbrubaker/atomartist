//! Behaviour of `LocalFsProvider` through the public `StorageProvider` API.
//!
//! The conformance suite (`local_fs_conformance.rs`) proves the shared
//! contract; this file covers what is specific to a real filesystem —
//! stamps derived from mtime, the atomic write's tidiness, and the error
//! shapes for paths that no local file can back. Only checks that need the
//! module's internals (the write-failure branches) live inside
//! `src/local_fs.rs`.
//!
//! Native only: `LocalFsProvider` and the blocking `await_job` are both
//! compiled out on wasm.

#![cfg(not(target_arch = "wasm32"))]

use std::fs;

use atomartist_storage::conformance::await_job;
use atomartist_storage::{
    Capabilities, LocalFsProvider, Precondition, StorageError, StorageProvider, StorageUri,
};

/// Scratch directory unique to this process and test, removed on drop.
struct Scratch {
    provider: LocalFsProvider,
    root: StorageUri,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("atomartist-localfs-{}-{label}", std::process::id()));
        let provider = LocalFsProvider::new();
        let root = StorageUri::from_local_path(&path).expect("temp dir has a URI form");
        let _ = fs::remove_dir_all(&path);
        await_job(&provider.create_dir(&root)).expect("scratch directory");
        Scratch { provider, root }
    }

    fn write(&self, at: &StorageUri, bytes: &[u8]) -> atomartist_storage::Stamp {
        await_job(&self.provider.write(at, bytes.to_vec(), Precondition::None))
            .expect("write should succeed")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(path) = self.root.to_local_path() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[test]
fn advertises_the_planned_identity_and_capabilities() {
    let provider = LocalFsProvider::new();
    assert_eq!(provider.scheme(), "file");
    assert_eq!(provider.display_name(), "This PC");
    assert_eq!(
        provider.capabilities(),
        Capabilities {
            writable: true,
            can_list: true,
            can_create_dir: true,
            versioned: true,
            max_blob_bytes: None,
            requires_auth: false,
        }
    );
    assert_eq!(
        LocalFsProvider::with_display_name("Disk").display_name(),
        "Disk"
    );
    assert!(provider.native_picker().is_none());
}

#[test]
fn rejects_uris_from_another_scheme() {
    let provider = LocalFsProvider::new();
    let foreign: StorageUri = "mem:///a.atmr".parse().unwrap();
    assert_eq!(
        await_job(&provider.read(&foreign)),
        Err(StorageError::Unsupported)
    );
}

/// A `file:` URI that names nothing local fails with a descriptive `Io`,
/// never a panic or a silently relative path. Only reachable off Windows,
/// where a drive-letter URI has no meaning.
#[cfg(not(windows))]
#[test]
fn a_uri_without_a_local_path_fails_with_io() {
    let provider = LocalFsProvider::new();
    let drive: StorageUri = "file:///C:/projects/a.atmr".parse().unwrap();
    match await_job(&provider.read(&drive)) {
        Err(StorageError::Io(message)) => {
            assert!(message.contains("no local filesystem path"), "{message}");
        }
        other => panic!("expected Io, got {other:?}"),
    }
}

/// Same-length overwrites inside one mtime tick must still change the stamp,
/// and must never reuse an *earlier* stamp either, or `IfMatch` could not
/// detect the edit.
#[test]
fn repeated_equal_length_writes_never_repeat_a_stamp() {
    let scratch = Scratch::new("stamp");
    let at = scratch.root.join("a.bin");

    let mut seen = Vec::new();
    for payload in [b"one", b"two", b"six", b"ten", b"ace", b"bed"] {
        let stamp = scratch.write(&at, payload);
        assert!(!seen.contains(&stamp), "stamp {stamp} repeated: {seen:?}");
        let from_stat = await_job(&scratch.provider.stat(&at))
            .expect("stat")
            .expect("just written")
            .stamp;
        assert_eq!(
            from_stat,
            Some(stamp.clone()),
            "stat must report the stamp the write returned"
        );
        seen.push(stamp);
    }
}

/// Repeated `stat`s of unchanged content agree — a stamp is a version
/// handle, not a nonce — and `list` reports the same one.
#[test]
fn stamp_is_stable_while_the_file_is_unchanged() {
    let scratch = Scratch::new("stable");
    let at = scratch.root.join("a.bin");
    scratch.write(&at, b"x");

    let first = await_job(&scratch.provider.stat(&at))
        .expect("stat")
        .expect("present")
        .stamp;
    let listed = await_job(&scratch.provider.list(&scratch.root))
        .expect("list")
        .into_iter()
        .find(|entry| entry.uri == at)
        .expect("listed")
        .stamp;
    let second = await_job(&scratch.provider.stat(&at))
        .expect("stat")
        .expect("present")
        .stamp;

    assert_eq!(first, second);
    assert_eq!(first, listed, "list and stat must report the same stamp");
}

#[test]
fn if_absent_conflicts_on_an_existing_file_without_touching_it() {
    let scratch = Scratch::new("ifabsent");
    let at = scratch.root.join("a.bin");

    let created = await_job(&scratch.provider.write(
        &at,
        b"first".to_vec(),
        Precondition::IfAbsent,
    ))
    .expect("IfAbsent on a free name");

    assert_eq!(
        await_job(
            &scratch
                .provider
                .write(&at, b"second".to_vec(), Precondition::IfAbsent)
        ),
        Err(StorageError::Conflict {
            expected: None,
            actual: Some(created),
        })
    );
    assert_eq!(
        await_job(&scratch.provider.read(&at)).expect("read"),
        b"first".to_vec()
    );
}

#[test]
fn if_match_with_a_stale_stamp_conflicts_and_writes_nothing() {
    let scratch = Scratch::new("ifmatch");
    let at = scratch.root.join("a.bin");

    let stale = scratch.write(&at, b"one");
    let current = scratch.write(&at, b"two");

    assert_eq!(
        await_job(&scratch.provider.write(
            &at,
            b"three".to_vec(),
            Precondition::IfMatch(stale.clone())
        )),
        Err(StorageError::Conflict {
            expected: Some(stale),
            actual: Some(current.clone()),
        })
    );
    assert_eq!(
        await_job(&scratch.provider.read(&at)).expect("read"),
        b"two".to_vec()
    );

    await_job(
        &scratch
            .provider
            .write(&at, b"three".to_vec(), Precondition::IfMatch(current)),
    )
    .expect("the current stamp must be accepted");
}

#[test]
fn non_empty_directory_cannot_be_deleted() {
    let scratch = Scratch::new("rmdir");
    let dir = scratch.root.join("d");
    await_job(&scratch.provider.create_dir(&dir)).expect("create_dir");
    let inside = dir.join("f.bin");
    scratch.write(&inside, b"x");

    assert!(
        matches!(
            await_job(&scratch.provider.delete(&dir)),
            Err(StorageError::Io(_))
        ),
        "deleting a non-empty directory must fail"
    );
    assert_eq!(
        await_job(&scratch.provider.read(&inside)).expect("read"),
        b"x".to_vec()
    );

    await_job(&scratch.provider.delete(&inside)).expect("delete file");
    await_job(&scratch.provider.delete(&dir)).expect("delete empty dir");
    assert_eq!(await_job(&scratch.provider.stat(&dir)), Ok(None));
}

/// Reading a directory reports `NotFound` like `MemoryProvider`, rather than
/// leaking the platform's `IsADirectory` / `PermissionDenied` difference.
#[test]
fn reading_a_directory_is_not_found() {
    let scratch = Scratch::new("readdir");
    let dir = scratch.root.join("d");
    await_job(&scratch.provider.create_dir(&dir)).expect("create_dir");

    assert_eq!(
        await_job(&scratch.provider.read(&dir)),
        Err(StorageError::NotFound)
    );
    // …and the directory still stats as a directory, with no stamp.
    let entry = await_job(&scratch.provider.stat(&dir))
        .expect("stat")
        .expect("present");
    assert!(entry.is_dir);
    assert_eq!(entry.stamp, None);
}

/// The temp-file dance must not leave droppings: after successful writes the
/// directory holds exactly the target file.
#[test]
fn atomic_write_leaves_no_temp_files_behind() {
    let scratch = Scratch::new("temp");
    let at = scratch.root.join("a.bin");
    scratch.write(&at, b"one");
    scratch.write(&at, b"two-longer");

    // Read the raw directory rather than `list`, which hides temp names.
    let dir = scratch.root.to_local_path().expect("native path");
    let names: Vec<String> = fs::read_dir(&dir)
        .expect("read scratch")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["a.bin".to_string()], "stray files: {names:?}");
    assert_eq!(
        await_job(&scratch.provider.read(&at)).expect("read"),
        b"two-longer".to_vec()
    );
}

/// A write rejected before it starts changes nothing on disk.
#[test]
fn a_rejected_write_creates_nothing() {
    let scratch = Scratch::new("reject");
    let dir = scratch.root.join("d");
    await_job(&scratch.provider.create_dir(&dir)).expect("create_dir");

    assert!(matches!(
        await_job(
            &scratch
                .provider
                .write(&dir, b"x".to_vec(), Precondition::None)
        ),
        Err(StorageError::Io(_))
    ));
    assert!(await_job(&scratch.provider.list(&dir))
        .expect("list")
        .is_empty());
}
