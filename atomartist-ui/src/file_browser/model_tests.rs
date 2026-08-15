//! Unit tests for [`crate::file_browser::model`].
//!
//! Two provider shapes, because they exercise different halves of the
//! contract: `MemoryProvider` settles inline (so a listing lands during
//! `refresh` itself), while `FlakyProvider` holds results until the test
//! advances *both* clocks — the provider's `pump()` and the app's
//! `pump_storage()`. Only the latter can produce two listings in flight at
//! once, which is what the generation guard exists for.
//!
//! Split out of `model.rs` with `#[path]` per the house convention, so
//! `use super::*` still reaches its private items.

use super::*;
use atomartist_storage::{
    FlakyConfig, FlakyProvider, MemoryProvider, Precondition, StorageProvider, StorageRegistry,
};

/// App state whose only provider is an in-memory store, plus that store.
fn sync_state() -> (AppState, Arc<MemoryProvider>) {
    let provider = Arc::new(MemoryProvider::new("mem", "Memory"));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    (state_with(registry), provider)
}

/// App state whose only provider delays every result by `latency` ticks.
fn flaky_state(config: FlakyConfig) -> (AppState, Arc<FlakyProvider>) {
    let provider = Arc::new(FlakyProvider::new(
        Arc::new(MemoryProvider::new("mem", "Memory")),
        config,
    ));
    let mut registry = StorageRegistry::new();
    registry
        .register(provider.clone() as Arc<dyn StorageProvider>)
        .expect("fresh registry");
    (state_with(registry), provider)
}

fn state_with(registry: StorageRegistry) -> AppState {
    AppState::with_storage(
        atomartist_lib::Graph::new(),
        atomartist_lib::registry::NodeRegistry::new(),
        Arc::new(registry),
    )
}

/// Advance both clocks `rounds` times — one simulated frame each.
fn frames(state: &AppState, provider: &Arc<FlakyProvider>, rounds: usize) {
    for _ in 0..rounds {
        provider.pump();
        state.pump_storage();
    }
}

fn uri(path: &str) -> StorageUri {
    StorageUri::new("mem", path)
}

/// Plant a file, creating its ancestors implicitly.
fn put(provider: &dyn StorageProvider, path: &str) {
    provider
        .write(&uri(path), b"x".to_vec(), Precondition::None)
        .take()
        .expect("memory writes settle inline")
        .expect("seed write succeeds");
}

fn names(model: &BrowserModel) -> Vec<String> {
    model
        .visible_entries()
        .into_iter()
        .map(|e| e.name)
        .collect()
}

/// The sidebar is the registry in registration order, each provider rooted
/// at its own `scheme:///`.
#[test]
fn roots_follow_registration_order() {
    let mut registry = StorageRegistry::new();
    registry
        .register(Arc::new(MemoryProvider::new("mem", "Memory")))
        .unwrap();
    registry
        .register(Arc::new(MemoryProvider::new("other", "Other Store")))
        .unwrap();
    let model = BrowserModel::new(&registry);

    let roots = model.roots();
    assert_eq!(
        roots.iter().map(|r| r.scheme.as_str()).collect::<Vec<_>>(),
        vec!["mem", "other"]
    );
    assert_eq!(roots[1].display_name, "Other Store");
    assert_eq!(roots[0].root, uri("/"));
    assert_eq!(
        model.cwd(),
        Some(uri("/")),
        "the browser opens on the first provider"
    );
}

/// A build with no providers has nowhere to browse — and must say so
/// rather than show an empty pane forever.
#[test]
fn a_registry_with_no_providers_reports_an_error_not_a_blank_pane() {
    let state = state_with(StorageRegistry::new());
    let model = BrowserModel::opened_on(&state);
    assert_eq!(model.cwd(), None);
    assert!(matches!(model.listing(), Listing::Error(_)));
    assert!(model.breadcrumbs().is_empty());
}

/// Navigating in and back out, over a synchronous provider: each step
/// lists the directory it lands on.
#[test]
fn entering_a_directory_and_going_up_lists_each_step() {
    let (state, provider) = sync_state();
    put(provider.as_ref(), "/projects/a.atmr");
    put(provider.as_ref(), "/loose.atmr");

    let model = BrowserModel::opened_on(&state);
    assert_eq!(names(&model), vec!["projects", "loose.atmr"]);

    let projects = model
        .visible_entries()
        .into_iter()
        .find(|e| e.name == "projects")
        .expect("the directory is listed");
    assert!(model.enter_dir(&state, &projects));
    assert_eq!(model.cwd(), Some(uri("/projects")));
    assert_eq!(names(&model), vec!["a.atmr"]);

    // A file is not a destination.
    let file = model.visible_entries().into_iter().next().unwrap();
    assert!(!model.enter_dir(&state, &file));
    assert_eq!(model.cwd(), Some(uri("/projects")));

    assert!(model.can_go_up());
    assert!(model.up(&state));
    assert_eq!(model.cwd(), Some(uri("/")));
    assert!(!model.can_go_up(), "the provider root is the ceiling");
    assert!(!model.up(&state));
}

/// The trail starts at the provider's display name and names every
/// directory between the root and the cwd.
#[test]
fn breadcrumbs_run_from_the_provider_root_to_the_cwd() {
    let (state, provider) = sync_state();
    put(provider.as_ref(), "/projects/2026/bracket.atmr");

    let model = BrowserModel::opened_on(&state);
    assert_eq!(
        model.breadcrumbs(),
        vec![Crumb {
            label: "Memory".to_string(),
            uri: uri("/")
        }]
    );

    model.navigate_to(&state, uri("/projects/2026"));
    let crumbs = model.breadcrumbs();
    assert_eq!(
        crumbs.iter().map(|c| c.label.clone()).collect::<Vec<_>>(),
        vec!["Memory", "projects", "2026"]
    );
    assert_eq!(
        crumbs.iter().map(|c| c.uri.clone()).collect::<Vec<_>>(),
        vec![uri("/"), uri("/projects"), uri("/projects/2026")]
    );
}

/// Directories first, then case-insensitively by name — the order both
/// ancestor browsers use.
#[test]
fn entries_sort_directories_first_then_case_insensitively() {
    let (state, provider) = sync_state();
    for path in [
        "/zebra.atmr",
        "/Apple.atmr",
        "/beta/x.atmr",
        "/Alpha/x.atmr",
        "/apple.atmr",
    ] {
        put(provider.as_ref(), path);
    }

    let model = BrowserModel::opened_on(&state);
    assert_eq!(
        names(&model),
        vec!["Alpha", "beta", "Apple.atmr", "apple.atmr", "zebra.atmr"]
    );
}

/// An empty directory is its own state, distinct from "still loading".
#[test]
fn an_empty_directory_reports_empty() {
    let (state, provider) = sync_state();
    provider
        .create_dir(&uri("/empty"))
        .take()
        .expect("settles")
        .expect("create_dir succeeds");

    let model = BrowserModel::opened_on(&state);
    model.navigate_to(&state, uri("/empty"));
    assert_eq!(model.listing(), Listing::Empty);
    assert!(model.visible_entries().is_empty());
}

/// A failed listing surfaces the provider's message, not a blank pane.
#[test]
fn a_failed_listing_becomes_an_error_state() {
    let (state, _provider) = sync_state();
    let model = BrowserModel::opened_on(&state);
    model.navigate_to(&state, uri("/no-such-dir"));

    match model.listing() {
        Listing::Error(message) => assert!(!message.is_empty()),
        other => panic!("expected an error listing, got {other:?}"),
    }
}

/// Filtering happens on the current listing only, case-insensitively, and
/// leaves the listing itself untouched so clearing the box restores it.
#[test]
fn search_filters_the_current_listing_case_insensitively() {
    let (state, provider) = sync_state();
    for path in ["/Bracket.atmr", "/bracket-v2.atmr", "/gear.atmr"] {
        put(provider.as_ref(), path);
    }

    let model = BrowserModel::opened_on(&state);
    model.set_search("BRACK");
    // `-` sorts before `.`, so the v2 file leads — the point here is that
    // both cases matched, and that filtering preserves the sort order.
    assert_eq!(names(&model), vec!["bracket-v2.atmr", "Bracket.atmr"]);
    assert_eq!(
        model.listing().entries().len(),
        3,
        "the filter must not destroy the listing"
    );

    model.set_search("nothing-matches");
    assert!(names(&model).is_empty());

    model.set_search("");
    assert_eq!(names(&model).len(), 3);
}

/// Navigating is a fresh start: a filter and a selection belonging to the
/// directory being left must not carry into the next one.
#[test]
fn navigation_clears_the_search_and_the_selection() {
    let (state, provider) = sync_state();
    put(provider.as_ref(), "/projects/a.atmr");
    put(provider.as_ref(), "/gear.atmr");

    let model = BrowserModel::opened_on(&state);
    model.set_search("gear");
    model.select(Some(uri("/gear.atmr")));
    assert_eq!(model.selected_entry().map(|e| e.name), Some("gear.atmr".to_string()));

    model.navigate_to(&state, uri("/projects"));
    assert_eq!(model.search(), "");
    assert_eq!(model.selected(), None);
    assert_eq!(names(&model), vec!["a.atmr"]);
}

/// The state before the bytes arrive is `Loading`, and the entries appear
/// on the frame the pump applies the continuation.
#[test]
fn a_slow_listing_shows_loading_until_it_lands() {
    let (state, provider) = flaky_state(FlakyConfig::default().with_latency(2));
    put(provider.inner().as_ref(), "/a.atmr");

    let model = BrowserModel::opened_on(&state);
    assert_eq!(model.listing(), Listing::Loading);
    // `_all`, not `pending_op_count`: listings are *quiet* operations
    // (`storage_ops`, "Loud and quiet operations"), so the loud count —
    // the one the File menu's busy gate reads — deliberately ignores
    // them. See `no_file_action_is_refused_while_a_listing_is_in_flight`
    // in `atomartist-ui-test/tests/favorites_bar.rs` for why that matters.
    assert_eq!(state.pending_op_count_all(), 1, "the listing is in flight");

    frames(&state, &provider, 1);
    assert_eq!(model.listing(), Listing::Loading);

    frames(&state, &provider, 1);
    assert_eq!(state.pending_op_count_all(), 0);
    assert_eq!(names(&model), vec!["a.atmr"]);
}

/// The generation guard, stated as the bug it prevents: the user opens a
/// slow directory, changes their mind and opens another, and the *first*
/// listing lands last. Without the guard its entries would replace the
/// ones the user is actually looking at.
#[test]
fn a_stale_listing_never_replaces_the_current_one() {
    let (state, provider) = flaky_state(FlakyConfig::default().with_latency(2));
    put(provider.inner().as_ref(), "/slow/from-slow.atmr");
    put(provider.inner().as_ref(), "/quick/from-quick.atmr");

    let model = BrowserModel::opened_on(&state);
    frames(&state, &provider, 2);

    // Listing of `/slow` starts, then the user leaves for `/quick`.
    model.navigate_to(&state, uri("/slow"));
    let stale_generation = model.generation();
    frames(&state, &provider, 1);
    model.navigate_to(&state, uri("/quick"));
    assert!(model.generation() > stale_generation);
    assert_eq!(state.pending_op_count_all(), 2, "both listings are in flight");

    // The `/slow` listing lands first, into a model that has moved on.
    frames(&state, &provider, 1);
    assert_eq!(
        model.listing(),
        Listing::Loading,
        "the abandoned directory's entries must be dropped"
    );

    frames(&state, &provider, 1);
    assert_eq!(state.pending_op_count_all(), 0);
    assert_eq!(model.cwd(), Some(uri("/quick")));
    assert_eq!(names(&model), vec!["from-quick.atmr"]);
}

/// The same guard for the plainer case: two refreshes of the *same*
/// directory. The first result must not un-do the second's `Loading`, and
/// the second must be the one that shows.
#[test]
fn a_refresh_during_a_refresh_keeps_only_the_newer_result() {
    let (state, provider) = flaky_state(FlakyConfig::default().with_latency(2));
    put(provider.inner().as_ref(), "/a.atmr");

    let model = BrowserModel::opened_on(&state);
    let first = model.generation();
    frames(&state, &provider, 1);

    model.refresh(&state);
    assert_eq!(model.generation(), first + 1);
    assert_eq!(state.pending_op_count_all(), 2);

    // The first listing settles into a superseded generation.
    frames(&state, &provider, 1);
    assert_eq!(model.listing(), Listing::Loading);

    frames(&state, &provider, 1);
    assert_eq!(state.pending_op_count_all(), 0);
    assert_eq!(names(&model), vec!["a.atmr"]);
    assert_eq!(model.generation(), first + 1, "no extra refresh happened");
}

/// A *failure* is generation-stamped like any other result, so a stale one
/// cannot blank out the listing the user is looking at.
///
/// The gesture: the user opens a directory that does not exist on a slow
/// provider, then — without waiting — switches to a fast provider's real
/// directory. The good listing lands first and the `NotFound` arrives
/// afterwards; ungurarded, it would replace the entries on screen with an
/// error the user already navigated away from.
///
/// Two providers rather than one because the latency has to differ per
/// listing, and `FlakyProvider`'s clock is per-provider — which also makes
/// this the cross-provider (sidebar switch) navigation case.
#[test]
fn a_stale_failure_cannot_overwrite_a_good_listing() {
    let slow = Arc::new(FlakyProvider::new(
        Arc::new(MemoryProvider::new("slow", "Slow Store")),
        FlakyConfig::default().with_latency(4),
    ));
    let fast = Arc::new(FlakyProvider::new(
        Arc::new(MemoryProvider::new("fast", "Fast Store")),
        FlakyConfig::default().with_latency(1),
    ));
    slow.inner()
        .write(
            &StorageUri::new("slow", "/seed.atmr"),
            b"x".to_vec(),
            Precondition::None,
        )
        .take()
        .expect("settles")
        .expect("seed write succeeds");
    fast.inner()
        .write(
            &StorageUri::new("fast", "/good/x.atmr"),
            b"x".to_vec(),
            Precondition::None,
        )
        .take()
        .expect("settles")
        .expect("seed write succeeds");

    let mut registry = StorageRegistry::new();
    registry
        .register(slow.clone() as Arc<dyn StorageProvider>)
        .unwrap();
    registry
        .register(fast.clone() as Arc<dyn StorageProvider>)
        .unwrap();
    let state = state_with(registry);

    // Opens on the slow provider's root, then heads for a directory that
    // is not there, then changes provider — all before anything lands.
    let model = BrowserModel::opened_on(&state);
    model.navigate_to(&state, StorageUri::new("slow", "/missing"));
    model.navigate_to(&state, StorageUri::new("fast", "/good"));
    assert_eq!(
        state.pending_op_count_all(),
        3,
        "all three listings are in flight"
    );

    // Enough frames for every one of them, including the slow failure.
    for _ in 0..6 {
        slow.pump();
        fast.pump();
        state.pump_storage();
        assert!(
            !matches!(model.listing(), Listing::Error(_)),
            "a listing the user navigated away from must never reach the screen"
        );
    }

    assert_eq!(state.pending_op_count_all(), 0);
    assert_eq!(model.cwd(), Some(StorageUri::new("fast", "/good")));
    assert_eq!(names(&model), vec!["x.atmr"]);
}

/// Selection is single: a second `select` replaces the first, and a
/// selection that is no longer in the listing resolves to nothing.
#[test]
fn selection_is_single_and_resolves_against_the_listing() {
    let (state, provider) = sync_state();
    put(provider.as_ref(), "/a.atmr");
    put(provider.as_ref(), "/b.atmr");

    let model = BrowserModel::opened_on(&state);
    model.select(Some(uri("/a.atmr")));
    model.select(Some(uri("/b.atmr")));
    assert_eq!(model.selected(), Some(uri("/b.atmr")));
    assert_eq!(model.selected_entry().map(|e| e.name), Some("b.atmr".to_string()));

    model.select(Some(uri("/vanished.atmr")));
    assert_eq!(model.selected_entry(), None);

    model.select(None);
    assert_eq!(model.selected(), None);
}

/// Every clone is a view onto the same state — the widget's copy and the
/// one captured by an in-flight continuation must not diverge.
#[test]
fn clones_share_one_state() {
    let (state, provider) = sync_state();
    put(provider.as_ref(), "/a.atmr");

    let model = BrowserModel::opened_on(&state);
    let other = model.clone();
    other.select(Some(uri("/a.atmr")));
    assert_eq!(model.selected(), Some(uri("/a.atmr")));
    assert_eq!(other.cwd(), model.cwd());
}
