//! `ThumbnailCache` — the file browser's preview store.
//!
//! The browser lists packages (`.atmr` projects, `.3mf` meshes) that may
//! carry a `Metadata/thumbnail.png` (design §3). Turning one of those into
//! pixels means reading the **whole blob** — no provider has a partial-read
//! API — then pulling the preview entry out of the zip
//! ([`atomartist_lib::serialization::read_thumbnail_from_bytes`]) and
//! decoding the PNG. That is far too expensive to do for a directory of a
//! thousand files, so this module exists to do it *only for the rows that
//! are on screen*, and to remember the answers.
//!
//! Relationship to the rest of the browser: [`super::model::BrowserModel`]
//! produces [`Entry`] rows; the widget (next step) paints them and calls
//! [`ThumbnailCache::request`] once per visible row per frame; the cache
//! drives the reads through the Phase 4 pump
//! ([`AppState::submit_op`](crate::AppState::submit_op)) exactly as the
//! model drives its listings, so an asynchronous provider behaves like the
//! synchronous local one.
//!
//! # Keying and invalidation
//!
//! The key is `(uri, stamp, size, `[`CACHE_VERSION`]`)`. The stamp comes
//! from the listing [`Entry`], so a file that changed on disk arrives with
//! a new stamp, misses the cache, and is re-read — invalidation needs no
//! explicit purge. [`CACHE_VERSION`] is bumped by hand whenever the
//! decoding or scaling below changes, which invalidates every live entry
//! the same way.
//!
//! # Visibility gating, and why re-requesting *is* the queue
//!
//! A request never fetches directly; it parks a `Queued` slot stamped with
//! the current **frame** ([`ThumbnailCache::begin_frame`]) and a monotonic
//! sequence number. At most [`DEFAULT_MAX_IN_FLIGHT`] reads run at once;
//! when a slot frees, the cache picks the **most recently requested**
//! queued key — and, before picking, **drops every queued key that was not
//! requested during the current frame**.
//!
//! That drop rule is the whole design: the widget re-requests each visible
//! row every frame, so a row that scrolled off screen simply stops being
//! re-requested and its queued slot evaporates *without ever being read*.
//! A fast scroll across a thousand-file directory therefore fetches only
//! what came to rest under the viewport, not the hundreds of rows that
//! flashed past. (A shell that never calls `begin_frame` degrades safely:
//! nothing is ever considered stale, and every requested key is eventually
//! fetched.)
//!
//! The queue is consulted at two moments, which is what makes the
//! most-recent-first order matter rather than degenerate into arrival
//! order: from [`ThumbnailCache::request`] itself (so a synchronous
//! provider returns a decoded image from the first call), and from a
//! finished read — which lands from the frame pump *before* the next
//! visibility round starts, with the previous frame's rows still queued
//! and the most recently requested of them next in line.
//!
//! That second half leans on the shell's ordering: every shell calls
//! `pump_storage` at the top of the frame and the widget calls
//! [`begin_frame`](ThumbnailCache::begin_frame) during layout/paint, so a
//! completed read sees last frame's queue intact. If a shell ever inverted
//! that, the cache would still be *correct* — the queue would simply be
//! empty at completion time and the frame's own requests would refill it —
//! but the freed slot would idle for a frame, so the ordering is
//! load-bearing for throughput, not for behaviour.
//!
//! The queue scan is skipped outright when nothing is queued, which is the
//! steady state once a directory has loaded; the real cost of a full
//! directory of previews gets measured when the grid widget lands (house
//! rule: measure, don't guess).
//!
//! # Retries
//!
//! There are none. A [`ThumbState::Failed`] is sticky for its
//! `(uri, stamp)`: the widget re-requests every frame, and retrying on
//! each of those would hammer a failing provider at frame rate. A file
//! that changes gets a new stamp and a fresh attempt; a user who wants one
//! sooner re-lists the directory into a cache that
//! [`clear`](ThumbnailCache::clear)ed. Deliberate for v1 — a backoff
//! belongs with the retry policy the storage plan defers to Phase 8.
//!
//! # Bounds
//!
//! Decoded RGBA is bulky (a 128-px preview is 64 KB), so the store is
//! capped by a **byte budget** — [`DEFAULT_BYTE_BUDGET`], 32 MB, roughly
//! 500 previews — with LRU eviction, plus an entry ceiling
//! ([`DEFAULT_MAX_ENTRIES`]) that bounds the cheap negative entries. In-flight
//! and queued slots are never evicted.
//!
//! `Absent` (no preview in the package, or a kind of file that cannot have
//! one) and `Failed` are cached like any other answer: without that, a
//! 40 MB `.3mf` with no preview would be re-read on every single frame it
//! is visible.
//!
//! # Never a broken image
//!
//! The cache reports [`ThumbState::Absent`] rather than an error for
//! everything that legitimately has no preview — directories, `.stl`,
//! `.obj`, a package without the entry, or a preview stored in a format we
//! do not decode (a foreign package's JPEG). Choosing the fallback glyph is
//! the widget's job; the cache only ever hands back a *whole* image or a
//! reason there isn't one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use atomartist_lib::serialization::read_thumbnail_from_bytes;
use atomartist_storage::{Blob, Entry, Stamp, StorageError, StorageUri};

use crate::app_state::AppState;
use crate::app_state_storage::{read_job, uri_extension, uri_label};
use crate::storage_ops::JobOp;

// Bytes-to-pixels lives next door; the cache itself is about *when* to
// decode, not how.
#[path = "thumbs_decode.rs"]
mod decode;
use decode::decode_preview;

/// Bumped whenever the decode / scaling below changes, so previews cached
/// by an older build of the same process generation are not reused.
pub const CACHE_VERSION: u32 = 1;

/// Preview box (longest edge, in pixels) requested by
/// [`ThumbnailCache::request`]. Embedded previews are 256×192, so the
/// default halves them and quarters their memory.
pub const DEFAULT_THUMB_SIZE: u32 = 128;

/// Decoded-pixel ceiling: ~32 MB, about 500 previews at the default size.
pub const DEFAULT_BYTE_BUDGET: usize = 32 * 1024 * 1024;

/// Cached answers (including the cheap `Absent` / `Failed` ones) kept at
/// most, so a long browsing session cannot grow the map without bound.
pub const DEFAULT_MAX_ENTRIES: usize = 4096;

/// Concurrent reads. Two keeps the pump busy without letting a directory
/// of large packages monopolise a slow provider.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 2;

/// Extensions whose files are zip packages that may carry
/// `Metadata/thumbnail.png`. Everything else is [`ThumbState::Absent`]
/// without any provider read at all.
pub const PACKAGE_EXTENSIONS: &[&str] = &["atmr", "3mf"];

/// A decoded preview: straight-alpha RGBA8, **top-down** row order — the
/// order `agg_gui::widgets::ImageView` and `DrawCtx::draw_image_rgba` want,
/// and the order PNG stores. The bottom-up convention that governs widget
/// coordinates does not apply to raw pixel buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ThumbnailImage {
    /// Decoded size in bytes — what the cache's byte budget counts.
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

/// What the cache knows about one entry right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbState {
    /// Nothing has been asked for this key (or its queued slot was dropped
    /// because the row stopped being visible).
    NotRequested,
    /// Queued or being read. The widget shows its loading affordance.
    Pending,
    /// A decoded preview. Shared, so handing it to a widget is a refcount
    /// bump rather than a buffer copy.
    Ready(Arc<ThumbnailImage>),
    /// There is legitimately no preview: a directory, a format that cannot
    /// carry one, a package without the entry, or an image we do not
    /// decode. The widget paints its format glyph.
    Absent,
    /// The read itself failed; the string is the provider's message.
    /// Sticky for this `(uri, stamp)` — there is no automatic retry (see
    /// the module docs).
    Failed(String),
}

impl ThumbState {
    pub fn image(&self) -> Option<&Arc<ThumbnailImage>> {
        match self {
            ThumbState::Ready(image) => Some(image),
            _ => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ThumbState::Pending)
    }
}

/// Cache key. `stamp` is the listing entry's — a changed file lists with a
/// new stamp and therefore misses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub uri: StorageUri,
    pub stamp: Option<Stamp>,
    pub size: u32,
    pub version: u32,
}

impl ThumbKey {
    pub fn for_entry(entry: &Entry, size: u32) -> ThumbKey {
        ThumbKey {
            uri: entry.uri.clone(),
            stamp: entry.stamp.clone(),
            size: size.max(1),
            version: CACHE_VERSION,
        }
    }
}

/// Whether an entry could possibly have an embedded preview. Directories
/// and mesh formats without a package wrapper (`.stl`, `.obj`) never do, so
/// they are answered without touching a provider.
pub fn can_have_thumbnail(entry: &Entry) -> bool {
    !entry.is_dir && PACKAGE_EXTENSIONS.contains(&uri_extension(&entry.uri).as_str())
}

#[derive(Debug)]
enum SlotState {
    Queued,
    InFlight,
    Ready(Arc<ThumbnailImage>),
    Absent,
    Failed(String),
}

impl SlotState {
    fn is_pending(&self) -> bool {
        matches!(self, SlotState::Queued | SlotState::InFlight)
    }

    fn byte_len(&self) -> usize {
        match self {
            SlotState::Ready(image) => image.byte_len(),
            _ => 0,
        }
    }

    fn public(&self) -> ThumbState {
        match self {
            SlotState::Queued | SlotState::InFlight => ThumbState::Pending,
            SlotState::Ready(image) => ThumbState::Ready(image.clone()),
            SlotState::Absent => ThumbState::Absent,
            SlotState::Failed(message) => ThumbState::Failed(message.clone()),
        }
    }
}

struct Slot {
    state: SlotState,
    /// LRU stamp: the cache's own counter at the last request or result.
    touched: u64,
    /// Frame of the last request — the visibility gate.
    requested_frame: u64,
    /// Request order, so the queue can serve the most recent first.
    requested_seq: u64,
}

struct Inner {
    entries: HashMap<ThumbKey, Slot>,
    bytes_used: usize,
    byte_budget: usize,
    max_entries: usize,
    max_in_flight: usize,
    in_flight: usize,
    /// How many slots are [`SlotState::Queued`], maintained rather than
    /// counted: `next_start` runs on every request, including the pure
    /// cache hits that are the steady state, and scanning the whole map
    /// each time would cost the visible rows × the cache size per frame.
    queued: usize,
    /// Bumped by every finished read. Paired with `reconciled_at` it
    /// bounds the in-flight repair scan to one per completed read.
    progress: u64,
    reconciled_at: u64,
    frame: u64,
    seq: u64,
    clock: u64,
    /// Re-entrancy guard: a continuation that runs inline from `submit_op`
    /// re-enters the queue pump on the same stack, and recursing per queued
    /// item would grow the stack with the directory size.
    pumping: bool,
    pump_again: bool,
}

impl Inner {
    fn touch(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Claim the next key to read, or `None` when the concurrency cap is
    /// reached or nothing current is queued.
    ///
    /// Queued keys that were not requested during the current frame are
    /// dropped here rather than fetched — the visibility gate. They are
    /// only examined when a slot is free, so a saturated cache holds stale
    /// keys a little longer; it still never *reads* one.
    ///
    /// The two map scans below only run when something is actually queued,
    /// which in the steady state (every visible row a cache hit) is never.
    fn next_start(&mut self) -> Option<ThumbKey> {
        if self.queued == 0 {
            return None;
        }
        if self.in_flight >= self.max_in_flight {
            // Repair, not bookkeeping-by-scan: `PendingOp::apply` has a
            // branch that reports a job which produced no result and drops
            // the continuation unrun, which would leak this counter and —
            // after `max_in_flight` of them — silently stop the cache
            // fetching anything ever again. Recount from the slots
            // themselves, at most once per completed read, so a stall that
            // outlives every finish costs one scan rather than one a
            // frame.
            if self.reconciled_at == self.progress {
                return None;
            }
            self.reconciled_at = self.progress;
            let actual = self
                .entries
                .values()
                .filter(|slot| matches!(slot.state, SlotState::InFlight))
                .count();
            // The repair is silent in release, but a *disagreement* means
            // a slot went missing — the leak this guards against, or a new
            // bug in the bookkeeping. Tests should hear about it.
            debug_assert_eq!(
                actual, self.in_flight,
                "in-flight slots and the counter disagree: a read was lost"
            );
            self.in_flight = actual;
            if self.in_flight >= self.max_in_flight {
                return None;
            }
        }
        let frame = self.frame;
        let stale: Vec<ThumbKey> = self
            .entries
            .iter()
            .filter(|(_, slot)| {
                matches!(slot.state, SlotState::Queued) && slot.requested_frame != frame
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            if self.entries.remove(&key).is_some() {
                self.queued -= 1;
            }
        }

        let best = self
            .entries
            .iter()
            .filter(|(_, slot)| matches!(slot.state, SlotState::Queued))
            .max_by_key(|(_, slot)| slot.requested_seq)
            .map(|(key, _)| key.clone())?;
        if let Some(slot) = self.entries.get_mut(&best) {
            slot.state = SlotState::InFlight;
            self.queued -= 1;
        }
        self.in_flight += 1;
        Some(best)
    }

    /// Drop least-recently-touched settled entries until both bounds hold.
    ///
    /// Pending slots are never evicted — their read is already paid for and
    /// dropping them would strand the continuation's bookkeeping. Neither
    /// is the entry the current operation just touched: evicting *that*
    /// would leave the widget re-requesting, re-reading and re-evicting the
    /// same file every frame. A single preview bigger than the whole budget
    /// is therefore kept, and the budget overshot, rather than re-read
    /// forever.
    fn evict(&mut self) {
        while self.bytes_used > self.byte_budget || self.entries.len() > self.max_entries {
            let newest = self.clock;
            let victim = self
                .entries
                .iter()
                .filter(|(_, slot)| !slot.state.is_pending() && slot.touched != newest)
                .min_by_key(|(_, slot)| slot.touched)
                .map(|(key, _)| key.clone());
            match victim {
                Some(key) => {
                    if let Some(slot) = self.entries.remove(&key) {
                        self.bytes_used = self.bytes_used.saturating_sub(slot.state.byte_len());
                    }
                }
                // Everything left is in flight or is the entry that just
                // landed; the bounds are restored as those settle and as
                // the next operation makes this one evictable.
                None => break,
            }
        }
    }
}

/// The browser's preview store. Clone it to share with a widget or a
/// continuation; every clone sees the same entries.
#[derive(Clone)]
pub struct ThumbnailCache {
    inner: Arc<Mutex<Inner>>,
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        ThumbnailCache::new()
    }
}

impl ThumbnailCache {
    pub fn new() -> Self {
        ThumbnailCache::with_limits(
            DEFAULT_BYTE_BUDGET,
            DEFAULT_MAX_ENTRIES,
            DEFAULT_MAX_IN_FLIGHT,
        )
    }

    pub fn with_limits(byte_budget: usize, max_entries: usize, max_in_flight: usize) -> Self {
        ThumbnailCache {
            inner: Arc::new(Mutex::new(Inner {
                entries: HashMap::new(),
                bytes_used: 0,
                byte_budget,
                max_entries: max_entries.max(1),
                max_in_flight: max_in_flight.max(1),
                in_flight: 0,
                queued: 0,
                progress: 0,
                // Not `progress`: the first stall must be allowed to
                // reconcile, and `0 == 0` would skip it.
                reconciled_at: u64::MAX,
                frame: 1,
                seq: 0,
                clock: 0,
                pumping: false,
                pump_again: false,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start a new visibility round. The widget calls this once per frame
    /// before requesting its visible rows; queued keys not re-requested
    /// after it are dropped instead of read.
    pub fn begin_frame(&self) {
        self.lock().frame += 1;
    }

    /// Declare `entry` visible and ask for its preview at
    /// [`DEFAULT_THUMB_SIZE`].
    pub fn request(&self, state: &AppState, entry: &Entry) -> ThumbState {
        self.request_sized(state, entry, DEFAULT_THUMB_SIZE)
    }

    /// [`request`](Self::request) at a caller-chosen preview box (longest
    /// edge in pixels); the size is part of the key, so two grids at two
    /// zoom levels do not fight over one slot.
    pub fn request_sized(&self, state: &AppState, entry: &Entry, size: u32) -> ThumbState {
        if !can_have_thumbnail(entry) {
            // No read, no cache entry: the answer is a property of the
            // name, and re-deriving it is cheaper than storing it.
            return ThumbState::Absent;
        }
        let key = ThumbKey::for_entry(entry, size);
        {
            // The lock is dropped before `pump_queue`, which submits jobs
            // whose continuations re-lock (storage_ops' re-entrancy
            // contract).
            let mut inner = self.lock();
            let touched = inner.touch();
            let frame = inner.frame;
            let seq = inner.next_seq();
            if let Some(slot) = inner.entries.get_mut(&key) {
                slot.touched = touched;
                if matches!(slot.state, SlotState::Queued) {
                    // Still visible: refresh its place in the queue.
                    slot.requested_frame = frame;
                    slot.requested_seq = seq;
                }
            } else {
                inner.entries.insert(
                    key.clone(),
                    Slot {
                        state: SlotState::Queued,
                        touched,
                        requested_frame: frame,
                        requested_seq: seq,
                    },
                );
                inner.queued += 1;
                inner.evict();
            }
        }
        self.pump_queue(state);
        // Read *after* pumping: a synchronous provider settles inline, so
        // the first request on the desktop path already returns the image.
        self.peek(&key)
    }

    /// The cached state of `key` without requesting anything.
    pub fn peek(&self, key: &ThumbKey) -> ThumbState {
        self.lock()
            .entries
            .get(key)
            .map(|slot| slot.state.public())
            .unwrap_or(ThumbState::NotRequested)
    }

    /// State of `entry` at `size` without requesting anything — the
    /// non-mutating view a test or a diagnostic wants.
    pub fn peek_entry(&self, entry: &Entry, size: u32) -> ThumbState {
        if !can_have_thumbnail(entry) {
            return ThumbState::Absent;
        }
        self.peek(&ThumbKey::for_entry(entry, size))
    }

    /// Decoded bytes currently held.
    pub fn bytes_used(&self) -> usize {
        self.lock().bytes_used
    }

    /// Cached answers held, including pending and negative ones.
    pub fn entry_count(&self) -> usize {
        self.lock().entries.len()
    }

    /// Reads running right now — never more than the configured cap.
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight
    }

    /// Keys waiting for a slot.
    pub fn queued(&self) -> usize {
        self.lock().queued
    }

    /// Forget every settled answer. Reads already in flight are left alone
    /// — their continuations still have bookkeeping to do — and land in the
    /// emptied cache as usual.
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.retain(|_, slot| slot.state.is_pending());
        inner.bytes_used = 0;
    }

    /// Start reads until the concurrency cap is reached.
    ///
    /// Iterative rather than recursive: `submit_op` runs the continuation
    /// inline for a synchronous provider, and that continuation calls back
    /// in here, so a naive implementation would recurse once per queued
    /// row. The guard turns the re-entrant call into a flag the outer loop
    /// picks up.
    fn pump_queue(&self, state: &AppState) {
        {
            let mut inner = self.lock();
            if inner.pumping {
                inner.pump_again = true;
                return;
            }
            inner.pumping = true;
        }
        loop {
            let next = self.lock().next_start();
            match next {
                Some(key) => self.spawn(state, key),
                None => {
                    let mut inner = self.lock();
                    if inner.pump_again {
                        inner.pump_again = false;
                        continue;
                    }
                    inner.pumping = false;
                    return;
                }
            }
        }
    }

    /// Submit the whole-blob read for one key. There is no partial-read
    /// API, so a preview costs the entire package.
    ///
    /// Submitted *quiet* (see `crate::storage_ops`, "Loud and quiet
    /// operations"): nobody asked for a preview by name, so it stays out
    /// of the status bar, out of the File menu's busy check, and out of
    /// the shutdown wait.
    fn spawn(&self, state: &AppState, key: ThumbKey) {
        let job = read_job(&state.storage, &key.uri);
        let cache = self.clone();
        state.submit_op(Box::new(JobOp::new_quiet(
            format!("Preview {}", uri_label(&key.uri)),
            job,
            move |state, result| cache.finish(state, key, result),
        )));
    }

    /// Apply one finished read: extract, decode, cache, free the slot, and
    /// hand the freed slot to the next queued key.
    ///
    /// This runs either inline inside `pump_queue`'s own loop (synchronous
    /// provider) — where the re-entrancy guard turns the follow-up pump
    /// into a flag — or from the frame pump, *before* the widget's paint
    /// has begun the next visibility round. In the latter case the queue
    /// still holds the previous frame's keys, which are exactly the rows
    /// that were on screen, so the most recently requested of them takes
    /// the freed slot.
    fn finish(&self, state: &AppState, key: ThumbKey, result: Result<Blob, StorageError>) {
        let outcome = match result {
            Ok(bytes) => match read_thumbnail_from_bytes(&bytes) {
                Some(image_bytes) => match decode_preview(&image_bytes, key.size) {
                    Ok(Some(image)) => SlotState::Ready(Arc::new(image)),
                    // A preview in a format we have no decoder for (a
                    // foreign package's JPEG): an absence, not a fault.
                    Ok(None) => SlotState::Absent,
                    // A PNG we should have been able to read and could
                    // not, or one whose canvas is beyond the decode cap.
                    Err(message) => SlotState::Failed(message),
                },
                None => SlotState::Absent,
            },
            Err(err) => SlotState::Failed(err.to_string()),
        };

        {
            // Scoped: `pump_queue` below submits jobs whose continuations
            // re-enter this lock (storage_ops' re-entrancy contract).
            let mut inner = self.lock();
            inner.in_flight = inner.in_flight.saturating_sub(1);
            inner.progress += 1;
            let touched = inner.touch();
            let frame = inner.frame;
            let seq = inner.seq;
            let bytes = outcome.byte_len();
            let replaced = inner
                .entries
                .get(&key)
                .map(|slot| slot.state.byte_len())
                .unwrap_or(0);
            inner.bytes_used = inner.bytes_used.saturating_sub(replaced);
            // Inserting rather than patching in place also covers the slot
            // having been dropped while the read was out; the answer was
            // paid for either way.
            inner.entries.insert(
                key,
                Slot {
                    state: outcome,
                    touched,
                    requested_frame: frame,
                    requested_seq: seq,
                },
            );
            inner.bytes_used += bytes;
            inner.evict();
        }
        self.pump_queue(state);
    }
}

// Tests live alongside in `thumbs_tests.rs` (house convention, keeps this
// file under the 800-line cap).
#[cfg(test)]
#[path = "thumbs_tests.rs"]
mod thumbs_tests;
