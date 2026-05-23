//! Smoke tests against the bundled mesh fixtures in `tests/meshes/`.
//!
//! Every file under `tests/meshes/` should be importable by
//! [`mesh_node::decode_mesh`] without errors. New fixtures land in
//! that directory; one happy-path test below per representative
//! example. When a future bug surfaces because a real-world file fails
//! to import, copy the file into `tests/meshes/` first, add a test
//! that exercises it, then fix the importer.

use std::path::PathBuf;

use atomartist_lib::nodes::mesh::mesh_node;

/// Absolute path to the bundled mesh fixture `name`, computed from
/// `CARGO_MANIFEST_DIR` so the test runs regardless of the user's CWD.
pub fn mesh_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("meshes")
        .join(name)
}

#[test]
fn simple_box_ascii_stl_imports_as_twelve_triangles() {
    // `Simple Box.stl` from MatterCAD / NodeDesigner ships as ASCII
    // STL (the format Tinkercad / FreeCAD / SolidWorks "Ascii STL"
    // produce by default). 12 triangles = 2 per face × 6 faces of a
    // cube, which is what the importer must round-trip.
    let path = mesh_fixture("simple_box.stl");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    let mesh = mesh_node::decode_mesh(&bytes, "stl")
        .unwrap_or_else(|e| panic!("decode simple_box.stl: {}", e));
    assert_eq!(mesh.tri_verts.len() / 3, 12, "cube has 12 triangles");
    assert!(
        mesh.vert_properties.len() > 0,
        "imported vertex buffer must be non-empty"
    );
}

#[test]
fn simple_box_round_trips_through_three_mf() {
    // The on-disk asset path in atmr always re-encodes meshes as 3MF.
    // Verify that round trip preserves the triangle count for this
    // ASCII-STL-sourced fixture.
    let bytes = std::fs::read(mesh_fixture("simple_box.stl")).unwrap();
    let mesh = mesh_node::decode_mesh(&bytes, "stl").unwrap();
    let three_mf = atomartist_lib::serialization::export_3mf(&mesh)
        .expect("export_3mf must succeed");
    let reloaded = mesh_node::decode_mesh(&three_mf, "3mf")
        .expect("import_3mf must succeed");
    assert_eq!(
        reloaded.tri_verts.len(),
        mesh.tri_verts.len(),
        "3MF round trip must preserve triangle count",
    );
}
