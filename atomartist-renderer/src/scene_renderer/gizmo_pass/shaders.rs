//! WGSL shader for the gizmo line pass.
//!
//! Behavioural port of NodeDesigner's `LineBasicMaterial`-driven gizmo
//! rendering. Each gizmo draws as a `LineList` (2 vertices per
//! segment) in one or both of two variants:
//!
//! * **Solid** — depth-tested against the scene depth attachment so
//!   the gizmo is occluded by geometry it sits behind.
//! * **Overlay** — no depth test, semi-transparent (default
//!   `occluded_alpha = 0.25` from NodeDesigner's `bounds-gizmo.js`)
//!   so the user can still see where occluded parts of the gizmo
//!   are.
//!
//! A single shader handles both variants — the alpha multiplier is
//! carried in the colour vec4 (the host populates it with the
//! occluded-alpha for the overlay variant, full alpha for the solid
//! variant). The two variants share the same uniform layout so the
//! per-frame uniform write is cheap.

/// Uniform layout consumed by the gizmo line shader. `mvp` is the
/// renderer's jittered projection × view × model (model defaults to
/// identity but the gizmo can apply its own transform by passing a
/// matrix-folded mvp). `color` is RGBA; the overlay pass folds the
/// occluded-alpha multiplier into this colour before writing the
/// uniform.
pub const GIZMO_LINE_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;

struct VOut {
    @builtin(position) clip: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>) -> VOut {
    var o: VOut;
    o.clip = u.mvp * vec4<f32>(pos, 1.0);
    return o;
}

@fragment
fn fs(_in: VOut) -> @location(0) vec4<f32> {
    return u.color;
}
"#;
