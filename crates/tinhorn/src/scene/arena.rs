//! The static casino-tray furniture for the Bevy arena — the felt bed, the
//! mahogany tray walls and rails, the wood table, the room floor, the gradient
//! backdrop, the stage curtains, the poker-chip stacks, and the rug. Ported from
//! the software `ui::render_arena`, reusing its [`ArenaStyle`] palette so the two
//! renderers share one look. Real geometry lit by the same key/rim as the dice;
//! shadow maps replace the old baked contact shadows.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::render3d::color::Rgb;
use crate::ui::ArenaStyle;
use tinhorn_core::physics::{HX, HY, HZ};

/// `render3d` palette colour → Bevy sRGB colour.
fn col(c: Rgb) -> Color {
    Color::srgb_u8(c.0, c.1, c.2)
}

// Furniture tones the tray palette doesn't carry (approximating render_arena).
const RAIL: Rgb = Rgb(128, 86, 58); // lighter wood along the wall tops
const TABLE: Rgb = Rgb(92, 60, 40); // mahogany table slab
const APRON: Rgb = Rgb(74, 47, 31); // its shadowed front/side apron
const OAK: Rgb = Rgb(150, 112, 72); // bright lit-oak room floor
const RUG_BORDER: Rgb = Rgb(68, 20, 22); // deep oxblood band
const RUG_FIELD: Rgb = Rgb(112, 30, 32); // lighter crimson field
const CURTAIN: Rgb = Rgb(82, 22, 24); // heavy oxblood drape

const WALL_T: f32 = 0.35; // tray wall thickness
const FELT_TOP: f32 = -HY; // felt surface = the physics floor

/// Spawn every static piece of the arena. Called once at startup.
pub fn spawn(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    let style = ArenaStyle::DEFAULT;
    let lip = style.lip_top;

    let matte = |mats: &mut Assets<StandardMaterial>, c: Rgb, rough: f32| {
        mats.add(StandardMaterial {
            base_color: col(c),
            perceptual_roughness: rough,
            ..default()
        })
    };

    // --- Green-baize felt bed, its surface at the physics floor. ---
    let felt = matte(materials, style.floor, 0.95);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0, 0.25, HZ * 2.0))),
        MeshMaterial3d(felt),
        Transform::from_xyz(0.0, FELT_TOP - 0.125, 0.0),
    ));

    // --- Mahogany tray walls (back + two sides; front open) with wood rails. ---
    let wall = matte(materials, style.wall, 0.8);
    let rail = matte(materials, RAIL, 0.6);
    let wall_y = FELT_TOP + lip * 0.5;
    let rail_y = FELT_TOP + lip + 0.04;
    let spans: [(Vec3, Vec3); 3] = [
        // back wall (spans the full width incl. the corners)
        (
            Vec3::new(0.0, wall_y, -HZ - WALL_T * 0.5),
            Vec3::new(HX * 2.0 + WALL_T * 2.0, lip, WALL_T),
        ),
        // left wall
        (
            Vec3::new(-HX - WALL_T * 0.5, wall_y, 0.0),
            Vec3::new(WALL_T, lip, HZ * 2.0),
        ),
        // right wall
        (
            Vec3::new(HX + WALL_T * 0.5, wall_y, 0.0),
            Vec3::new(WALL_T, lip, HZ * 2.0),
        ),
    ];
    for (pos, size) in spans {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(wall.clone()),
            Transform::from_translation(pos),
        ));
        // A lighter rail sitting proud on the wall's top.
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x + 0.06, 0.12, size.z + 0.06))),
            MeshMaterial3d(rail.clone()),
            Transform::from_xyz(pos.x, rail_y, pos.z),
        ));
    }

    // --- Wood table the tray rests on: a slab with a visible apron. ---
    let table_top = FELT_TOP - 0.25;
    let table = matte(materials, TABLE, 0.7);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0 + 1.6, 0.35, HZ * 2.0 + 1.6))),
        MeshMaterial3d(table),
        Transform::from_xyz(0.0, table_top - 0.175, 0.2),
    ));
    let apron = matte(materials, APRON, 0.75);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0 + 1.6, 0.7, HZ * 2.0 + 1.6))),
        MeshMaterial3d(apron),
        Transform::from_xyz(0.0, table_top - 0.35 - 0.35, 0.2),
    ));

    // --- Dark-red casino rug: an oxblood border band under a crimson field. ---
    let rug_y = table_top - 1.05;
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0 + 6.0, 0.04, HZ * 2.0 + 5.0))),
        MeshMaterial3d(mat_unlit(materials, RUG_BORDER)),
        Transform::from_xyz(0.0, rug_y, 1.0),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0 + 4.4, 0.05, HZ * 2.0 + 3.6))),
        MeshMaterial3d(mat_unlit(materials, RUG_FIELD)),
        Transform::from_xyz(0.0, rug_y + 0.01, 1.0),
    ));

    // --- Broad room floor of lit oak, well below the rug. ---
    let floor = matte(materials, OAK, 0.85);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(46.0, 0.1, 46.0))),
        MeshMaterial3d(floor),
        Transform::from_xyz(0.0, rug_y - 0.12, -4.0),
    ));

    // --- Emissive gradient backdrop: warm at the floor seam → dim ceiling. Big
    // enough to fill the flight framing so a rolling die isn't tumbling in a
    // black void; unlit so it glows regardless of the key light's reach. ---
    let seam = OAK; // warm lit-floor tone at the horizon seam
    let ceiling = Rgb(46, 32, 26); // dim warm ceiling (not black, so the room reads)
    let grad = images.add(vertical_gradient(seam, ceiling, 96));
    let backdrop = materials.add(StandardMaterial {
        base_color_texture: Some(grad),
        unlit: true,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(70.0, 34.0))),
        MeshMaterial3d(backdrop),
        Transform::from_xyz(0.0, rug_y + 13.0, -13.0),
    ));

    // --- Heavy oxblood stage curtains flanking the backdrop. ---
    let curtain = materials.add(StandardMaterial {
        base_color: col(CURTAIN),
        perceptual_roughness: 0.9,
        ..default()
    });
    for side in [-1.0f32, 1.0] {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(3.2, 12.0, 0.4))),
            MeshMaterial3d(curtain.clone()),
            Transform::from_xyz(side * 7.5, rug_y + 6.0, -9.5),
        ));
    }

    // --- Poker-chip stacks clustered at the open-front corners. ---
    for (i, side) in [-1.0f32, 1.0].into_iter().enumerate() {
        let stacks = 2 + i; // a little variety left vs right
        for s in 0..stacks {
            let sx = side * (HX + 0.7 + s as f32 * 0.55);
            let sz = HZ + 0.5 + s as f32 * 0.3;
            let height = 4 + ((s + i) * 3) % 5; // hash-ish varied height
            for k in 0..height {
                let chip = chip_color(materials, (s + k + i) % 4);
                commands.spawn((
                    Mesh3d(meshes.add(Cylinder::new(0.34, 0.06))),
                    MeshMaterial3d(chip),
                    Transform::from_xyz(sx, FELT_TOP - 0.24 + 0.06 * k as f32, sz),
                ));
            }
        }
    }
}

/// A flat, pure-ambient tone (rug bands read as plain colour, no light streaks).
fn mat_unlit(materials: &mut Assets<StandardMaterial>, c: Rgb) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: col(c),
        unlit: true,
        ..default()
    })
}

/// One poker chip's colour, cycling the usual casino denominations.
fn chip_color(materials: &mut Assets<StandardMaterial>, idx: usize) -> Handle<StandardMaterial> {
    let c = [
        Rgb(220, 220, 224), // white
        Rgb(190, 54, 54),   // red
        Rgb(46, 92, 180),   // blue
        Rgb(40, 42, 46),    // black
    ][idx % 4];
    materials.add(StandardMaterial {
        base_color: col(c),
        perceptual_roughness: 0.5,
        ..default()
    })
}

/// Lerp two `render3d` colours in sRGB byte space.
fn lerp_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Rgb(l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

/// A `2×h` sRGB image running `bottom` (row h-1) → `top` (row 0), for the
/// backdrop's warm-seam-to-dark-ceiling gradient.
fn vertical_gradient(bottom: Rgb, top: Rgb, h: u32) -> Image {
    let w = 2u32;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let t = y as f32 / (h - 1) as f32; // 0 at the top row → 1 at the bottom
        let c = lerp_rgb(top, bottom, t);
        for _ in 0..w {
            data.extend_from_slice(&[c.0, c.1, c.2, 255]);
        }
    }
    Image::new(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}
