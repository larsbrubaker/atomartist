# AtomArtist

A visual node-based 3D design tool. Wire together typed nodes — primitives, transforms,
boolean operations, extrusions, imported meshes — and watch the resulting 3D geometry
update live in the viewport.

Targets **Windows**, **macOS**, and the **web** (WASM via WebGPU / WebGL2).

> **Status:** Phase 0 — empty workspace skeleton. Active development.

---

## Architecture

Pure Rust, no JavaScript or TypeScript. Built on:

- **[agg-gui](https://github.com/larsbrubaker/agg-gui)** — immediate-mode GUI framework with wgpu rendering
- **[manifold-rust](https://github.com/larsbrubaker/manifold-rust)** — `MeshGL` and `CrossSection` types, boolean operations
- **[clipper2-rust](https://github.com/larsbrubaker/clipper2-rust)** — 2D path boolean / offset
- **[tess2-rust](https://github.com/larsbrubaker/tess2-rust)** — extrude cap tessellation
- **[wgpu](https://wgpu.rs)** — cross-platform GPU rendering (Vulkan / Metal / DX12 / WebGPU / WebGL2)

### Workspace layout

```
atomartist/
├── atomartist-lib/         # Graph engine, node types, geometry, serialization
├── atomartist-renderer/    # 3D viewport (wgpu) — orbit camera, shaders, gizmos
├── atomartist-ui/          # Shared widget tree — node canvas, property panel, toolbar
├── demo-native/            # winit + wgpu native binary (Windows / macOS / Linux)
└── demo-wasm/              # wasm-bindgen entry for the browser
```

---

## Build

### Native

```bash
cargo run -p demo-native --release
```

### Web (WASM)

```bash
# One-time install:
cargo install wasm-pack basic-http-server

# Build + serve:
wasm-pack build demo-wasm --target web
basic-http-server .
# Open http://localhost:4000/
```

### Tests

```bash
cargo test --workspace
```

---

## License

MIT — see [LICENSE](./LICENSE).
