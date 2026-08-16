//! Operand import for the Boolean node — the "correctness floor" half of
//! `boolean_node.rs` (plan step B-1 of `docs/boolean-node-plan.md`).
//!
//! Ports MatterCAD's `ManifoldKernel.Import` / `WeldSeams`
//! (`Submodules/agg-sharp/PolygonMesh/Csg/ManifoldKernel.cs:500-540, 623-655`):
//!
//!   * **Always the robust import.** Strictly manifold input behaves exactly
//!     like the plain import (the handle is not marked soup, so the `Auto`
//!     engine still picks the fast exact pipeline); closed-but-non-manifold
//!     input becomes a triangle-soup handle instead of being rejected. Only
//!     geometry that is not even closed fails.
//!   * **One weld retry on `NotClosed`.** Positions are `f32` and every
//!     transform on the way here re-rounds them, so a seam that was shared
//!     upstream can come apart in the last digit. The retry welds with a
//!     tolerance taken from both the bounding box's size and its distance
//!     from the origin, so the tolerance means the same thing for a 1 mm part
//!     and a 300 mm one, at the origin or across the bed.
//!   * **Never absorb a failure.** A refused operand is returned as an
//!     [`ImportFailure`] so the node can raise a `NodeError` naming it — a
//!     boolean that swallowed an error operand as empty geometry would still
//!     report success, and the part would silently vanish from the output.
//!     That includes the quiet case the kernel calls success: an operand
//!     that arrives with triangles and imports as an *empty* solid is
//!     refused as [`ImportFailure::NoSolidGeometry`].
//!   * **Bound-check before the weld.** `MeshGL::merge` indexes `tri_verts`
//!     unchecked, so a hostile index panics before the kernel's own
//!     validation ever runs. Indices are checked here first.
//!
//! Vertex welding uses `MeshGL::merge` (manifold-rust's BVH + union-find
//! weld), replacing the hand-rolled 1e-5 hash-bucket weld this module grew
//! out of.

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{BooleanConfig, BooleanEngine, Error, MeshGL};

/// Fraction of the operand's scale that counts as "the same point" when
/// re-welding a seam. MatterCAD's `WeldSeams` tolerance.
const SEAM_TOLERANCE_FRACTION: f64 = 1e-5;

/// Install the process-wide default boolean engine.
///
/// `Auto` is what makes the robust import worth having: soup-backed operands
/// (closed non-manifold input) and self-intersecting operands route to the
/// robust engine, while clean manifold operands keep the exact pipeline.
///
/// Placed here, behind a `Once`, rather than in a shell's startup: the
/// default is a property of *this library's* use of manifold-rust, and both
/// shells (`atomartist-ui` native and wasm), the icon renderer, and every
/// `atomartist-lib` / `atomartist-ui-test` test reach the boolean through
/// this module. An init function the shells had to remember to call would be
/// a fifth caller to keep in sync, and tests would silently run a different
/// engine than the app.
pub fn ensure_default_engine() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| BooleanConfig::set_default_engine(BooleanEngine::Auto));
}

/// Why an operand could not be used. Kept distinct from `manifold_rust`'s
/// `Error` because one refusal has no status behind it: an import that
/// *succeeds* and hands back an empty solid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportFailure {
    /// The kernel refused the mesh with this status.
    Status(Error),
    /// The mesh imported cleanly but bounds no volume — every triangle was
    /// degenerate, or the shell collapsed. The kernel calls that `NoError`
    /// and an empty solid; for a boolean operand it means the part would
    /// vanish from the output without a word, which is the one outcome this
    /// module exists to prevent.
    NoSolidGeometry,
}

/// Import one Boolean operand, returning the reason for refusal on failure.
///
/// See the module docs for the policy. `Ok` covers both a true manifold and
/// a validated triangle soup.
pub fn import_operand(mesh: &MeshGL) -> Result<Manifold, ImportFailure> {
    ensure_default_engine();

    if mesh.num_prop < 3 {
        return Err(ImportFailure::Status(Error::MissingPositionProperties));
    }
    let mut prepared = positions_only(mesh);
    // The kernel's own bounds check runs *after* `MeshGL::merge`, which
    // indexes `tri_verts` unchecked — a hostile index panics before any
    // status exists. Check first.
    if !indices_in_bounds(&prepared) {
        return Err(ImportFailure::Status(Error::VertexOutOfBounds));
    }
    let had_triangles = !prepared.tri_verts.is_empty();
    prepared.merge();
    let imported = Manifold::from_mesh_gl_robust(&prepared);
    if imported.status() == Error::NoError {
        return check_non_empty(imported, had_triangles);
    }

    // Only a seam gap is worth a second chance; every other status is a
    // genuine property of the geometry.
    if imported.status() == Error::NotClosed {
        if let Some(welded) = weld_seams(&prepared) {
            let retried = Manifold::from_mesh_gl_robust(&welded);
            if retried.status() == Error::NoError {
                return check_non_empty(retried, had_triangles);
            }
        }
    }
    Err(ImportFailure::Status(imported.status()))
}

/// An operand that arrived with triangles must leave the import with volume.
fn check_non_empty(m: Manifold, had_triangles: bool) -> Result<Manifold, ImportFailure> {
    if had_triangles && m.is_empty() {
        return Err(ImportFailure::NoSolidGeometry);
    }
    Ok(m)
}

/// True if every triangle index addresses a vertex that exists.
fn indices_in_bounds(mesh: &MeshGL) -> bool {
    let n = (mesh.vert_properties.len() / 3) as u32;
    mesh.tri_verts.iter().all(|&i| i < n)
}

/// A `num_prop = 3` (positions-only) copy of `mesh`.
///
/// Our meshes carry per-face-flat normals in slots 3..6, which duplicate
/// every seam vertex once per incident face. Dropping the normals is what
/// lets those duplicates weld back together, and it also keeps manifold from
/// interpolating normals across newly cut vertices (mid-face averages) —
/// `boolean_node` recomputes flat normals on the result instead. Topology is
/// left to `MeshGL::merge`.
///
/// `mesh.num_prop` must be at least 3 (positions are the first three slots);
/// anything smaller carries no positions at all and yields an empty mesh —
/// `import_operand` rejects that case up front with
/// `MissingPositionProperties` rather than letting it look like empty input.
pub fn positions_only(mesh: &MeshGL) -> MeshGL {
    let stride = mesh.num_prop as usize;
    if stride < 3 {
        return MeshGL { num_prop: 3, ..Default::default() };
    }
    let n = mesh.vert_properties.len() / stride;
    let mut out = Vec::with_capacity(n * 3);
    for i in 0..n {
        out.extend_from_slice(&mesh.vert_properties[i * stride..i * stride + 3]);
    }
    MeshGL {
        num_prop: 3,
        vert_properties: out,
        tri_verts: mesh.tri_verts.clone(),
        ..Default::default()
    }
}

/// MatterCAD's `WeldSeams`: merge coincident-within-tolerance vertices, drop
/// the faces the weld collapsed, and drop the vertices those faces used.
///
/// `mesh` must already be positions-only. Returns `None` when the operand
/// has no finite extent to scale a tolerance against (welding is not the
/// answer to a non-finite vertex either).
fn weld_seams(mesh: &MeshGL) -> Option<MeshGL> {
    let (lo, hi) = bounds(mesh)?;
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let diagonal = (size[0] * size[0] + size[1] * size[1] + size[2] * size[2]).sqrt();
    if !(diagonal > 0.0) || !diagonal.is_finite() {
        return None;
    }
    // The part's own size is only half of what sets the scale of a seam gap:
    // f32 positions round on the float grid at their absolute coordinate, so
    // a part parked far from the origin comes apart at a coarser step than
    // its own diagonal implies. Whichever is larger sets the tolerance.
    let dist_from_origin = max_abs_component(lo).max(max_abs_component(hi));
    let tolerance = diagonal.max(dist_from_origin) * SEAM_TOLERANCE_FRACTION;
    // Area rather than length, and well under the tolerance squared: this
    // only drops triangles the weld itself collapsed, not thin ones the
    // model meant to have.
    let min_face_area = tolerance * tolerance / 10.0;

    let mut welded = mesh.clone();
    welded.tolerance = tolerance as f32;
    welded.merge_from_vert.clear();
    welded.merge_to_vert.clear();
    welded.merge();

    let remap = merge_map(&welded);
    welded.merge_from_vert.clear();
    welded.merge_to_vert.clear();
    // The kernel's own tolerance is a simplification budget, not a weld
    // tolerance — hand it a clean mesh, not a licence to move surfaces.
    welded.tolerance = 0.0;
    apply_remap(&mut welded, &remap, min_face_area);
    Some(welded)
}

/// Resolve `merge_from_vert` / `merge_to_vert` into a per-vertex index map.
/// `MeshGL::merge` records merges as index pairs rather than rewriting the
/// mesh, so following the chain to a representative is on us.
fn merge_map(mesh: &MeshGL) -> Vec<u32> {
    // Positions-only by construction; see `positions_only`.
    let n = (mesh.vert_properties.len() / 3) as u32;
    let mut map: Vec<u32> = (0..n).collect();
    for (from, to) in mesh
        .merge_from_vert
        .iter()
        .zip(mesh.merge_to_vert.iter())
    {
        if (*from as usize) < map.len() && (*to as usize) < map.len() {
            map[*from as usize] = *to;
        }
    }
    // Follow chains to a representative (bounded by n steps).
    for i in 0..map.len() {
        let mut r = map[i];
        let mut guard = 0;
        while map[r as usize] != r && guard < map.len() {
            r = map[r as usize];
            guard += 1;
        }
        map[i] = r;
    }
    map
}

/// Rewrite triangles through `remap`, drop degenerate faces, and compact
/// away the vertices that leaves unused.
///
/// Indices outside `remap` drop their triangle rather than panicking:
/// `import_operand` bound-checks before it gets here, but this stays local so
/// a future caller can't turn hostile input into a crash.
fn apply_remap(mesh: &mut MeshGL, remap: &[u32], min_face_area: f64) {
    let mut tris: Vec<u32> = Vec::with_capacity(mesh.tri_verts.len());
    for tri in mesh.tri_verts.chunks_exact(3) {
        if tri.iter().any(|&i| i as usize >= remap.len()) {
            continue;
        }
        let t = [
            remap[tri[0] as usize],
            remap[tri[1] as usize],
            remap[tri[2] as usize],
        ];
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            continue;
        }
        if face_area(mesh, t) < min_face_area {
            continue;
        }
        tris.extend_from_slice(&t);
    }

    // Compact: keep only referenced vertices, in first-use order.
    let n = mesh.vert_properties.len() / 3;
    let mut new_index: Vec<Option<u32>> = vec![None; n];
    let mut positions: Vec<f32> = Vec::with_capacity(mesh.vert_properties.len());
    for v in tris.iter_mut() {
        let old = *v as usize;
        let id = match new_index[old] {
            Some(id) => id,
            None => {
                let id = (positions.len() / 3) as u32;
                positions.extend_from_slice(&mesh.vert_properties[old * 3..old * 3 + 3]);
                new_index[old] = Some(id);
                id
            }
        };
        *v = id;
    }
    mesh.vert_properties = positions;
    mesh.tri_verts = tris;
}

/// Area of a triangle given as positions-only vertex indices.
fn face_area(mesh: &MeshGL, t: [u32; 3]) -> f64 {
    let p = |i: u32| {
        let o = i as usize * 3;
        [
            mesh.vert_properties[o] as f64,
            mesh.vert_properties[o + 1] as f64,
            mesh.vert_properties[o + 2] as f64,
        ]
    };
    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt()
}

/// Axis-aligned bounds of a positions-only mesh, or `None` if it is empty or
/// carries a non-finite coordinate.
fn bounds(mesh: &MeshGL) -> Option<([f64; 3], [f64; 3])> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for v in mesh.vert_properties.chunks_exact(3) {
        for k in 0..3 {
            let x = v[k] as f64;
            if !x.is_finite() {
                return None;
            }
            lo[k] = lo[k].min(x);
            hi[k] = hi[k].max(x);
        }
        any = true;
    }
    if any {
        Some((lo, hi))
    } else {
        None
    }
}

fn max_abs_component(v: [f64; 3]) -> f64 {
    v[0].abs().max(v[1].abs()).max(v[2].abs())
}

/// A human-readable node-error message for a refused operand. `operand` is
/// the input socket's name so the user knows which part of the graph to fix.
pub fn refusal_message(operand: &str, failure: ImportFailure) -> String {
    let status = match failure {
        ImportFailure::NoSolidGeometry => {
            return format!(
                "Boolean: input '{}' cannot be used — it produced no solid geometry \
                 (every triangle is degenerate, or the shell has no volume)",
                operand
            )
        }
        ImportFailure::Status(s) => s,
    };
    let detail = match status {
        Error::NotClosed => {
            "it is not a closed solid — it has holes, or faces wound inconsistently"
        }
        Error::NotManifold => "it is not a manifold solid",
        Error::NonFiniteVertex => "it has non-finite (NaN or infinite) vertex positions",
        Error::MissingPositionProperties => "it carries no vertex positions",
        Error::VertexOutOfBounds => "its triangles reference vertices that do not exist",
        Error::Cancelled => "the operation was cancelled",
        _ => "the geometry kernel refused it",
    };
    format!(
        "Boolean: input '{}' cannot be used — {} ({})",
        operand,
        detail,
        status.to_str()
    )
}
