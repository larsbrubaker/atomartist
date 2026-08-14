//! Guardrail: `std::fs` belongs to storage providers, not to the app.
//!
//! `docs/storage-architecture-plan.md` §3.1 makes `StorageUri` the identity
//! of a project and `StorageProvider` the only thing that knows how bytes
//! are persisted. That boundary is easy to state and easy to erode — one
//! `std::fs::read` "just for this one import" and the WASM build silently
//! loses a feature, or a cloud project becomes unopenable. This test scans
//! the application source trees and fails on any `std::fs` reference
//! outside the short, documented allowlist below.
//!
//! Same root-resolution and scan machinery as `file_line_count.rs`; kept
//! separate because it checks a different property of a different subset
//! of the tree.

use std::fs;
use std::path::{Path, PathBuf};

/// Source trees that must go through a storage provider. `demo-native`
/// is deliberately absent: it is the platform shell, it owns the settings
/// file and screenshots, and it is where `LocalFsProvider` is registered.
const SCANNED_DIRS: &[&str] = &["atomartist-ui/src", "atomartist-lib/src"];

/// Files permitted to touch `std::fs`, each for a stated reason. Adding an
/// entry here is an architectural decision — prefer moving the IO behind a
/// `StorageProvider` instead.
const ALLOWED: &[(&str, &str)] = &[
    (
        // Application configuration (window placement, theme, HUD
        // toggles) owned by the platform shell. It is not project
        // storage: it is read before any provider is registered and must
        // work even when no provider exists at all.
        "atomartist-ui/src/settings.rs",
        "app-config IO, shell-owned — not project storage",
    ),
    (
        // Native-only node whose `path` property names a mesh on the
        // local disk directly (a string path, not a project reference).
        // The read is `cfg`-gated out of the wasm build.
        "atomartist-lib/src/nodes/mesh/library_mesh_node.rs",
        "native-only string-path mesh property, cfg-gated",
    ),
];

/// Forms that reach the filesystem module. `std::fs` catches
/// `std::fs::read(...)` and `use std::fs;`; the brace form catches
/// `use std::{fs, io};`, which the first pattern misses entirely.
const NEEDLES: &[&str] = &["std::fs", "std::{fs"];

/// True when a source line mentions the filesystem module in code.
///
/// Known limitation: a *string literal* containing `std::fs` (in a doc
/// example, an error message, or this file's own constants) would be
/// reported. That has not come up, and the alternative is a real Rust
/// lexer for a guard test; if it ever does, the fix is to strip string
/// literals here rather than to allowlist the file.
fn mentions_fs(line: &str) -> bool {
    let normalized = line.replace(' ', "");
    NEEDLES.iter().any(|needle| normalized.contains(needle))
}

#[test]
fn app_code_reaches_storage_only_through_providers() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("atomartist-lib crate should live under the workspace root");

    let mut offenders = Vec::new();
    for dir in SCANNED_DIRS {
        let path = workspace_root.join(dir);
        assert!(
            path.is_dir(),
            "scanned directory {} does not exist — has the tree moved?",
            path.display()
        );
        visit_files(workspace_root, &path, &mut offenders);
    }

    if !offenders.is_empty() {
        offenders.sort();
        panic!(
            "`std::fs` may only be used inside a StorageProvider (or an allowlisted \
             file in `no_fs_outside_provider.rs`); offenders:\n{}",
            offenders.join("\n")
        );
    }
}

/// Code lines of `text`, with `//` lines and `/* … */` blocks removed.
///
/// Comments are the whole reason this filter exists: the doc header of
/// `app_state_files.rs` says "this module never touches `std::fs`", which
/// is the *opposite* of a violation, and an allowlist entry must not stay
/// alive just because a doc comment mentions the needle.
fn code_lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut in_block_comment = false;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if in_block_comment {
            // Good enough for real source: a block comment ends at the
            // first `*/` on a line, and anything after it on that same
            // line is rare enough to ignore.
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("/*") {
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        // Trailing `// …` on a code line: keep only the code half.
        let code = match line.find("//") {
            Some(cut) => &line[..cut],
            None => line,
        };
        out.push((i + 1, code));
    }
    out
}

fn visit_files(root: &Path, dir: &Path, offenders: &mut Vec<String>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read directory {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|err| panic!("failed to read entry in {}: {err}", dir.display()));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|err| panic!("failed to stat {}: {err}", path.display()));
        if file_type.is_dir() {
            visit_files(root, &path, offenders);
        } else if file_type.is_file() && is_rust_source(&path) {
            check_file(root, &path, offenders);
        }
    }
}

fn check_file(root: &Path, path: &Path, offenders: &mut Vec<String>) {
    let rel = relative_slash_path(root, path);
    if ALLOWED.iter().any(|(allowed, _why)| *allowed == rel) {
        return;
    }
    let text = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {} as UTF-8: {err}", path.display()));
    for (line_no, code) in code_lines(&text) {
        if mentions_fs(code) {
            offenders.push(format!("  {rel}:{line_no}  {}", code.trim()));
        }
    }
}

fn is_rust_source(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("rs")
}

fn relative_slash_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The allowlist is a design statement, so it must not rot: every entry
/// has to name a file that still exists and still needs the exemption.
#[test]
fn every_allowlist_entry_is_still_needed() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    for (rel, why) in ALLOWED {
        let path: PathBuf = workspace_root.join(rel);
        assert!(path.is_file(), "allowlisted file {rel} no longer exists");
        let text = fs::read_to_string(&path).expect("allowlisted file is UTF-8");
        // Same code-only filter as the scan: a doc comment mentioning
        // `std::fs` must not keep a stale exemption alive.
        assert!(
            code_lines(&text)
                .into_iter()
                .any(|(_line_no, code)| mentions_fs(code)),
            "allowlisted file {rel} no longer uses `std::fs` in code ({why}) — drop the entry"
        );
    }
}
