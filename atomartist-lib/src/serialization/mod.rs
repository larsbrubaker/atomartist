//! File-format serialization: graph JSON, mesh import/export, and the
//! `.atmr` project archive.
//!
//! Everything here is byte-oriented — nothing in this module touches
//! the filesystem. Projects go in and out through
//! [`write_project_to_bytes`] / [`read_project_from_bytes`]; deciding
//! where those bytes live (disk, browser storage, a remote service) is
//! the storage layer's job. [`atmr::write_atmr_into`] /
//! [`atmr::read_graph_json_from_atmr`] are the lower-level stream
//! primitives for callers that already have a `Write + Seek` /
//! `Read + Seek`.

pub mod asset_store;
pub mod atmr;
pub mod change_detection;
pub mod graph_json;
pub mod mesh_3mf;
pub mod mesh_io;
pub mod mesh_mcx;
pub mod mesh_obj;
pub mod nodedesigner_import;

pub use asset_store::{AssetEntry, AssetRef, AssetStore};
pub use change_detection::{ChangeTracker, SavedBaseline};
pub use atmr::{
    read_graph_json_from_atmr, read_project_from_bytes, write_atmr_into, write_project_to_bytes,
    AtmrError, GRAPH_ENTRY_NAME, PROJECT_EXTENSION,
};
pub use graph_json::{
    graph_from_json_str, graph_to_json_string, load_graph, save_graph, GraphFile, JsonPortValue,
    LoadResult, SCHEMA_VERSION,
};
pub use mesh_3mf::{export_3mf, import_3mf, ThreemfError};
pub use mesh_mcx::{import_mcx, McxError, McxPart};
pub use mesh_io::{export_stl, import_stl, StlError};
pub use mesh_obj::{export_obj, import_obj, ObjError};
pub use nodedesigner_import::import_nodedesigner_scene_str;
