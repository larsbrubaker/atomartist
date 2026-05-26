//! Geometry primitives, mesh utilities, and 2D path helpers.
//!
//! - `mesh3d` — `MeshGL` constructors, normals, merge, transform
//! - `primitives` — generate_box / cylinder / sphere
//! - `path2d` — `CrossSection` re-export plus winding helpers

pub mod geometry3d;
pub mod mesh3d;
pub mod path2d;
pub mod primitives;

pub use geometry3d::{is_inherit_color, Body, Geometry3d, DEFAULT_GEOMETRY_COLOR, INHERIT_COLOR};
pub use mesh3d::{
    apply_transform, bounds, compute_flat_normals, get_normal, get_pos, make_mesh, merge_meshes,
    num_tris, num_verts, NUM_PROP, STRIDE,
};
pub use primitives::{
    generate_box, generate_cone, generate_cylinder, generate_cylinder_advanced,
    generate_pyramid, generate_sphere, generate_torus, generate_wedge,
};
