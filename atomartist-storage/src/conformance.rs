//! Provider conformance suite.
//!
//! One set of checks every [`StorageProvider`] must pass, shipped as public
//! API (not `#[cfg(test)]`) so a third-party provider crate — Dropbox, S3,
//! WebDAV, a MatterHackers cloud build — can call it from its own tests and
//! prove it honours the contract, including error shapes.
//!
//! Usage:
//!
//! ```
//! use atomartist_storage::{conformance, MemoryProvider, StorageProvider};
//!
//! let provider = MemoryProvider::new("mem", "Memory");
//! let root = provider.root();
//! conformance::run_conformance(&provider, &root);
//! ```
//!
//! Failures are reported by panicking, the way `assert!` does, so a failing
//! check names itself in the test output. Checks that a provider's
//! [`Capabilities`](crate::Capabilities) rule out (read-only, no listing, no
//! directories, unversioned) skip themselves.
//!
//! ## Platform scope
//!
//! The checks are `async` so that one suite serves both worlds:
//!
//! - **native** — [`run_conformance`] drives them with [`block_on`], which
//!   busy-polls exactly as `await_job` always has. Providers that complete
//!   inline or on a worker thread need nothing else.
//! - **wasm** — a genuinely asynchronous provider (`BrowserProvider`, whose
//!   jobs only settle when the browser event loop delivers a promise) is
//!   checked by awaiting [`run_conformance_async`] from a
//!   `wasm-bindgen-test`. Blocking is impossible there, which is why the
//!   suite could not stay synchronous. See `atomartist-storage/README.md`
//!   for the command that runs it.

mod settle;

#[cfg(not(target_arch = "wasm32"))]
pub use settle::{await_job, block_on};
pub use settle::{settle, Settle};

use crate::error::StorageError;
use crate::provider::{Entry, Precondition, Stamp, StorageProvider};
use crate::uri::StorageUri;

/// Run every conformance check against `provider`, using `root` as a
/// scratch directory. `root` must already exist and be writable; the suite
/// removes what it creates.
///
/// Native only — it blocks. On wasm, await [`run_conformance_async`].
#[cfg(not(target_arch = "wasm32"))]
pub fn run_conformance(provider: &dyn StorageProvider, root: &StorageUri) {
    block_on(run_conformance_async(provider, root));
}

/// [`run_conformance`] for callers that already have an executor — the
/// browser event loop, or a test harness driving futures.
pub async fn run_conformance_async(provider: &dyn StorageProvider, root: &StorageUri) {
    write_read_round_trip(provider, root).await;
    list_shows_written_file(provider, root).await;
    overwrite_replaces_contents_and_updates_stamp(provider, root).await;
    if_match_stale_stamp_conflicts(provider, root).await;
    if_match_on_missing_file_conflicts(provider, root).await;
    if_absent_on_existing_file_conflicts(provider, root).await;
    delete_then_read_is_not_found(provider, root).await;
    stat_missing_file_is_none(provider, root).await;
    create_dir_then_list(provider, root).await;
    file_ancestor_blocks_nested_paths(provider, root).await;
    unversioned_provider_rejects_preconditions(provider, root).await;
    traversal_uris_cannot_reach_the_provider(provider, root).await;
}

/// No URI a provider can be handed contains a `.` or `..` segment, so
/// `read` / `write` / `stat` / `list` / `delete` cannot be aimed outside
/// the provider's root by traversal — the value simply cannot be built.
///
/// This is a property of [`StorageUri`] rather than of any one backend,
/// which is exactly why it belongs here: it is the reason a rooted
/// provider (OPFS, a per-account cloud prefix) is allowed to trust the
/// path it is given, and it must keep holding for every provider anyone
/// writes. See `docs/storage-architecture-plan.md` §13 open question 6.
///
/// ## What this verifies, and what it cannot
///
/// **Verifies:** every traversal spelling is rejected by `FromStr`,
/// `try_new` and `try_join`, so no such value exists to hand to
/// `provider`; and that `root` — built by the caller with the same
/// constructors — is a real location this provider answers `stat` for, so
/// the rejections above are not vacuously passing against a bogus scheme.
///
/// **Cannot verify:** that a given provider would refuse to *resolve* a
/// traversal path, because there is no way to express one to it. That is
/// the design, not a gap in the check: the guarantee is enforced by the
/// type, once, instead of by each backend remembering to re-check. A
/// provider that concatenates URI paths with attacker-controlled strings
/// of its own is outside what this suite can see.
pub async fn traversal_uris_cannot_reach_the_provider(
    provider: &dyn StorageProvider,
    root: &StorageUri,
) {
    // The root the caller built must be a location this provider actually
    // serves; otherwise the rejections below prove nothing about it.
    settle(&provider.stat(root))
        .await
        .expect("stat on the conformance root should succeed");

    let scheme = root.scheme();
    for path in ["/a/../b", "/../up", "/./x", "/.."] {
        let text = format!("{scheme}://{path}");
        assert!(
            text.parse::<StorageUri>().is_err(),
            "`{text}` must not parse into a URI this provider could be handed"
        );
        assert!(
            StorageUri::try_new(scheme, path).is_err(),
            "`{scheme}` + `{path}` must not construct"
        );
    }
    for child in ["..", "../escape", "./here", "a/../../escape"] {
        assert!(
            root.try_join(child).is_err(),
            "joining `{child}` onto the provider root must not construct"
        );
    }
}

/// Bytes written come back byte-identical, and the write reports a stamp.
pub async fn write_read_round_trip(provider: &dyn StorageProvider, root: &StorageUri) {
    if !provider.capabilities().writable {
        return;
    }
    let at = root.join("conformance-round-trip.bin");
    remove_if_present(provider, &at).await;

    let payload: Vec<u8> = (0u8..=255).collect();
    let stamp = settle(&provider.write(&at, payload.clone(), Precondition::None))
        .await
        .expect("write should succeed");

    let read_back = settle(&provider.read(&at)).await.expect("read should succeed");
    assert_eq!(read_back, payload, "read must return the bytes written");

    let entry = settle(&provider.stat(&at))
        .await
        .expect("stat should succeed")
        .expect("stat must find the file just written");
    assert!(!entry.is_dir, "a written blob must not stat as a directory");
    if let Some(size) = entry.size {
        assert_eq!(size as usize, payload.len(), "stat size must match payload");
    }
    if let Some(entry_stamp) = entry.stamp {
        assert_eq!(
            entry_stamp, stamp,
            "stat must report the stamp the write returned"
        );
    }

    remove_if_present(provider, &at).await;
}

/// A written file appears in its parent's listing exactly once.
pub async fn list_shows_written_file(provider: &dyn StorageProvider, root: &StorageUri) {
    let caps = provider.capabilities();
    if !caps.can_list || !caps.writable {
        return;
    }
    let name = "conformance-listed.bin";
    let at = root.join(name);
    remove_if_present(provider, &at).await;

    settle(&provider.write(&at, b"listed".to_vec(), Precondition::None))
        .await
        .expect("write should succeed");

    let listing = settle(&provider.list(root))
        .await
        .expect("list should succeed");
    let matches: Vec<&Entry> = listing.iter().filter(|e| e.uri == at).collect();
    assert_eq!(
        matches.len(),
        1,
        "listing must contain the written file exactly once: {:?}",
        listing
            .iter()
            .map(|e| e.uri.to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(matches[0].name, name, "entry name must be the last segment");
    assert!(!matches[0].is_dir);

    remove_if_present(provider, &at).await;
}

/// Rewriting a file changes its contents, and — for a provider that claims
/// to be versioned — its stamp.
///
/// The stamp half is gated on `versioned` because that flag is precisely
/// the promise that stored versions can be compared. An unversioned
/// backend (OPFS, whose only metadata is `lastModified` + size) can
/// legitimately produce the same stamp for two same-length writes inside
/// one clock tick; demanding otherwise would force it to either lie or
/// spin until the wall clock moves.
pub async fn overwrite_replaces_contents_and_updates_stamp(
    provider: &dyn StorageProvider,
    root: &StorageUri,
) {
    let caps = provider.capabilities();
    if !caps.writable {
        return;
    }
    let at = root.join("conformance-overwrite.bin");
    remove_if_present(provider, &at).await;

    let first = settle(&provider.write(&at, b"one".to_vec(), Precondition::None))
        .await
        .expect("first write should succeed");
    let second = settle(&provider.write(&at, b"two".to_vec(), Precondition::None))
        .await
        .expect("overwrite should succeed");

    if caps.versioned {
        assert_ne!(
            first, second,
            "a versioned provider must produce a new stamp for an overwrite"
        );
    }
    assert_eq!(
        settle(&provider.read(&at)).await.expect("read should succeed"),
        b"two".to_vec(),
        "read must return the newest bytes"
    );

    remove_if_present(provider, &at).await;
}

/// `IfMatch` with a stale stamp fails with `Conflict`, and nothing is written.
pub async fn if_match_stale_stamp_conflicts(provider: &dyn StorageProvider, root: &StorageUri) {
    let caps = provider.capabilities();
    if !caps.versioned || !caps.writable {
        return;
    }
    let at = root.join("conformance-ifmatch.bin");
    remove_if_present(provider, &at).await;

    let stale = settle(&provider.write(&at, b"one".to_vec(), Precondition::None))
        .await
        .expect("first write should succeed");
    let current = settle(&provider.write(&at, b"two".to_vec(), Precondition::None))
        .await
        .expect("second write should succeed");

    let err = settle(&provider.write(&at, b"three".to_vec(), Precondition::IfMatch(stale.clone())))
        .await
        .expect_err("writing against a stale stamp must fail");
    match err {
        StorageError::Conflict { expected, actual } => {
            assert_eq!(
                expected,
                Some(stale),
                "conflict must echo the expected stamp"
            );
            // `actual: None` is legitimate — an HTTP 412 need not carry an
            // ETag — but a reported stamp must be the truth.
            if let Some(actual) = actual {
                assert_eq!(
                    actual, current,
                    "a reported `actual` must be the stamp actually stored"
                );
            }
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(
        settle(&provider.read(&at)).await.expect("read should succeed"),
        b"two".to_vec(),
        "a rejected write must not modify the stored bytes"
    );

    // The same write with the fresh stamp must succeed.
    let latest = settle(&provider.stat(&at))
        .await
        .expect("stat should succeed")
        .and_then(|entry| entry.stamp)
        .expect("a versioned provider must report stamps from stat");
    settle(&provider.write(&at, b"three".to_vec(), Precondition::IfMatch(latest)))
        .await
        .expect("writing against the current stamp must succeed");

    remove_if_present(provider, &at).await;
}

/// `IfMatch` against a target that does not exist is a conflict, never a
/// silent create: the caller's stamp cannot match "nothing stored".
pub async fn if_match_on_missing_file_conflicts(provider: &dyn StorageProvider, root: &StorageUri) {
    let caps = provider.capabilities();
    if !caps.versioned || !caps.writable {
        return;
    }
    let at = root.join("conformance-ifmatch-missing.bin");
    remove_if_present(provider, &at).await;

    let expected = Stamp::new("conformance-stamp-that-cannot-exist");
    let err = settle(&provider.write(
        &at,
        b"nope".to_vec(),
        Precondition::IfMatch(expected.clone()),
    ))
    .await
    .expect_err("IfMatch against a missing file must fail");
    match err {
        StorageError::Conflict {
            expected: e,
            actual,
        } => {
            assert_eq!(e, Some(expected), "conflict must echo the expected stamp");
            assert_eq!(actual, None, "nothing is stored, so there is no stamp");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(
        settle(&provider.stat(&at)).await.expect("stat should succeed"),
        None,
        "a rejected IfMatch must not create the file"
    );
}

/// `IfAbsent` succeeds on a free name and conflicts on an existing one.
pub async fn if_absent_on_existing_file_conflicts(
    provider: &dyn StorageProvider,
    root: &StorageUri,
) {
    let caps = provider.capabilities();
    if !caps.versioned || !caps.writable {
        return;
    }
    let at = root.join("conformance-ifabsent.bin");
    remove_if_present(provider, &at).await;

    let created = settle(&provider.write(&at, b"first".to_vec(), Precondition::IfAbsent))
        .await
        .expect("IfAbsent must succeed when nothing is stored");

    let err = settle(&provider.write(&at, b"second".to_vec(), Precondition::IfAbsent))
        .await
        .expect_err("IfAbsent must fail when the file exists");
    match err {
        StorageError::Conflict { expected, actual } => {
            assert_eq!(
                expected, None,
                "an IfAbsent conflict expected *nothing* to be stored"
            );
            if let Some(actual) = actual {
                assert_eq!(
                    actual, created,
                    "a reported `actual` must be the stamp actually stored"
                );
            }
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(
        settle(&provider.read(&at)).await.expect("read should succeed"),
        b"first".to_vec(),
        "a rejected IfAbsent write must not modify the stored bytes"
    );

    remove_if_present(provider, &at).await;
}

/// After a delete, reads fail with `NotFound` and stat reports `None`.
pub async fn delete_then_read_is_not_found(provider: &dyn StorageProvider, root: &StorageUri) {
    if !provider.capabilities().writable {
        return;
    }
    let at = root.join("conformance-delete.bin");
    remove_if_present(provider, &at).await;

    settle(&provider.write(&at, b"bye".to_vec(), Precondition::None))
        .await
        .expect("write should succeed");
    settle(&provider.delete(&at))
        .await
        .expect("delete should succeed");

    match settle(&provider.read(&at)).await {
        Err(StorageError::NotFound) => {}
        Err(other) => panic!("expected NotFound after delete, got {other:?}"),
        Ok(_) => panic!("read must fail after the file is deleted"),
    }
    assert_eq!(
        settle(&provider.stat(&at)).await.expect("stat should succeed"),
        None,
        "stat must report None after delete"
    );
    match settle(&provider.delete(&at)).await {
        Err(StorageError::NotFound) => {}
        Err(other) => panic!("expected NotFound deleting twice, got {other:?}"),
        Ok(()) => panic!("deleting a missing file must fail with NotFound"),
    }
}

/// `stat` distinguishes "absent" (`Ok(None)`) from a failure, while `read`
/// on the same URI fails with `NotFound`.
pub async fn stat_missing_file_is_none(provider: &dyn StorageProvider, root: &StorageUri) {
    let at = root.join("conformance-never-written.bin");
    if provider.capabilities().writable {
        remove_if_present(provider, &at).await;
    }

    assert_eq!(
        settle(&provider.stat(&at))
            .await
            .expect("stat of a missing file must not error"),
        None
    );
    match settle(&provider.read(&at)).await {
        Err(StorageError::NotFound) => {}
        Err(other) => panic!("expected NotFound reading a missing file, got {other:?}"),
        Ok(_) => panic!("reading a missing file must fail"),
    }
}

/// A created directory lists as a directory and can hold files.
pub async fn create_dir_then_list(provider: &dyn StorageProvider, root: &StorageUri) {
    let caps = provider.capabilities();
    if !caps.can_create_dir || !caps.can_list || !caps.writable {
        return;
    }
    let dir = root.join("conformance-dir");
    let file = dir.join("inside.bin");
    remove_if_present(provider, &file).await;
    remove_if_present(provider, &dir).await;

    settle(&provider.create_dir(&dir))
        .await
        .expect("create_dir should succeed");

    let listing = settle(&provider.list(root))
        .await
        .expect("list should succeed");
    let entry = listing
        .iter()
        .find(|e| e.uri == dir)
        .expect("the new directory must appear in its parent's listing");
    assert!(entry.is_dir, "a created directory must list as a directory");

    settle(&provider.write(&file, b"inside".to_vec(), Precondition::None))
        .await
        .expect("writing inside the new directory should succeed");
    let inner = settle(&provider.list(&dir))
        .await
        .expect("listing the new directory should succeed");
    assert_eq!(inner.len(), 1, "the new directory holds exactly one file");
    assert_eq!(inner[0].uri, file);

    remove_if_present(provider, &file).await;
    remove_if_present(provider, &dir).await;
}

/// A stored file may not become a directory by implication: writing or
/// creating a directory beneath an existing file must fail and change
/// nothing, so a path is never both a file and a directory (which would let
/// the same name appear twice in one listing).
pub async fn file_ancestor_blocks_nested_paths(provider: &dyn StorageProvider, root: &StorageUri) {
    let caps = provider.capabilities();
    if !caps.writable {
        return;
    }
    let file = root.join("conformance-ancestor.bin");
    let nested = file.join("nested.bin");
    remove_if_present(provider, &nested).await;
    remove_if_present(provider, &file).await;

    settle(&provider.write(&file, b"leaf".to_vec(), Precondition::None))
        .await
        .expect("write should succeed");

    match settle(&provider.write(&nested, b"nope".to_vec(), Precondition::None)).await {
        Err(StorageError::Io(_)) => {}
        Err(other) => panic!("expected Io writing under a file ancestor, got {other:?}"),
        Ok(_) => panic!("writing under a file ancestor must fail"),
    }
    if caps.can_create_dir {
        match settle(&provider.create_dir(&nested)).await {
            Err(StorageError::Io(_)) => {}
            Err(other) => panic!("expected Io creating a dir under a file, got {other:?}"),
            Ok(()) => panic!("creating a directory under a file ancestor must fail"),
        }
    }

    if caps.can_list {
        let listing = settle(&provider.list(root))
            .await
            .expect("list should succeed");
        let matches: Vec<&Entry> = listing.iter().filter(|e| e.uri == file).collect();
        assert_eq!(
            matches.len(),
            1,
            "the file must appear exactly once, never as both file and directory"
        );
        assert!(!matches[0].is_dir, "it must still be a file");
    }
    assert_eq!(
        settle(&provider.stat(&nested))
            .await
            .expect("stat should succeed"),
        None,
        "the rejected write must not have created anything"
    );

    remove_if_present(provider, &file).await;
}

/// A provider that reports `versioned: false` must reject preconditions it
/// cannot honour with [`StorageError::Unsupported`] rather than silently
/// ignoring them. Skipped (trivially) by versioned providers.
pub async fn unversioned_provider_rejects_preconditions(
    provider: &dyn StorageProvider,
    root: &StorageUri,
) {
    let caps = provider.capabilities();
    if caps.versioned || !caps.writable {
        return;
    }
    let at = root.join("conformance-unversioned.bin");
    remove_if_present(provider, &at).await;

    for pre in [
        Precondition::IfAbsent,
        Precondition::IfMatch(Stamp::new("anything")),
    ] {
        match settle(&provider.write(&at, b"x".to_vec(), pre.clone())).await {
            Err(StorageError::Unsupported) => {}
            Err(other) => panic!("expected Unsupported for {pre:?}, got {other:?}"),
            Ok(_) => panic!("an unversioned provider must not silently ignore {pre:?}"),
        }
    }
    assert_eq!(
        settle(&provider.stat(&at)).await.expect("stat should succeed"),
        None,
        "a rejected precondition must not write anything"
    );
}

/// Best-effort cleanup: delete `at`, tolerating "it was not there".
async fn remove_if_present(provider: &dyn StorageProvider, at: &StorageUri) {
    match settle(&provider.delete(at)).await {
        Ok(()) | Err(StorageError::NotFound) => {}
        Err(other) => panic!("cleanup delete of {at} failed unexpectedly: {other:?}"),
    }
}
