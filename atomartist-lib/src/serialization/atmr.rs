//! AtomArtist project file format (`.atmr`).
//!
//! An `.atmr` file is a regular ZIP archive that contains the project
//! graph as `graph.json` at the archive root. The archive is the only
//! place the schema gets to evolve over time without breaking the file
//! format itself — additional entries (baked geometry caches, embedded
//! images, future scene resources) can be appended later without forcing
//! existing readers to understand them.
//!
//! Why a ZIP rather than a single JSON file?
//!
//! * Bundled resources. Eventually we'll embed referenced bitmaps /
//!   meshes / fonts alongside the graph so a project survives being
//!   moved between machines without dangling absolute paths.
//! * Future per-entry compression. JSON compresses 5–10× with deflate,
//!   so a project that grows past a few MiB of node data still loads
//!   instantly off slow storage.
//! * Round-trippable in any zip tool. `unzip foo.atmr` gives you
//!   `graph.json` you can read or hand-edit; `zip foo.atmr graph.json`
//!   makes it again.
//!
//! The plain-JSON path (`.json`) is still supported for backwards
//! compatibility with files saved by earlier builds — see
//! [`save_project_to_path`] / [`load_project_from_path`] for the
//! extension-aware dispatch.
//!
//! ## Layout
//!
//! ```text
//! foo.atmr (zip)
//! └─ graph.json   ← serialized GraphFile (see graph_json.rs)
//! ```
//!
//! Future additions (manifest, baked meshes, embedded textures) will
//! get their own top-level entries; readers must therefore tolerate
//! unknown entries.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::graph::graph::Graph;
use crate::registry::NodeRegistry;
use crate::serialization::graph_json::{
    graph_from_json_str, graph_to_json_string, LoadResult,
};

/// Conventional file extension for an AtomArtist project file. Lowercase
/// — callers that need to match user-typed extensions should compare
/// case-insensitively.
pub const PROJECT_EXTENSION: &str = "atmr";

/// Name of the graph JSON entry inside an `.atmr` archive. Pinned so
/// future format revisions can detect "old vs new" archives by entry
/// presence rather than a separate version field.
pub const GRAPH_ENTRY_NAME: &str = "graph.json";

/// User-readable error type for ATMR I/O. Wraps both filesystem and
/// zip-library errors so callers can show a single message without
/// matching on the inner kind.
#[derive(Debug)]
pub enum AtmrError {
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    /// The archive opened cleanly but didn't contain `graph.json`.
    /// Typically means the user picked a stray zip rather than an
    /// AtomArtist project.
    MissingGraphJson,
    /// `graph.json` was present but `serde_json` rejected its contents.
    BadJson(serde_json::Error),
}

impl std::fmt::Display for AtmrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtmrError::Io(e) => write!(f, "{}", e),
            AtmrError::Zip(e) => write!(f, "zip error: {}", e),
            AtmrError::MissingGraphJson => write!(
                f,
                "archive does not contain `{}` — not an AtomArtist project file",
                GRAPH_ENTRY_NAME
            ),
            AtmrError::BadJson(e) => write!(f, "graph JSON parse failed: {}", e),
        }
    }
}

impl std::error::Error for AtmrError {}

impl From<std::io::Error> for AtmrError {
    fn from(e: std::io::Error) -> Self { AtmrError::Io(e) }
}
impl From<zip::result::ZipError> for AtmrError {
    fn from(e: zip::result::ZipError) -> Self { AtmrError::Zip(e) }
}
impl From<serde_json::Error> for AtmrError {
    fn from(e: serde_json::Error) -> Self { AtmrError::BadJson(e) }
}

/// Save `graph` as an `.atmr` archive at `path`.
///
/// The archive is overwritten if it already exists. Compression is
/// `Deflated` — JSON shrinks roughly 5–10× for typical project sizes,
/// which keeps the file small enough to email or version-control even
/// after the format grows companion entries.
pub fn save_atmr_to_path(path: &Path, graph: &Graph) -> Result<(), AtmrError> {
    let json = graph_to_json_string(graph);
    let file = File::create(path)?;
    write_atmr_into(file, &json)?;
    Ok(())
}

/// Encode an ATMR archive containing only the graph JSON into the
/// supplied writer. Split out so tests / future callers can stream
/// into a buffer or in-memory cursor without touching the filesystem.
pub fn write_atmr_into<W: Write + Seek>(writer: W, graph_json: &str) -> Result<W, AtmrError> {
    let mut zw = ZipWriter::new(writer);
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        // Mid-range compression level — quality 6 is the deflate
        // default and matches the storage / CPU trade-off used by
        // most desktop tools (7-Zip "Normal", `gzip` default, etc.).
        .compression_level(Some(6));
    zw.start_file(GRAPH_ENTRY_NAME, opts)?;
    zw.write_all(graph_json.as_bytes())?;
    let writer = zw.finish()?;
    Ok(writer)
}

/// Load a graph from an `.atmr` archive at `path`. Returns the parsed
/// `LoadResult` (graph + non-fatal warnings) on success.
pub fn load_atmr_from_path(
    path: &Path,
    registry: &NodeRegistry,
) -> Result<LoadResult, AtmrError> {
    let file = File::open(path)?;
    let json = read_graph_json_from_atmr(file)?;
    Ok(graph_from_json_str(&json, registry)?)
}

/// Extract the `graph.json` entry from an open zip reader and return
/// its contents as a `String`. Surfaces `MissingGraphJson` if the
/// archive opened but didn't contain the expected entry.
pub fn read_graph_json_from_atmr<R: Read + Seek>(reader: R) -> Result<String, AtmrError> {
    let mut archive = ZipArchive::new(reader)?;
    let mut entry = match archive.by_name(GRAPH_ENTRY_NAME) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Err(AtmrError::MissingGraphJson),
        Err(e) => return Err(AtmrError::Zip(e)),
    };
    let mut json = String::with_capacity(entry.size() as usize);
    entry.read_to_string(&mut json)?;
    Ok(json)
}

/// Convenience: save `graph` to `path` choosing the file format from
/// the path's extension. `.atmr` (or no extension at all) writes a
/// zip archive; `.json` writes the legacy plain-JSON file. Unknown
/// extensions are treated as `.atmr` so the user always gets the
/// modern format by default.
pub fn save_project_to_path(path: &Path, graph: &Graph) -> Result<(), AtmrError> {
    if has_extension(path, "json") {
        let json = graph_to_json_string(graph);
        std::fs::write(path, json)?;
        Ok(())
    } else {
        save_atmr_to_path(path, graph)
    }
}

/// Convenience: load a graph from `path`, picking the parser based on
/// the file extension. Falls back to "try ATMR first, then JSON" when
/// the extension is missing or unrecognised so users who renamed a
/// file by hand still get a useful result.
pub fn load_project_from_path(
    path: &Path,
    registry: &NodeRegistry,
) -> Result<LoadResult, AtmrError> {
    if has_extension(path, "json") {
        let s = std::fs::read_to_string(path)?;
        Ok(graph_from_json_str(&s, registry)?)
    } else {
        load_atmr_from_path(path, registry)
    }
}

fn has_extension(path: &Path, want: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(want))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use crate::graph::graph::Graph;

    fn empty_registry() -> NodeRegistry { NodeRegistry::new() }

    #[test]
    fn empty_graph_round_trips_through_atmr() {
        let original = Graph::new();
        let mut buf: Vec<u8> = Vec::new();
        let cursor = Cursor::new(&mut buf);
        let json = graph_to_json_string(&original);
        let _ = write_atmr_into(cursor, &json).expect("write atmr");

        // Re-read the archive from the in-memory buffer and confirm
        // the embedded graph.json round-trips.
        let read_cursor = Cursor::new(buf.as_slice());
        let recovered = read_graph_json_from_atmr(read_cursor).expect("read graph.json");
        assert_eq!(recovered, json);
    }

    #[test]
    fn missing_graph_json_returns_missing_error() {
        // Build an archive with an entry that isn't `graph.json` and
        // confirm the loader surfaces a meaningful error instead of
        // a generic ZipError.
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = ZipWriter::new(cursor);
            let opts = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored);
            zw.start_file("readme.txt", opts).unwrap();
            zw.write_all(b"not a project").unwrap();
            zw.finish().unwrap();
        }
        let read_cursor = Cursor::new(buf.as_slice());
        let err = read_graph_json_from_atmr(read_cursor).expect_err("expected missing-graph error");
        assert!(matches!(err, AtmrError::MissingGraphJson));
    }

    #[test]
    fn save_project_to_path_dispatches_on_extension() {
        let dir = std::env::temp_dir();
        let json_path = dir.join("__atmr_test.json");
        let atmr_path = dir.join("__atmr_test.atmr");
        let no_ext_path = dir.join("__atmr_test_noext");

        let g = Graph::new();
        save_project_to_path(&json_path, &g).expect("save json");
        save_project_to_path(&atmr_path, &g).expect("save atmr");
        save_project_to_path(&no_ext_path, &g).expect("save no ext");

        // .json: plain text starting with `{`
        let raw = std::fs::read(&json_path).unwrap();
        assert_eq!(raw.first(), Some(&b'{'));

        // .atmr: zip "PK\x03\x04" local file header.
        let raw = std::fs::read(&atmr_path).unwrap();
        assert_eq!(&raw[..4], b"PK\x03\x04");

        // No extension defaults to atmr (zip).
        let raw = std::fs::read(&no_ext_path).unwrap();
        assert_eq!(&raw[..4], b"PK\x03\x04");

        // Round trip: load each one back through the dispatcher.
        let reg = empty_registry();
        let _ = load_project_from_path(&json_path, &reg).expect("load json");
        let _ = load_project_from_path(&atmr_path, &reg).expect("load atmr");
        let _ = load_project_from_path(&no_ext_path, &reg).expect("load no ext");

        let _ = std::fs::remove_file(json_path);
        let _ = std::fs::remove_file(atmr_path);
        let _ = std::fs::remove_file(no_ext_path);
    }
}
