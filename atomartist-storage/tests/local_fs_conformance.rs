//! Runs the public provider conformance suite against `LocalFsProvider`.
//!
//! The companion to `memory_conformance.rs`: the same checks, the same
//! `&dyn StorageProvider` entry points, but against a real filesystem. The
//! scratch tree lives under `std::env::temp_dir()` in a directory named after
//! this process, so concurrent test binaries never collide, and it is removed
//! afterwards.
//!
//! Everything here goes through the provider — no `std::fs` — which doubles
//! as proof that the trait is sufficient for real work.

use std::sync::Arc;

use atomartist_storage::conformance::{await_job, run_conformance};
use atomartist_storage::{
    Capabilities, LocalFsProvider, Precondition, StorageError, StorageProvider, StorageRegistry,
    StorageUri,
};

/// Provider-managed scratch directory, removed when the guard drops.
struct Scratch {
    provider: LocalFsProvider,
    root: StorageUri,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "atomartist-storage-conformance-{}-{label}",
            std::process::id()
        ));
        let provider = LocalFsProvider::new();
        let root = StorageUri::from_local_path(&path).expect("temp dir has a URI form");
        remove_tree(&provider, &root);
        await_job(&provider.create_dir(&root)).expect("scratch directory");
        Scratch { provider, root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove_tree(&self.provider, &self.root);
    }
}

/// Depth-first delete through the provider itself; tolerates a missing tree.
fn remove_tree(provider: &LocalFsProvider, at: &StorageUri) {
    if let Ok(children) = await_job(&provider.list(at)) {
        for child in children {
            remove_tree(provider, &child.uri);
        }
    }
    let _ = await_job(&provider.delete(at));
}

#[test]
fn local_fs_provider_passes_conformance() {
    let scratch = Scratch::new("root");
    run_conformance(&scratch.provider, &scratch.root);
}

#[test]
fn local_fs_provider_passes_conformance_in_a_subdirectory() {
    let scratch = Scratch::new("subdir");
    let nested = scratch.root.join("projects/2026");
    await_job(&scratch.provider.create_dir(&nested)).expect("nested scratch directory");
    run_conformance(&scratch.provider, &nested);
}

#[test]
fn conformance_runs_through_a_dyn_provider_from_the_registry() {
    let scratch = Scratch::new("registry");
    let mut registry = StorageRegistry::new();
    registry.register(Arc::new(LocalFsProvider::new())).unwrap();

    let provider = registry
        .resolve(&scratch.root)
        .expect("file scheme is registered");
    run_conformance(provider.as_ref(), &scratch.root);
}

#[test]
fn local_fs_provider_advertises_the_capabilities_the_suite_relies_on() {
    let provider = LocalFsProvider::new();
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
    assert_eq!(provider.scheme(), "file");
    assert_eq!(provider.display_name(), "This PC");
}

#[test]
fn entries_report_size_and_modified_metadata() {
    let scratch = Scratch::new("metadata");
    let at = scratch.root.join("meta.bin");
    await_job(&scratch.provider.write(&at, vec![7; 12], Precondition::None)).unwrap();

    let entry = await_job(&scratch.provider.stat(&at))
        .unwrap()
        .expect("just written");
    assert_eq!(entry.size, Some(12));
    assert!(entry.modified.is_some(), "the filesystem has an mtime");
    assert_eq!(entry.name, "meta.bin");
    assert_eq!(entry.uri, at, "stat must echo the caller's URI");
    assert!(entry.stamp.is_some(), "a versioned provider reports stamps");
}

/// Listing a path that is not a directory reports `NotFound`, matching
/// `MemoryProvider` — "there is no directory here".
#[test]
fn listing_a_file_or_a_missing_directory_is_not_found() {
    let scratch = Scratch::new("listing");
    let at = scratch.root.join("a.bin");
    await_job(
        &scratch
            .provider
            .write(&at, b"x".to_vec(), Precondition::None),
    )
    .unwrap();

    assert_eq!(
        await_job(&scratch.provider.list(&at)),
        Err(StorageError::NotFound)
    );
    assert_eq!(
        await_job(&scratch.provider.list(&scratch.root.join("nope"))),
        Err(StorageError::NotFound)
    );
}
