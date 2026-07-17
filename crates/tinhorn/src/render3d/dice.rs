//! Dice meshes for the six standard polyhedra, as `render3d` [`Mesh`]es.
//!
//! The geometry itself — the polyhedra, the cup profile, the per-face read-face
//! data — lives in [`tinhorn_core::dice_geom`], shared with the physics collision
//! hulls (and, later, the web renderer). This module is the thin adapter that
//! copies that plain glam data into `render3d`'s own [`Mesh`]/[`Vertex`] types
//! for the software rasterizer. The converted meshes are cached, so the render
//! loop's per-frame `mesh_for`/`cup` calls stay a pointer bump.

use std::sync::Arc;

use crate::render3d::mesh::{Mesh, Vertex};
use tinhorn_core::dice_geom;

// The read-face geometry is pure glam data with no `render3d` types, so the
// renderer and the number overlay use it straight from core.
pub use tinhorn_core::dice_geom::{face_geometry, FaceGeom};

/// Copy a core geometry mesh into `render3d`'s `Mesh` (identical fields, distinct
/// types — the crate boundary is the only reason for the copy).
fn to_render_mesh(src: &dice_geom::Mesh) -> Mesh {
    let vertices = src
        .vertices
        .iter()
        .map(|v| Vertex {
            position: v.position,
            normal: v.normal,
            uv: v.uv,
        })
        .collect();
    Mesh::new(vertices, src.indices.clone())
}

/// The dice cup mesh (see [`dice_geom::cup`]), converted once and cached.
pub fn cup() -> Arc<Mesh> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Arc<Mesh>> = OnceLock::new();
    CACHE
        .get_or_init(|| Arc::new(to_render_mesh(&dice_geom::cup())))
        .clone()
}

/// The mesh for a die of `sides` (see [`dice_geom::mesh_for`]); non-standard
/// sizes fall back to the cube. The six polyhedra are converted once and cached,
/// so each call returns a shared `Arc` — a pointer bump, no vertex copy.
pub fn mesh_for(sides: u32) -> Arc<Mesh> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<[(u32, Arc<Mesh>); 6]> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        [4u32, 6, 8, 10, 12, 20].map(|s| (s, Arc::new(to_render_mesh(&dice_geom::mesh_for(s)))))
    });
    cache
        .iter()
        .find(|(s, _)| *s == sides)
        .map(|(_, m)| m.clone())
        .unwrap_or_else(|| cache[1].1.clone())
}
