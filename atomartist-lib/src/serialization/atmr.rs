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
//! ## Bytes in, bytes out
//!
//! This module deals only in byte buffers: [`write_project_to_bytes`]
//! encodes a project into a `Vec<u8>` and [`read_project_from_bytes`]
//! decodes one from a `&[u8]`. Deciding *where* those bytes live
//! (local filesystem, browser storage, a remote service) is the
//! storage layer's job, not the format layer's — which is what lets
//! the same encoder serve the native shell and the WASM build.
//!
//! The zip archive is the *only* project format — there is no bare
//! `.json` project any more, on either the read or the write side.
//! Reading therefore ignores file extensions entirely (there may not
//! be a filename at all behind a byte stream) and simply parses the
//! buffer as an archive.
//!
//! ## Layout
//!
//! ```text
//! foo.atmr (zip)
//! ├─ graph.json              ← serialized GraphFile (see graph_json.rs)
//! └─ Metadata/thumbnail.png  ← optional preview (see thumbnail.rs)
//! ```
//!
//! The thumbnail follows the OPC / 3MF convention so other tools that
//! sniff that path get our previews for free. It is written only when a
//! caller supplies one ([`write_project_to_bytes_with_thumbnail`]) and
//! is ignored on read — [`super::thumbnail::read_thumbnail_from_bytes`]
//! is the cheap way to pull it back out without decoding the graph.
//!
//! Future additions (manifest, baked meshes, embedded textures) will
//! get their own top-level entries; readers must therefore tolerate
//! unknown entries.

use std::io::{Cursor, Read, Seek, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::graph::graph::Graph;
use crate::registry::NodeRegistry;
use crate::serialization::asset_store::AssetStore;
use crate::serialization::graph_json::{
    graph_from_json_str, graph_to_json_string_with_view, LoadResult,
};
use crate::serialization::thumbnail::THUMBNAIL_ENTRY_NAME;
use crate::serialization::view_state::ProjectView;

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

/// Encode `graph` + `assets` as a complete `.atmr` archive in memory.
///
/// This is the only project-writing entry point: there is no
/// plain-JSON output format any more. `graph.json` is written first so
/// streaming readers can pull the topology without scanning the whole
/// buffer; assets follow in deterministic hash order. A project with an
/// empty `AssetStore` is byte-compatible with the pre-asset format.
pub fn write_project_to_bytes(
    graph: &Graph,
    assets: &AssetStore,
) -> Result<Vec<u8>, AtmrError> {
    write_project_to_bytes_with_thumbnail(graph, assets, None)
}

/// [`write_project_to_bytes`] plus an optional preview image, stored at
/// [`THUMBNAIL_ENTRY_NAME`].
///
/// A sibling rather than a signature change so the many callers that
/// have no viewport to capture (tests, importers, headless shells) stay
/// as they are: the entry is optional forever, and `None` produces
/// bytes identical to the no-thumbnail encoder.
pub fn write_project_to_bytes_with_thumbnail(
    graph: &Graph,
    assets: &AssetStore,
    thumbnail_png: Option<&[u8]>,
) -> Result<Vec<u8>, AtmrError> {
    write_project_to_bytes_with_view(graph, assets, thumbnail_png, None)
}

/// [`write_project_to_bytes_with_thumbnail`] plus the per-project view
/// state (canvas pan/zoom, splitter, camera — see
/// [`crate::serialization::view_state`]).
///
/// This is the full project encoder; the two shorter spellings above
/// exist for the many callers with no view to save (importers, tests,
/// headless tools). Passing `None` — or an empty [`ProjectView`] —
/// produces bytes identical to those encoders.
pub fn write_project_to_bytes_with_view(
    graph: &Graph,
    assets: &AssetStore,
    thumbnail_png: Option<&[u8]>,
    view: Option<&ProjectView>,
) -> Result<Vec<u8>, AtmrError> {
    let json = graph_to_json_string_with_view(graph, view);
    let cursor = write_atmr_into_with_thumbnail(
        Cursor::new(Vec::new()),
        &json,
        assets,
        thumbnail_png,
    )?;
    Ok(cursor.into_inner())
}

/// Decode a project from raw bytes.
///
/// The buffer must be an `.atmr` archive: graph JSON at
/// [`GRAPH_ENTRY_NAME`] plus any embedded assets. Anything else fails
/// — a buffer that isn't a zip surfaces `AtmrError::Zip`, and a zip
/// without the graph entry surfaces `AtmrError::MissingGraphJson`.
pub fn read_project_from_bytes(
    bytes: &[u8],
    registry: &NodeRegistry,
) -> Result<(LoadResult, AssetStore), AtmrError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let json = read_graph_json_entry(&mut archive)?;
    let assets = AssetStore::read_from_zip(&mut archive)?;
    let load = graph_from_json_str(&json, registry)?;
    Ok((load, assets))
}

/// Encode an ATMR archive containing the graph JSON + every asset in
/// `assets` into the supplied writer. Split out so tests / future
/// callers can stream into a buffer or in-memory cursor without
/// touching the filesystem.
pub fn write_atmr_into<W: Write + Seek>(
    writer: W,
    graph_json: &str,
    assets: &AssetStore,
) -> Result<W, AtmrError> {
    write_atmr_into_with_thumbnail(writer, graph_json, assets, None)
}

/// [`write_atmr_into`] with an optional preview image written directly
/// after the graph. Stored (not deflated): a PNG is already compressed,
/// so deflating it costs CPU on every save for no size win — and
/// leaving it stored keeps the browser-side preview read a plain copy.
pub fn write_atmr_into_with_thumbnail<W: Write + Seek>(
    writer: W,
    graph_json: &str,
    assets: &AssetStore,
    thumbnail_png: Option<&[u8]>,
) -> Result<W, AtmrError> {
    let mut zw = ZipWriter::new(writer);
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        // Mid-range compression level — quality 6 is the deflate
        // default and matches the storage / CPU trade-off used by
        // most desktop tools (7-Zip "Normal", `gzip` default, etc.).
        .compression_level(Some(6));
    zw.start_file(GRAPH_ENTRY_NAME, opts)?;
    zw.write_all(graph_json.as_bytes())?;
    if let Some(png) = thumbnail_png {
        if !png.is_empty() {
            let stored =
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zw.start_file(THUMBNAIL_ENTRY_NAME, stored)?;
            zw.write_all(png)?;
        }
    }
    assets.write_into_zip(&mut zw)?;
    let writer = zw.finish()?;
    Ok(writer)
}

/// Extract the `graph.json` entry from an open zip reader and return
/// its contents as a `String`. Surfaces `MissingGraphJson` if the
/// archive opened but didn't contain the expected entry.
pub fn read_graph_json_from_atmr<R: Read + Seek>(reader: R) -> Result<String, AtmrError> {
    let mut archive = ZipArchive::new(reader)?;
    read_graph_json_entry(&mut archive)
}

fn read_graph_json_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<String, AtmrError> {
    let mut entry = match archive.by_name(GRAPH_ENTRY_NAME) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Err(AtmrError::MissingGraphJson),
        Err(e) => return Err(AtmrError::Zip(e)),
    };
    let mut json = String::with_capacity(entry.size() as usize);
    entry.read_to_string(&mut json)?;
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use crate::graph::graph::Graph;
    use crate::serialization::graph_json::graph_to_json_string;

    fn empty_registry() -> NodeRegistry { NodeRegistry::new() }

    #[test]
    fn empty_graph_round_trips_through_atmr() {
        let original = Graph::new();
        let mut buf: Vec<u8> = Vec::new();
        let cursor = Cursor::new(&mut buf);
        let json = graph_to_json_string(&original);
        let _ = write_atmr_into(cursor, &json, &AssetStore::new()).expect("write atmr");

        // Re-read the archive from the in-memory buffer and confirm
        // the embedded graph.json round-trips.
        let read_cursor = Cursor::new(buf.as_slice());
        let recovered = read_graph_json_from_atmr(read_cursor).expect("read graph.json");
        assert_eq!(recovered, json);
    }

    #[test]
    fn assets_round_trip_through_atmr() {
        let mut assets = AssetStore::new();
        let r = assets.insert(
            b"<fake 3mf bytes>".to_vec(),
            "bunny.stl".into(),
            Some("Bunny".into()),
            Some("3mf".into()),
        );

        let bytes = write_project_to_bytes(&Graph::new(), &assets).expect("write");

        let reg = empty_registry();
        let (_, recovered) = read_project_from_bytes(&bytes, &reg).expect("read");
        assert_eq!(recovered.len(), 1);
        let entry = recovered.get(&r).expect("asset survives the round trip");
        assert_eq!(entry.bytes, b"<fake 3mf bytes>".to_vec());
        assert_eq!(entry.original_filename, "bunny.stl");
        assert_eq!(entry.extension, "3mf");
        assert_eq!(entry.label.as_deref(), Some("Bunny"));
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
    fn write_project_always_emits_zip_bytes() {
        let bytes = write_project_to_bytes(&Graph::new(), &AssetStore::new())
            .expect("write project");
        assert_eq!(&bytes[..4], b"PK\x03\x04");

        // And those bytes read back through the project reader.
        let reg = empty_registry();
        let _ = read_project_from_bytes(&bytes, &reg).expect("read zip bytes");
    }

    #[test]
    fn decode_warnings_reach_the_caller() {
        // Skipped nodes must be reported, not silently dropped: the UI
        // prints `LoadResult::warnings` after a load, so a project
        // referencing a node type this build doesn't know about has to
        // surface a warning naming it.
        let json = r#"{
            "version": 1,
            "next_socket_uid": 0,
            "nodes": [
                {"id": 0, "type_id": "WidgetFromTheFuture", "position": [0,0], "inputs": [], "outputs": [], "properties": {}}
            ],
            "noodles": []
        }"#;
        let buf = write_atmr_into(Cursor::new(Vec::new()), json, &AssetStore::new())
            .expect("write atmr")
            .into_inner();

        let reg = empty_registry();
        let (load, _) = read_project_from_bytes(&buf, &reg).expect("read project");
        assert_eq!(load.graph.node_count(), 0);
        assert!(
            load.warnings.iter().any(|w| w.contains("WidgetFromTheFuture")),
            "expected a warning naming the skipped node type, got {:?}",
            load.warnings
        );
    }

    #[test]
    fn thumbnail_round_trips_and_leaves_the_graph_readable() {
        use crate::serialization::thumbnail::read_thumbnail_from_bytes;
        let png = b"\x89PNG\r\n\x1a\n<pretend preview>".to_vec();
        let bytes =
            write_project_to_bytes_with_thumbnail(&Graph::new(), &AssetStore::new(), Some(&png))
                .expect("write with thumbnail");

        assert_eq!(read_thumbnail_from_bytes(&bytes), Some(png));
        // The graph decoder is untouched by the extra entry.
        let reg = empty_registry();
        let _ = read_project_from_bytes(&bytes, &reg).expect("graph still decodes");
    }

    #[test]
    fn no_thumbnail_means_no_entry() {
        use crate::serialization::thumbnail::read_thumbnail_from_bytes;
        let with_none =
            write_project_to_bytes_with_thumbnail(&Graph::new(), &AssetStore::new(), None)
                .expect("write without thumbnail");
        assert!(read_thumbnail_from_bytes(&with_none).is_none());
        // …and it is byte-identical to the plain encoder, so existing
        // files and hashes don't move.
        let plain = write_project_to_bytes(&Graph::new(), &AssetStore::new()).expect("write");
        assert_eq!(with_none, plain);
    }

    /// View state rides in the project bytes and comes back out on read.
    #[test]
    fn view_state_round_trips_through_a_project() {
        use crate::serialization::view_state::{CameraState, CanvasView};

        let view = ProjectView {
            view_state: Some(CanvasView { scale: 0.85, offset: [120.0, -40.0] }),
            divider_position: Some(0.42),
            camera_state: Some(CameraState {
                position: [60.0, -80.0, 45.0],
                target: [1.0, 2.0, 3.0],
                initial_position: Some([10.0, 20.0, 30.0]),
                initial_target: Some([0.0, 0.0, 0.0]),
            }),
        };
        let bytes =
            write_project_to_bytes_with_view(&Graph::new(), &AssetStore::new(), None, Some(&view))
                .expect("write with view");

        let reg = empty_registry();
        let (load, _) = read_project_from_bytes(&bytes, &reg).expect("read");
        assert_eq!(load.view.as_ref(), Some(&view));
    }

    /// A project with nothing to say about its view is byte-identical to
    /// one written by the plain encoder — old files and their hashes
    /// don't move, and a graph-only save can't smuggle a view in.
    #[test]
    fn an_empty_view_writes_the_same_bytes_as_no_view() {
        let plain = write_project_to_bytes(&Graph::new(), &AssetStore::new()).expect("write");
        let with_none = write_project_to_bytes_with_view(
            &Graph::new(),
            &AssetStore::new(),
            None,
            Some(&ProjectView::default()),
        )
        .expect("write empty view");
        assert_eq!(plain, with_none);

        // …and such a project reads back with no view at all.
        let reg = empty_registry();
        let (load, _) = read_project_from_bytes(&plain, &reg).expect("read");
        assert!(load.view.is_none(), "missing view must stay missing");
    }

    #[test]
    fn read_project_rejects_bare_graph_json_bytes() {
        // The bare-JSON project format is gone: only zip archives are
        // projects, so plain graph JSON must be refused rather than
        // silently accepted.
        let json = graph_to_json_string(&Graph::new());
        let reg = empty_registry();
        match read_project_from_bytes(json.as_bytes(), &reg) {
            Ok(_) => panic!("bare JSON must not load as a project"),
            Err(e) => assert!(matches!(e, AtmrError::Zip(_)), "unexpected error: {e}"),
        }
    }
}
