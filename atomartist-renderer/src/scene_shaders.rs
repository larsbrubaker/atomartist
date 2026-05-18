//! WGSL shader sources for scene_renderer.
//!
//! Kept separate from render-pass setup so scene_renderer.rs stays
//! below the repository file-size guardrail while shader text remains
//! readable and uncompressed.

pub(super) const SHADER: &str = r#"
struct Uniforms {
    mvp: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    light_dir: vec4<f32>,
    base_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>) -> VOut {
    var o: VOut;
    o.clip = u.mvp * vec4<f32>(pos, 1.0);
    o.world_normal = (u.normal_mat * vec4<f32>(normal, 0.0)).xyz;
    return o;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let l = normalize(u.light_dir.xyz);
    let diff = max(dot(n, l), 0.0);
    let fill = max(dot(n, vec3<f32>(-l.x, l.y * 0.4, -l.z)), 0.0) * 0.35;
    let ambient = 0.18;
    let lit = ambient + diff * 0.85 + fill;
    let rgb = u.base_color.rgb * lit;
    return vec4<f32>(rgb, u.base_color.a);
}
"#;

pub(super) const GRID_SHADER: &str = r#"
struct GridU {
    mvp: mat4x4<f32>,
    cell: vec4<f32>,        // x = minor cell size, y = major cell stride, z = grid_y
    line_color: vec4<f32>,
    bg_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: GridU;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_xz: vec2<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>) -> VOut {
    var o: VOut;
    let p = vec3<f32>(pos.x, u.cell.z, pos.z);
    o.clip = u.mvp * vec4<f32>(p, 1.0);
    o.world_xz = p.xz;
    return o;
}

// Coverage of a 1-pixel-wide line at integer grid coordinates, derived
// from screen-space derivatives so it stays sharp at any zoom.
fn line_coverage(coord: vec2<f32>) -> f32 {
    let d = fwidth(coord);
    let g = abs(fract(coord - 0.5) - 0.5) / d;
    let line = min(g.x, g.y);
    return 1.0 - clamp(line, 0.0, 1.0);
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let cell  = u.cell.x;
    let major = u.cell.y;

    // Minor + major grid coverages (in world-space cell units).
    let minor_c = coord_to_cell(in.world_xz, cell);
    let major_c = coord_to_cell(in.world_xz, cell * major);
    let minor_a = line_coverage(minor_c) * 0.35;
    let major_a = line_coverage(major_c);

    let alpha = max(minor_a, major_a);
    if alpha < 0.01 {
        discard;
    }
    let col = mix(u.bg_color.rgb, u.line_color.rgb, alpha);
    return vec4<f32>(col, alpha * u.line_color.a);
}

fn coord_to_cell(p: vec2<f32>, cell: f32) -> vec2<f32> {
    return p / cell;
}
"#;

pub(super) const OUTLINE_SHADER: &str = r#"
struct U {
    mvp: mat4x4<f32>,
    color: vec4<f32>,
    width: vec4<f32>, // x = world-space inflation distance
};

@group(0) @binding(0) var<uniform> u: U;

struct VOut {
    @builtin(position) clip: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>) -> VOut {
    var o: VOut;
    let inflated = pos + normalize(normal) * u.width.x;
    o.clip = u.mvp * vec4<f32>(inflated, 1.0);
    return o;
}

@fragment
fn fs(_in: VOut) -> @location(0) vec4<f32> {
    return u.color;
}
"#;

