//! The Boolean node's **degradation policy** — plan step B-5 of
//! `docs/boolean-node-plan.md`, ported from MatterCAD's
//! `Object3DBooleanOperations` (`GetTouchingMeshes` L108-156,
//! `CombineParticipants` L685-814, `RepairAndRetryUnion` L513-673,
//! `ClassifyRepairedOperand` L458-471, `CopyRescuedOperand` L315-347) and
//! `agg-sharp/PolygonMesh/Csg/ManifoldKernel.RunBoolean` (L126-242).
//!
//! It answers one question: what should a **union** do when one of its
//! operands is geometry the kernel will not take? B-1 made that a named
//! node error, which is right for an operation defined by every operand —
//! but for a union it costs the user every other part as well. MatterCAD's
//! answer, and now ours: union everything that works, keep the rest
//! **visible**, and say so.
//!
//! Only unions degrade. `RunBoolean` is explicit about why (L143-146): "a
//! union is the one operation where leaving an operand out still has an
//! answer: the other operands' union. Subtract and Intersect are defined
//! by every operand." So Combine (and the solid/hole sub-unions inside it)
//! come through here; Intersect, Subtract's per-keep cut, and Subtract's
//! remover union keep B-1's hard failure. The remover union is not an
//! oversight: `DoSubtract` (L188-193) calls `CombineParticipants` with no
//! `operandReport`, and that parameter's own docs say leaving it null
//! "keeps the old all-or-nothing behaviour". Losing a remover would
//! silently change where the cut lands, which is not something a warning
//! can make good.
//!
//! ## The three moves
//!
//! 1. **Touching sets.** Operands whose world-space AABBs form a connected
//!    set are unioned together; separate sets never meet the kernel at
//!    all, they just come out as separate bodies. Scattered parts are the
//!    common case in an imported assembly, and a CSG that cannot change
//!    anything is pure cost.
//! 2. **Partial union.** Inside a set, a refused operand is left out of
//!    the fold instead of failing the node.
//! 3. **Repair-and-retry triage.** Each refused operand gets one repair
//!    attempt, and the result is classified three ways
//!    ([`RepairedOperandUse`]): it rejoins the union (associativity — the
//!    partial result is unioned with it, the set is never re-run), it is
//!    kept **beside** the union when it is watertight but
//!    self-intersecting (MatterCAD: a hole-filled 41-operand union ran for
//!    half an hour without finishing), or the repair is refused too and
//!    the user's **original** geometry is carried into the output as its
//!    own body. Nothing vanishes on any path.
//!
//! ## What our repair can and cannot do
//!
//! MatterCAD repairs with its Repair tool's hole fill, which can create
//! faces. manifold-rust has no hole filler, so
//! [`repair_weld`](super::boolean_import::repair_weld) — a much coarser
//! seam weld — is the whole of our repair. The triage is nevertheless the
//! real MatterCAD one, and it is written against a repair *seam*
//! ([`union_degrading_with`]) exactly as MatterCAD's is
//! (`RepairOfRefusedOperand`, L364): the branches are what needs testing,
//! and driving a real repair to each of them on demand is not something a
//! fixture can promise.

use manifold_rust::manifold::Manifold;
use manifold_rust::types::{MeshGL, OpType};

use super::boolean_colors::{operand_color, tag_original, Palette};
use super::boolean_import::{import_operand, refusal_message, repair_weld};
use super::boolean_ops::{
    apply_repair, baked_operands_of, boolean_op, finish_mesh, painted_result_body, split_roles,
    BooleanOptions, InputGroup, Operand,
};
use crate::geometry::{Body, BodyRole, Geometry3d, DEFAULT_GEOMETRY_COLOR};
use crate::registry::{compose_with_upstream_and_mesh, EvalCtx, NodeError};

/// How many operands a message names before it starts counting them
/// instead (`BooleanObject3D.MaxNamedOperands`, L641): a broken import can
/// put a hundred parts in one Combine, and a hundred names is not a
/// message anyone reads.
const MAX_NAMED_OPERANDS: usize = 5;

/// One body of a degraded union.
pub enum DegradedBody {
    /// A kernel result — a set's union, or a repaired operand that had to
    /// be kept beside one. Still needs
    /// [`finish`](super::boolean_ops::finish).
    Solid {
        solid: Manifold,
        /// The colour this body wears where the run data cannot answer,
        /// and the `Body::color` every consumer that ignores the
        /// per-vertex overlay sees: the first part of the set that made
        /// it (B-6, [`super::boolean_colors::painted_body`]).
        base: [f32; 4],
    },
    /// Geometry the kernel refused, handed back exactly as the user
    /// modelled it (world space, un-unioned, uncut). MatterCAD's
    /// `CopyRescuedOperand`: "the part could not take part in the boolean,
    /// but it still has to be in the scene afterwards".
    Rescued {
        /// The operand's name, so a caller that has to *drop* the rescue
        /// (a hole cuts nothing, and a hole nobody can cut with is not
        /// output) can say which part it dropped.
        label: String,
        /// World-space, as baked for the import.
        mesh: MeshGL,
        /// The body it came from — its colour and role
        /// ([`Operand::source`]).
        source: Body,
    },
}

/// What one operand's repair turned out to be worth
/// (`RepairedOperandUse` / `ClassifyRepairedOperand`, L426-471).
#[derive(Debug, PartialEq, Eq)]
pub enum RepairedOperandUse {
    /// Clean: union it into the partial result.
    RejoinsUnion,
    /// Watertight but self-intersecting: carried into the result beside
    /// the union rather than in it. The kernel would take it and then
    /// leave its exact pipeline for the robust one, whose cost on
    /// repaired geometry is open-ended.
    KeptSeparate,
    /// Not a repair at all — the user's original geometry is what goes in
    /// the output.
    RepairFailed,
}

/// One operand the union could not use: the name a summary lists it under
/// and the sentence B-1 wrote about *why* the kernel refused it
/// (MatterCAD's `SkippedBooleanOperand` carries the same pair).
pub struct SkippedOperand {
    pub label: String,
    /// [`refusal_message`](super::boolean_import::refusal_message)'s
    /// sentence — the actionable half, and the one the user needs to fix
    /// the part rather than merely know it was left out.
    pub detail: String,
    /// Whether the part is still in the node's output.
    ///
    /// A rescued **solid** is (uncut, un-unioned, but there). A rescued
    /// **hole** is not, when the Combine has solids: it is negative space
    /// that cut nothing, and emitting it beside the material would show
    /// the user a body where they asked for a void. That distinction has
    /// to reach the message — "it is still in the result" would otherwise
    /// be a lie about the one part that went wrong.
    pub kept: bool,
}

/// What the union had to do to the user's parts, in the terms a message
/// needs (MatterCAD's `BooleanOperandReport`).
#[derive(Default)]
pub struct OperandReport {
    /// Operands left out of the union and rescued as-is.
    pub skipped: Vec<SkippedOperand>,
    /// Operands the repair made usable — reported because the node
    /// changed geometry on the user's behalf, and the result looking
    /// right is exactly why silence would be wrong.
    pub repaired: Vec<String>,
    /// How many operands the union was asked to take, for "N of M".
    pub total: usize,
}

impl OperandReport {
    /// Fold another union's report into this one — a Combine runs two
    /// (solids and holes) and the user has one set of parts.
    pub fn absorb(&mut self, other: OperandReport) {
        self.skipped.extend(other.skipped);
        self.repaired.extend(other.repaired);
        self.total += other.total;
    }

    /// True when the union used every part exactly as it arrived.
    pub fn is_clean(&self) -> bool {
        self.skipped.is_empty() && self.repaired.is_empty()
    }

    /// Record that the named rescued operand did **not** make it into the
    /// output after all — the caller consumed that side of the Combine.
    pub fn mark_dropped(&mut self, label: &str) {
        for s in self.skipped.iter_mut().filter(|s| s.label == label) {
            s.kept = false;
        }
    }

    /// The non-fatal messages for this report — what
    /// `BooleanObject3D.ReportSkippedOperands` (L577) and
    /// `ReportRepairedOperands` (L596) say, in that order.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        // Two sentences, because the two cases end differently for the
        // user's geometry and one sentence covering both would have to
        // lie about one of them.
        let kept: Vec<String> = self
            .skipped
            .iter()
            .filter(|s| s.kept)
            .map(|s| s.label.clone())
            .collect();
        let dropped: Vec<String> = self
            .skipped
            .iter()
            .filter(|s| !s.kept)
            .map(|s| s.label.clone())
            .collect();
        if !kept.is_empty() {
            out.push(format!(
                "Boolean: {} of {} parts are not watertight solids and were left out of \
                 the union: {}. They are still in the result, uncombined — repair them \
                 (fill their holes) and try again.",
                kept.len(),
                self.total,
                operand_names(&kept)
            ));
        }
        if !dropped.is_empty() {
            out.push(format!(
                "Boolean: {} of {} parts are not watertight solids and could not be used \
                 as holes: {}. They cut nothing and are NOT in the result — repair them \
                 (fill their holes) and try again.",
                dropped.len(),
                self.total,
                operand_names(&dropped)
            ));
        }
        if !self.skipped.is_empty() {
            // Then B-1's own sentence per part: the summaries say which
            // parts, these say what is wrong with them, which is the half
            // the user can act on. Capped at the same count a name list
            // is, for the same reason.
            out.extend(
                self.skipped
                    .iter()
                    .take(MAX_NAMED_OPERANDS)
                    .map(|s| s.detail.clone()),
            );
        }
        if self.repaired.len() == 1 {
            out.push(format!(
                "Boolean: automatically repaired a part that was not a watertight solid: {}.",
                operand_names(&self.repaired)
            ));
        } else if self.repaired.len() > 1 {
            out.push(format!(
                "Boolean: automatically repaired {} parts that were not watertight solids: {}.",
                self.repaired.len(),
                operand_names(&self.repaired)
            ));
        }
        out
    }
}

/// A degraded union's result: its bodies (one per touching set, plus one
/// per operand that could not join a union) and what it had to do to get
/// them.
#[derive(Default)]
pub struct Degraded {
    pub bodies: Vec<DegradedBody>,
    pub report: OperandReport,
    /// Which operand painted which run of the union's result (B-6). Built
    /// here rather than passed in because it is a *product* of the union:
    /// only this module knows which operands the kernel actually took, and
    /// only a taken operand has an id in the result's run data.
    pub palette: Palette,
}

/// `'a', 'b', 'c' and 2 more` — MatterCAD's `OperandNames` (L643).
fn operand_names(names: &[String]) -> String {
    let listed = names
        .iter()
        .take(MAX_NAMED_OPERANDS)
        .map(|n| format!("'{}'", n))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > MAX_NAMED_OPERANDS {
        format!("{} and {} more", listed, names.len() - MAX_NAMED_OPERANDS)
    } else {
        listed
    }
}

/// Combine, under the degradation policy: union the solids, union the
/// holes, subtract the second from the first
/// (`BooleanMeshBuilder.CombineMeshes` L104-192, through
/// `CombineParticipants`' touching sets).
///
/// Each touching set is its own body — MatterCAD concatenates the sets
/// into one mesh with disjoint components, and a body list is what that
/// means here. A Combine of parts that all touch is still exactly one
/// body, as it was before B-5.
///
/// Three paths a refused part can take, all of them named in the warning:
///   * a **rescued solid** is emitted uncut — the kernel would not take
///     it, so it cannot be a subtract operand either;
///   * a **rescued hole**, when the Combine has solids, is **dropped**: it
///     cuts nothing, and emitting negative space as a body would show
///     material where the user asked for a void (MatterCAD's rescue puts
///     it in the hole mesh, which the subtract consumes — same outcome).
///     This is the one path on which geometry does not come out, so it
///     gets its own sentence in the report
///     ([`OperandReport::mark_dropped`]) rather than the "still in the
///     result" one;
///   * with **no solids at all** the holes are the whole answer, so
///     everything on that side comes out — rescues included — still
///     marked as a hole.
pub fn combine_degrading(
    ctx: &EvalCtx,
    groups: &[InputGroup],
    opts: BooleanOptions,
) -> Result<(Geometry3d, OperandReport), NodeError> {
    let (solid_groups, hole_groups) = split_roles(groups);
    // One palette per union, folded back together below: the solids and
    // the holes are two unions over one set of the user's parts, and the
    // hole union's operands can still paint the result (a hole cuts a
    // solid; the faces it leaves behind are its own).
    let solids = union_degrading(
        baked_operands_of(&solid_groups.iter().collect::<Vec<_>>()),
        opts,
        new_palette(ctx),
    );
    let holes = union_degrading(
        baked_operands_of(&hole_groups.iter().collect::<Vec<_>>()),
        opts,
        new_palette(ctx),
    );

    let mut report = solids.report;
    report.absorb(holes.report);
    let mut palette = solids.palette;
    palette.absorb(holes.palette);

    if solids.bodies.is_empty() {
        // Nothing to cut: the holes are the whole answer, and they stay
        // holes so a downstream Combine can still use them.
        let bodies = emit_bodies(ctx, holes.bodies, &[], BodyRole::Hole, opts, &palette)?;
        return Ok((Geometry3d::from_bodies(bodies), report));
    }

    let mut cutters: Vec<Manifold> = Vec::new();
    for body in holes.bodies {
        match body {
            DegradedBody::Solid { solid, .. } => cutters.push(solid),
            // A hole the kernel refused cuts nothing, and putting it in
            // the output would show material where the user asked for a
            // void. It is dropped — and the report is told so, because
            // "still in the result" is exactly what it is not.
            DegradedBody::Rescued { label, .. } => report.mark_dropped(&label),
        }
    }
    let bodies = emit_bodies(ctx, solids.bodies, &cutters, BodyRole::Solid, opts, &palette)?;
    Ok((Geometry3d::from_bodies(bodies), report))
}

/// The palette a union should fill: a real one normally, a disabled one
/// when the node's own `Color` already decides the answer and the re-tag
/// would only be thrown away.
fn new_palette(ctx: &EvalCtx) -> Palette {
    if super::boolean_colors::node_color_override(ctx).is_some() {
        Palette::disabled()
    } else {
        Palette::new()
    }
}

/// Finish a union's bodies into output bodies, cutting each solid one with
/// whichever cutters can reach it.
///
/// The bounds test is not an optimisation detail: it is the same reasoning
/// as the touching sets. A hole on the other side of the bed cannot change
/// this body, and running the boolean anyway would re-triangulate it for
/// nothing.
fn emit_bodies(
    ctx: &EvalCtx,
    bodies: Vec<DegradedBody>,
    cutters: &[Manifold],
    role: BodyRole,
    opts: BooleanOptions,
    palette: &Palette,
) -> Result<Vec<Body>, NodeError> {
    let mut out = Vec::new();
    for body in bodies {
        match body {
            DegradedBody::Solid { mut solid, base } => {
                for cutter in cutters {
                    if solid
                        .bounding_box()
                        .does_overlap_box(&cutter.bounding_box())
                    {
                        solid = boolean_op(&solid, cutter, OpType::Subtract, opts);
                    }
                }
                out.extend(painted_result_body(ctx, &solid, palette, base, role)?);
            }
            // The rescue keeps the part's own colour (and role): it goes
            // through the same `compose_with_upstream` rule as
            // `pass_through` and Keep Subtracted Parts, with the baked
            // mesh in place of the upstream one. The part that visibly
            // failed to combine must not also change colour.
            DegradedBody::Rescued { mesh, source, .. } => {
                out.push(compose_with_upstream_and_mesh(
                    ctx,
                    &source,
                    finish_mesh(mesh),
                ));
            }
        }
    }
    Ok(out)
}

/// Union `operands` under the degradation policy, with the repair this
/// codebase can actually perform.
///
/// `palette` is the colour bookkeeping to fill — [`Palette::disabled`]
/// when the caller already knows the result's colour (the node's own
/// `Color` overrides it), so no operand pays for the re-tag.
pub fn union_degrading(operands: Vec<Operand>, opts: BooleanOptions, palette: Palette) -> Degraded {
    union_degrading_with(operands, opts, palette, &|mesh| repair_weld(mesh))
}

/// [`union_degrading`] with the repair attempt supplied — the seam
/// MatterCAD's `RepairOfRefusedOperand` delegate exists for. `repair`
/// returns `None` when it has nothing usable to hand back.
pub fn union_degrading_with(
    operands: Vec<Operand>,
    opts: BooleanOptions,
    palette: Palette,
    repair: &dyn Fn(&MeshGL) -> Option<MeshGL>,
) -> Degraded {
    let mut out = Degraded { palette, ..Degraded::default() };
    out.report.total = operands.len();
    for set in touching_sets(operands) {
        union_one_set(set, opts, repair, &mut out);
    }
    out
}

/// One touching set: the partial union of everything the kernel took,
/// then the triage of everything it refused.
fn union_one_set(
    set: Vec<Operand>,
    opts: BooleanOptions,
    repair: &dyn Fn(&MeshGL) -> Option<MeshGL>,
    out: &mut Degraded,
) {
    let mut partial: Option<Manifold> = None;
    // Each refused operand travels with B-1's sentence about it: the
    // refusal is discovered here and reported much later, and re-deriving
    // "why" from a rescued mesh is not possible.
    let mut refused: Vec<(Operand, String)> = Vec::new();
    // The set's stand-in colour: the first part the user wired into it.
    // A union of several parts has no single colour, and this is the one
    // the run data will agree with over the largest share of the surface
    // in the common case (a part unioned with its own detailing).
    let union_base = set
        .first()
        .map(|o| operand_color(&o.source))
        .unwrap_or(DEFAULT_GEOMETRY_COLOR);
    for operand in set {
        match import_operand(&operand.mesh) {
            Ok(solid) => {
                // Tagged with the part's own colour before it joins the
                // fold: after the union its triangles are only findable
                // through the run data (B-6).
                let solid = tag_original(
                    apply_repair(solid, opts),
                    operand_color(&operand.source),
                    &mut out.palette,
                );
                partial = Some(union_into(partial, solid, opts));
            }
            Err(failure) => {
                let detail = refusal_message(&operand.label, failure);
                refused.push((operand, detail));
            }
        }
    }

    // Triage first, union after: a repaired operand that rejoins is
    // unioned with the partial result rather than re-running the set —
    // union is associative, so the solid is the same and the work is not
    // (MatterCAD, L499-504: "re-running 144 operands to add 40 costs the
    // 104 that already worked a second time").
    let mut beside: Vec<DegradedBody> = Vec::new();
    for (operand, detail) in refused {
        let color = operand_color(&operand.source);
        let repaired = repair(&operand.mesh).map(|mesh| classify_repaired(&mesh, opts));
        match repaired {
            // A repaired operand is still the user's part, so it keeps its
            // colour on both of the paths that put it in the kernel.
            Some((RepairedOperandUse::RejoinsUnion, Some(solid))) => {
                let solid = tag_original(solid, color, &mut out.palette);
                partial = Some(union_into(partial, solid, opts));
                out.report.repaired.push(operand.label);
            }
            Some((RepairedOperandUse::KeptSeparate, Some(solid))) => {
                // Its own colour, not the set's: this body *is* that one
                // part, carried beside the union rather than in it.
                beside.push(DegradedBody::Solid {
                    solid: tag_original(solid, color, &mut out.palette),
                    base: color,
                });
                out.report.repaired.push(operand.label);
            }
            // No repair, or one the kernel refuses too. A repair the
            // kernel would not take is not an improvement worth showing
            // the user in place of what they modelled, so the original
            // geometry is what goes in the output.
            _ => {
                beside.push(DegradedBody::Rescued {
                    label: operand.label.clone(),
                    mesh: operand.mesh,
                    source: operand.source,
                });
                // Kept by default: a rescue *is* in the output, unless the
                // caller has to drop it (see [`combine_degrading`]), which
                // flips this through [`OperandReport::mark_dropped`].
                out.report.skipped.push(SkippedOperand {
                    label: operand.label,
                    detail,
                    kept: true,
                });
            }
        }
    }

    if let Some(solid) = partial {
        out.bodies.push(DegradedBody::Solid { solid, base: union_base });
    }
    out.bodies.extend(beside);
}

/// Whether a repaired mesh can go back into the union, has to be carried
/// beside it, or is not a repair at all — one import answers all three
/// (`ClassifyRepairedOperand`, L458).
pub fn classify_repaired(
    mesh: &MeshGL,
    opts: BooleanOptions,
) -> (RepairedOperandUse, Option<Manifold>) {
    match import_operand(mesh) {
        Err(_) => (RepairedOperandUse::RepairFailed, None),
        Ok(solid) => {
            let solid = apply_repair(solid, opts);
            if solid.has_self_intersections() {
                (RepairedOperandUse::KeptSeparate, Some(solid))
            } else {
                (RepairedOperandUse::RejoinsUnion, Some(solid))
            }
        }
    }
}

fn union_into(partial: Option<Manifold>, next: Manifold, opts: BooleanOptions) -> Manifold {
    match partial {
        None => next,
        Some(acc) => boolean_op(&acc, &next, OpType::Add, opts),
    }
}

/// Group operands into sets whose world-space AABBs form a connected
/// component (`GetTouchingMeshes`, L108). Operands in different sets
/// cannot affect each other, so unioning them would only cost time and
/// re-triangulate geometry that was already right.
///
/// An operand with no finite extent (an empty or non-finite mesh) gets a
/// set of its own; the import will refuse it and the triage will report
/// it, which is a better answer than letting an infinite box swallow every
/// other operand into one set.
pub fn touching_sets(operands: Vec<Operand>) -> Vec<Vec<Operand>> {
    let boxes: Vec<Option<Aabb>> = operands.iter().map(|o| aabb_of(&o.mesh)).collect();
    let n = operands.len();
    let mut set_of: Vec<Option<usize>> = vec![None; n];
    let mut sets: Vec<Vec<usize>> = Vec::new();

    for i in 0..n {
        if set_of[i].is_some() {
            continue;
        }
        let id = sets.len();
        sets.push(vec![i]);
        set_of[i] = Some(id);
        // Breadth-first over "touches anything already in the set", which
        // is what makes the grouping transitive: a chain of parts that
        // each touch only their neighbour is one set.
        let mut cursor = 0;
        while cursor < sets[id].len() {
            let a = sets[id][cursor];
            for b in 0..n {
                if set_of[b].is_some() {
                    continue;
                }
                if touches(&boxes[a], &boxes[b]) {
                    set_of[b] = Some(id);
                    sets[id].push(b);
                }
            }
            cursor += 1;
        }
    }

    // Move the operands out in set order, keeping each set's members in
    // wiring order so a message and a body list read the way the user
    // wired the node.
    let mut slots: Vec<Option<Operand>> = operands.into_iter().map(Some).collect();
    sets.into_iter()
        .map(|mut members| {
            members.sort_unstable();
            members
                .into_iter()
                .filter_map(|i| slots[i].take())
                .collect()
        })
        .collect()
}

/// World-space axis-aligned bounds.
#[derive(Clone, Copy)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    /// Inclusive overlap: two parts that share a face have equal bounds on
    /// that axis and must land in the same set — they are exactly the
    /// parts a union has work to do on.
    pub fn touches(&self, other: &Aabb) -> bool {
        (0..3).all(|k| self.min[k] <= other.max[k] && other.min[k] <= self.max[k])
    }
}

fn touches(a: &Option<Aabb>, b: &Option<Aabb>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.touches(b),
        _ => false,
    }
}

/// Bounds of a mesh's vertex positions (the first three slots of each
/// vertex, whatever the layout). `None` for an empty or non-finite mesh.
pub fn aabb_of(mesh: &MeshGL) -> Option<Aabb> {
    let stride = mesh.num_prop as usize;
    if stride < 3 {
        return None;
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for v in mesh.vert_properties.chunks_exact(stride) {
        for k in 0..3 {
            let x = v[k] as f64;
            if !x.is_finite() {
                return None;
            }
            min[k] = min[k].min(x);
            max[k] = max[k].max(x);
        }
        any = true;
    }
    if any {
        Some(Aabb { min, max })
    } else {
        None
    }
}
