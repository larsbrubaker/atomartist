//! Unit tests for [`crate::file_browser::thumbs`].
//!
//! Three provider shapes: `MemoryProvider` (settles inline, so a request
//! returns the decoded image immediately), `FlakyProvider` (holds results
//! until the test advances both clocks — the only way to observe the
//! concurrency cap and the visibility gate), and `CountingProvider`, a
//! local wrapper that records *which* `read`s reached the backend, in
//! order. The last one is how "no provider read at all" and "this row went
//! first" are asserted rather than assumed — the cache's own `ThumbState`
//! cannot tell queued from in-flight, so it cannot answer either question.
//!
//! Split out of `thumbs.rs` with `#[path]` per the house convention, so
//! `use super::*` still reaches its private items.

use super::*;

use std::sync::PoisonError;

use atomartist_lib::serialization::write_project_to_bytes_with_thumbnail;
use atomartist_lib::serialization::AssetStore;
use atomartist_storage::{
    Bytes, Capabilities, FlakyConfig, FlakyProvider, Job, MemoryProvider, Precondition,
    StorageProvider, StorageRegistry,
};

/// A pass-through provider that records the reads it forwards, in order.
/// The "non-package extensions never touch storage" and "the row that
/// scrolled away is never read" rules are only meaningful if they are
/// measured at the provider, and the queue-order rule needs to know *which*
/// file was read, not just how many.
struct CountingProvider {
    inner: Arc<dyn StorageProvider>,
    reads: ReadLog,
}

/// Every URI read so far, in order.
#[derive(Clone, Default)]
struct ReadLog(Arc<Mutex<Vec<String>>>);

impl ReadLog {
    fn record(&self, uri: &StorageUri) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(uri.to_string());
    }

    fn count(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Names read since (and including) index `from`, last segment only.
    fn names_from(&self, from: usize) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .skip(from)
            .map(|uri| uri.rsplit('/').next().unwrap_or(uri).to_string())
            .collect()
    }
}

impl CountingProvider {
    fn new(inner: Arc<dyn StorageProvider>) -> (Arc<CountingProvider>, ReadLog) {
        let reads = ReadLog::default();
        (
            Arc::new(CountingProvider {
                inner,
                reads: reads.clone(),
            }),
            reads,
        )
    }
}

impl StorageProvider for CountingProvider {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>> {
        self.inner.list(dir)
    }
    fn read(&self, at: &StorageUri) -> Job<Blob> {
        self.reads.record(at);
        self.inner.read(at)
    }
    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp> {
        self.inner.write(at, bytes, pre)
    }
    fn delete(&self, at: &StorageUri) -> Job<()> {
        self.inner.delete(at)
    }
    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>> {
        self.inner.stat(at)
    }
    fn create_dir(&self, at: &StorageUri) -> Job<()> {
        self.inner.create_dir(at)
    }
}

fn state_with(provider: Arc<dyn StorageProvider>) -> AppState {
    let mut registry = StorageRegistry::new();
    registry.register(provider).expect("fresh registry");
    AppState::with_storage(
        atomartist_lib::Graph::new(),
        atomartist_lib::registry::NodeRegistry::new(),
        Arc::new(registry),
    )
}

fn uri(path: &str) -> StorageUri {
    StorageUri::new("mem", path)
}

/// A tiny solid-colour PNG, produced by the same encoder that writes real
/// previews.
fn png(w: u32, h: u32) -> Vec<u8> {
    let rgb = vec![0x40u8; (w as usize) * (h as usize) * 3];
    crate::thumbnail::encode_rgb_png(&rgb, w, h).expect("encode test preview")
}

/// Project bytes carrying `thumbnail` at `Metadata/thumbnail.png`.
fn atmr_with(thumbnail: Option<&[u8]>) -> Vec<u8> {
    write_project_to_bytes_with_thumbnail(
        &atomartist_lib::Graph::new(),
        &AssetStore::new(),
        thumbnail,
    )
    .expect("write test project")
}

/// Store `bytes` at `path` and return the listing entry for it — stamp
/// included, since that is half the cache key.
fn put(provider: &dyn StorageProvider, path: &str, bytes: Vec<u8>) -> Entry {
    provider
        .write(&uri(path), bytes, Precondition::None)
        .take()
        .expect("memory writes settle inline")
        .expect("seed write succeeds");
    provider
        .stat(&uri(path))
        .take()
        .expect("settles")
        .expect("stat succeeds")
        .expect("the entry exists")
}

/// Advance both clocks once — one simulated frame.
fn frames(state: &AppState, provider: &Arc<FlakyProvider>, rounds: usize) {
    for _ in 0..rounds {
        provider.pump();
        state.pump_storage();
    }
}

/// The desktop path: a synchronous provider settles inline, so the very
/// first request hands back the decoded preview at the requested size.
#[test]
fn a_ready_preview_round_trips_its_dimensions() {
    let provider = Arc::new(MemoryProvider::new("mem", "Memory"));
    let state = state_with(provider.clone());
    let entry = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(256, 192))));

    let cache = ThumbnailCache::new();
    let image = match cache.request(&state, &entry) {
        ThumbState::Ready(image) => image,
        other => panic!("expected a decoded preview, got {other:?}"),
    };
    // 256×192 fitted into the 128-px box, aspect preserved.
    assert_eq!((image.width, image.height), (128, 96));
    assert_eq!(image.rgba.len(), 128 * 96 * 4);
    assert_eq!(image.rgba[3], 255, "opaque source stays opaque");
    assert_eq!(cache.bytes_used(), image.byte_len());
}

/// A second request for the same entry is a hit; a request for the same
/// URI carrying a *new* stamp is a miss, which is the whole invalidation
/// story.
#[test]
fn a_new_stamp_misses_the_cache_and_re_reads() {
    let memory = Arc::new(MemoryProvider::new("mem", "Memory"));
    let (provider, reads) = CountingProvider::new(memory.clone());
    let state = state_with(provider.clone());
    let entry = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(64, 64))));

    let cache = ThumbnailCache::new();
    assert!(cache.request(&state, &entry).image().is_some());
    assert_eq!(reads.count(), 1);

    // Same entry again: served from the cache.
    assert!(cache.request(&state, &entry).image().is_some());
    assert_eq!(reads.count(), 1, "a hit must not re-read");

    // The file changes on disk and lists with a new stamp.
    let rewritten = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(32, 32))));
    assert_ne!(rewritten.stamp, entry.stamp, "the provider re-stamped it");
    let image = cache
        .request(&state, &rewritten)
        .image()
        .cloned()
        .expect("the new bytes decode");
    assert_eq!(reads.count(), 2, "a new stamp must re-read");
    assert_eq!((image.width, image.height), (32, 32));
}

/// The version is part of the key, so bumping it strands every entry the
/// old code cached — the manual invalidation lever.
#[test]
fn a_version_bump_invalidates_a_cached_entry() {
    let provider = Arc::new(MemoryProvider::new("mem", "Memory"));
    let state = state_with(provider.clone());
    let entry = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(64, 64))));

    let cache = ThumbnailCache::new();
    assert!(cache.request(&state, &entry).image().is_some());

    let current = ThumbKey::for_entry(&entry, DEFAULT_THUMB_SIZE);
    assert!(cache.peek(&current).image().is_some());
    let next_version = ThumbKey {
        version: CACHE_VERSION + 1,
        ..current
    };
    assert_eq!(
        cache.peek(&next_version),
        ThumbState::NotRequested,
        "a bumped CACHE_VERSION must not see the old entry"
    );
}

/// A package with no preview, and a read that fails, are both remembered.
/// Without that, a bad file would be re-read on every frame it is visible.
#[test]
fn absent_and_failed_answers_are_cached_not_re_read() {
    let memory = Arc::new(MemoryProvider::new("mem", "Memory"));
    let (provider, reads) = CountingProvider::new(memory.clone());
    let state = state_with(provider.clone());
    let no_preview = put(provider.as_ref(), "/plain.atmr", atmr_with(None));
    let corrupt = put(provider.as_ref(), "/broken.atmr", b"not a zip".to_vec());

    let cache = ThumbnailCache::new();
    assert_eq!(cache.request(&state, &no_preview), ThumbState::Absent);
    // Not a zip at all: still an absence, never a broken image.
    assert_eq!(cache.request(&state, &corrupt), ThumbState::Absent);
    assert_eq!(reads.count(), 2);

    for _ in 0..5 {
        cache.begin_frame();
        assert_eq!(cache.request(&state, &no_preview), ThumbState::Absent);
        assert_eq!(cache.request(&state, &corrupt), ThumbState::Absent);
    }
    assert_eq!(
        reads.count(),
        2,
        "negative answers must be served from the cache"
    );
    assert_eq!(cache.bytes_used(), 0, "negative answers cost no pixels");

    // A read that fails outright is cached the same way, as `Failed`.
    let missing = Entry::file(uri("/gone.atmr"), 0, Stamp::new("x"));
    assert!(matches!(
        cache.request(&state, &missing),
        ThumbState::Failed(_)
    ));
    let before = reads.count();
    cache.begin_frame();
    assert!(matches!(
        cache.request(&state, &missing),
        ThumbState::Failed(_)
    ));
    assert_eq!(reads.count(), before);
}

/// Directories and formats that cannot carry a preview are answered from
/// the name alone — measured at the provider, not assumed.
#[test]
fn non_package_entries_are_absent_without_any_provider_read() {
    let memory = Arc::new(MemoryProvider::new("mem", "Memory"));
    let (provider, reads) = CountingProvider::new(memory.clone());
    let state = state_with(provider.clone());

    let cache = ThumbnailCache::new();
    for path in ["/mesh.stl", "/mesh.obj", "/notes.txt", "/plain"] {
        let entry = put(provider.as_ref(), path, b"payload".to_vec());
        assert_eq!(
            cache.request(&state, &entry),
            ThumbState::Absent,
            "{path} cannot carry a preview"
        );
        assert_eq!(cache.peek_entry(&entry, DEFAULT_THUMB_SIZE), ThumbState::Absent);
    }
    let dir = Entry::dir(uri("/projects"));
    assert_eq!(cache.request(&state, &dir), ThumbState::Absent);

    assert_eq!(
        reads.count(),
        0,
        "nothing but the name is needed to answer these"
    );
    assert_eq!(cache.entry_count(), 0, "and nothing is stored for them");
    // A `.3mf` *can* carry one (design §3), so it is read.
    let three_mf = put(provider.as_ref(), "/part.3mf", atmr_with(Some(&png(16, 16))));
    assert!(cache.request(&state, &three_mf).image().is_some());
    assert_eq!(reads.count(), 1);
}

/// Under a byte budget the least recently requested preview is the one
/// that goes.
#[test]
fn lru_eviction_holds_the_byte_budget() {
    let provider = Arc::new(MemoryProvider::new("mem", "Memory"));
    let state = state_with(provider.clone());
    let mut entries = Vec::new();
    for i in 0..3 {
        entries.push(put(
            provider.as_ref(),
            &format!("/p{i}.atmr"),
            atmr_with(Some(&png(32, 32))),
        ));
    }
    // Room for two 32×32 RGBA previews, not three.
    let one = 32 * 32 * 4;
    let cache = ThumbnailCache::with_limits(one * 2, DEFAULT_MAX_ENTRIES, 1);

    assert!(cache.request(&state, &entries[0]).image().is_some());
    assert!(cache.request(&state, &entries[1]).image().is_some());
    assert_eq!(cache.bytes_used(), one * 2);

    // Touch #0 so #1 becomes the least recently used, then add #2.
    cache.begin_frame();
    assert!(cache.request(&state, &entries[0]).image().is_some());
    assert!(cache.request(&state, &entries[2]).image().is_some());

    assert_eq!(cache.bytes_used(), one * 2, "the budget is respected");
    assert!(cache
        .peek_entry(&entries[0], DEFAULT_THUMB_SIZE)
        .image()
        .is_some());
    assert!(cache
        .peek_entry(&entries[2], DEFAULT_THUMB_SIZE)
        .image()
        .is_some());
    assert_eq!(
        cache.peek_entry(&entries[1], DEFAULT_THUMB_SIZE),
        ThumbState::NotRequested,
        "the least recently used preview is the one evicted"
    );
}

/// A preview bigger than the entire budget is kept anyway. Evicting the
/// entry that just landed would leave the widget re-requesting,
/// re-reading and re-evicting the same file on every frame.
#[test]
fn a_preview_larger_than_the_budget_is_kept_not_re_read_forever() {
    let memory = Arc::new(MemoryProvider::new("mem", "Memory"));
    let (provider, reads) = CountingProvider::new(memory.clone());
    let state = state_with(provider.clone());
    let entry = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(64, 64))));

    let cache = ThumbnailCache::with_limits(16, DEFAULT_MAX_ENTRIES, 1);
    assert!(cache.request(&state, &entry).image().is_some());
    for _ in 0..3 {
        cache.begin_frame();
        assert!(cache.request(&state, &entry).image().is_some());
    }
    assert_eq!(
        reads.count(),
        1,
        "the entry must survive its own arrival"
    );
}

/// Never more reads out at once than the cap, however many rows are
/// visible.
#[test]
fn the_in_flight_cap_is_respected() {
    let memory = Arc::new(MemoryProvider::new("mem", "Memory"));
    let (counting, reads) = CountingProvider::new(memory.clone());
    let provider = Arc::new(FlakyProvider::new(
        counting.clone(),
        FlakyConfig::default().with_latency(3),
    ));
    let state = state_with(provider.clone());
    let entries: Vec<Entry> = (0..5)
        .map(|i| {
            put(
                counting.as_ref(),
                &format!("/p{i}.atmr"),
                atmr_with(Some(&png(16, 16))),
            )
        })
        .collect();
    let seeded = reads.count();

    let cache = ThumbnailCache::with_limits(DEFAULT_BYTE_BUDGET, DEFAULT_MAX_ENTRIES, 2);
    for entry in &entries {
        assert_eq!(cache.request(&state, entry), ThumbState::Pending);
    }
    assert_eq!(cache.in_flight(), 2, "only two reads may be out at once");
    assert_eq!(cache.queued(), 3);
    assert_eq!(reads.count() - seeded, 2);
    // Preview reads are quiet: on the pump's queue, but never in the
    // status bar's or the File menu's idea of "storage is busy".
    assert_eq!(state.pending_op_count_all(), 2);
    assert_eq!(state.pending_op_count(), 0);
    assert_eq!(state.storage_activity_text(), None);

    // Drain, re-requesting every row each frame as the widget would.
    for _ in 0..40 {
        frames(&state, &provider, 1);
        cache.begin_frame();
        for entry in &entries {
            cache.request(&state, entry);
        }
        assert!(cache.in_flight() <= 2, "the cap holds while draining");
    }
    for entry in &entries {
        assert!(
            cache.peek_entry(entry, DEFAULT_THUMB_SIZE).image().is_some(),
            "every visible row eventually loads"
        );
    }
}

/// A leaked in-flight slot must not stop the cache fetching forever.
///
/// `PendingOp::apply` has a branch that reports a job which produced no
/// result and drops the continuation unrun; `finish` — and with it the
/// counter's decrement — never happens. Two of those with the default cap
/// and the browser would silently stop showing previews, so the counter is
/// recounted from the slots when it claims to be saturated but nothing is
/// moving. Simulated here by leaking it directly, since the branch it
/// defends against is unreachable through the public API.
#[test]
fn a_leaked_in_flight_slot_is_reconciled_rather_than_stalling_forever() {
    let provider = Arc::new(MemoryProvider::new("mem", "Memory"));
    let state = state_with(provider.clone());
    let entry = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(16, 16))));

    let cache = ThumbnailCache::with_limits(DEFAULT_BYTE_BUDGET, DEFAULT_MAX_ENTRIES, 1);
    cache.lock().in_flight = 1;

    if cfg!(debug_assertions) {
        // A debug build asserts on the disagreement instead of quietly
        // papering over it — losing a read is a bug, and a test build must
        // say so. Verify *that*, then stop: the cache's counters are
        // indeterminate once the assert has unwound out of `next_start`.
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.request(&state, &entry)
        }));
        assert!(
            panicked.is_err(),
            "a debug build must surface the lost read rather than repair it silently"
        );
        return;
    }
    assert!(
        cache.request(&state, &entry).image().is_some(),
        "the stalled cap must be recounted, not believed"
    );
    assert_eq!(cache.in_flight(), 0);
    assert_eq!(cache.queued(), 0);
}

/// The visibility gate: a row requested once and then scrolled away is
/// dropped from the queue rather than fetched. This is what keeps a fast
/// scroll from reading hundreds of packages.
#[test]
fn a_row_that_stops_being_requested_is_dropped_not_fetched() {
    let memory = Arc::new(MemoryProvider::new("mem", "Memory"));
    let (counting, reads) = CountingProvider::new(memory.clone());
    let provider = Arc::new(FlakyProvider::new(
        counting.clone(),
        FlakyConfig::default().with_latency(2),
    ));
    let state = state_with(provider.clone());
    let visible = put(counting.as_ref(), "/stays.atmr", atmr_with(Some(&png(16, 16))));
    let scrolled_away = put(counting.as_ref(), "/leaves.atmr", atmr_with(Some(&png(16, 16))));
    let seeded = reads.count();

    // One read at a time, so the second row can only ever be queued.
    let cache = ThumbnailCache::with_limits(DEFAULT_BYTE_BUDGET, DEFAULT_MAX_ENTRIES, 1);
    assert_eq!(cache.request(&state, &visible), ThumbState::Pending);
    assert_eq!(cache.request(&state, &scrolled_away), ThumbState::Pending);
    assert_eq!(cache.in_flight(), 1);
    assert_eq!(cache.queued(), 1);

    // Subsequent frames only ask for the row still on screen.
    for _ in 0..10 {
        frames(&state, &provider, 1);
        cache.begin_frame();
        cache.request(&state, &visible);
    }

    assert!(cache.peek_entry(&visible, DEFAULT_THUMB_SIZE).image().is_some());
    assert_eq!(
        cache.peek_entry(&scrolled_away, DEFAULT_THUMB_SIZE),
        ThumbState::NotRequested,
        "the queued row must be dropped once it stops being requested"
    );
    assert_eq!(
        reads.count() - seeded,
        1,
        "the row that scrolled away must never be read"
    );
}

/// The queue serves the most recently requested key first, so the rows the
/// user has most recently looked at win the free slot.
///
/// Asserted on *which URI the provider was asked for*, not on the cache's
/// own state: `ThumbState` collapses queued and in-flight into `Pending`,
/// so a state-only assertion holds under arrival order too (swap
/// `max_by_key` for `min_by_key` in `next_start` and it stays green — the
/// read log does not).
#[test]
fn the_queue_serves_the_most_recent_request_first() {
    let memory = Arc::new(MemoryProvider::new("mem", "Memory"));
    let (counting, reads) = CountingProvider::new(memory.clone());
    let provider = Arc::new(FlakyProvider::new(
        counting.clone(),
        FlakyConfig::default().with_latency(2),
    ));
    let state = state_with(provider.clone());
    let first = put(counting.as_ref(), "/first.atmr", atmr_with(Some(&png(16, 16))));
    let second = put(counting.as_ref(), "/second.atmr", atmr_with(Some(&png(16, 16))));
    let third = put(counting.as_ref(), "/third.atmr", atmr_with(Some(&png(16, 16))));
    let seeded = reads.count();

    let cache = ThumbnailCache::with_limits(DEFAULT_BYTE_BUDGET, DEFAULT_MAX_ENTRIES, 1);
    cache.request(&state, &first); // takes the only slot
    cache.request(&state, &second);
    cache.request(&state, &third);
    assert_eq!(cache.queued(), 2);
    assert_eq!(
        reads.names_from(seeded),
        vec!["first.atmr"],
        "only the row that found the slot free is read"
    );

    // The first read lands from the pump — before the next visibility
    // round begins, with both queued rows still current. The most recently
    // requested of them takes the freed slot.
    frames(&state, &provider, 3);
    assert!(cache.peek_entry(&first, DEFAULT_THUMB_SIZE).image().is_some());
    assert_eq!(
        reads.names_from(seeded),
        vec!["first.atmr", "third.atmr"],
        "the freed slot goes to the most recent request, not the oldest"
    );

    // And the one still queued stays queued rather than being dropped, so
    // long as the widget keeps asking for it.
    cache.begin_frame();
    for entry in [&first, &second, &third] {
        cache.request(&state, entry);
    }
    assert_eq!(
        cache.peek_entry(&second, DEFAULT_THUMB_SIZE),
        ThumbState::Pending
    );
    assert_eq!(cache.queued(), 1);
    assert_eq!(cache.in_flight(), 1);
}

/// The same file at two preview sizes is two entries, not one fight over
/// a single slot.
#[test]
fn size_is_part_of_the_key() {
    let provider = Arc::new(MemoryProvider::new("mem", "Memory"));
    let state = state_with(provider.clone());
    let entry = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(256, 192))));

    let cache = ThumbnailCache::new();
    let small = cache
        .request_sized(&state, &entry, 32)
        .image()
        .cloned()
        .expect("decodes");
    let large = cache
        .request_sized(&state, &entry, 128)
        .image()
        .cloned()
        .expect("decodes");
    assert_eq!((small.width, small.height), (32, 24));
    assert_eq!((large.width, large.height), (128, 96));
    assert_eq!(cache.entry_count(), 2);
}

/// A preview smaller than the box is stored as-is: upscaling only costs
/// memory.
#[test]
fn a_small_preview_is_not_upscaled() {
    assert_eq!(decode::fit_within(48, 32, 128), (48, 32));
    assert_eq!(decode::fit_within(256, 192, 128), (128, 96));
    // Degenerate aspect: the short edge floors at one pixel rather than
    // rounding away to a zero-sized buffer.
    assert_eq!(decode::fit_within(1, 1000, 100), (1, 100));
}

/// Decoding is defensive: a non-PNG image is an absence (we have no JPEG
/// decoder), a corrupt PNG is a failure, and an implausibly large canvas
/// is refused before it is allocated.
#[test]
fn decode_refuses_what_it_cannot_safely_handle() {
    assert_eq!(decode_preview(b"\xff\xd8\xff-jpeg-ish", 128), Ok(None));

    let mut corrupt = png(8, 8);
    let tail = corrupt.len() - 8;
    corrupt.truncate(tail);
    assert!(decode_preview(&corrupt, 128).is_err());

    // A valid header claiming 30000×30000 — 900 megapixels — must be
    // rejected on the declared size, never decoded.
    let huge = huge_png_header(30_000, 30_000);
    match decode_preview(&huge, 128) {
        Err(message) => assert!(message.contains("implausibly large"), "{message}"),
        other => panic!("expected a size refusal, got {other:?}"),
    }
}

/// A PNG signature plus a well-formed IHDR for `w`×`h` and nothing else —
/// enough for the header parse the size check runs on.
fn huge_png_header(w: u32, h: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in bytes {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(b"IHDR");
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, no interlace
    let mut out = Vec::new();
    out.extend_from_slice(decode::PNG_MAGIC);
    out.extend_from_slice(&((ihdr.len() - 4) as u32).to_be_bytes());
    out.extend_from_slice(&ihdr);
    out.extend_from_slice(&crc32(&ihdr).to_be_bytes());
    out
}

/// Every clone is a view onto the same store — the widget's copy and the
/// one captured by an in-flight continuation must not diverge.
#[test]
fn clones_share_one_store() {
    let provider = Arc::new(MemoryProvider::new("mem", "Memory"));
    let state = state_with(provider.clone());
    let entry = put(provider.as_ref(), "/a.atmr", atmr_with(Some(&png(16, 16))));

    let cache = ThumbnailCache::new();
    let other = cache.clone();
    assert!(other.request(&state, &entry).image().is_some());
    assert!(cache
        .peek_entry(&entry, DEFAULT_THUMB_SIZE)
        .image()
        .is_some());
    assert_eq!(cache.entry_count(), other.entry_count());

    cache.clear();
    assert_eq!(other.entry_count(), 0);
    assert_eq!(other.bytes_used(), 0);
}
