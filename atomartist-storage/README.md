# atomartist-storage

Pluggable project storage: `StorageUri`, `Job`, the `StorageProvider` trait,
the scheme registry, and the providers that ship in the open-source build.
Design: `docs/storage-architecture-plan.md`.

| Provider | Scheme | Platform | Notes |
|---|---|---|---|
| `LocalFsProvider` | `file:` | native | the only place allowed to touch `std::fs` for project storage |
| `BrowserProvider` | `browser:` | wasm | Origin Private File System (OPFS), main thread, promise API |
| `MemoryProvider` | any | both | in-process reference backend used by tests |
| `FlakyProvider` | wraps another | both | fault injection |

## Running the tests

Native (part of `cargo test --workspace`):

```powershell
cargo test -p atomartist-storage
```

`BrowserProvider` needs a real browser, so its conformance run is a separate
wasm test target that native CI cannot execute. Everything about it that
*can* be checked without a browser — URI → OPFS path segments, stamp
derivation, entry assembly, and the `DOMException` → `StorageError` table —
lives in `src/browser/paths.rs` and is covered by the native run above.

To execute the browser half (`tests/browser_opfs.rs`):

```powershell
# One-time: the test runner (a Rust binary; no JS tooling is involved).
# The version MUST match the `wasm-bindgen` version in Cargo.lock, or the
# runner refuses the module with a schema-mismatch error.
cargo install wasm-bindgen-cli --version 0.2.120

# Chromedriver must match the *installed* Chrome (check
# chrome://version), from https://googlechromelabs.github.io/chrome-for-testing/
$env:CHROMEDRIVER = "C:\tools\chromedriver\chromedriver.exe"
$env:CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER = "wasm-bindgen-test-runner"
cargo test -p atomartist-storage --target wasm32-unknown-unknown --test browser_opfs
```

`wasm-pack test --headless --chrome atomartist-storage` runs the same tests
and installs a matching `wasm-bindgen` for you, but it downloads the
*latest* chromedriver — which fails with `Error: http status: 404` when the
locally installed Chrome is a release or two behind. Setting `CHROMEDRIVER`
as above and calling `cargo test` directly is the reliable route.

Last verified: Chrome 151.0.7922.110 on Windows, 6 tests passing (the full
conformance suite plus five `BrowserProvider`-specific checks, including a
one-megabyte round trip that covers the write path's copy to the JS heap).

If chromedriver reports `Error: http status: 404`, it is refusing a Chrome it
does not match. Check the *running* browser's version (`chrome://version`, or
the `ProductVersion` of `chrome.exe`) rather than the newest folder under
`…\Google\Chrome\Application\`: a staged auto-update leaves the next version's
directory and a `new_chrome.exe` on disk long before it is the browser that
actually launches.

The suite writes only under `browser:///atomartist-conformance/…` and removes
what it creates, so running it against a browser profile that also holds real
projects is safe.

## Why the conformance suite is `async`

The checks in `src/conformance.rs` are `async fn`s driven two ways:
`run_conformance` (native) blocks on them with a tiny poll loop, while
`run_conformance_async` is awaited on the browser event loop. A synchronous
suite cannot test `BrowserProvider` at all: its jobs only settle when the
event loop delivers a promise, and spinning on the browser's single thread
would deadlock rather than wait. One suite, no duplicated assertions.
