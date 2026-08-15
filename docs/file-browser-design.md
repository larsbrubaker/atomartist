# In-App File Browser, Favorites Bar & Drag-Drop — Phase 6 Design

Status: **proposal**
Author: planning session, 2026-08-14
Parent plan: `storage-architecture-plan.md` (Phase 6 row, §7 items 1/6)
Reference codebases: `C:\Development\FDS\NodeDesigner` (parts bar + file
browser), `C:\Development\MatterCAD` (favorites persistence, thumbnail
cache, bed drag-drop). Concrete file citations throughout come from the
2026-08-14 exploration of both trees.

---

## 1. Goal

One browsing component, three faces:

1. **Open / Save modal** — the multi-provider picker the storage plan
   promised. Replaces `WebDialogs`' placeholder on web; native local
   keeps the OS dialog via `native_picker()`.
2. **Left favorites bar** — a collapsed rail on the canvas's left edge
   that expands into full file navigation. Mirror of NodeDesigner's
   right-side parts bar (`static/js/node-editor/ui/parts-bar.js`),
   placed left per product direction.
3. **Drag-drop source** — items dragged from the bar (or its expanded
   browser) insert into the scene, exactly like MatterCAD's library →
   bed flow and NodeDesigner's `parts-bar-drag.js`.

Plus the substrate both need: **thumbnails**, embedded in `.atmr` and
cached per provider entry.

## 2. What we take from the ancestors

| Decision | Source | AtomArtist shape |
|---|---|---|
| One component, mode-switched (`open/save/component`), embeddable | NodeDesigner `file-browser-dialog.js` (modal + `mountEmbedded()`) | One widget + `BrowserMode` enum; embedded mode drops header/footer/keyboard capture, delivers picks via callback |
| Handle = toggle **and** resize grip (3 px threshold; pull-open; snap-closed < 120 px keeps stored width) | `parts-bar.js` handle logic | Same constants to start; bar on left so drag **right** widens |
| Lazy mount of the expanded panel; icons fill in after first paint | `parts-bar.js` (`browserMounted`, deferred `renderPartIcons`) | Expanded panel widget built on first expand; thumbs stream in via jobs |
| Favorites are `{kind, stable_key, display_name}` records, seeded on first run, "user emptied" ≠ "never seeded" | MatterCAD `FavoritesService.cs` | Same triple, serialized in `UiSettings`; kinds: `NodeType` (primitive palette) and `Project(StorageUri)` |
| Stale-response guards on every async listing | `file-browser-dialog.js` generation counters | Listing model stamps each `list()` job with a generation; stale results dropped |
| Empty / loading / error states are first-class | `file-browser-file-grid.js` | `Listing::{Loading, Ready(entries), Error(msg), Empty}` painted distinctly; never a blank pane |
| Thumbnail cache with version-stamped names + visibility-gated generation | MatterCAD `ThumbnailsConfig.cs`, `ListViewItemBase.cs` | In-memory cache keyed `(uri, stamp, size, CACHE_VERSION)`; decode/generate only for rows on screen |
| Drop pipeline: threshold → ghost → real insertion on canvas-enter → live eval → single undo on commit; leave = remove + re-ghost | `parts-bar-drag.js`, MatterCAD `ViewDragDropHandler.cs` | Reuse the existing OS file-drop import path for the insert; wrap gesture in one undo command; base-position snapshot so move math never accumulates |
| Insert-then-load with placeholder for async payloads; capture the target graph at insert time, not resolve time | MatterCAD `InsertionGroupObject3D.cs` | Continuations capture the graph Arc they insert into (drill-in safe) |

Deliberately **not** copied: `innerHTML`-style full-list rebuild with no
virtualization (their known weakness — our grid model is
data-driven and can window later); MatterCAD's swap-the-whole-view
collapse (NodeDesigner's animated width is cheaper); thumbnail sidecar
servers (no server here).

## 3. Thumbnails in `.atmr`

`.atmr` is a zip and pre-release: embed the preview at
**`Metadata/thumbnail.png`** — the OPC/3MF convention NodeDesigner
already reads from 3MF packages (`dfs-local-backend.ts:676-733`), so
slicers and other tools that sniff that path get our previews for free.

- **Implemented as an opportunistic background capture, not a
  save-time one.** Reading pixels back off the GPU can only happen
  inside the frame loop and cannot be made synchronous without stalling
  the save, so the shell (`demo-native::thumbnail_capture`) refreshes a
  preview roughly every **5 s** (first one after a 2 s warm-up, and only
  on frames that actually paint — an idle app's preview simply ages) and
  parks the encoded PNG in `AppState::latest_thumbnail` via
  `set_thumbnail_png`. Save embeds whatever is in that slot.
- The crop is the **3-D viewport widget's rectangle** (found by the
  `"viewport-3d"` id, flipped from agg-gui's bottom-up coordinates into
  framebuffer rows and clipped to the surface), with NodeDesigner's
  fixed ~4:3 `capturePreviewImage` crop applied *inside* it, box-scaled
  to a 256×192 PNG. A window-wide crop would mostly show the node
  canvas and the side panels. No viewport on screen ⇒ no capture.
- **Accepted staleness:** the embedded preview can be a few seconds
  older than the graph it ships with. What is *not* accepted is a
  preview of a different project — File → New and File → Open both
  clear the slot, so a save issued before the next capture writes no
  preview rather than the previous model's.
- `read_project_from_bytes` ignores it; a separate cheap
  `read_thumbnail_from_bytes(bytes) -> Option<Vec<u8>>` lets the
  browser pull previews without decoding graphs. It reads only the
  `.rels` part plus the declared/conventional image paths, each with a
  hard size cap, so a hostile package can't turn a listing into an
  out-of-memory abort.
- Absent entry ⇒ glyph fallback (never a broken image — the
  ancestors' rule).
- Headless shells (tests, `--screenshot-to` off-path) save without a
  thumbnail; the entry is optional forever.

> **Resolved (6b, first item):** the capture used to frame the node
> canvas because `Widget::bounds()` is *parent-local* and the viewport
> widget resets its origin to (0, 0) in layout. Fixed by adding
> `find_widget_screen_rect` to agg-gui (absolute placement via the
> inspector's transform walk; hidden subtrees return `None`) and
> extracting the crop derivation into
> `atomartist_ui::viewport_framebuffer_crop`, pinned by
> `thumbnail_crop_frames_the_viewport_not_the_node_canvas`.

Mesh files (`.stl`/`.obj`/`.3mf`) shown in the browser: 3MF may carry
its own `Metadata/thumbnail.png` (read it); STL/OBJ get the format
glyph in v1 — offline mesh rendering for thumbs is deferred (MatterCAD
needed an offscreen render farm for this; not v1).

## 4. Architecture

```
atomartist-ui/src/file_browser/
  model.rs        BrowserModel: provider roots, cwd, Listing state,
                  generation counters, selection, search filter.
                  Drives StorageProvider::list()/stat() jobs through
                  the Phase 4 pump. No widgets.
  thumbs.rs       ThumbnailCache: (uri, stamp, size, version) -> image;
                  visibility-gated decode queue; glyph fallbacks.
  widget.rs       The shared browser widget (breadcrumb, grid, name
                  field, provider sidebar). Renders a BrowserModel.
  modal.rs        Open/Save dialog host (modal chrome, OK/Cancel,
                  mode-specific affordances).
favorites_bar.rs  Handle + rail + lazy expanded panel hosting the
                  shared widget in embedded mode.
drag_insert.rs    The gesture controller (threshold, ghost, insert /
                  remove / commit, undo wrapping).
```

- **agg-gui question** (plan §4): the generic pieces — a resizable
  edge-docked panel with handle semantics, and a thumbnail grid — are
  candidates to graduate upstream *after* they prove out here. v1
  builds them in AtomArtist; promotion is a later additive move.
- `FileDialogProvider` finally becomes what the plan intended: pickers
  return through the browser modal on every provider except
  native-local `file:` (OS dialog via `native_picker()`). `WebDialogs`
  dies.
- Bar state (expanded, width, favorites) persists in `UiSettings` —
  which both shells now persist (native file / web localStorage).

## 5. Steps

| # | Step | Contents | Depends on | Status |
|---|---|---|---|---|
| 6a | Thumbnail substrate | `Metadata/thumbnail.png` in atmr write/read; viewport capture on save; `read_thumbnail_from_bytes` | — | **done** |
| 6b | Browser core | `BrowserModel` + listing states + generation guards; `ThumbnailCache` (+ quiet storage ops); shared widget | 6a | **done** |
| 6c | Open/Save modal | modal host; `FileDialogProvider` rewired; `WebDialogs` retired; native keeps OS dialog | 6b | **done** |
| 6d | Favorites bar | rail + handle + lazy expanded panel (embedded browser); favorites records in `UiSettings`; seeding | 6b | **done** |
| 6e | Drag-drop insert | gesture controller; ghost; insert-on-enter (node types; file payloads keep the ghost and import on release — live carry of an async import needs a MatterCAD-style placeholder, hooks left in place); single undo; cancel semantics | 6d | **done** |

Each step: implementer → reviewer → fixes → commit, tests green
throughout, harness tests over MemoryProvider + FlakyProvider so async
providers stay honest.

## 6. Test seams

- `BrowserModel` is widget-free: unit-test navigation, generations,
  stale-response dropping, search filtering natively.
- Bar + modal drive through `atomartist-ui-test` (`find_by_id`
  "favorites-bar", "file-browser", synthetic drags via the harness's
  mouse events — the drag controller listens to the same
  `on_mouse_*` stream the canvas does).
- A `window.getPartsBar`-equivalent inspection hook: the bar exposes
  its state (expanded, width, drag-in-flight) via widget `properties()`
  reflection, the same channel StatusBar already uses.
- Thumbnail cache: pure unit tests (eviction, stamp invalidation,
  version bump).

## 7. Open questions

1. **Ghost rendering** — agg-gui overlay layer vs a floating widget;
   needs a look at agg-gui's z-order/overlay facilities before 6e.
2. **Search scope** — v1 filters the current listing only (both
   ancestors debounce-search server-side; we have no server). Provider
   `search()` can join the trait later without breaking (plan §10).
3. **Favorites double-duty** — NodeDesigner's rail is a *primitive
   palette*; ours also wants pinned projects/folders. Seed with
   primitives (MatterCAD's model), let users pin storage entries; the
   `kind` field keeps the two cleanly separate.
