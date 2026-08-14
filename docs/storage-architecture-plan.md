# Pluggable Storage & Accounts — Architecture Plan

Status: **proposal** (not yet implemented)
Author: planning session, 2026-08-13
Scope: `atomartist-lib`, `atomartist-ui`, `demo-native`, `demo-wasm`, plus three
new crates (`atomartist-storage`, `atomartist-storage-http`, `atomartist-auth`).

> **Pre-release freedom.** AtomArtist has not shipped, so there are no users,
> no saved projects, and no file format in the wild to preserve. This plan
> therefore assumes we may **change any interface, on-disk format, or wire
> format outright** rather than layering compatibility shims. What lands here
> becomes the behaviour of the first official deployment. Every "migrate the
> old thing" step is deleted on purpose — if a design is right, we adopt it
> as the only design.

---

## 1. Goal

One project-storage abstraction that serves three very different backends
without the rest of the app knowing which is in use:

| Backend | Where it runs | Shipped in OSS repo |
|---|---|---|
| Native filesystem | desktop | yes |
| Browser-local (OPFS / IndexedDB) | WASM | yes |
| Remote HTTP service (subscription accounts) | both | the *generic* provider yes; the MatterHackers deployment no |

Secondary goals:

- A third party can add Dropbox / Google Drive / S3 / WebDAV / self-hosted
  storage by implementing one trait in their own crate — no fork.
- MatterHackers can ship a closed-source provider crate + login flow that
  drops into the same seam, giving subscribers their files on any device.
- The open-source app remains fully usable with **zero** account, zero
  network, zero telemetry. Sign-in is opt-in and additive.

Non-goals for v1: real-time multi-user co-editing, server-side graph
evaluation, sharing/permissions UI, asset CDN streaming. The design leaves
room for these (§10) but does not build them.

---

## 2. What exists today (and why it must change)

Findings from the current tree:

- `atomartist-ui/src/app_state_files.rs` — every operation takes `&Path`
  and calls `std::fs::read` / `std::fs::write` directly
  (`load_graph_from_path`, `save_graph_to_path`, `import_mesh_file`,
  `import_mcx_file`, `import_project_file`, `export_mesh_to_path`).
- `atomartist-lib/src/serialization/atmr.rs` — path-based entry points, but
  already has `write_atmr_into<W: Write + Seek>` and
  `read_graph_json_from_atmr<R: Read + Seek>`. The byte-oriented core is
  half-present; the path wrappers are the thin part.
- `atomartist-ui/src/top_menu_bar.rs` — `FileDialogProvider` returns
  `Option<PathBuf>` from **blocking** calls; `demo-native` implements it with
  `rfd`, `demo-wasm` uses `NoFileDialogs` (no open/save on web at all).
- `UiSettings.last_project_path` / `recent_projects` are `PathBuf`.

Three structural problems:

1. **`PathBuf` is the identity of a project.** A cloud object has no path.
2. **All IO is synchronous.** `rfd`'s blocking dialog is fine because the
   agg-gui loop is paused. A network round-trip is not fine — and on WASM
   blocking is impossible at all (no threads, no blocking on the main task).
3. **No notion of "who am I".** Nothing carries an identity or a token.

Problem 2 is the expensive one and drives the whole design. Retrofitting
async later would touch every call site twice, so it goes in first.

---

## 3. Core concepts

### 3.1 `StorageUri` — replaces `PathBuf` as project identity

```rust
/// Opaque, serializable location of a project or asset.
/// Rendered as a URI so it can live in settings, recents, and the
/// window title without the UI knowing the scheme.
pub struct StorageUri {
    scheme: Arc<str>,   // "file", "browser", "mh", "webdav", "s3", ...
    path:   Arc<str>,   // provider-defined, always '/'-separated
}
```

Examples:
`file:///C:/Users/lars/Documents/bracket.atmr`,
`browser:///projects/bracket.atmr`,
`mh:///u/1a2b/projects/bracket.atmr`.

Rules:
- The UI never parses the path segment; it asks the provider for a
  `display_name()` and `parent()`.
- `file:` round-trips losslessly to `PathBuf` on native (helper
  `StorageUri::to_local_path() -> Option<PathBuf>`), so drag-and-drop,
  CLI args, and OS "Open With" work.
- Recents and `last_project_path` are `Vec<StorageUri>` /
  `Option<StorageUri>` — the settings struct is simply redefined, with no
  migration path from the current `PathBuf` fields. Any developer's stale
  settings file is discarded on parse failure.
- `PathBuf` disappears from `AppState` entirely. It survives only inside
  `LocalFsProvider`, which is the one place allowed to touch `std::fs`.
  Enforce with a grep-based test (`no_fs_outside_provider`) in the same
  spirit as the existing `file_line_count.rs` guard.

### 3.2 `StorageProvider` — the plug-in seam

Object-safe, no `async fn` in the trait (keeps it dyn-compatible and avoids
forcing an executor on native). Every fallible/slow call returns a job
handle that the UI polls once per frame.

```rust
pub trait StorageProvider: Send + Sync {
    fn scheme(&self) -> &str;
    fn display_name(&self) -> &str;          // "This PC", "MatterHackers Cloud"
    fn capabilities(&self) -> Capabilities;  // see below

    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>>;
    fn read(&self, at: &StorageUri) -> Job<Blob>;
    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp>;
    fn delete(&self, at: &StorageUri) -> Job<()>;
    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>>;
    fn create_dir(&self, at: &StorageUri) -> Job<()>;

    /// Optional: providers that prefer the OS picker (native local)
    /// answer `Some`, and the UI shows the native dialog instead of the
    /// in-app browser. Cloud providers return `None`.
    fn native_picker(&self) -> Option<&dyn NativePicker> { None }
}

pub struct Capabilities {
    pub writable: bool,
    pub can_list: bool,
    pub can_create_dir: bool,
    pub versioned: bool,      // supports Precondition::IfMatch
    pub max_blob_bytes: Option<u64>,
    pub requires_auth: bool,
}

pub struct Entry {
    pub uri: StorageUri,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified: Option<SystemTimeish>, // wall-clock ms, WASM-safe
    pub stamp: Option<Stamp>,            // ETag / generation / mtime hash
}

pub enum Precondition { None, IfMatch(Stamp), IfAbsent }
```

`Precondition` + `Stamp` are what make "same file from two devices" safe:
a save that was based on stamp *X* fails loudly if the server moved on,
and the UI offers **Keep mine / Take theirs / Save as copy** rather than
silently clobbering.

### 3.3 `Job<T>` — the async bridge

The single concession the whole app must make to networking. A `Job<T>` is a
handle to work in flight:

```rust
pub struct Job<T> { /* Arc<Mutex<JobSlot<T>>> */ }

impl<T> Job<T> {
    pub fn poll(&self) -> JobState<&T>;      // Pending { progress } | Ready | Failed
    pub fn cancel(&self);
    pub fn ready(v: T) -> Job<T>;            // for sync providers
}
```

- Native providers run the work on a small `std::thread` pool and park the
  result in the slot.
- WASM providers use `wasm_bindgen_futures::spawn_local` and park the result
  in the same slot. Same type, same call sites.
- The local providers on both platforms can complete *synchronously* via
  `Job::ready`, so the common desktop path keeps today's zero-latency feel
  and existing tests don't need to spin.
- `AppState` grows a `pending: Mutex<Vec<PendingOp>>` drained once per frame
  by an existing tick hook; each entry maps a finished job to its
  continuation (swap in the graph, show an error, refresh the browser list).
  Long-running jobs surface in the status bar with a cancel affordance.

This is deliberately *not* `async fn` + a runtime: adding tokio to a WASM
GUI is a bigger hammer than a poll-per-frame slot, and the frame loop is
already the natural place to apply results.

### 3.4 `StorageRegistry`

Mirrors the existing node `registry.rs` pattern: providers register under
their scheme at startup; `AppState` holds an `Arc<StorageRegistry>` and
resolves `StorageUri -> &dyn StorageProvider`. Shells decide what to
register:

- `demo-native`: `LocalFsProvider` (+ any provider crate the build enables)
- `demo-wasm`: `BrowserProvider`
- MatterHackers builds: additionally `MhCloudProvider`

---

## 4. Crate layout

```
atomartist-storage/          NEW — traits, StorageUri, Job, registry,
                                   LocalFsProvider (native),
                                   BrowserProvider (wasm, OPFS+IndexedDB),
                                   MemoryProvider (tests)
atomartist-storage-http/     NEW — generic REST provider + WebDAV example,
                                   the reference third-party integration
atomartist-auth/             NEW — AuthProvider trait, OAuth2 PKCE flow,
                                   token storage (keyring / IndexedDB),
                                   NoAuth default
atomartist-lib/serialization byte-oriented save/load (path fns become
                                   wrappers)
atomartist-ui                file browser widget, account UI, AppState
                                   ops rewritten against StorageUri
```

Why separate crates: a proprietary MatterHackers provider can depend on
`atomartist-storage` + `atomartist-auth` alone without pulling the GUI, and
`atomartist-lib` stays free of any network dependency. Per CLAUDE.md, nothing
here belongs upstream in `agg-gui` — this is application-domain, not widget
toolkit. (One possible exception: if the in-app file browser turns out to be
a generally useful list/tree widget, the *widget* goes to `agg-gui` and the
storage-aware model stays here.)

---

## 5. Making serialization byte-oriented

Precondition for everything else, and independently valuable.

```rust
// atomartist-lib/src/serialization/atmr.rs
pub fn write_project_to_bytes(graph: &Graph, assets: &AssetStore) -> Result<Vec<u8>, AtmrError>;
pub fn read_project_from_bytes(bytes: &[u8], registry: &Registry)
    -> Result<(LoadResult, AssetStore), AtmrError>;
```

These become the **only** project entry points. The six path-based
functions (`save_project_to_path`, `load_project_with_assets_from_path`,
`save_atmr_to_path`, …) are **deleted**, not wrapped — path IO belongs to
`LocalFsProvider`, and leaving convenience wrappers around is exactly how
`std::fs` leaks back into the app layer. Tests that currently round-trip
through temp files move to in-memory buffers, which makes them faster and
lets them run under WASM.

`write_atmr_into` already proves the zip layer is stream-clean; the read
side needs `Cursor<&[u8]>` instead of `File`.

Since the `.atmr` format is not yet released, fold the format decisions in
§8 (content-addressed assets) and §9 into it **now**, and set
`SCHEMA_VERSION` to `1` at first deployment rather than carrying whatever
number development has drifted to.

Mesh import/export (`import_mcx`, `decode_mesh`, `export_stl/obj/3mf`) are
already byte-based — only their `std::fs::read` callers in
`app_state_files.rs` change.

---

## 6. Authentication

### 6.1 Trait

```rust
pub trait AuthProvider: Send + Sync {
    fn id(&self) -> &str;
    fn status(&self) -> AuthStatus;            // SignedOut | Pending | SignedIn(Account)
    fn begin_sign_in(&self) -> Job<AuthStatus>;
    fn sign_out(&self) -> Job<()>;
    /// Fresh bearer token, refreshing if needed. Storage providers call
    /// this per request rather than caching a token themselves.
    fn access_token(&self) -> Job<Option<SecretString>>;
}

pub struct Account { pub id: String, pub display_name: String,
                     pub email: Option<String>, pub entitlements: Vec<String> }
```

`entitlements` is how a subscription gates cloud storage: the provider
advertises `requires_auth`, and the UI shows an upgrade prompt (rendered from
a provider-supplied message + URL — the OSS app hardcodes no MatterHackers
strings) when the entitlement is missing.

### 6.2 Flow: OAuth 2.0 Authorization Code + PKCE

The right choice for both shells, and the same flow any third-party
provider will need:

- **Native**: open the system browser to the authorize URL; a loopback
  listener on `127.0.0.1:<ephemeral>` catches the redirect. No embedded
  webview, no password ever entering AtomArtist. Refresh token stored in the
  OS credential store (`keyring` crate: DPAPI / Keychain / libsecret),
  falling back to a file with 0600 perms and a clear warning.
- **WASM**: redirect (or popup) to the authorize URL, return to a
  `/auth/callback` route; refresh token in IndexedDB, access token in memory
  only. State + PKCE verifier in `sessionStorage`.

**Explicitly out of scope:** AtomArtist never collects a password, never
implements its own credential form, and never stores card details. Sign-in
happens on the provider's own domain in the user's browser.

Default build registers `NoAuth` — status is permanently `SignedOut`, no
account UI is shown, no network code is reachable.

---

## 7. UI changes

0. **`FileDialogProvider` is replaced, not extended.** Its methods return
   `Job<Option<StorageUri>>`; `demo-native`'s `rfd` implementation is kept
   only behind `native_picker()` for the `file:` scheme. No transitional
   two-trait period.
1. **In-app file browser** (`atomartist-ui/src/file_browser/`) — a modal with
   a provider sidebar ("This PC", "This Browser", "MatterHackers Cloud"),
   a listing pane driven by `list()`, name field, and Open/Save actions. Used
   for every provider except native-local, which keeps the OS dialog via
   `native_picker()`. Follows the bottom-up Y convention.
2. `NoFileDialogs` stays for headless tests.
3. **Account chip** in the top-right of the menu bar: signed-out = "Sign in",
   signed-in = avatar/initials + a menu (Account, Storage usage, Sign out).
   Hidden entirely when the only registered auth provider is `NoAuth`.
4. **Sync status in the status bar**: idle / "Saving…" / "Saved 2 min ago" /
   "Offline — changes saved locally" / conflict badge.
5. **Conflict dialog**: Keep mine (save as new version) / Take theirs
   (reload) / Save as copy. Never auto-merge graphs.

   **Failure-reporting policy (settled in Phase 4c).** Once storage calls
   became asynchronous, every failure had a natural home in the status-bar
   notice queue — and that turned out to be too quiet for the operations
   that matter. The rule we landed on: **save and open failures raise a
   modal `show_error` *in addition to* the notice; export, import, and the
   recent-list prune stay notice-only.** The split is about what losing the
   result costs. A failed save loses work the user cannot get back by
   repeating the action, and it most often happens on the window-close path
   where the next thing to happen is the app disappearing; a failed open
   that only writes a status line is indistinguishable from the app
   ignoring the click. Export, import, and pruning are non-destructive and
   land immediately after an explicit user action, so the status bar is
   enough — a modal there is just a second click. Mechanically the modal is
   raised from the operation's continuation, which the frame pump runs on
   the main thread (where `rfd` needs to be anyway); `menu_actions` gets the
   `FileDialogProvider` as an `Arc<dyn …>` so it can be cloned into that
   continuation, which is why the trait carries `Send + Sync`. The
   startup auto-reopen is deliberately *outside* this policy: nobody asked
   for it, so `AppState::reopen_last_project` reports at `Info` and prunes
   the stale entry rather than parking a sticky error in the one display
   slot.
6. **Web gets Open/Save for the first time** — even without any cloud, the
   `browser:` provider plus this UI fixes today's `NoFileDialogs` gap.
   Native file-system access on web also gets a `file:` bridge where the
   browser supports the File System Access API, with download/upload
   fallback elsewhere.

---

## 8. Offline, caching, and autosave

- **Write-through local cache** for remote providers: every remote project
  also lands in the platform-local store keyed by URI + stamp. Opening a
  cached project while offline works read-only-until-reconnect.
- **Dirty queue**: saves attempted while offline are queued and retried;
  the status bar shows the pending count. Queue survives restart.
- **Autosave** to the *local* store on a timer regardless of backend, so a
  crash never costs work even mid-upload.
- **Asset dedup**: `AssetStore` is already content-hash keyed, so the
  remote protocol can `PUT /assets/{hash}` once and reference it from many
  projects. Big win for cloud storage cost and upload time; worth designing
  into the wire format in v1 even if the first server implementation is
  naive (blob-per-project).

---

## 9. Reference HTTP protocol (`atomartist-storage-http`)

Ships in the OSS repo so anyone can stand up a server. Deliberately boring
REST + bearer token:

```
GET    /v1/files?prefix=/projects        -> [Entry]
GET    /v1/files/{path}                  -> bytes  (ETag)
PUT    /v1/files/{path}                  -> Stamp  (If-Match / If-None-Match)
DELETE /v1/files/{path}
GET    /v1/account                       -> { id, display_name, entitlements, quota }
PUT    /v1/assets/{sha256}               -> 201 | 204 already-present
GET    /v1/assets/{sha256}               -> bytes
```

Configured by a small TOML/JSON profile (base URL, OAuth endpoints, client
id, scheme name) so a self-hoster adds a backend with a config file and no
Rust at all. MatterHackers' provider is then "this generic provider with a
shipped profile", plus whatever proprietary extras it needs — which keeps
the OSS path honest, because it's the same code path we run.

---

## 10. Room left for later

Not built now, but the design must not preclude:

- Sharing links / permissions (`Entry` gains an ACL field).
- Versions & history (`Stamp` is already a version handle; add
  `list_versions`).
- Real-time collaboration (would need an op-log, not this file-blob model —
  but a CRDT layer could sit above `StorageProvider` without changing it).
- Server-side rendering/evaluation (unrelated seam).

---

## 11. Implementation phases

Each phase compiles, passes the full suite, and is independently shippable.

| # | Phase | Deliverable | Risk |
|---|---|---|---|
| 1 | Byte-oriented serialization + final `.atmr` format | `write_project_to_bytes` / `read_project_from_bytes` as the only entry points; path fns deleted; content-addressed assets baked in; `SCHEMA_VERSION = 1` | low |
| 2 | `atomartist-storage` skeleton | `StorageUri`, `Job`, `StorageProvider`, `StorageRegistry`, `MemoryProvider`, conformance suite | low |
| 3 | `LocalFsProvider` + AppState rewrite | `app_state_files.rs` speaks `StorageUri`; `PathBuf` gone from `AppState`; settings struct redefined; `no_fs_outside_provider` guard test | medium — wide, but no compatibility constraint |
| 4 | Job pump in the frame loop | `AppState::pending`, status-bar progress, cancel | medium |
| 5 | `BrowserProvider` (OPFS + IndexedDB) | web can save/open projects locally | medium |
| 6 | In-app file browser widget | multi-provider open/save UI; native keeps OS dialog | medium |
| 7 | `atomartist-auth` + OAuth PKCE | loopback (native) and redirect (wasm) flows, token storage, account chip | medium |
| 8 | `atomartist-storage-http` | generic REST provider, profile config, conflict handling, offline cache + dirty queue | medium |
| 9 | Docs + example server | protocol spec, "write your own provider" guide, minimal reference server (pure Rust) | low |

**Phases 1–3 should land as one continuous stretch of work**, ideally before
much more node/UI code accumulates against the current `PathBuf` API. The
refactor cost grows with every new call site, and it is the single cheapest
it will ever be right now. Phase 3 was the plan's high-risk item only
because of compatibility; with nothing to preserve, its tests are free to
change alongside it and "did behaviour drift?" stops being a question we
have to answer.

**Ordering note:** phases 1–6 deliver real user value with *no* account and
*no* network — web file support alone justifies them. Cloud is strictly
additive on top, and phases 7–8 can slip past the first deployment without
stranding anything, provided the format decisions from phase 1 are in.

---

## 12. Testing strategy

Per CLAUDE.md — tests exercise production code, and bugs get a reproducing
test first.

- `MemoryProvider` + a `FlakyProvider` wrapper (injectable latency, failures,
  stamp conflicts) as the backbone for provider-agnostic tests.
- **Conformance suite**: one test module every provider must pass
  (round-trip, list, overwrite, `IfMatch` conflict, delete, missing-file
  error shape). Third-party providers get it as a public test helper.
- `atomartist-ui-test` gains storage-flow tests: open/save via the harness
  against `MemoryProvider`, conflict dialog resolution, offline queue drain.
  Job-pump tests drive the frame tick explicitly rather than sleeping.
- Auth tested against a stub authorization server (PKCE verification,
  refresh, expiry) — no live network in CI.
- Existing file tests (`file_menu_features.rs`, `workflows.rs`,
  `drag_drop_mesh.rs`, `nodedesigner_examples.rs`) are **rewritten**
  against `StorageUri` + `MemoryProvider` rather than preserved. They are
  the specification of intended behaviour, not a compatibility contract —
  but each rewrite must keep asserting the same *user-visible outcome*, so
  "the test changed" never becomes cover for "the feature broke".
- Once serialization is byte-oriented, most of these lose their temp-file
  dependency and can run in the WASM test target too.

---

## 13. Open questions

1. ~~**Project granularity on the wire**~~ — **settled**: blob for the graph,
   content-addressed assets from day one (§8). This was only ever a question
   because retrofitting the asset split is a wire-format break; pre-release,
   we simply adopt the better format and never carry the other one.
2. **Quota/entitlement UX** — how much does the OSS app know about
   subscription state? Recommendation: nothing beyond an opaque
   `entitlements: Vec<String>` and a provider-supplied message + URL.
3. **Do we want a `browser:`-backed recents on web** that syncs to the
   account once signed in, or keep recents strictly per-device?
4. **Encryption at rest for the cloud provider** — server-side only, or
   optional client-side envelope encryption? The latter conflicts with
   server-side thumbnailing/search later.
5. **UNC / network paths on Windows.** `StorageUri` is authority-less
   (`scheme:///path`), so a UNC share has nowhere to put its host:
   `\\server\share\p.atmr` would normalize to
   `file:///server/share/p.atmr` and come back as `/server/share/p.atmr`,
   which Windows resolves against the *current drive* — a save that
   reports success while writing elsewhere. Phase 3 therefore makes
   `StorageUri::from_local_path` return `Option` and **refuses** UNC and
   verbatim (`\\?\`, `\\.\`) paths outright; the shell tells the user to
   map the share to a drive letter, which round-trips like any other
   path. Proper support needs a decision: add an optional host component
   to `StorageUri` (and therefore to the `scheme://host/path` rendering),
   or let `LocalFsProvider` own a private encoding of the share name
   inside the path segment. The first is cleaner and also serves remote
   providers that want `mh://tenant/…`; it is a breaking change to the
   URI grammar, so it should land before the first deployment if we want
   it at all.
6. ~~**Path traversal once a provider is rooted**~~ — **settled**: `.` and
   `..` segments are **rejected outright** at construction/parse time, not
   resolved. A URI is an authorization boundary the moment a provider has a
   root (`browser:` OPFS, a per-account cloud prefix), and a value that
   cannot *express* traversal needs no root re-check anywhere; resolving
   instead would invite ordering bugs about when normalization ran relative
   to that check. `StorageUri::try_new` / `try_join` / `FromStr` /
   `Deserialize` return `UriParseError::TraversalSegment`; the infallible
   `new` / `join` keep their signatures and panic, documented as a
   programmer-error precondition for code-authored literals. Local paths
   legitimately containing `..` (the user navigated up in a picker) are
   resolved *lexically* by `from_local_path` before URI-ification — no
   filesystem access, so an as-yet-nonexistent save target still works and a
   symlinked path is not silently rewritten — and refused when the `..`s
   underflow or would pop a Windows drive prefix (`C:\a\..\..\b`, which
   would otherwise name a different volume). The conformance suite
   (`traversal_uris_cannot_reach_the_provider`) pins that traversal URIs
   remain unrepresentable at the type level for every provider — it cannot
   test a provider's *resolution* of such a path, because no such value
   can be built to hand it.
