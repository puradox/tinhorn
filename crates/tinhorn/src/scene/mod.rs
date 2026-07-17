//! Stage-2 Bevy visual spike: a handful of dice, the shared arena camera, and a
//! shadow-casting light, rendered headless by Bevy into an offscreen image,
//! read back to the CPU with Bevy 0.19's built-in `gpu_readback`, and blitted
//! into the terminal as half-blocks through `bevy_ratatui`'s `RatatuiContext`.
//!
//! This is deliberately minimal — fixed dice poses, one HalfBlock strategy, no
//! chrome — its only job is to prove the pipeline end to end (headless Bevy →
//! texture → CPU → ratatui) and be the tuning surface for later parity work. It
//! reuses the *same* dice geometry ([`dice_geom`]) and camera framing
//! ([`view_math`]) as the shipping software renderer, so what it draws is the
//! real arena's dice, not stand-ins. Everything here is compiled only under the
//! `bevy` feature; nothing on the one-shot CLI path can reach it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::window::{ExitCondition, WindowPlugin};
use bevy_ratatui::event::KeyMessage;
use bevy_ratatui::{RatatuiContext, RatatuiPlugins};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Color as TColor, Style};

use tinhorn_core::app::{App as DiceApp, Die, crit_face, fumble_face};
use tinhorn_core::{dice_geom, physics, view_math};

mod convert;

/// Offscreen render-target resolution. A wide-ish 8:5 so the arena framing (which
/// derives its distance from the view aspect) matches a typical terminal; the
/// blit downsamples this to whatever cell grid the terminal offers.
const RENDER_W: u32 = 480;
const RENDER_H: u32 = 300;

/// wgpu requires texture→buffer copies to pad each row to this many bytes, and
/// Bevy's readback hands back the *padded* buffer verbatim — so the blit reads
/// with this stride, not `width * 4`.
const ROW_ALIGN: usize = 256;

/// Build and run the Bevy arena. Blocks until the user quits (`q`/Esc). Only
/// called from the interactive path, never one-shot, so entering raw mode and
/// initialising a GPU here can't affect scripting.
pub fn run(expr: String, seed: Option<u64>) {
    // Headless self-validation: `TINHORN_BEVY_SNAPSHOT=<path>` renders the same
    // scene to a PNG instead of the terminal, then exits. No RatatuiContext, so
    // no TTY is required — this is how the render is checked in CI or from a
    // non-interactive shell (open the PNG to eyeball the dice, felt, and shadows).
    if let Some(path) = std::env::var_os("TINHORN_BEVY_SNAPSHOT") {
        run_snapshot(&expr, seed, PathBuf::from(path));
    } else {
        run_interactive(&expr, seed);
    }
}

/// The scene, minus the presentation layer: the headless render plugins, the
/// core sim as the single source of truth, and the systems that advance it and
/// mirror its dice into Bevy entities. Shared by the interactive and snapshot
/// paths. `bevy_window` is pulled in transitively by the render features, so
/// DefaultPlugins carries a WindowPlugin (and, without winit, no loop driver):
/// render headless — no primary window, and don't exit just because there are
/// none — and drive the update loop ourselves at ~60 fps.
fn base_app(expr: &str, seed: Option<u64>) -> App {
    // `App` (the core sim) owns the dice and the physics; the Bevy entities are a
    // pure view of it, so all roll logic and the seed contract stay in core.
    let mut sim = match seed {
        Some(s) => DiceApp::with_seed(expr.to_string(), s),
        None => DiceApp::new(expr.to_string()),
    };
    // The sim's arena size feeds its launch/particle geometry; pick a cell grid
    // whose aspect matches the render target so throws arc through the frame.
    sim.arena_w = 64.0;
    sim.arena_h = 20.0;
    sim.roll(); // kick off an animated roll so the dice tumble on screen

    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins.set(WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            ..default()
        }),
        ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0)),
    ))
    .insert_resource(ClearColor(Color::srgb(0.015, 0.02, 0.03)))
    .insert_resource(Sim(sim))
    .init_resource::<ArenaImage>()
    .add_systems(Startup, setup)
    .add_systems(Update, (advance_sim, sync_dice_scene).chain());
    app
}

/// The real path: terminal context + input + per-frame blit.
fn run_interactive(expr: &str, seed: Option<u64>) {
    let mut app = base_app(expr, seed);
    app.add_plugins(RatatuiPlugins::default())
        .add_systems(Update, (handle_input, draw_arena))
        .run();
}

/// The validation path: render until the roll settles, dump a PNG, exit.
fn run_snapshot(expr: &str, seed: Option<u64>, path: PathBuf) {
    let mut app = base_app(expr, seed);
    app.insert_resource(Snapshot { path, frames: 0 })
        .add_systems(Update, save_snapshot)
        .run();
}

/// The core sim — the single source of truth. Bevy reads it; only the input
/// system writes it (a later stage).
#[derive(Resource)]
struct Sim(DiceApp);

/// Tags a Bevy entity as the view of `sim.0.dice[index]`.
#[derive(Component)]
struct DieView(usize);

/// The CPU-side copy of the rendered arena, refreshed every frame by the readback
/// observer. `pixels` is row-padded RGBA8 (see [`ROW_ALIGN`]).
#[derive(Resource, Default)]
struct ArenaImage {
    handle: Handle<Image>,
    pixels: Vec<u8>,
}

fn setup(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut arena: ResMut<ArenaImage>,
) {
    // 1. The image the camera renders into and we read back each frame.
    let size = Extent3d {
        width: RENDER_W,
        height: RENDER_H,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::all(),
    );
    image.texture_descriptor.usage |=
        TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    let handle = images.add(image);
    arena.handle = handle.clone();

    // 2. Camera, posed by the shared core framing (settled overhead read), aimed
    //    at the offscreen image. Same fov the framing math assumes.
    let aspect = RENDER_W as f32 / RENDER_H as f32;
    let cam = view_math::arena_camera(glam::Vec3::ZERO, aspect, 1.0);
    commands.spawn((
        Camera3d::default(),
        // In Bevy 0.19 the camera's destination is a `RenderTarget` component,
        // not a `Camera` field.
        RenderTarget::from(handle.clone()),
        Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_4,
            ..default()
        }),
        Transform::from_translation(convert::vec3(cam.position))
            .looking_at(convert::vec3(cam.target), Vec3::Y),
        // AmbientLight is a per-camera component in 0.19.
        AmbientLight {
            color: Color::srgb(0.6, 0.7, 0.9),
            brightness: 220.0,
            ..default()
        },
    ));

    // 3. Read the render target back to the CPU every frame (Bevy 0.19 built-in).
    commands
        .spawn(Readback::texture(handle))
        .observe(on_readback);

    // 4. Warm key light with shadow maps (the payoff over the old baked contact
    //    shadows), a cool rim, and a little fill (ambient sits on the camera).
    commands.spawn((
        PointLight {
            color: Color::srgb(1.0, 0.86, 0.66),
            intensity: 5_000_000.0,
            range: 60.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(0.8, physics::HY + 3.0, physics::HZ * 0.5),
    ));
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.55, 0.65, 1.0),
            illuminance: 3000.0,
            ..default()
        },
        Transform::from_xyz(-2.0, 3.0, -3.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // 5. Green-baize felt floor: a thin slab set at the tray bottom.
    let felt = materials.add(StandardMaterial {
        base_color: Color::srgb(0.05, 0.30, 0.12),
        perceptual_roughness: 0.95,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(physics::HX * 2.0, 0.3, physics::HZ * 2.0))),
        MeshMaterial3d(felt),
        Transform::from_xyz(0.0, -physics::HY - 0.15, 0.0),
    ));

    // The dice themselves are spawned and posed by `sync_dice_scene` from the sim.
}

/// Step the core sim by real elapsed time; its fixed-step accumulator keeps the
/// physics deterministic regardless of Bevy's frame pacing.
fn advance_sim(time: Res<Time>, mut sim: ResMut<Sim>) {
    sim.0.update(time.delta_secs());
}

/// Mirror `sim.0.dice` into the scene: spawn a mesh+material per new die, copy
/// each existing view's pose and colour every frame, and despawn any view whose
/// die has been cleared.
fn sync_dice_scene(
    mut commands: Commands,
    sim: Res<Sim>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut views: Query<(
        Entity,
        &DieView,
        &mut Transform,
        &MeshMaterial3d<StandardMaterial>,
    )>,
) {
    let dice = &sim.0.dice;
    let mut has_view = vec![false; dice.len()];

    for (entity, view, mut transform, material) in &mut views {
        match dice.get(view.0) {
            Some(die) => {
                *transform = die_transform(die);
                if let Some(mut mat) = materials.get_mut(&material.0) {
                    mat.base_color = die_color(die);
                }
                has_view[view.0] = true;
            }
            None => {
                commands.entity(entity).despawn();
            }
        }
    }

    for (i, die) in dice.iter().enumerate() {
        if has_view[i] {
            continue;
        }
        let mesh = meshes.add(convert::dice_mesh(&dice_geom::mesh_for(die.sides)));
        let material = materials.add(StandardMaterial {
            base_color: die_color(die),
            perceptual_roughness: 0.4,
            ..default()
        });
        commands.spawn((
            DieView(i),
            Mesh3d(mesh),
            MeshMaterial3d(material),
            die_transform(die),
        ));
    }
}

/// World transform for a die: its sim pose, scaled to the die's world radius.
fn die_transform(die: &Die) -> Transform {
    Transform::from_translation(convert::vec3(die.pos))
        .with_rotation(convert::quat(die.rot))
        .with_scale(Vec3::splat(physics::DIE_R))
}

/// Die colour: dropped dice grey out, a settled crit burns gold and a fumble red,
/// otherwise a bone ivory faintly tinted per term so multiple terms read apart.
fn die_color(die: &Die) -> Color {
    if !die.kept {
        return Color::srgb(0.34, 0.34, 0.31);
    }
    if die.settled && crit_face(die.sides, die.final_value) {
        return Color::srgb(1.0, 0.84, 0.28);
    }
    if die.settled && fumble_face(die.sides, die.final_value) {
        return Color::srgb(0.82, 0.22, 0.2);
    }
    let tints = [
        Color::srgb(0.92, 0.90, 0.84),
        Color::srgb(0.85, 0.89, 0.92),
        Color::srgb(0.92, 0.87, 0.85),
    ];
    tints[die.color_idx % tints.len()]
}

/// Copy each completed GPU readback into the CPU-side arena image.
fn on_readback(readback: On<ReadbackComplete>, mut arena: ResMut<ArenaImage>) {
    arena.pixels = readback.event().data.clone();
}

/// Quit on `q` or Esc (raw mode swallows Ctrl-C's default handling).
fn handle_input(mut keys: MessageReader<KeyMessage>, mut exit: MessageWriter<AppExit>) {
    for key in keys.read() {
        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
            exit.write_default();
        }
    }
}

/// Paint the latest readback into the terminal each frame.
fn draw_arena(mut context: ResMut<RatatuiContext>, arena: Res<ArenaImage>) -> Result {
    context.draw(|frame| {
        let area = frame.area();
        blit_half_blocks(&arena.pixels, RENDER_W, RENDER_H, frame.buffer_mut(), area);
    })?;
    Ok(())
}

/// Downsample the row-padded RGBA8 render into terminal half-block cells (fg =
/// upper pixel, bg = lower). A no-op until the first readback lands.
fn blit_half_blocks(pixels: &[u8], iw: u32, ih: u32, buf: &mut Buffer, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let stride = aligned_row_bytes(iw);
    if pixels.len() < stride * ih as usize {
        return; // no frame read back yet
    }
    let sample = |x: u32, y: u32| -> (u8, u8, u8) {
        let x = x.min(iw - 1) as usize;
        let y = y.min(ih - 1) as usize;
        let i = y * stride + x * 4;
        (pixels[i], pixels[i + 1], pixels[i + 2])
    };
    for row in 0..area.height {
        for col in 0..area.width {
            let ix = ((col as f32 + 0.5) / area.width as f32 * iw as f32) as u32;
            let iy_upper =
                ((row as f32 * 2.0 + 0.5) / (area.height as f32 * 2.0) * ih as f32) as u32;
            let iy_lower =
                ((row as f32 * 2.0 + 1.5) / (area.height as f32 * 2.0) * ih as f32) as u32;
            let up = sample(ix, iy_upper);
            let lo = sample(ix, iy_lower);
            let cell = &mut buf[(area.x + col, area.y + row)];
            cell.set_char('▀');
            cell.set_style(
                Style::default()
                    .fg(TColor::Rgb(up.0, up.1, up.2))
                    .bg(TColor::Rgb(lo.0, lo.1, lo.2)),
            );
        }
    }
}

/// Bytes per padded image row (wgpu's 256-byte copy alignment).
fn aligned_row_bytes(width: u32) -> usize {
    let bytes = width as usize * 4;
    bytes.div_ceil(ROW_ALIGN) * ROW_ALIGN
}

/// Where to write the headless snapshot, and how many frames have elapsed.
#[derive(Resource)]
struct Snapshot {
    path: PathBuf,
    frames: u32,
}

/// Wait for the readback to land AND the roll to come to rest, then write the
/// arena image to a PNG and exit. The validation counterpart of [`draw_arena`];
/// snapshotting the *settled* roll makes the PNG a stable regression reference.
fn save_snapshot(
    mut snapshot: ResMut<Snapshot>,
    arena: Res<ArenaImage>,
    sim: Res<Sim>,
    mut exit: MessageWriter<AppExit>,
) {
    snapshot.frames += 1;

    // The first readback is a few frames out (render → copy → map_async → the
    // next extract triggers ReadbackComplete). Wait for a warmup, a landed
    // readback, and the dice to settle — or a hard frame cap so it can't hang.
    let ready = !arena.pixels.is_empty();
    let done = snapshot.frames >= 16 && ready && sim.0.all_settled();
    if !done && snapshot.frames < 600 {
        return;
    }
    if snapshot.frames >= 600 {
        eprintln!("tinhorn: roll didn't settle in 600 frames; snapshotting anyway");
    }

    match save_png(&arena.pixels, RENDER_W, RENDER_H, &snapshot.path) {
        Ok(()) => eprintln!("tinhorn: wrote snapshot {}", snapshot.path.display()),
        Err(err) => eprintln!("tinhorn: failed to write snapshot: {err}"),
    }
    exit.write_default();
}

/// Strip the readback's per-row padding into tight RGBA8 and encode a PNG.
fn save_png(padded: &[u8], w: u32, h: u32, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let stride = aligned_row_bytes(w);
    let row = w as usize * 4;
    let mut rgba = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        rgba.extend_from_slice(&padded[y * stride..y * stride + row]);
    }
    image::save_buffer(path, &rgba, w, h, image::ExtendedColorType::Rgba8)?;
    Ok(())
}
