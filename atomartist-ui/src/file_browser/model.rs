//! `BrowserModel` — the widget-free core of the in-app file browser.
//!
//! Holds everything the browser widget will paint and nothing that paints
//! it: the provider roots (in [`StorageRegistry`] registration order,
//! which *is* the sidebar order), the current directory, the [`Listing`]
//! state, the search filter, and the single selection. Every directory
//! read goes out as a [`StorageProvider::list`](atomartist_storage::StorageProvider::list)
//! job submitted to the Phase 4 frame pump
//! ([`AppState::submit_op`](crate::AppState::submit_op)), and comes back
//! through a continuation — so an asynchronous provider (OPFS, HTTP)
//! behaves exactly like the synchronous local one.
//!
//! Two rules from the design (`docs/file-browser-design.md` §2) shape the
//! whole type:
//!
//! - **Never a blank pane.** [`Listing`] is always one of `Loading`,
//!   `Ready`, `Empty`, or `Error`, so there is no state the widget can
//!   render as nothing-at-all.
//! - **Stale responses are dropped.** Every [`BrowserModel::refresh`]
//!   stamps a monotonically increasing generation; a continuation whose
//!   generation is no longer current returns without touching anything.
//!   This is NodeDesigner's `file-browser-dialog.js` generation-counter
//!   guard: without it, a slow listing of the directory the user just left
//!   overwrites the fast listing of the one they are looking at.
//!
//! **Sort order** is directories first, then case-insensitive by name —
//! the default both ancestors (NodeDesigner's file grid and MatterCAD's
//! library view) use. Entries are sorted once, when a listing lands; the
//! search filter is applied on read, since it changes far more often.
//!
//! The model is a cheap clonable handle over `Arc<Mutex<…>>` — the same
//! shape [`AppState`] uses — because continuations
//! outlive the call that submitted them and widgets need their own copy.
//! Its lock is never held across
//! [`submit_op`](crate::AppState::submit_op), which may run the
//! continuation inline (see the re-entrancy contract in
//! `crate::storage_ops`).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use atomartist_storage::{Entry, StorageError, StorageRegistry, StorageUri};

use crate::app_state::AppState;
use crate::app_state_storage::{list_job, uri_label};
use crate::storage_ops::JobOp;

/// What the browser shows for the current directory. There is deliberately
/// no "nothing yet" variant: a fresh model starts `Loading`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// A `list` job is in flight for the current directory.
    ///
    /// Also the state of a model built by [`BrowserModel::new`] and never
    /// refreshed — that one has *no* job in flight, so a widget rendering
    /// it would spin forever. [`BrowserModel::opened_on`] is the normal
    /// entry point precisely because it starts the first listing.
    Loading,
    /// The directory listed and has entries, already sorted.
    Ready(Vec<Entry>),
    /// The directory listed and is empty — distinct from `Loading` so the
    /// widget can say "This folder is empty" instead of spinning forever.
    Empty,
    /// The listing failed; the string is the message to show the user.
    Error(String),
}

impl Listing {
    /// Entries the listing carries, empty for every other state.
    pub fn entries(&self) -> &[Entry] {
        match self {
            Listing::Ready(entries) => entries,
            _ => &[],
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Listing::Loading)
    }
}

/// One row of the provider sidebar: a registered provider and its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRoot {
    pub scheme: String,
    /// The provider's own [`display_name`](atomartist_storage::StorageProvider::display_name)
    /// — "This PC", "This Browser".
    pub display_name: String,
    pub root: StorageUri,
}

/// One step of the breadcrumb trail. The first crumb of a trail is always
/// the provider root, labelled with the provider's display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub uri: StorageUri,
}

/// Shown when the build registered no storage providers at all — the
/// browser has nowhere to look, and saying so beats an empty sidebar.
const NO_PROVIDERS: &str = "No storage providers are available in this build.";

struct Inner {
    roots: Vec<ProviderRoot>,
    /// `None` only when there are no providers.
    cwd: Option<StorageUri>,
    listing: Listing,
    generation: u64,
    search: String,
    selected: Option<StorageUri>,
}

/// The browser's state. Clone to share it with a continuation or a widget;
/// every clone observes the same navigation and the same listing.
#[derive(Clone)]
pub struct BrowserModel {
    inner: Arc<Mutex<Inner>>,
}

impl BrowserModel {
    /// Build a model over `registry`'s providers, starting at the first
    /// one's root.
    ///
    /// **Nothing is listed until [`refresh`](Self::refresh) (or any
    /// navigation call) is given an [`AppState`] to submit jobs to.** The
    /// model starts in [`Listing::Loading`] with *no job in flight*, so a
    /// widget built from `new` alone shows a spinner forever.
    /// [`opened_on`](Self::opened_on) is the normal entry point; `new`
    /// exists for callers that have a registry but not yet an `AppState`
    /// (and for tests that want to inspect the roots without any IO).
    pub fn new(registry: &StorageRegistry) -> Self {
        let roots: Vec<ProviderRoot> = registry
            .providers()
            .map(|provider| ProviderRoot {
                scheme: provider.scheme().to_string(),
                display_name: provider.display_name().to_string(),
                root: StorageUri::new(provider.scheme(), "/"),
            })
            .collect();
        let cwd = roots.first().map(|r| r.root.clone());
        let listing = match cwd {
            Some(_) => Listing::Loading,
            None => Listing::Error(NO_PROVIDERS.to_string()),
        };
        BrowserModel {
            inner: Arc::new(Mutex::new(Inner {
                roots,
                cwd,
                listing,
                generation: 0,
                search: String::new(),
                selected: None,
            })),
        }
    }

    /// Model over the state's registry, listing its first provider's root
    /// straight away — what a browser widget wants when it is created.
    pub fn opened_on(state: &AppState) -> Self {
        let model = BrowserModel::new(&state.storage);
        model.refresh(state);
        model
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sidebar rows, in registration order.
    pub fn roots(&self) -> Vec<ProviderRoot> {
        self.lock().roots.clone()
    }

    /// Directory currently being browsed.
    pub fn cwd(&self) -> Option<StorageUri> {
        self.lock().cwd.clone()
    }

    /// Scheme of the provider currently being browsed.
    pub fn provider_scheme(&self) -> Option<String> {
        self.lock().cwd.as_ref().map(|uri| uri.scheme().to_string())
    }

    /// The current listing state — always one of the four, never nothing.
    pub fn listing(&self) -> Listing {
        self.lock().listing.clone()
    }

    /// Generation of the most recent [`refresh`](Self::refresh). Exposed
    /// for tests and diagnostics.
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Re-list the current directory, invalidating any listing still in
    /// flight.
    pub fn refresh(&self, state: &AppState) {
        // The lock is dropped before `submit_op`, which runs the
        // continuation inline for a synchronous provider — and that
        // continuation locks this same mutex.
        let (dir, generation) = {
            let mut inner = self.lock();
            // Bumped before the `cwd` check, not after: an error state is
            // as much a listing as any other, and stamping it with a fresh
            // generation means no in-flight job can ever land on top of
            // it. Unreachable today (a model with no cwd has no provider
            // to have submitted a job to), but this is the invariant, not
            // an accident of the current call graph.
            inner.generation += 1;
            let Some(dir) = inner.cwd.clone() else {
                inner.listing = Listing::Error(NO_PROVIDERS.to_string());
                return;
            };
            inner.listing = Listing::Loading;
            (dir, inner.generation)
        };

        let job = list_job(&state.storage, &dir);
        let model = self.clone();
        // **Quiet** (see `crate::storage_ops`, "Loud and quiet
        // operations"). A listing reports itself: the browser paints
        // `Loading` / `Error` in the very pane the user is looking at, so
        // the status bar has nothing to add. And a loud listing would
        // make `menu_actions::storage_busy` refuse File actions — and the
        // favorites bar's own project opens — for as long as a directory
        // is on screen, which on an asynchronous provider is most of the
        // time the browser is open.
        state.submit_op(Box::new(JobOp::new_quiet(
            format!("Listing {}", uri_label(&dir)),
            job,
            move |_state, result| model.apply_listing(generation, result),
        )));
    }

    /// Apply a completed listing, unless a newer refresh has superseded it.
    fn apply_listing(&self, generation: u64, result: Result<Vec<Entry>, StorageError>) {
        let mut inner = self.lock();
        if generation != inner.generation {
            // Stale: the user navigated (or refreshed) while this listing
            // was in flight, so its entries describe a directory that is
            // no longer on screen.
            return;
        }
        inner.listing = match result {
            Ok(entries) if entries.is_empty() => Listing::Empty,
            Ok(mut entries) => {
                sort_entries(&mut entries);
                Listing::Ready(entries)
            }
            Err(err) => Listing::Error(err.to_string()),
        };
    }

    /// Browse `uri`, discarding the search filter and selection that
    /// belonged to the directory being left.
    pub fn navigate_to(&self, state: &AppState, uri: StorageUri) {
        {
            let mut inner = self.lock();
            inner.cwd = Some(uri);
            inner.search.clear();
            inner.selected = None;
        }
        self.refresh(state);
    }

    /// Browse into `entry`. A file entry is not a destination, so this
    /// reports `false` and changes nothing.
    pub fn enter_dir(&self, state: &AppState, entry: &Entry) -> bool {
        if !entry.is_dir {
            return false;
        }
        self.navigate_to(state, entry.uri.clone());
        true
    }

    /// Browse the containing directory. `false` at a provider root, which
    /// is where [`StorageUri::parent`] stops.
    pub fn up(&self, state: &AppState) -> bool {
        let parent = self.lock().cwd.as_ref().and_then(StorageUri::parent);
        match parent {
            Some(parent) => {
                self.navigate_to(state, parent);
                true
            }
            None => false,
        }
    }

    /// Whether [`up`](Self::up) would go anywhere.
    pub fn can_go_up(&self) -> bool {
        self.lock()
            .cwd
            .as_ref()
            .and_then(StorageUri::parent)
            .is_some()
    }

    /// Trail from the provider root to the current directory, root first.
    /// Built by walking [`StorageUri::parent`] rather than splitting the
    /// path, because a URI path is the provider's business.
    pub fn breadcrumbs(&self) -> Vec<Crumb> {
        let inner = self.lock();
        let Some(cwd) = inner.cwd.clone() else {
            return Vec::new();
        };
        let mut trail = Vec::new();
        let mut at = Some(cwd);
        while let Some(uri) = at {
            match uri.parent() {
                Some(parent) => {
                    trail.push(Crumb {
                        label: uri.file_name().unwrap_or_default().to_string(),
                        uri: uri.clone(),
                    });
                    at = Some(parent);
                }
                // The root: labelled with the provider's display name.
                None => {
                    let label = inner
                        .roots
                        .iter()
                        .find(|r| r.scheme == uri.scheme())
                        .map(|r| r.display_name.clone())
                        .unwrap_or_else(|| uri.scheme().to_string());
                    trail.push(Crumb { label, uri });
                    at = None;
                }
            }
        }
        trail.reverse();
        trail
    }

    /// Current search text.
    pub fn search(&self) -> String {
        self.lock().search.clone()
    }

    /// Filter the *current listing* by a case-insensitive substring of the
    /// entry name. v1 deliberately searches no further than the directory
    /// on screen (design §7.2): there is no server to ask, and a provider
    /// `search()` can join the trait later without breaking this.
    pub fn set_search(&self, query: &str) {
        self.lock().search = query.to_string();
    }

    /// Entries to paint: the sorted listing with the search filter applied.
    /// Empty for every non-`Ready` state.
    pub fn visible_entries(&self) -> Vec<Entry> {
        let inner = self.lock();
        let needle = inner.search.to_lowercase();
        inner
            .listing
            .entries()
            .iter()
            .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    /// Select an entry (v1 is single-select), or clear the selection with
    /// `None`.
    pub fn select(&self, uri: Option<StorageUri>) {
        self.lock().selected = uri;
    }

    /// The selected URI as last set — which may name an entry that has
    /// since left the listing (a refresh where the file was deleted, or a
    /// caller that selected something never listed at all).
    ///
    /// Use this for painting the highlight; anything that *acts* on the
    /// selection (Open, Save-over, drag) must go through
    /// [`selected_entry`](Self::selected_entry), which resolves against
    /// the listing actually on screen.
    pub fn selected(&self) -> Option<StorageUri> {
        self.lock().selected.clone()
    }

    /// The selected entry, if it is still in the current listing.
    pub fn selected_entry(&self) -> Option<Entry> {
        let inner = self.lock();
        let selected = inner.selected.as_ref()?;
        inner
            .listing
            .entries()
            .iter()
            .find(|e| &e.uri == selected)
            .cloned()
    }
}

/// Directories first, then case-insensitive by name, with the raw name as
/// a tie-break so the order is total (`README` vs `readme`).
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

// Tests live alongside in `model_tests.rs` (house convention, keeps this
// file well under the 800-line cap).
#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;
