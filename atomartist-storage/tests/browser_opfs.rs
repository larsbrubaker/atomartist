//! Runs the public provider conformance suite against `BrowserProvider`.
//!
//! The wasm sibling of `memory_conformance.rs` / `local_fs_conformance.rs`.
//! It needs a real browser — OPFS is a browser API — so it is **not** part
//! of `cargo test --workspace`; the whole file compiles out on native. Run
//! it with a headless Chrome:
//!
//! ```text
//! cargo install wasm-bindgen-cli          # once, provides the test runner
//! cargo test -p atomartist-storage --target wasm32-unknown-unknown --test browser_opfs
//! ```
//!
//! (`wasm-pack test --headless --chrome atomartist-storage` works too.)
//! `CHROMEDRIVER` must point at a chromedriver matching the installed
//! Chrome. See `atomartist-storage/README.md`.
//!
//! Everything runs inside a scratch directory under the origin's private
//! root so a failed run cannot eat a real project, and the scratch tree is
//! removed afterwards.

#![cfg(target_arch = "wasm32")]

use atomartist_storage::conformance::{run_conformance_async, settle};
use atomartist_storage::{
    BrowserProvider, Capabilities, Precondition, StorageError, StorageProvider, StorageUri,
};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// A fresh, empty scratch directory named after `label`, so tests that run
/// in the same origin cannot collide.
async fn scratch(provider: &BrowserProvider, label: &str) -> StorageUri {
    let root = provider.root().join("atomartist-conformance").join(label);
    remove_tree(provider, &root).await;
    settle(&provider.create_dir(&root))
        .await
        .expect("scratch directory");
    root
}

/// Best-effort recursive delete: the provider deliberately refuses to
/// remove a non-empty directory, so children go first.
async fn remove_tree(provider: &BrowserProvider, at: &StorageUri) {
    if let Ok(entries) = settle(&provider.list(at)).await {
        for entry in entries {
            if entry.is_dir {
                Box::pin(remove_tree(provider, &entry.uri)).await;
            } else {
                let _ = settle(&provider.delete(&entry.uri)).await;
            }
        }
    }
    let _ = settle(&provider.delete(at)).await;
}

#[wasm_bindgen_test]
async fn browser_provider_passes_conformance() {
    let provider = BrowserProvider::new();
    let root = scratch(&provider, "suite").await;
    run_conformance_async(&provider, &root).await;
    remove_tree(&provider, &root).await;
}

#[wasm_bindgen_test]
async fn browser_provider_advertises_the_capabilities_the_suite_relies_on() {
    let provider = BrowserProvider::new();
    assert_eq!(
        provider.capabilities(),
        Capabilities {
            writable: true,
            can_list: true,
            can_create_dir: true,
            // OPFS has no ETag: see the `browser/opfs.rs` header.
            versioned: false,
            max_blob_bytes: None,
            requires_auth: false,
        }
    );
    assert_eq!(provider.scheme(), "browser");
    assert_eq!(provider.display_name(), "This Browser");
    assert!(provider.native_picker().is_none());
    assert_eq!(provider.root().to_string(), "browser:///");
}

/// A save into a browser that has never seen the directory must work
/// without any mkdir step — this is the flow `demo-wasm`'s default save
/// target relies on.
#[wasm_bindgen_test]
async fn writing_creates_missing_parent_directories() {
    let provider = BrowserProvider::new();
    let root = scratch(&provider, "deep").await;
    let at = root.join("projects").join("nested").join("bracket.atmr");

    let stamp = settle(&provider.write(&at, b"project bytes".to_vec(), Precondition::None))
        .await
        .expect("write into a fresh tree");
    assert_eq!(
        settle(&provider.read(&at)).await.expect("read"),
        b"project bytes".to_vec()
    );

    let entry = settle(&provider.stat(&at))
        .await
        .expect("stat")
        .expect("just written");
    assert_eq!(entry.stamp, Some(stamp));
    assert_eq!(entry.size, Some(13));
    assert!(entry.modified.is_some(), "OPFS reports lastModified");

    let listing = settle(&provider.list(&root.join("projects")))
        .await
        .expect("list");
    assert_eq!(listing.len(), 1);
    assert!(listing[0].is_dir);
    assert_eq!(listing[0].name, "nested");

    remove_tree(&provider, &root).await;
}

/// A payload big enough that the write is not a single small chunk, read
/// back byte-for-byte. This is the check on `do_write` copying its bytes to
/// the JS heap: handing `write()` a `Uint8Array` view over wasm linear
/// memory would risk a detached buffer (thrown error, or wrong bytes
/// persisted) once the heap grows, and a one-megabyte project is where that
/// starts to matter.
#[wasm_bindgen_test]
async fn a_large_payload_round_trips_byte_for_byte() {
    let provider = BrowserProvider::new();
    let root = scratch(&provider, "large").await;
    let at = root.join("large.bin");

    let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    settle(&provider.write(&at, payload.clone(), Precondition::None))
        .await
        .expect("write a megabyte");

    let read_back = settle(&provider.read(&at)).await.expect("read");
    assert_eq!(read_back.len(), payload.len());
    assert!(read_back == payload, "every byte must survive the round trip");

    remove_tree(&provider, &root).await;
}

#[wasm_bindgen_test]
async fn a_uri_from_another_scheme_is_unsupported() {
    let provider = BrowserProvider::new();
    let foreign: StorageUri = "file:///C:/x.atmr".parse().unwrap();
    assert_eq!(
        settle(&provider.read(&foreign)).await,
        Err(StorageError::Unsupported)
    );
}

/// `versioned: false` is a promise to *refuse* preconditions, not to
/// ignore them. (The suite checks this too; pinned separately because it
/// is the one place a browser save could silently lose data.)
#[wasm_bindgen_test]
async fn preconditions_are_refused_rather_than_ignored() {
    let provider = BrowserProvider::new();
    let root = scratch(&provider, "preconditions").await;
    let at = root.join("guarded.bin");

    assert_eq!(
        settle(&provider.write(&at, b"x".to_vec(), Precondition::IfAbsent)).await,
        Err(StorageError::Unsupported)
    );
    assert_eq!(settle(&provider.stat(&at)).await, Ok(None));

    remove_tree(&provider, &root).await;
}
