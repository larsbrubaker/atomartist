//! Tests for [`crate::node_icons`] — evaluating a node type with its own
//! defaults and rendering the result into a palette icon
//! (`docs/file-browser-design.md` §5b, step 6f-2).
//!
//! The cache is process-wide, so nothing here asserts on the *absence*
//! of a cached entry: another test in the same binary may have filled it
//! already. Assertions are on monotone facts (an entry exists
//! afterwards; one call resolves at most one new type) and on the
//! cache-free [`render_icon`] path.

use std::time::Instant;

use atomartist_lib::nodes;
use atomartist_lib::registry::NodeRegistry;

use crate::file_browser::favorites::SEED_NODE_TYPES;
use crate::mesh_raster::{fit_distance, ICON_SIZE};

use super::*;

fn builtins() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    nodes::register_all(&mut registry);
    registry
}

/// A Box renders its real geometry: right size, transparent corners, a
/// covered middle, and more than one shade (i.e. the lights are on).
#[test]
fn box_renders_a_shaded_icon() {
    let icon = render_icon(&builtins(), "Box", ICON_SIZE).expect("Box renders an icon");
    assert_eq!((icon.width, icon.height), (ICON_SIZE, ICON_SIZE));
    let mid = ICON_SIZE / 2;
    assert_eq!(
        icon.pixel(mid, mid).expect("centre pixel")[3],
        255,
        "the middle of a primitive icon must be opaque"
    );
    for (x, y) in [(0, 0), (ICON_SIZE - 1, ICON_SIZE - 1)] {
        assert_eq!(
            icon.pixel(x, y).expect("corner pixel")[3],
            0,
            "corner ({x}, {y}) must stay transparent"
        );
    }
    // Whole RGB triples, not one channel: a Box wears MatterCAD's
    // near-white "Cube" red, whose red channel clamps to 255 on every
    // lit face — the per-face difference shows in green/blue.
    let mut shades: Vec<[u8; 3]> = icon
        .rgba
        .chunks_exact(4)
        .filter(|p| p[3] == 255)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    shades.sort_unstable();
    shades.dedup();
    assert!(
        shades.len() >= 3,
        "a flat fill would mean the lighting never ran; saw {shades:?}"
    );
}

/// Different primitives produce different pictures — the icons are
/// renders of the real generators, not one shared placeholder.
#[test]
fn sphere_and_box_icons_differ() {
    let registry = builtins();
    let boxed = render_icon(&registry, "Box", ICON_SIZE).expect("Box icon");
    let sphere = render_icon(&registry, "Sphere", ICON_SIZE).expect("Sphere icon");
    let differing = boxed
        .rgba
        .iter()
        .zip(sphere.rgba.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differing > boxed.rgba.len() / 10,
        "a sphere must not look like a box (only {differing} bytes differ)"
    );
}

/// Framing rule pinned: NodeDesigner's `r / tan(fov/2) · 1.15` at fov 30°.
#[test]
fn camera_distance_matches_the_ancestors_formula() {
    let expected = 10.0 / (30.0_f32.to_radians() * 0.5).tan() * 1.15;
    assert!(
        (fit_distance(10.0) - expected).abs() < 1e-4,
        "fit_distance(10) = {}, expected {expected}",
        fit_distance(10.0)
    );
    // Linear in the radius, so framing is scale-invariant.
    assert!((fit_distance(20.0) - 2.0 * fit_distance(10.0)).abs() < 1e-3);
}

/// A type the registry has never heard of renders nothing rather than
/// panicking — the strip keeps its glyph.
#[test]
fn unknown_type_renders_nothing() {
    assert!(render_icon(&builtins(), "NoSuchNodeType", ICON_SIZE).is_none());
}

/// A node type with no geometry output (a Number) is not iconable.
#[test]
fn non_geometry_node_renders_nothing() {
    assert!(render_icon(&builtins(), "Number", ICON_SIZE).is_none());
}

/// [`render_next`] resolves at most one new type per call and stops
/// once the whole set is cached.
#[test]
fn render_next_fills_the_cache_one_type_at_a_time() {
    let registry = builtins();
    let ids: Vec<&str> = SEED_NODE_TYPES.to_vec();
    let before = ids.iter().filter(|id| is_resolved(id, ICON_SIZE)).count();
    if before < ids.len() {
        assert!(
            render_next(&registry, &ids, ICON_SIZE),
            "there was work to do"
        );
        let after = ids.iter().filter(|id| is_resolved(id, ICON_SIZE)).count();
        assert_eq!(after, before + 1, "one call resolves exactly one type");
    }
    // Drive to completion, then the pump goes quiet.
    for _ in 0..ids.len() {
        render_next(&registry, &ids, ICON_SIZE);
    }
    assert!(
        !render_next(&registry, &ids, ICON_SIZE),
        "a full cache does no work"
    );
    for id in &ids {
        assert!(
            icon(id, ICON_SIZE).is_some(),
            "`{id}` should have a cached icon"
        );
    }
}

/// The pixel size is part of the cache key, so a device-scale change
/// simply misses and renders once more at the new size (rather than
/// serving a blurry point-sampled blit of the old one).
#[test]
fn the_pixel_size_is_part_of_the_cache_key() {
    // A size no other test in this binary asks for, so the "not yet
    // resolved" half of the assertion cannot be raced.
    const ODD_SIZE: u32 = 37;
    let registry = builtins();
    assert!(!is_resolved("Box", ODD_SIZE));
    assert!(render_next(&registry, &["Box"], ODD_SIZE));

    let small = icon("Box", ODD_SIZE).expect("a Box icon at the odd size");
    assert_eq!((small.width, small.height), (ODD_SIZE, ODD_SIZE));
    // The reference-size entry is a *different* key. (`map_or`, not
    // `is_none_or`: the crate's MSRV is older than that method.)
    assert!(
        icon("Box", ICON_SIZE).map_or(true, |big| big.width == ICON_SIZE),
        "sizes must not alias in the cache"
    );
}

/// Startup cost: the whole seeded palette renders well inside a
/// perceptible budget, which is what lets the strip fill in over a
/// handful of frames without a thread (CLAUDE.md: measure, don't guess).
#[test]
fn the_seed_palette_renders_within_the_startup_budget() {
    let registry = builtins();
    let start = Instant::now();
    for id in SEED_NODE_TYPES {
        assert!(
            render_icon(&registry, id, ICON_SIZE).is_some(),
            "`{id}` renders"
        );
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "{} seed icons took {elapsed:?}; expected well under half a second",
        SEED_NODE_TYPES.len()
    );
}
