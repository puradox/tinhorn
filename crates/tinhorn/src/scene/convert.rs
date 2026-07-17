//! The single sanctioned crossing between core's glam 0.33 types and Bevy's own
//! (distinct) glam. Core has no Bevy dependency, so every core→Bevy value is
//! converted here component-wise; a stray direct assignment would be a
//! type-mismatch compile error rather than a silent unit bug.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::Mesh;
use bevy::render::mesh::Indices;
use bevy::render::render_resource::PrimitiveTopology;

use tinhorn_core::dice_geom;

/// core glam 0.33 `Vec3` → Bevy `Vec3`.
pub fn vec3(v: glam::Vec3) -> bevy::math::Vec3 {
    bevy::math::Vec3::new(v.x, v.y, v.z)
}

/// core glam 0.33 `Quat` → Bevy `Quat`.
#[allow(dead_code)]
pub fn quat(q: glam::Quat) -> bevy::math::Quat {
    bevy::math::Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// Build a flat-shaded Bevy [`Mesh`] from a core [`dice_geom::Mesh`] — the same
/// unit-circumradius polyhedron the physics hull and the old rasterizer use, so
/// the die you see is the die the sim tumbles.
pub fn dice_mesh(m: &dice_geom::Mesh) -> Mesh {
    let positions: Vec<[f32; 3]> = m
        .vertices
        .iter()
        .map(|v| [v.position.x, v.position.y, v.position.z])
        .collect();
    let normals: Vec<[f32; 3]> = m
        .vertices
        .iter()
        .map(|v| [v.normal.x, v.normal.y, v.normal.z])
        .collect();
    let uvs: Vec<[f32; 2]> = m.vertices.iter().map(|v| v.uv).collect();

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(m.indices.clone()))
}
