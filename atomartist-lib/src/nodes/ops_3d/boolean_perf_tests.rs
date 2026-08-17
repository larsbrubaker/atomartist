//! Boolean node **measurements** — plan step B-5a(b) of
//! `docs/boolean-node-plan.md`.
//!
//! CLAUDE.md forbids guessing at performance from reading code, and B-5
//! and B-6 each left an unmeasured suspicion behind:
//!
//! * B-6: `boolean_colors::tag_original` calls `Manifold::as_original`
//!   per operand per evaluation, which clones the handle and recomputes
//!   normals / coplanar data inside manifold-rust.
//! * B-5: `boolean_degrade::aabb_of` walks every vertex of every operand
//!   per evaluation, and `touching_sets` is O(n²) per set on top.
//!
//! This file is the number, not the fix. It is a plain `#[test]` timing
//! loop rather than a criterion benchmark on purpose: the workspace has
//! no criterion dev-dependency, the question here is "is this anywhere
//! near the 10 ms budget", and a test runs in CI with the rest of the
//! suite. The assertions are budgets with headroom, so this file fails
//! only on a real regression, never on a noisy machine.
//!
//! # Results
//!
//! Measured 2026-08-17 on the author's Windows 11 machine, `cargo test`
//! dev profile (workspace members `opt-level = 1`, dependencies — which
//! is where manifold-rust's CSG lives — `opt-level = 2`). These are
//! **machine-relative**: read the ratios, not the absolutes.
//!
//! Operands are default-segment spheres (32 × 16 → 1024 triangles, 561
//! vertices), arranged in a touching chain so the union really runs CSG
//! on every one of them.
//!
//! Whole-node Combine (`the_combine_sweep`, `#[ignore]`d):
//!
//! | N operands | colours on | colours off |
//! |---|---|---|
//! | 2  | 16.7 ms | 16.0 ms |
//! | 8  | 232 ms  | 220 ms  |
//! | 32 | 3.88 s  | 3.90 s  |
//!
//! The on/off difference is noise: it is +4 %, +5 %, then *negative* at
//! N = 32. So the tagging cost is measured directly instead
//! (`one_colour_retag_costs_far_less_than_importing_the_operand`):
//!
//! * robust import of one sphere: **≈ 4.1 ms**
//! * bare handle clone: **≈ 92 µs**
//! * clone + `as_original` + record: **≈ 304 µs**
//! * ⇒ the re-tag itself: **≈ 213 µs per operand**, about 5 % of that
//!   operand's own import and ~0.7 % of an 8-operand evaluation.
//!
//! `touching_sets` (the AABB walk plus the O(n²) grouping) is reported
//! as an **upper bound**, because each timed sample must also deep-clone
//! the operands the call consumes: **≤ 35 µs at N = 2 and N = 8, and
//! 240–440 µs across runs at N = 32**, the bulk of which is that clone. Subtracting a separately
//! timed clone was tried and abandoned — the clone's own median swings
//! ~4× between runs, several times the quantity being measured, so the
//! difference is noise, not a number. The bound is enough to answer the
//! question: even counting a full mesh copy, this is ~0.01 % of the
//! evaluation it precedes and three orders of magnitude under the 10 ms
//! budget, at an operand count no one wires by hand.
//!
//! ## What that justifies
//!
//! **Nothing, in either suspect.** Neither the colour re-tag nor the
//! AABB / grouping pass is worth optimizing: caching a `Palette` or an
//! AABB across evaluations would buy ~1 % and cost real
//! cache-invalidation semantics. The upstream ask floated in B-6 (expose
//! the imported handle's original id so `as_original` can be skipped)
//! is likewise not justified by these numbers — 213 µs against a 4.1 ms
//! import.
//!
//! What the numbers *do* say is that a Boolean is expensive per
//! **operand import**, not per anything we wrote: two spheres cost
//! 16.7 ms, of which ~8 ms is the two robust imports. A Boolean of
//! realistic primitives is therefore over the project's 10 ms
//! re-evaluation budget on its own, and the lever that matters is not
//! re-evaluating it (the executor's dirty tracking, which exists) — or,
//! someday, not re-importing unchanged operands. Both are much bigger
//! changes than this step, and neither is guesswork any more.
//!
//! Re-run: `cargo test -p atomartist-lib --lib perf_tests -- --nocapture`
//! (add `the_combine_sweep -- --ignored` for the table).

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::boolean_colors::{tag_original, Palette};
use super::super::boolean_degrade::touching_sets;
use super::super::boolean_import::import_operand;
use super::super::boolean_ops::Operand;
use super::tests::{op, run_boolean_outputs, translated};
use crate::geometry::{generate_sphere, Body, Geometry3d};
use crate::graph::node::PortValue;

/// Default sphere-node segmentation — the mesh a user actually wires in.
const SEG_U: u32 = 32;
const SEG_V: u32 = 16;

/// Operand counts to report. 32 is well past any realistic hand-wired
/// Boolean; it is here to show the *shape* of the curve.
const COUNTS: [usize; 3] = [2, 8, 32];

/// `n` spheres in a row, each overlapping its neighbour by half a
/// radius, so every operand lands in one touching set and the union has
/// real work to do.
fn sphere_chain(n: usize) -> Vec<(String, Geometry3d)> {
    (0..n)
        .map(|i| {
            let mesh = translated(
                &generate_sphere(5.0, SEG_U, SEG_V),
                i as f32 * 7.5,
                0.0,
                0.0,
            );
            (format!("op{i}"), Geometry3d::from_mesh(Arc::new(mesh)))
        })
        .collect()
}

/// The same chain in `boolean_degrade`'s own operand form.
fn operand_chain(n: usize) -> Vec<Operand> {
    (0..n)
        .map(|i| {
            let mesh = translated(
                &generate_sphere(5.0, SEG_U, SEG_V),
                i as f32 * 7.5,
                0.0,
                0.0,
            );
            Operand {
                label: format!("op{i}"),
                source: Body::from_mesh(Arc::new(mesh.clone())),
                mesh,
            }
        })
        .collect()
}

/// One Combine evaluation, colours on or off. Colours are disabled the
/// way the node itself disables them: an explicit `color` override means
/// no operand is ever tagged (`boolean_colors::node_color_override`).
fn run_combine(operands: &[(String, Geometry3d)], colors: bool) {
    let borrowed: Vec<(&str, Geometry3d)> = operands
        .iter()
        .map(|(name, g)| (name.as_str(), g.clone()))
        .collect();
    let extra: Vec<(&str, PortValue)> = if colors {
        Vec::new()
    } else {
        vec![("color", PortValue::Color([0.2, 0.6, 0.9, 1.0]))]
    };
    run_boolean_outputs(&borrowed, op("Combine"), None, &extra)
        .expect("a chain of overlapping spheres unions cleanly");
}

/// Median of `iters` samples — median rather than mean so one scheduler
/// hiccup on a busy CI box does not decide the number.
fn median<F: FnMut()>(iters: usize, mut body: F) -> Duration {
    let mut samples: Vec<Duration> = (0..iters)
        .map(|_| {
            let t = Instant::now();
            body();
            t.elapsed()
        })
        .collect();
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// The B-6 question, measured **directly** rather than by differencing
/// two whole evaluations: what does one `as_original` re-tag cost?
///
/// The end-to-end A/B (colours on vs off, `the_combine_sweep` below) is
/// far too noisy to answer this — the CSG's own run-to-run variance is
/// larger than the entire tagging cost, so the difference changes sign
/// between runs. Timing the seam itself is the honest measurement.
#[test]
fn one_colour_retag_costs_far_less_than_importing_the_operand() {
    let mesh = generate_sphere(5.0, SEG_U, SEG_V);
    // Warm-up: the first import through manifold-rust pays one-time
    // engine setup that no later evaluation repeats.
    let _ = import_operand(&mesh).expect("a sphere is a closed solid");

    let import = median(9, || {
        let solid = import_operand(&mesh).expect("a sphere is a closed solid");
        std::hint::black_box(solid);
    });
    // The tag is timed against a bare handle clone, which is the work
    // `tag_original` does *besides* `as_original` — so the difference is
    // the re-tag itself and nothing else.
    let solid = import_operand(&mesh).expect("a sphere is a closed solid");
    let clone_only = median(99, || {
        std::hint::black_box(solid.clone());
    });
    let clone_and_tag = median(99, || {
        let mut palette = Palette::new();
        let tagged = tag_original(solid.clone(), [0.2, 0.6, 0.9, 1.0], &mut palette);
        std::hint::black_box((tagged, palette));
    });
    // "Was it actually tagged" is checked once, *outside* the timed
    // closure — the check is small, but so is what is being measured.
    let mut witness = Palette::new();
    let _ = tag_original(solid.clone(), [0.2, 0.6, 0.9, 1.0], &mut witness);
    assert!(!witness.is_empty(), "the operand really was tagged");
    let tagging = clone_and_tag.saturating_sub(clone_only);
    println!(
        "per operand: import {import:.2?}, handle clone {clone_only:.2?}, \
         clone+tag {clone_and_tag:.2?} → re-tag ≈ {tagging:.2?}"
    );
    // Measured at ~5 % of the import in B-5a; the bound is deliberately
    // loose (one import) so a noisy machine never fails the suite, while
    // a real regression — a re-tag that costs more than re-importing the
    // operand from scratch — still trips it, and *that* is the number
    // that would justify asking manifold-rust for the imported handle's
    // original id instead.
    assert!(
        tagging < import,
        "the re-tag ({tagging:?}) now costs more than the import it follows ({import:?}) — \
         time to ask manifold-rust for the imported handle's original id"
    );
}

/// The end-to-end sweep the module docs' table comes from. `#[ignore]`d
/// because a 32-operand Combine takes seconds: this is a measurement to
/// re-run by hand (`cargo test -p atomartist-lib the_combine_sweep --
/// --ignored --nocapture`), not a per-commit budget.
#[test]
#[ignore = "seconds-long measurement; run by hand when the numbers matter"]
fn the_combine_sweep() {
    for n in COUNTS {
        let operands = sphere_chain(n);
        run_combine(&operands, true);
        run_combine(&operands, false);

        let with = median(3, || run_combine(&operands, true));
        let without = median(3, || run_combine(&operands, false));
        println!(
            "Combine N={n:2}: colours on {:>10.2?}, off {:>10.2?}",
            with, without
        );
    }
}

/// The B-5 question: the AABB walk plus the O(n²) grouping, on their own.
#[test]
fn touching_sets_is_negligible_next_to_the_csg_it_precedes() {
    for n in COUNTS {
        // Meshes are built once, outside the clock: what is being timed
        // is the AABB walk plus the grouping, not sphere generation.
        // `touching_sets` consumes its operands, so each sample has to
        // clone them first. No attempt is made to subtract that clone
        // back out: the clone's own median swings by 4× run to run,
        // which is far more than the grouping costs, so a difference of
        // medians here is noise wearing a number's clothes. What is
        // reported instead is an honest **upper bound** — deep-copying
        // every operand mesh AND grouping them — which is all the
        // question needs, because even the bound is negligible.
        let operands = operand_chain(n);
        let mut sets_len = 0;
        let elapsed = median(9, || {
            let sets = touching_sets(operands.clone());
            sets_len = sets.len();
            std::hint::black_box(sets);
        });
        assert_eq!(sets_len, 1, "the chain is one touching set");
        println!(
            "touching_sets N={n:2}: ≤ {elapsed:>8.2?} \
             (upper bound — includes a deep clone of every operand)"
        );
        assert!(
            elapsed < Duration::from_millis(10),
            "grouping {n} operands took {elapsed:?}; the whole 50-node graph budget is 10 ms"
        );
    }
}
