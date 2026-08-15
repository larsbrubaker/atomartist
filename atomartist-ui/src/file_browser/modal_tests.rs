//! Unit tests for the widget-free parts of the Open/Save modal: the pick
//! resolution rules and the handle's queueing contract. Everything that
//! needs real events (clicking OK, Escape, input swallowing) is driven
//! through `atomartist-ui-test`'s `TestHarness` instead, since the host is
//! now part of `build_app`'s tree.

use std::sync::Arc;

use atomartist_storage::{MemoryProvider, Precondition, StorageProvider, StorageRegistry};

use super::*;

fn state_with_memory() -> AppState {
    let provider = Arc::new(MemoryProvider::new("mem", "Test Memory"));
    for path in ["/alpha.atmr", "/notes.txt"] {
        provider
            .write(
                &StorageUri::new("mem", path),
                b"seed".to_vec(),
                Precondition::None,
            )
            .take()
            .expect("memory writes settle inline")
            .expect("seed write succeeds");
    }
    let mut registry = StorageRegistry::new();
    registry
        .register(provider as Arc<dyn StorageProvider>)
        .expect("fresh registry accepts the memory provider");
    AppState::with_storage(
        atomartist_lib::Graph::new(),
        atomartist_lib::registry::NodeRegistry::new(),
        Arc::new(registry),
    )
}

#[test]
fn open_mode_resolves_only_a_selected_file() {
    let state = state_with_memory();
    let model = BrowserModel::opened_on(&state);

    assert_eq!(resolve_pick(BrowserMode::Open, &model, ""), None);

    model.select(Some(StorageUri::new("mem", "/alpha.atmr")));
    assert_eq!(
        resolve_pick(BrowserMode::Open, &model, ""),
        Some(StorageUri::new("mem", "/alpha.atmr"))
    );

    // A selection that is not in the listing resolves to nothing —
    // `selected_entry`, not the raw `selected()` URI.
    model.select(Some(StorageUri::new("mem", "/ghost.atmr")));
    assert_eq!(resolve_pick(BrowserMode::Open, &model, ""), None);
}

#[test]
fn save_mode_joins_the_name_onto_the_current_directory() {
    let state = state_with_memory();
    let model = BrowserModel::opened_on(&state);

    assert_eq!(
        resolve_pick(BrowserMode::Save, &model, "design"),
        Some(StorageUri::new("mem", "/design.atmr"))
    );
    // Surrounding whitespace is not part of a file name.
    assert_eq!(
        resolve_pick(BrowserMode::Save, &model, "  design  "),
        Some(StorageUri::new("mem", "/design.atmr"))
    );
}

/// The picker saves *projects*, so whatever the user types comes back
/// with the project extension — and comes back naming a file the app's
/// own [`uri_extension`] rule agrees is an `.atmr`.
#[test]
fn save_mode_forces_the_project_extension() {
    let state = state_with_memory();
    let model = BrowserModel::opened_on(&state);
    let save = |name: &str| resolve_pick(BrowserMode::Save, &model, name);

    // A dot inside the name is part of the name, not a chosen extension.
    assert_eq!(
        save("Version 1.2"),
        Some(StorageUri::new("mem", "/Version 1.2.atmr"))
    );
    // Trailing dots are trimmed — Windows will not create such a file.
    assert_eq!(
        save("design."),
        Some(StorageUri::new("mem", "/design.atmr"))
    );
    // A leading dot starts the *name*: `.atmr` has no extension at all
    // (the dotfile rule `split_stem_ext` documents), so it gets one.
    assert_eq!(save(".atmr"), Some(StorageUri::new("mem", "/.atmr.atmr")));
    // Already an `.atmr`, in either case: left exactly as typed.
    assert_eq!(
        save("bracket.atmr"),
        Some(StorageUri::new("mem", "/bracket.atmr"))
    );
    assert_eq!(
        save("BRACKET.ATMR"),
        Some(StorageUri::new("mem", "/BRACKET.ATMR"))
    );
    // Some other extension is a name, not a format choice — this picker
    // has exactly one format.
    assert_eq!(
        save("design.stl"),
        Some(StorageUri::new("mem", "/design.stl.atmr"))
    );

    for name in [
        "Version 1.2",
        "design.",
        ".atmr",
        "bracket.atmr",
        "BRACKET.ATMR",
    ] {
        let uri = save(name).expect("every case above resolves");
        assert_eq!(
            crate::app_state_storage::uri_extension(&uri),
            "atmr",
            "`{name}` must resolve to something the app reads as a project"
        );
    }
}

#[test]
fn save_mode_refuses_empty_traversing_and_path_like_names() {
    let state = state_with_memory();
    let model = BrowserModel::opened_on(&state);

    assert_eq!(resolve_pick(BrowserMode::Save, &model, ""), None);
    assert_eq!(resolve_pick(BrowserMode::Save, &model, "   "), None);
    // `try_join` rejects the escape; the modal must not fall back to
    // anything cleverer.
    assert_eq!(resolve_pick(BrowserMode::Save, &model, "../escape"), None);
    // Nothing survives the trim.
    assert_eq!(resolve_pick(BrowserMode::Save, &model, "."), None);
    assert_eq!(resolve_pick(BrowserMode::Save, &model, "..."), None);
    // The name field names a file in the directory on screen; navigating
    // is the browser's job, so a separator is refused rather than
    // silently obeyed.
    assert_eq!(resolve_pick(BrowserMode::Save, &model, "docs/inner"), None);
    assert_eq!(resolve_pick(BrowserMode::Save, &model, "docs\\inner"), None);
}

#[test]
fn a_second_open_while_one_is_queued_is_refused_as_cancelled() {
    let handle = FileBrowserModalHandle::new();
    let first = handle.open(BrowserMode::Open, "");
    assert!(handle.is_open());
    assert!(
        first.poll().is_pending(),
        "the first request waits for the host"
    );

    let second = handle.open(BrowserMode::Save, "other");
    assert_eq!(
        second.take(),
        Some(Err(StorageError::Cancelled)),
        "a stacked open fails rather than reading as a user cancellation"
    );
    assert!(first.poll().is_pending(), "and leaves the first one alone");
}

/// A dialog that goes away without being answered — the window closing,
/// the whole tree being dropped — cancels its job instead of leaving the
/// caller with the completer's "worker dropped" `Io` error.
#[test]
fn dropping_a_live_session_cancels_its_job() {
    let (job, completer) = Job::pending();
    let session = Session {
        visible: Rc::new(Cell::new(true)),
        outcome: Rc::new(RefCell::new(None)),
        completer: Some(completer),
    };

    drop(session);

    assert_eq!(job.take(), Some(Ok(None)));
}
