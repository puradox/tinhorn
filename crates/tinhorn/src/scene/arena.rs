//! The static casino-tray furniture for the Bevy arena — the felt bed, the
//! mahogany tray walls and rails, the wood table, the room floor, the gradient
//! backdrop, the stage curtains, the poker-chip stacks, and the rug. Real Bevy
//! geometry drawn from the [`ArenaStyle`] palette, lit by the same key/rim as
//! the dice; shadow maps cast the dice's contact shadows onto the felt.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::prelude::*;
use bevy::render::mesh::Indices;
use bevy::render::render_resource::{Extent3d, PrimitiveTopology, TextureDimension, TextureFormat};

use crate::paint::Rgb;
use crate::ui::ArenaStyle;
use tinhorn_core::physics::{HX, HY, HZ};

/// palette colour → Bevy sRGB colour.
fn col(c: Rgb) -> Color {
    Color::srgb_u8(c.0, c.1, c.2)
}

/// Wrap a baked procedural texture (RGBA, row-major) as a Bevy sRGB image — the
/// same procedural generators the software renderer uses, straight onto Bevy
/// materials (the plan's "wrap as `Image::new`" path).
fn tex_image(t: &crate::paint::Texture) -> Image {
    Image::new(
        Extent3d {
            width: t.width,
            height: t.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        t.data.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

/// A lit material showing a baked texture as-is (white base, so the texture's own
/// colours come through rather than being multiplied down).
fn textured(
    materials: &mut Assets<StandardMaterial>,
    image: Handle<Image>,
    rough: f32,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(image),
        perceptual_roughness: rough,
        ..default()
    })
}

/// Set an image to wrap, so a `uv_transform` scale actually tiles it instead of
/// clamping to one stretched copy.
fn repeat(mut img: Image) -> Image {
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..default()
    });
    img
}

/// A textured material whose UVs are scaled so the texture *tiles* `scale` times
/// per face (narrow grain on a wide wall, instead of one stretched-flat copy).
fn textured_tiled(
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    tex: &crate::paint::Texture,
    rough: f32,
    scale: Vec2,
) -> Handle<StandardMaterial> {
    let image = images.add(repeat(tex_image(tex)));
    materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(image),
        uv_transform: Affine2::from_scale(scale),
        perceptual_roughness: rough,
        ..default()
    })
}

// Furniture tones the tray palette doesn't carry (from the old software palette).
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

    // --- Green-baize felt bed (mottled pile + recess AO baked in), its surface
    //     at the physics floor. ---
    let felt_tex = images.add(tex_image(&crate::ui::felt_texture(style.floor)));
    let felt = textured(materials, felt_tex, 0.95);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0, 0.25, HZ * 2.0))),
        MeshMaterial3d(felt),
        Transform::from_xyz(0.0, FELT_TOP - 0.125, 0.0),
    ));

    // --- Mahogany tray walls (back + two sides; front open). Real wood grain
    //     (not a flat single tone) on the body, plus a distinctly lighter, fatter
    //     top rail proud of the wall and a lit inner face flaring up from the felt
    //     edge — so a wall reads as a solid framed lip, not a coloured slab. ---
    let wall = textured_tiled(
        materials,
        images,
        &crate::ui::grain_texture(style.wall),
        0.75,
        Vec2::new(5.0, 2.0),
    );
    let rail = textured_tiled(
        materials,
        images,
        &crate::ui::grain_texture(RAIL),
        0.5,
        Vec2::new(6.0, 1.0),
    );
    let inner = matte(materials, lerp_rgb(style.wall, RAIL, 0.4), 0.7);
    let wall_y = FELT_TOP + lip * 0.5;
    let rail_y = FELT_TOP + lip + 0.05;
    let spans: [(Vec3, Vec3, Vec3); 3] = [
        // (centre, size, inward-face offset toward the felt)
        (
            Vec3::new(0.0, wall_y, -HZ - WALL_T * 0.5),
            Vec3::new(HX * 2.0 + WALL_T * 2.0, lip, WALL_T),
            Vec3::new(0.0, 0.0, WALL_T * 0.5),
        ),
        (
            Vec3::new(-HX - WALL_T * 0.5, wall_y, 0.0),
            Vec3::new(WALL_T, lip, HZ * 2.0),
            Vec3::new(WALL_T * 0.5, 0.0, 0.0),
        ),
        (
            Vec3::new(HX + WALL_T * 0.5, wall_y, 0.0),
            Vec3::new(WALL_T, lip, HZ * 2.0),
            Vec3::new(-WALL_T * 0.5, 0.0, 0.0),
        ),
    ];
    for (pos, size, inface) in spans {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(wall.clone()),
            Transform::from_translation(pos),
        ));
        // A lit inner face flaring up from the felt edge (catches the key light).
        let inner_size = if size.x > size.z {
            Vec3::new(size.x - WALL_T, lip * 0.9, 0.06)
        } else {
            Vec3::new(0.06, lip * 0.9, size.z - WALL_T)
        };
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(inner_size.x, inner_size.y, inner_size.z))),
            MeshMaterial3d(inner.clone()),
            Transform::from_translation(pos + inface),
        ));
        // A fat, lighter rail sitting proud on the wall's top.
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x + 0.12, 0.2, size.z + 0.12))),
            MeshMaterial3d(rail.clone()),
            Transform::from_xyz(pos.x, rail_y, pos.z),
        ));
    }

    // --- Wood table the tray rests on: a slab with a visible apron, extended
    //     forward and down so its front reads below the tray's open front. ---
    let table_top = FELT_TOP - 0.2;
    let table = matte(materials, TABLE, 0.65);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0 + 2.6, 0.4, HZ * 2.0 + 2.6))),
        MeshMaterial3d(table),
        Transform::from_xyz(0.0, table_top - 0.2, 0.7),
    ));
    let apron = matte(materials, APRON, 0.7);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(HX * 2.0 + 2.6, 1.0, HZ * 2.0 + 2.6))),
        MeshMaterial3d(apron),
        Transform::from_xyz(0.0, table_top - 0.9, 0.7),
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

    // --- Broad room floor of oak floorboards, well below the rug. Unlit (like
    //     the software floor's pure-ambient treatment) so it stays a genuinely
    //     bright oak at every angle. The plank texture is *tiled* (narrow planks
    //     running front-to-back) with a repeat sampler + a UV scale — not
    //     stretched once across the whole floor, which made each plank huge. ---
    let mut floor_img = tex_image(&crate::ui::floor_texture(OAK));
    floor_img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..default()
    });
    let floor_tex = images.add(floor_img);
    let floor = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(floor_tex),
        // More repeats across (narrow planks) than along (long boards front-to-back).
        uv_transform: Affine2::from_scale(Vec2::new(16.0, 5.0)),
        unlit: true,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(64.0, 0.1, 64.0))),
        MeshMaterial3d(floor),
        Transform::from_xyz(0.0, rug_y - 0.12, -8.0),
    ));

    // --- Textured room back wall: the software renderer's baked backdrop (a warm
    // vertical gradient — bright at the floor seam → dim ceiling — with a
    // wainscot/chair-rail band and a panelled tone below it). Big enough to fill
    // the flight framing so a rolling die isn't tumbling in a black void; unlit so
    // it glows regardless of the key light's reach. Tiled a couple of times across
    // so the wainscot panels read at a sensible width on the wide wall. ---
    let mut wall_img = repeat(tex_image(&crate::ui::backdrop_texture()));
    wall_img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::ClampToEdge, // keep the gradient/wainscot vertical layout
        ..default()
    });
    let backdrop = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(images.add(wall_img)),
        uv_transform: Affine2::from_scale(Vec2::new(4.0, 1.0)),
        unlit: true,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(100.0, 42.0))),
        MeshMaterial3d(backdrop),
        Transform::from_xyz(0.0, rug_y + 16.0, -18.0),
    ));

    // --- Heavy oxblood stage curtains flanking the backdrop: real corrugated
    //     fold geometry (the key/rim light shades the folds), hanging free from
    //     the floor to well above the frame with a velvet streak texture. ---
    let velvet = images.add(tex_image(&crate::ui::velvet_texture(CURTAIN)));
    let curtain = textured(materials, velvet, 0.92);
    // Close in and tall/wide, so the drapes fill the sides of the frame from the
    // floor to above the top — a proscenium framing the tray. Extended well past
    // the frame edge in x so the outer end never shows.
    for side in [-1.0f32, 1.0] {
        commands.spawn((
            Mesh3d(meshes.add(curtain_mesh(16.0, 30.0, 14))),
            MeshMaterial3d(curtain.clone()),
            Transform::from_xyz(side * 10.5, rug_y + 13.0, -6.0),
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
        perceptual_roughness: 0.42,
        reflectance: 0.55,
        ..default()
    })
}

/// A free-hanging stage-curtain panel: a heavy velvet drape `w` wide × `h` tall,
/// facing +Z. A single clean cosine corrugation reads as a machined washboard, so
/// the fold profile layers three *incommensurate* waves — the pleats never line
/// up into a regular pattern, varying in width and depth like gathered fabric —
/// and the amplitude swells toward the floor so the drape splays as it falls, with
/// a gentle forward belly. Normals come from the profile's own slope (finite
/// differences in both directions), so the key/rim light pools in the valleys and
/// catches the crests instead of flat-shading a slab.
fn curtain_mesh(w: f32, h: f32, folds: u32) -> Mesh {
    let cols = folds * 8; // facets across the width — enough to round each pleat
    let rows = 24u32; // segments down the drop, for a smooth swell
    let tau = std::f32::consts::TAU;
    let f = folds as f32;

    // The fold depth at (u across, v down): three waves at unrelated frequencies
    // so the pleats stay irregular, swelling deeper toward the floor.
    let profile = |u: f32, v: f32| -> f32 {
        let amp = 0.5 * (0.6 + 0.7 * v); // shallow gather up top → deep splay below
        let a = (u * f * tau).sin();
        let b = (u * f * 2.3 * tau + 1.7).sin();
        let c = (u * f * 0.55 * tau + 4.1).sin();
        amp * (0.62 * a + 0.26 * b + 0.34 * c)
    };
    // A gentle forward bow so heavy fabric bellies toward the room as it hangs,
    // rather than falling as a flat plane.
    let belly = |v: f32| -> f32 { 0.6 * v * v };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let eps = 1.0e-3;
    for r in 0..=rows {
        let v = r as f32 / rows as f32; // 0 top → 1 bottom
        let y = (0.5 - v) * h;
        for c in 0..=cols {
            let u = c as f32 / cols as f32;
            let x = (u - 0.5) * w;
            let z = profile(u, v) + belly(v);
            positions.push([x, y, z]);
            uvs.push([u, v]);
            // Surface slope in u and v → normal facing +Z (the room).
            let dz_du = (profile(u + eps, v) - profile(u - eps, v)) / (2.0 * eps);
            let dz_dv = (profile(u, v + eps) - profile(u, v - eps) + belly(v + eps)
                - belly(v - eps))
                / (2.0 * eps);
            let tu = Vec3::new(w, 0.0, dz_du);
            let tv = Vec3::new(0.0, -h, dz_dv);
            let n = tv.cross(tu).normalize();
            normals.push([n.x, n.y, n.z]);
        }
    }
    let stride = cols + 1;
    for r in 0..rows {
        for c in 0..cols {
            let i = r * stride + c;
            indices.extend_from_slice(&[i, i + stride, i + 1, i + 1, i + stride, i + stride + 1]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// Lerp two palette colours in sRGB byte space.
fn lerp_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Rgb(l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}
