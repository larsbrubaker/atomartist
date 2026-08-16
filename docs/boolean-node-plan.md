# Boolean Node — MatterCAD Parity on manifold-rust

Status: **proposal**
Author: planning session, 2026-08-15 (from the same-day exploration of
`C:\Development\MatterCAD` and `C:\Development\rust-apps\manifold-rust`;
all file:line citations below come from that pass)
Scope: `atomartist-lib` (boolean_node.rs rewrite), `agg-gui`
(icon-row enum editor), possibly `manifold-rust` (additive helpers only —
the needed API already exists).

## 1. Goal

Replace today's minimal Boolean node with MatterCAD's proven model:
four operations, robust non-manifold handling, a degradation policy
that never silently loses geometry, and the ancestor's operation icons.
User direction: "use manifold-rust to do both manifold and non-manifold
ops; copy the behavior from MatterCAD including the icons."

## 2. What's broken today (why the screenshot is a dark blob)

`atomartist-lib/src/nodes/ops_3d/boolean_node.rs` (222 lines):
`operation` is a bare Number spinner 0..=2; only `a.first()` /
`b.first()` are used (remaining bodies silently dropped); a hand-rolled
1e-5 hash-bucket vertex weld (`strip_normals`); **strict**
`Manifold::from_mesh_gl` import; and `result.status()` is never checked
— a non-manifold input yields empty/garbage with no error surfaced.
No winding rule, no repair, no cancel/progress, no colours, no
multi-body, no hole semantics, one test.

## 3. MatterCAD's model (what we port)

### 3.1 One node, four operations

`MatterCADLib/DesignTools/BooleanObject3D.cs` (enum serialized by name,
L61-75): **Combine** (n-ary union), **Subtract** (cut selected parts
out of the others), **Intersect** (keep the shared volume),
**Subtract & Replace** (subtract and keep the removed volume as its own
part). Operation row renders as an icon row (16×16, L129-151); setting
the operation renames the node unless the user renamed it. `Update`
button = `[ShowUpdateButton]` (deferred rebuild).

### 3.2 Toggles (exact semantics, L163-173 + BooleanMeshBuilder.cs)

| Property | Semantics |
|---|---|
| Keep Inside Out Geometry | `WindingRule::Nonzero` (inverted shells count as material); forces the robust engine (BooleanMeshBuilder.cs:71-73) |
| Repair Winding Order | `repair_orientation()` on each imported handle *before* the boolean — fix the data instead of redefining "solid"; "usually the better answer of the two" |
| Keep Subtracted Parts | bookkeeping: after Subtract, re-add remover meshes as children (BooleanMeshBuilder.cs:269-285) |

Row visibility mirrors `UpdateControls` (L397-408): part-selection only
for Subtract/Subtract&Replace; Keep Subtracted only for plain Subtract.
Output **Hole/Solid** is base-object `OutputType`, not the boolean —
Combine splits holes from solids, unions each, subtracts hole-union
from solid-union (BooleanMeshBuilder.cs:104-192).

### 3.3 Part(s) to Subtract

`SelectedChildren` = set of child IDs over the operand list; default =
last child on wrap (L418-429); stale IDs pruned per rebuild (L435-450);
a lone operand is always a keep (BooleanMeshBuilder.cs:541-564); parts
to be cut ghost at alpha 0.2 in the editor.

### 3.4 Engine policy (the most valuable part —
`agg-sharp/PolygonMesh/Csg/ManifoldKernel.cs` +
`Object3DBooleanOperations.cs`)

- **Import**: always `from_mesh_gl64_robust` — manifold input behaves
  as plain import (Auto still picks the exact pipeline); closed
  non-manifold becomes a soup handle; only not-closed fails, and gets
  ONE retry through a seam weld (`tol = max(bbox diagonal,
  dist from origin) · 1e-5`, then merge + degenerate-face removal).
- **Refused operands are skippable only for Union** ("Subtract and
  Intersect are defined by every operand"). A partial union throws an
  exception that *carries the partial result* + skipped list.
- **AABB touching-sets** (Object3DBooleanOperations.cs:108-156): union
  each connected set separately, concatenate the sets — big win for
  scattered parts.
- **Repair-and-retry triage** (L513-673): repair refused operands
  (hole-fill), then classify — `Clean` → union partial + repaired
  (associative, no re-run); watertight-but-self-intersecting → **kept
  beside** the union, not unioned (a 41-operand hole-filled union ran
  half an hour without finishing); repair failed → keep the user's
  ORIGINAL geometry, report skipped.
- Progress caveat: a non-null reporter forces the pairwise left fold
  off `batch_boolean` — return None, not a forwarding lambda, when
  nobody is listening (`ScaledReporter`, L93-101).
- Error reporting: no fallback engine; "N of M parts are not watertight
  solids", ≤5 operands named.
- Multi-remover Subtract: union removers first, then subtract per keep,
  `CleanAndMerge` after each.
- **Colour preservation**: import-as-original + `RunIndex`/
  `RunOriginalId` run data maps each output triangle to its source
  operand's colour; per-face-coloured sources split by colour into
  sub-manifolds, unioned, matched back by centroid
  (ManifoldKernel.cs:747-927).

### 3.5 Icons

Four hand-authored SVGs in `MatterCAD/StaticData/Icons/`:
`combine.svg`, `subtract.svg`, `intersect.svg`,
`subtract_and_replace.svg` — 64-unit viewBox at 16×16; design doc lives
in combine.svg's comment (L2-33). Colour roles: ink `#4C4C4C`
(theme-recoloured), removed material `#9A9A9D` (saturation just above
the recolour cutoff — passes through both themes), kept material
`#4BA9E8` blue, retained remover red `#F20D0D`. Block 0..52 with 4-unit
outline; operand ring r=22/16 at (42,42); 6-unit band; arcs pre-chopped
≤30°.

## 4. manifold-rust already has everything (v0.13.1, path dep)

`from_mesh_gl64_robust` (soup import), `MeshGL::merge` (BVH weld —
replaces our hand-rolled one), `boolean_with_engine_rule_and_progress`
(+ batch variants), `BooleanEngine::{Exact,Robust,Auto}` +
`BooleanConfig::set_default_engine(Auto)` once at startup,
`WindingRule::{Positive,Nonzero}`, `status()`,
`has_self_intersections()` (cached), `repair_orientation()`,
`decompose/compose`, run-data on MeshGL for colours. Additions upstream
only if a gap emerges during the port.

## 5. Steps

| # | Step | Contents | Status |
|---|---|---|---|
| B-1 | Correctness floor | robust import + `status()` check surfaced as a node error; `MeshGL::merge` + seam-weld retry on NotClosed replaces `strip_normals`'s hand weld; `set_default_engine(Auto)` at startup; reproducing test for today's silent-garbage case first (it's a bug) | **done** — the dark blob was BOTH halves: a float-rounded seam imported as *empty* under the strict path (operand silently vanished from the union), and the welded result shaded wrong because `compute_flat_normals` on a shared-vertex mesh lets the last face win (41/44 triangles disagreed). Fixed via robust import + named errors + `split_for_flat_normals` (general, in mesh3d.rs). Hostile input (OOB indices, degenerate operands, num_prop < 3) is refused with named errors instead of panicking or vanishing. Engine default Auto behind a Once on the single import funnel. |
| B-1b | Node-error surfacing | Found during B-1: `atomartist-ui/src/app_state.rs` does `let _ = evaluate_dirty(...)` and evaluation bails on the FIRST `ExecuteError::Node` — a refused operand halts every downstream node with no message anywhere; the user sees a stale viewport instead of "input 'b' is not a closed solid". Surface node errors (status-bar notice at minimum; a per-node error badge on the canvas is the ND-style answer) and decide whether evaluation should continue past a failed node (evaluate its non-dependents). B-1's messages are only as good as this plumbing. | **done** — evaluation now walks past failures: `run_pass` collects `NodeFailure`s into an `EvalReport`, blocks the failed node's downstream cone (dependents keep last-good outputs), and a persistent runtime-only `NodeInstance::failed` flag keeps blocking across passes so a downstream edit can't compute against stale failed outputs. Errors surface as status-bar notices (posted only when new/changed per node — no per-tick flood) and as a per-node badge painted on the live widget tree upstream (outline + title-bar `!`; fingerprint hashes the error so async results appear immediately). Drilled-in views skip root-keyed badges (id spaces collide; drilled badging is future work with the subgraph-collapse deferral). Test call sites whose unwrap was the assertion now `expect_clean()`. |
| B-2 | Real operation enum + icons | enum param (Combine/Subtract/Intersect/Subtract & Replace) serialized by name, Number 0/1/2 migration; the four SVGs recreated; agg-gui gains an icon-capable enum editor (`EnumIcons`) — additive, `EnumButtons` as interim if the editor is large | |
| B-3 | N-ary operands + selection | combine_node's trailing-empty input model; bake `Body.matrix` per operand; Part(s) to Subtract set (default last, prune stale, lone operand = keep); property visibility per operation | |
| B-4 | Toggles + hole semantics | Keep Inside Out (Nonzero rule), Repair Winding Order (`repair_orientation`), Keep Subtracted Parts; OutputType Hole/Solid participation in Combine | |
| B-5 | Degradation policy | touching-sets, partial-union + skipped list, repair-and-retry triage via `has_self_intersections()`, rescue-with-original, "N of M not watertight" messaging | |
| B-6 | Colours + Subtract & Replace | run-data colour mapping into `Body.vertex_colors`/per-body colour; Subtract & Replace = Intersect+Subtract per keep×remover; cancel/progress plumbing (reporter only when someone listens) | |

Each step: implementer → reviewer → fixes → commit; test expectations
mined from `Tests/MatterCADTests/Standard/BooleanObject3DTests.cs`
(21 tests) and the four help docs under
`MatterCAD_Docs/docs/Help/operations/boolean/`.
