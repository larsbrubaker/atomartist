# Claude Code Guidelines — AtomArtist

## Philosophy

**Quality through iterations** — start with correct implementations, then improve.
This is a green-field port of NodeDesigner's visual node-graph editor; every node
type, widget, and geometry routine matters and should land working before we move
on.

**Be a collaborator, not a stenographer.** Don't treat the developer's
instructions as gospel. Apply judgment, push back when something looks wrong,
and propose the approach you believe is the best practice or the most
appropriate solution for the problem — even if it differs from what was asked.
Disagree respectfully, explain the trade-offs, then defer once a decision is made.

## Pure Rust mandate

The entire application is Rust — no JavaScript, no TypeScript, no Bun, no Node, no
web frameworks. The WASM build is pure Rust compiled to WebAssembly via
`wasm-bindgen`. For local WASM development, use a pure-Rust HTTP server like
`basic-http-server`. Do not introduce JS/TS tooling.

## Path-dependency convention

We depend on sibling submodules (`agg-gui`, `manifold-rust`, `clipper2-rust`,
`tess2-rust`) via `path = "..."` — see the `[patch.crates-io]` block in the
workspace `Cargo.toml`. This lets us extend or fix these libraries while building
AtomArtist. Eventually we flip to crates.io versions; structure changes
accordingly so that switch is a one-line edit per dep.

## Test-First Bug Fixing (Critical Practice)

When a bug is reported, always follow this workflow:

1. **Write a reproducing test first** — create a failing test
2. **Fix the bug** — minimal change to address the root cause
3. **Verify via passing test** — the test must now pass

Do not skip the reproducing test. Even if the fix seems obvious.
Never commit a bug fix that isn't covered by a test.

## Testing

- Tests MUST test actual production code, not copies
- All tests must pass before advancing to the next phase
- When test failures occur, treat all failures as real bugs — resolve through
  instrumentation and root-cause analysis, never by weakening tests

**Running tests:**
```bash
cargo test --workspace
cargo test -p atomartist-lib
cargo test test_name -- --exact
cargo test -- --nocapture
```

## Coding Standards

### File length
- **Hard limit: 800 lines.** Files at the limit must be refactored by splitting
  into focused modules before adding more code.
- Never reduce a file's line count by removing comments or blank lines to meet the
  limit — that is not refactoring. Split real logic into separate files/modules.

### Documentation
- Every file must begin with a comment block describing its purpose and how it
  relates to other modules.
- Add doc comments to functions when they clarify intent, non-obvious behavior, or
  cross-module relationships. Skip them when the function name and signature
  already tell the full story.
- Explain *why*, not *what*. A comment that restates the code is noise.

### Coordinate system
- **Y-axis is inverted (bottom-up)** — origin at bottom-left, +Y points upward.
  Inherited from agg-gui. Code from external sources almost always assumes top-down
  Y; watch for this in default positioning, collapse direction, SVG orientation,
  scroll offsets, and hit-testing.
- 3D viewport uses standard right-handed Y-up world coordinates (matches wgpu /
  glTF conventions). The 2D node canvas inherits agg-gui's bottom-left origin.

### Icons
- Use **Font Awesome** icons (Unicode code points in the Font Awesome font face),
  consistent with agg-gui.

### Node types
- Every node type implements the `NodeDef` trait (registry.rs).
- Each node category lives in its own subdirectory under `atomartist-lib/src/nodes/`.
- Node files name themselves after the type: `box_node.rs`, `extrude_node.rs`, etc.

### General style
- Prefer `Result` / `Option` over `unwrap` in library code; `expect` is acceptable
  in `main` for startup failures with a clear message.
- Avoid unsafe code unless there is no alternative; document every `unsafe` block.

### Names
Follow Rust conventions (`snake_case` functions/variables, `PascalCase` types,
`SCREAMING_SNAKE_CASE` constants).

### Performance
- **Never guess at performance problems by reading code.** Always measure first.
- Before optimizing, instrument the real workload and identify the actual
  bottleneck through profiling data or timing measurements.
- Validate that each change produces a measurable improvement.
- **Targets:** average frame render < 10 ms; 50-node graph re-evaluation < 10 ms.

### Refactoring
Improve code when it serves a purpose, not for aesthetics.

## Shell

This project uses **PowerShell** on Windows for build scripts. Claude Code's shell
tool runs bash on Windows; adapt commands accordingly (forward slashes in paths,
`/dev/null` instead of `NUL`).

## Out of scope

AtomArtist is the open-source descendant of NodeDesigner with all
MatterHackers / FDS-specific functionality stripped:

- No FDS cloud integration, no DFS service client
- No MatterHackers auth / token system
- No Google Cloud Storage or other proprietary cloud storage
- No admin dashboard, no usage licensing, no Gemini AI descriptions

Local file storage and pure-Rust crates only.
