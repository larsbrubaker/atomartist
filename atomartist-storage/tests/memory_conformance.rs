//! Runs the public provider conformance suite against `MemoryProvider`.
//!
//! This is both a test of `MemoryProvider` and a test of the suite itself:
//! every future provider (local filesystem, browser, HTTP) gets the same
//! treatment, and a third-party crate calls `run_conformance` exactly the way
//! this file does.
//!
//! Native only: `run_conformance` / `await_job` block, which a browser's
//! single thread cannot do. The wasm-side suite lives in `browser_opfs.rs`.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use atomartist_storage::conformance::{await_job, run_conformance};
use atomartist_storage::{
    Blob, Bytes, Capabilities, Entry, FlakyConfig, FlakyProvider, Job, MemoryProvider,
    Precondition, Stamp, StorageError, StorageProvider, StorageRegistry, StorageUri,
};

fn memory() -> MemoryProvider {
    MemoryProvider::new("mem", "Memory")
}

/// A provider that reports reduced capabilities and enforces them, used to
/// prove the suite's capability gating: a read-only or unversioned backend
/// must be able to run `run_conformance` without spurious failures.
struct LimitedProvider {
    inner: MemoryProvider,
    writable: bool,
    versioned: bool,
}

impl LimitedProvider {
    fn new(writable: bool, versioned: bool) -> Self {
        LimitedProvider {
            inner: memory(),
            writable,
            versioned,
        }
    }
}

impl StorageProvider for LimitedProvider {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }

    fn display_name(&self) -> &str {
        self.inner.display_name()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            writable: self.writable,
            versioned: self.versioned,
            can_create_dir: self.writable,
            ..Capabilities::default()
        }
    }

    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>> {
        self.inner.list(dir)
    }

    fn read(&self, at: &StorageUri) -> Job<Blob> {
        self.inner.read(at)
    }

    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp> {
        if !self.writable {
            return Job::failed(StorageError::PermissionDenied);
        }
        // The contract for `versioned: false`: never silently ignore a
        // precondition we cannot honour.
        if !self.versioned && pre != Precondition::None {
            return Job::failed(StorageError::Unsupported);
        }
        self.inner.write(at, bytes, pre)
    }

    fn delete(&self, at: &StorageUri) -> Job<()> {
        if !self.writable {
            return Job::failed(StorageError::PermissionDenied);
        }
        self.inner.delete(at)
    }

    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>> {
        self.inner.stat(at)
    }

    fn create_dir(&self, at: &StorageUri) -> Job<()> {
        if !self.writable {
            return Job::failed(StorageError::PermissionDenied);
        }
        self.inner.create_dir(at)
    }
}

#[test]
fn read_only_provider_passes_conformance() {
    let provider = LimitedProvider::new(false, false);
    let root = provider.inner.root();
    run_conformance(&provider, &root);
}

#[test]
fn unversioned_provider_must_reject_preconditions() {
    let provider = LimitedProvider::new(true, false);
    let root = provider.inner.root();
    run_conformance(&provider, &root);

    // And the check really does bite: a provider that silently ignored the
    // precondition would have written the file.
    let at = root.join("conformance-unversioned.bin");
    assert_eq!(
        await_job(&provider.write(&at, b"x".to_vec(), Precondition::IfAbsent)),
        Err(StorageError::Unsupported)
    );
    assert_eq!(await_job(&provider.stat(&at)), Ok(None));
}

#[test]
fn memory_provider_passes_conformance_at_the_root() {
    let provider = memory();
    let root = provider.root();
    run_conformance(&provider, &root);
}

#[test]
fn memory_provider_passes_conformance_in_a_subdirectory() {
    let provider = memory();
    let root = provider.root().join("projects");
    await_job(&provider.create_dir(&root)).unwrap();
    run_conformance(&provider, &root);
}

#[test]
fn conformance_runs_through_a_dyn_provider_from_the_registry() {
    // Exercises object safety end to end: the suite only ever sees
    // `&dyn StorageProvider`, resolved by scheme like the app will.
    let mut registry = StorageRegistry::new();
    registry.register(Arc::new(memory())).unwrap();

    let uri: StorageUri = "mem:///".parse().unwrap();
    let provider = registry.resolve(&uri).expect("mem scheme is registered");
    run_conformance(provider.as_ref(), &uri);
}

#[test]
fn flaky_wrapper_passes_conformance_when_configured_not_to_fail() {
    let inner = memory();
    let root = inner.root();
    let flaky = FlakyProvider::new(Arc::new(inner), FlakyConfig::default());
    run_conformance(&flaky, &root);
    assert!(
        flaky.call_count() > 0,
        "the suite must exercise the wrapper"
    );
}

#[test]
fn memory_provider_advertises_the_capabilities_the_suite_relies_on() {
    let provider = memory();
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
    assert!(provider.native_picker().is_none());
    assert_eq!(provider.scheme(), "mem");
    assert_eq!(provider.display_name(), "Memory");
}

#[test]
fn entries_report_size_and_modified_metadata() {
    let provider = memory();
    let at = provider.root().join("meta.bin");
    await_job(&provider.write(&at, vec![7; 12], Precondition::None)).unwrap();

    let entry = await_job(&provider.stat(&at))
        .unwrap()
        .expect("just written");
    assert_eq!(entry.size, Some(12));
    assert!(entry.modified.is_some(), "memory store has a fake clock");
    assert_eq!(entry.name, "meta.bin");
    assert_eq!(entry.uri, at);
}
