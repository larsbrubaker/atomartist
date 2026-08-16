//! Colour preservation across a boolean — plan step B-6 of
//! `docs/boolean-node-plan.md`, ported from MatterCAD's
//! `agg-sharp/PolygonMesh/Csg/ManifoldKernel.cs` (`ImportAsOriginal`
//! L670-709, `ExtractFaceColorsFromRuns` L870-927).
//!
//! ## What the kernel gives us
//!
//! A boolean result's [`MeshGL`] carries **run data**: `run_index` slices
//! the triangle list into contiguous spans, and `run_original_id[i]` says
//! which source mesh span `i` came from. That is the whole mechanism — a
//! triangle that survived from operand *X* is still, in the result, in a
//! run tagged with *X*'s original id.
//!
//! For that to identify an *operand* rather than "some mesh", each import
//! has to own an id. [`Manifold::as_original`] does exactly that: it
//! re-tags a handle as an original with a freshly reserved id, which then
//! shows up in every result derived from it. So the flow is
//!
//!   import → repair → [`tag_original`] (records id → the body's colour in
//!   a [`Palette`]) → boolean → [`Palette::paint_for`] on the result mesh.
//!
//! Colours are carried by **run**, not by vertex index, for the reason
//! MatterCAD's `MeshRepairRust` notes: welds and the boolean's own
//! re-triangulation reindex vertices freely, so any mapping keyed on
//! vertex identity is stale before it is read. A run is per *triangle*,
//! and the result's triangles are the ones being painted.
//!
//! ## Scope: one flat colour per operand
//!
//! MatterCAD goes one step further (`TrySplitByFaceColorsRust` L747-856):
//! a source that already carries *per-face* colours is split into one
//! sub-manifold per colour, unioned back, and matched to the result by
//! centroid. Nothing upstream of our Boolean node produces per-face
//! colours yet — [`Body`] carries one tint plus an optional per-vertex
//! overlay, and every producer sets the tint — so that refinement has no
//! input to work on and is left for the day one exists.
//!
//! ## Soup operands lose their colour, not the boolean
//!
//! A closed-but-non-manifold operand imports as a *soup* handle, and
//! `as_original` refuses those (`ManifoldKernel.ImportAsOriginal` L682-691
//! documents the same caveat). [`tag_original`] then keeps the untagged
//! import: its triangles arrive in the result under whatever run they
//! inherit rather than one this operand owns. In practice a soup operand
//! routes the whole operation through the robust engine, whose result
//! carries no run relations at all, so the colours degrade *wholesale* —
//! every body falls back to the stand-in its call site chose (see
//! [`Palette::paint_for`]). Losing the colours is much better than losing
//! the boolean.
//!
//! ## Precedence
//!
//! The node's own `Color` property is an **override** and wins over
//! everything, exactly as [`compose_with_upstream`] treats it for the
//! pass-through paths: run colours are the "upstream" side of that rule.
//! Left at the inherit sentinel (the default), the operands' colours show
//! through. With an override set, no operand is ever tagged in the first
//! place — the run data would only be thrown away, and `as_original`
//! is not free.
//!
//! [`compose_with_upstream`]: crate::registry::compose_with_upstream

use std::collections::HashMap;
use std::sync::Arc;

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{Error, MeshGL};

use crate::geometry::{is_inherit_color, Body, BodyRole, DEFAULT_GEOMETRY_COLOR};
use crate::registry::{EvalCtx, ParamSet};

/// The tint MatterCAD gives a retained remover — the "removed material"
/// red from the operation icons (`subtract_and_replace.svg`, plan §3.5).
/// Subtract & Replace wears it on the body it hands back in place of the
/// volume it cut away, so the replacement reads as *the part that was
/// removed* at a glance rather than as another keep.
pub const REPLACED_TINT: [f32; 4] = [0.949, 0.051, 0.051, 1.0];

/// Which colour each participating operand painted, keyed by the manifold
/// original id [`tag_original`] gave it.
///
/// A **disabled** palette records nothing and paints nothing — see
/// [`Palette::disabled`].
#[derive(Debug, Default)]
pub struct Palette {
    by_id: HashMap<u32, [f32; 4]>,
    /// Recording order, so [`Palette::fallback`] is the *first* operand's
    /// colour rather than whichever one the hash map happens to yield.
    order: Vec<u32>,
    disabled: bool,
}

/// What a result's run data says it should look like.
#[derive(Clone, Debug, PartialEq)]
pub enum RunPaint {
    /// Every triangle came from operands sharing one colour — a body tint
    /// says it, and a per-vertex buffer repeating the same RGBA would only
    /// cost memory and a shader branch.
    Uniform([f32; 4]),
    /// One colour per result triangle, parallel to the mesh's triangles.
    PerTriangle(Vec<[f32; 4]>),
}

impl Palette {
    pub fn new() -> Self {
        Self::default()
    }

    /// A palette that never records and never paints — what the node uses
    /// when its own `Color` overrides everything anyway, so no operand
    /// pays for [`Manifold::as_original`].
    pub fn disabled() -> Self {
        Self { disabled: true, ..Self::default() }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Note that the operand tagged `id` was `color`.
    pub fn record(&mut self, id: u32, color: [f32; 4]) {
        if self.disabled {
            return;
        }
        if self.by_id.insert(id, color).is_none() {
            self.order.push(id);
        }
    }

    /// The first colour recorded, if any. Not a fallback — see
    /// [`Palette::paint_for`], which takes the fallback from its caller:
    /// the operands are recorded in *import* order, which for a Subtract
    /// puts the removers first, so "the first colour recorded" would
    /// repaint a keep with the colour of the thing that cut it.
    pub fn first_recorded(&self) -> Option<[f32; 4]> {
        self.order
            .first()
            .and_then(|id| self.by_id.get(id))
            .copied()
    }

    /// Fold another palette in. Ids come from a process-wide counter, so
    /// two palettes can never disagree about one id and a plain merge is
    /// safe — a Combine runs two unions (solids and holes) and the user
    /// has one set of parts.
    pub fn absorb(&mut self, other: Palette) {
        if self.disabled {
            return;
        }
        for id in other.order {
            if let Some(c) = other.by_id.get(&id) {
                self.record(id, *c);
            }
        }
    }

    /// How `raw` — a mesh straight out of the kernel, run data intact —
    /// should be painted. `None` when this palette has nothing to say
    /// (disabled, or no operand was taggable).
    ///
    /// `fallback` is the colour a run nobody claims wears, and it is the
    /// **caller's** to choose: the body being painted knows which operand
    /// it is (a Subtract's keep, a union set's first part), and that is
    /// the part whose colour should stand in when the run data cannot
    /// say — most importantly for an operand that could not be tagged at
    /// all (a soup handle).
    ///
    /// A result whose triangles all resolve to the same colour comes back
    /// as [`RunPaint::Uniform`]: a Subtract that cut nothing, or a Combine
    /// of same-coloured parts, needs no per-vertex buffer at all.
    pub fn paint_for(&self, raw: &MeshGL, fallback: [f32; 4]) -> Option<RunPaint> {
        if self.disabled || self.by_id.is_empty() {
            return None;
        }
        let n_tri = raw.tri_verts.len() / 3;
        if n_tri == 0 {
            return Some(RunPaint::Uniform(fallback));
        }
        // `run_index` carries a trailing end sentinel, so a usable one has
        // a start per run *and* that end (`ExtractFaceColorsFromRuns`
        // L881). Anything shorter is malformed run data, not run data with
        // a missing tail: painting from it would spray one run's colour
        // over spans that belong to another.
        if raw.run_original_id.is_empty() || raw.run_index.len() < raw.run_original_id.len() + 1 {
            return Some(RunPaint::Uniform(fallback));
        }

        let mut colors = vec![fallback; n_tri];
        for (i, &id) in raw.run_original_id.iter().enumerate() {
            // A missing start defaults *past the end* — an inert run —
            // rather than to 0, which would let a malformed tail repaint
            // every triangle an earlier run had already claimed.
            let start = raw.run_index.get(i).map_or(n_tri, |v| *v as usize / 3);
            let end = raw.run_index.get(i + 1).map_or(n_tri, |v| *v as usize / 3);
            let color = self.by_id.get(&id).copied().unwrap_or(fallback);
            for slot in colors
                .iter_mut()
                .take(end.min(n_tri))
                .skip(start.min(n_tri))
            {
                *slot = color;
            }
        }

        if colors.windows(2).all(|w| w[0] == w[1]) {
            return Some(RunPaint::Uniform(colors[0]));
        }
        Some(RunPaint::PerTriangle(colors))
    }
}

/// Re-tag an imported operand as an original so the result's run data can
/// name it, recording `color` against the id it gets.
///
/// Returns the handle to run the boolean with. A soup handle comes back
/// untagged and unrecorded (see the module docs) — the caller cannot tell,
/// and should not have to: the operand simply wears the fallback colour.
pub fn tag_original(solid: Manifold, color: [f32; 4], palette: &mut Palette) -> Manifold {
    if palette.is_disabled() || solid.is_empty() {
        return solid;
    }
    let tagged = solid.as_original();
    if tagged.status() != Error::NoError || tagged.is_empty() {
        // A soup handle: `as_original` hands back an empty NotManifold
        // manifold. Keep the import — losing the colour is much better
        // than losing the operand.
        return solid;
    }
    let id = tagged.original_id();
    if id < 0 {
        return tagged;
    }
    palette.record(id as u32, color);
    tagged
}

/// The colour an operand contributes: its body tint, with the inherit
/// sentinel resolved to the same default the renderer would substitute —
/// a palette entry has to be a real colour, because it may end up as
/// another body's uniform tint.
pub fn operand_color(body: &Body) -> [f32; 4] {
    if is_inherit_color(&body.color) {
        DEFAULT_GEOMETRY_COLOR
    } else {
        body.color
    }
}

/// The node's own `Color`, when the user set one.
///
/// Read through the **op** preseed, whose default is the inherit
/// sentinel: an untouched Boolean node returns `None` and lets the
/// operands' colours through, and any explicit value (property or, on a
/// legacy graph, a wired `Color` socket) overrides everything.
pub fn node_color_override(ctx: &EvalCtx) -> Option<[f32; 4]> {
    let color = ParamSet::op().reader(ctx).color("color");
    if is_inherit_color(&color) {
        None
    } else {
        Some(color)
    }
}

/// The node's own `Matrix` — the transform every body it emits carries,
/// the operand matrices having been baked into the meshes already.
pub fn node_matrix(ctx: &EvalCtx) -> [f32; 16] {
    ParamSet::op().reader(ctx).matrix("matrix")
}

/// Expand per-triangle colours into the per-vertex RGBA buffer
/// [`Body::vertex_colors`] takes, against the **finished** mesh (one
/// vertex per triangle corner, after
/// [`split_for_flat_normals`](crate::geometry::mesh3d::split_for_flat_normals)).
///
/// `None` when the two cannot be matched up — the split drops a triangle
/// whose corner index was out of range, and a colour buffer that is off by
/// one triangle would mis-paint the whole tail of the mesh. A uniform body
/// tint is the honest answer there.
pub fn vertex_colors_for(tri_colors: &[[f32; 4]], finished: &MeshGL) -> Option<Arc<Vec<f32>>> {
    let stride = finished.num_prop as usize;
    if stride == 0 {
        return None;
    }
    let n_tri = finished.tri_verts.len() / 3;
    if n_tri == 0 || n_tri != tri_colors.len() {
        return None;
    }
    let n_verts = finished.vert_properties.len() / stride;
    let mut buf = vec![0.0f32; n_verts * 4];
    for (t, color) in tri_colors.iter().enumerate() {
        for k in 0..3 {
            let v = finished.tri_verts[t * 3 + k] as usize;
            if v >= n_verts {
                return None;
            }
            buf[v * 4..v * 4 + 4].copy_from_slice(color);
        }
    }
    Some(Arc::new(buf))
}

/// Assemble one output body from a finished mesh and whatever the run
/// data had to say about its colour.
///
/// Precedence, highest first:
///   1. the node's own `Color` ([`node_color_override`]);
///   2. `tint` — a colour the operation itself assigns (Subtract &
///      Replace's [`REPLACED_TINT`]);
///   3. the run colours: a per-vertex overlay when the result mixes
///      operands, a body tint when it does not;
///   4. `base`, when nothing above applies.
///
/// ## `base` is not decoration — it is the body's colour
///
/// The per-vertex overlay is **not** universal currency. The 3-D shader
/// uses it *instead of* `Body.color` (`body_buffers.rs` fills the colour
/// buffer from the overlay when there is one, and `opaque_shaders.rs`
/// shades `v_color` directly — a pick, not a multiply), and every
/// consumer that rebuilds a mesh drops it: the next node's import strips
/// vertex properties, and
/// [`compose_with_upstream_and_mesh`](crate::registry::compose_with_upstream_and_mesh)
/// sets `vertex_colors: None`. So a painted body still has to carry one
/// honest colour for everyone who cannot read the overlay, and `base` is
/// it: the colour of the part this body *is* — the keep of a Subtract,
/// the first part of a union set. A white "multiplier" would be exactly
/// wrong: nothing multiplies, and the part would turn white the moment a
/// downstream op rebuilt its mesh.
///
/// The body's matrix is the node's own: every operand's matrix was baked
/// into the mesh before the import.
pub fn painted_body(
    ctx: &EvalCtx,
    mesh: MeshGL,
    paint: Option<RunPaint>,
    tint: Option<[f32; 4]>,
    base: [f32; 4],
    role: BodyRole,
) -> Body {
    let forced = node_color_override(ctx).or(tint);
    let (color, vertex_colors) = match (forced, paint) {
        (Some(c), _) => (c, None),
        (None, Some(RunPaint::Uniform(c))) => (c, None),
        (None, Some(RunPaint::PerTriangle(tris))) => match vertex_colors_for(&tris, &mesh) {
            Some(buf) => (base, Some(buf)),
            None => (base, None),
        },
        (None, None) => (base, None),
    };
    Body {
        mesh: Arc::new(mesh),
        matrix: node_matrix(ctx),
        color,
        vertex_colors,
        origin: Some(ctx.instance.id),
        role,
    }
}
