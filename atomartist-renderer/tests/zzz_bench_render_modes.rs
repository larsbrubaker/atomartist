//! THROWAWAY perf probe (delete after use). Times the render_modes CPU
//! functions on split-vertex meshes of increasing triangle count so we
//! can see how much per-mesh-change work Outlines/Polygons/Overhang add.
//! Run: cargo test --release -p atomartist-renderer --test zzz_bench_render_modes -- --nocapture

use atomartist_renderer::scene_renderer::render_modes;
use manifold_rust::types::MeshGL;

/// A split-vertex grid: `n×n` quads, each quad = 2 tris, every triangle
/// carries its own 3 verts with a flat normal — matching AtomArtist's
/// real mesh layout so the position-keyed topology cost is realistic.
fn grid_mesh(n: usize) -> MeshGL {
    let mut vp: Vec<f32> = Vec::new();
    let mut tv: Vec<u32> = Vec::new();
    let step = 1.0 / n as f32;
    for j in 0..n {
        for i in 0..n {
            let x0 = i as f32 * step;
            let x1 = x0 + step;
            let y0 = j as f32 * step;
            let y1 = y0 + step;
            // Two tris; give them slightly different normals per quad so
            // feature detection has real work (not everything coplanar).
            let nz = if (i + j) % 2 == 0 { 1.0 } else { 0.9 };
            let corners = [
                [x0, y0, 0.0], [x1, y0, 0.0], [x1, y1, 0.0],
                [x0, y0, 0.0], [x1, y1, 0.0], [x0, y1, 0.0],
            ];
            for c in corners {
                let base = (vp.len() / 6) as u32;
                vp.extend_from_slice(&[c[0], c[1], c[2], 0.0, 1.0 - nz, nz]);
                tv.push(base);
            }
        }
    }
    MeshGL { num_prop: 6, vert_properties: vp, tri_verts: tv, ..Default::default() }
}

fn ms(f: impl FnOnce()) -> f32 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_secs_f32() * 1000.0
}

#[test]
fn bench_render_modes_cpu_cost() {
    let identity = {
        let mut m = [0.0f32; 16];
        m[0] = 1.0; m[5] = 1.0; m[10] = 1.0; m[15] = 1.0;
        m
    };
    for n in [64usize, 128, 200, 300] {
        let mesh = grid_mesh(n);
        let tris = mesh.tri_verts.len() / 3;
        // warm + time each
        let t_feat = ms(|| { std::hint::black_box(render_modes::feature_edges(&mesh, render_modes::OUTLINE_FEATURE_ANGLE_RAD)); });
        let t_all = ms(|| { std::hint::black_box(render_modes::all_edges(&mesh)); });
        let t_nm = ms(|| { std::hint::black_box(render_modes::non_manifold_edges(&mesh)); });
        let t_over = ms(|| { std::hint::black_box(render_modes::overhang_colors(&mesh, &identity)); });
        println!(
            "tris={:>7}  feature={:>7.2}ms  all={:>7.2}ms  nonmanifold={:>7.2}ms  overhang={:>7.2}ms",
            tris, t_feat, t_all, t_nm, t_over
        );
    }
}
