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

## 5b. Phase 6f — NodeDesigner-parity rework (user feedback, 2026-08-15)

First-use feedback on 6d/6e: the bar landed on the wrong pane and
diverged from NodeDesigner's iterated design. 6f realigns to the
ancestor, from a fresh 2026-08-15 exploration of
`C:\Development\FDS\NodeDesigner` (citations therein:
`parts-bar.js`, `parts-bar-icons.js`, `parts-bar-drag.js`,
`core/node-insertion.js`, `file-browser-*.js`).

| # | Step | Contents | Status |
|---|---|---|---|
| 6f-1 | Bar on the 3D pane; strip persists | Bar docks to the **3-D viewport pane** (left edge per product direction; ND uses right). DOM order handle → strip → panel; the 72 px icon strip **never collapses** — expanding grows only the browser panel beside it. Constants from ND: panel default 380, min 240, collapse threshold 120, max 70 % of the *pane*, handle 16×56 centered with chevron, icon slot 44×44, label 9 px; the strip scrolls when favorites overflow (ND `overflow-y: auto`). Unlike ND (absolute overlay, `pointer-events: none` container), our bar insets the viewport row — simpler, and the viewport rect self-corrects everywhere it is derived. | **done** |
| 6f-2 | Rendered primitive icons | Icons are runtime offscreen renders of the **real primitive generators with the node's own default properties** (ND: 96 px PNG, Z-up camera, dir `(-0.35,-1,0.55)`, fov 30, distance `r/tan(fov/2)·1.15`, ambient 0.55 + key 1.6 + fill 0.5, faceted per-face normals). Deferred past first paint — strip appears with labels, icons fill in. Investigate the cheapest render path (existing wgpu offscreen vs a tiny software rasterizer for 96 px; headless/wasm must not break). *Outcome: software rasterizer (`mesh_raster.rs`) — the renderer has no headless entry point, and 7 icons of a few hundred triangles measure 5.24 ms total. Icons rasterize at the slot's device-pixel size (both backends blit NEAREST, so ND's 96 px + linear-CSS downscale doesn't transfer); cache keyed (type_id, pixel_size); one icon per painted frame, idle after fill-in.* | **done** |
| 6f-3 | Browser parity pass | Tile grid `minmax(120px,1fr)` gap 12; card 80×60 thumb (`cover`), name **below** at 12 px word-break, type accent colors (folder `#f0c674`, file `#6ca4dc`, mesh `#b07cb0`); order folders → files; nav bar = back button (history, Alt+Left) + breadcrumb; search top-right with clear ×, 300 ms debounce, ≥2 chars (local filter — no server); ND state texts ("This folder is empty", "Loading...", search prompt). File-management footer (New Folder / Rename / Move / Delete) deferred — needs provider trait additions (rename/move). *Deviations: (a) **no debounce and no 2-character floor** — ND's are there to rate-limit a server round trip, and ours filters the listing already in memory, so the same UI runs on a different engine and filters from the first keystroke; (b) **keys are modal-face only** — Alt+Left (Back) and Escape (clear search) answer in Open/Save, while the embedded face stays inert per ND's `mountEmbedded()` contract, so the clear × is the way out of a filter in the favorites bar; (c) history is a plain back stack with no forward, matching ND's single nav button.* | **done** |
| 6f-4 | Drop on the 3-D bed | Parts drag targets the **3-D viewport**; release over the bed inserts the node, auto-wires first `geometry` output → first empty Output input (no wiring if none — silent, per ND), and places the node in the graph via a port of `findPositionLeftOfOutput`: `x = out.x − w − 50`, vertically centered, occupancy test inflated 20 px, scan ±30…300 up-first, pile-up fallback with a 30-px advancing offset wrapping at 150; no Output → node-canvas viewport center. One shared placement helper serves parts drag, OS mesh drop, and future component insertion (ND extracted exactly this). Drop-position→matrix raycast and live carry on the bed are deferred (ND does live carry; our v1 ghosts until release — hooks stay). Node-canvas drops keep working and now also auto-wire. *Outcome note: the wire is a `ConnectToFreeInputCmd` that re-resolves the Output's first free input on every do/redo — the Output regrows a disconnected input socket with a fresh uid, so a cached-uid ConnectCmd silently loses the wire on redo. Mesh imports share the same plan and batch the wire into their own undo step.* | **done** |

## 5c. Phase 6g — polish round (user feedback, 2026-08-15, second build)

| # | Step | Contents | Status |
|---|---|---|---|
| 6g-1 | Host waker wiring | demo-native installs `agg_gui::animation::set_host_waker` (EventLoopProxy nudge; agg-gui 851bff0) at startup, clears at shutdown; measure and then remove/narrow storage_ops' per-frame keep-alive so an idle open dialog stops repainting at full framerate; continuation delivery must stay prompt. demo-wasm's rAF loop needs nothing. *Outcome: measured 116–145 fps idle-with-dialog → the loop parks completely (0 frames) until a settle wakes it. Chain: `JobCompleter::settle` → atomartist-storage's new process-global completion hook → `signal_async_state_change` → agg-gui host waker → `EventLoopProxy` → pump. No fallback poll; a progress-reporting loud op is the one thing that still earns a frame per frame. `EvalTask` signals too (same bug class).* | **done** |
| 6g-2 | Bar layout corrections | Favorites strip sits **between the library panel and the viewport** (bar docked left → order: panel, strip, handle): the strip always stays at the bar's viewport edge and collapse hides only the browser panel. The handle is **only the 16×56 grip**, vertically centered — no full-height reserved lane (the grip floats at the bar's edge; the rest of that column is viewport). Browser panel: ND-style nav row (back + breadcrumb with ND spacing), search row above it, and the card grid gets the **entire remaining pane** (full width auto-fill; no narrow single column). Grid wheel-scroll steps are far too large — match a browser-normal per-notch distance (ND relies on native `overflow-y: auto`, ~50-100 px/notch; ours jumps much further). *Outcome: (a) `BarLayout` now anchors the strip to the bar's **right** edge (`strip.x = width − 72`) with the panel outboard of it at x = 0, and `COLLAPSED_W` drops from 88 to 72 — the handle rect **is** the 16 × 56 grip, centred, drawn over the strip and hit-tested before it, so a press beside the grip reaches the strip item underneath and the freed 16 px go to the viewport. (b) The **embedded face drops the provider sidebar** (`BrowserMode::shows_sidebar`); the modal keeps it. ND's embedded parts browser has no provider list, and inside a 380 px panel the 150 px sidebar left 218 px of content = exactly one `minmax(120px,1fr)` column; without it the grid gets 368 px = two columns. (c) The scroll jump was a **unit mismatch, not a step size**: agg-gui's wheel delta is a *notch* count (its `ScrollView` multiplies by 40, and its own native shell passes `LineDelta` through and divides `PixelDelta` by 40), but AtomArtist's `demo-native` multiplied `LineDelta` by 60 and demo-wasm forwarded raw `deltaY` pixels. One notch therefore moved the grid 60 × 62 ≈ **3720 px** (and the favorites strip 60 × 40 = 2400 px). Both shells now normalise to notches, and the grid's step became a flat `GRID_SCROLL_STEP = 60 px` instead of `card_h × 0.5` (62 px). Nothing else read the delta's magnitude — the canvas and viewport zooms use only its sign. The normalisation itself went **upstream** into agg-gui as `agg_gui::wheel` (`PIXELS_PER_NOTCH = 40`, `NOTCHES_PER_PAGE = 8`, `WheelDeltaMode::{Pixel,Line,Page}` + a per-axis `WheelNormalizer` that banks sub-notch trackpad travel and emits only whole notches, resetting on a direction flip); agg-gui's own `demo-wgpu::web_shell` uses it, and our `demo-wasm/index.html` glue — which calls the wasm export directly rather than through that shell — carries a commented port of the same function.* | **done** |
| 6g-3 | Drag ghost + icon colors | The drag ghost is the item's rendered icon (ND: 48 px, the cached icon image; label-text fallback), attached to the cursor for the whole gesture including over the bed. Icon colors match ND's softer look: compare ND/MatterCAD default node colors and add the missing linear→sRGB handling in mesh_raster shading (ours reads over-saturated). | |

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
