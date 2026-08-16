//! Rendered palette icons for node types (`docs/file-browser-design.md`
//! §5b, step 6f-2).
//!
//! The favourites strip (`crate::favorites_strip`) shows one 44 × 44
//! slot per favourite. NodeDesigner fills that slot with an offscreen
//! render of the *real* primitive built from the node's own default
//! properties (`parts-bar-icons.js`), so what the palette shows is
//! exactly what dragging it onto the bed produces. This module is the
//! AtomArtist equivalent: it evaluates a node type standing alone in a
//! throwaway graph, flattens the resulting [`Geometry3d`] into world
//! triangles, and hands them to [`crate::mesh_raster`] — a small
//! software rasterizer, chosen because `atomartist-renderer` only draws
//! into the shell's swapchain and has no headless entry point (see that
//! module's docs for the full rationale).
//!
//! # Cache
//!
//! Renders are memoised process-wide by `type_id`, including the
//! *failures*: a type that produces no geometry caches `None` so the
//! strip stops asking and keeps its glyph. The cache never invalidates
//! within a session, which matches the ancestor (its `iconCache` is a
//! plain `Map`) and is safe here because the input is a registry
//! definition plus its own defaults — neither changes at runtime.
//!
//! Subgraph / component types are deliberately *not* rendered: their
//! `type_id` is project-scoped, so caching one under that key would
//! leak an icon from one project into the next. They keep the glyph.
//!
//! # Deferred, one per frame
//!
//! [`render_next`] renders **at most one** missing icon per call. The
//! bar calls it at the end of its *paint*, so the strip is already on
//! screen with labels and glyphs before the first render runs and the
//! icons fill in over the next handful of frames (design §5b:
//! "deferred past first paint"). That keeps startup cost off the
//! critical path without a thread — the whole point, since wasm has no
//! threads to spawn onto. Measured: the seven seeded primitives render
//! in ≈5 ms *in total*, i.e. well under a millisecond per frame.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use atomartist_lib::geometry::geometry3d::{is_inherit_color, Geometry3d};
use atomartist_lib::geometry::mesh3d;
use atomartist_lib::graph::executor::evaluate_all;
use atomartist_lib::registry::NodeRegistry;
use atomartist_lib::{Graph, PortValue};

use crate::mesh_raster::{render_mesh_icon, IconImage, Triangle};

/// Tint used when the evaluated geometry carries no colour of its own —
/// NodeDesigner's `#cccccc` fallback.
const FALLBACK_COLOR: [f32; 4] = [0.8, 0.8, 0.8, 1.0];

/// Size guard on the flattened triangle soup: enough for any primitive
/// at its default resolution, and a bound on how much memory one icon
/// job can allocate before it gives up (each triangle is 36 bytes, so
/// this caps the buffer at ~1.8 MB). A mesh that trips it keeps the
/// glyph. Not a rendering-speed claim — none has been measured for
/// meshes that large.
const MAX_TRIANGLES: usize = 50_000;

/// Bumped whenever [`crate::mesh_raster`]'s output changes, so no icon
/// produced by an older rendering rule can be served. 2 = the sRGB
/// output encode (step 6g-3); 3 = the Lambert BRDF's `1/π` on every
/// light term, matching three's physical `MeshLambertMaterial` (step
/// 6h-1).
///
/// The cache below is in-memory and process-scoped, so today this can
/// never actually differ within a run — it is in the key so the
/// invalidation contract is already there if these renders ever gain a
/// persistent tier, the way the browser's thumbnails did
/// (`file_browser::thumbs::CACHE_VERSION`).
pub const RENDER_VERSION: u32 = 3;

/// Cache key: the type id, the pixel size it was rasterized at, and the
/// [`RENDER_VERSION`] that produced it. The size is part of the key
/// because the strip asks for whatever the current device scale makes a
/// slot — a scale change simply misses and renders once more rather than
/// needing an invalidation hook.
type CacheKey = (String, u32, u32);
type Cache = Mutex<HashMap<CacheKey, Option<IconImage>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lock the cache, recovering from poisoning.
///
/// A poisoned lock means some render panicked while holding it. The
/// map itself is a plain `HashMap` of finished values, so it cannot be
/// left half-updated; refusing to touch it afterwards would be worse
/// than using it — the pump would re-render the same type on *every*
/// frame forever, re-panicking each time.
fn lock_cache() -> MutexGuard<'static, HashMap<CacheKey, Option<IconImage>>> {
    cache().lock().unwrap_or_else(PoisonError::into_inner)
}

/// The rendered `size × size` icon for `type_id`, if one has already
/// been generated.
///
/// Never renders: this is the per-frame probe the strip uses, and a
/// miss simply means "keep the glyph for now".
pub fn icon(type_id: &str, size: u32) -> Option<IconImage> {
    lock_cache()
        .get(&(type_id.to_string(), size, RENDER_VERSION))
        .cloned()
        .flatten()
}

/// True once `type_id` has been attempted at `size`, whatever the
/// outcome.
pub fn is_resolved(type_id: &str, size: u32) -> bool {
    lock_cache().contains_key(&(type_id.to_string(), size, RENDER_VERSION))
}

/// Render the first of `type_ids` that has not been attempted yet at
/// `size`, and record the outcome. Returns `true` when it did work —
/// the caller requests a redraw so the new icon reaches the screen and
/// the next frame picks up the next one.
pub fn render_next(registry: &NodeRegistry, type_ids: &[&str], size: u32) -> bool {
    let Some(next) = type_ids.iter().find(|id| !is_resolved(id, size)) else {
        return false;
    };
    let rendered = render_icon(registry, next, size);
    lock_cache().insert(((*next).to_string(), size, RENDER_VERSION), rendered);
    true
}

/// Render `type_id` at `size × size` now, bypassing the cache. Used by
/// the tests and by [`render_next`]; callers on the frame path want
/// [`icon`].
pub fn render_icon(registry: &NodeRegistry, type_id: &str, size: u32) -> Option<IconImage> {
    let def = registry.get(type_id)?;
    // Project-scoped type ids must not land in a process-wide cache.
    if def.subgraph_template().is_some() {
        return None;
    }
    let geometry = default_geometry(registry, type_id)?;
    let triangles = world_triangles(&geometry)?;
    render_mesh_icon(&triangles, icon_color(&geometry), size)
}

/// Evaluate one instance of `type_id`, alone in a throwaway graph with
/// nothing wired to it, and return whatever geometry its first
/// `Geometry3d` output produced. Property defaults come from
/// `Graph::add_new_node`, so this is literally "what the user would get
/// by dropping this node on an empty canvas".
fn default_geometry(registry: &NodeRegistry, type_id: &str) -> Option<Arc<Geometry3d>> {
    let mut graph = Graph::new();
    let id = graph.add_new_node(type_id, [0.0, 0.0], registry).ok()?;
    evaluate_all(&mut graph, registry).ok()?;
    let node = graph.get(id)?;
    // Socket order, not `HashMap` order: a node with two geometry
    // outputs must pick the same one on every run.
    node.outputs
        .iter()
        .find_map(|socket| match node.cached_outputs.get(&socket.uid) {
            Some(PortValue::Geometry3d(geometry)) if !geometry.is_empty() => Some(geometry.clone()),
            _ => None,
        })
}

/// Flatten every body into world-space triangles, applying each body's
/// own transform. Vertex normals are dropped on purpose — the
/// rasterizer computes per-face normals for the ancestor's faceted look.
///
/// `None` for an empty result or one that would exceed
/// [`MAX_TRIANGLES`]; the budget is checked *as the buffer grows*, so an
/// unexpectedly huge mesh is abandoned rather than materialized in full
/// and then rejected.
fn world_triangles(geometry: &Geometry3d) -> Option<Vec<Triangle>> {
    let mut out = Vec::new();
    for body in &geometry.bodies {
        let matrix = glam::Mat4::from_cols_array(&body.matrix);
        let mesh = &body.mesh;
        let verts = mesh3d::num_verts(mesh);
        for tri in mesh.tri_verts.chunks_exact(3) {
            let mut corners = [[0.0f32; 3]; 3];
            let mut ok = true;
            for (slot, index) in corners.iter_mut().zip(tri) {
                let index = *index as usize;
                if index >= verts {
                    ok = false;
                    break;
                }
                let p = mesh3d::get_pos(mesh, index);
                *slot = matrix
                    .transform_point3(glam::Vec3::from_array(p))
                    .to_array();
            }
            if ok {
                if out.len() >= MAX_TRIANGLES {
                    return None;
                }
                out.push(corners);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The tint the icon is shaded with: the first body's colour, or the
/// ancestor's neutral grey when nothing along the chain set one (the
/// `INHERIT_COLOR` sentinel — see `geometry3d`).
fn icon_color(geometry: &Geometry3d) -> [f32; 4] {
    match geometry.bodies.first() {
        Some(body) if !is_inherit_color(&body.color) => body.color,
        _ => FALLBACK_COLOR,
    }
}

#[cfg(test)]
#[path = "node_icons_tests.rs"]
mod tests;
