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

use tinhorn_core::{dice_geom, parse, physics, view_math};

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
    // The spike shows fixed poses rather than a seeded animated roll, so the seed
    // isn't consumed yet; kept in the signature for when the sim drives the scene.
    let _ = seed;

    // Headless self-validation: `TINHORN_BEVY_SNAPSHOT=<path>` renders the same
    // scene to a PNG instead of the terminal, then exits. No RatatuiContext, so
    // no TTY is required — this is how the render is checked in CI or from a
    // non-interactive shell (open the PNG to eyeball the dice, felt, and shadows).
    if let Some(path) = std::env::var_os("TINHORN_BEVY_SNAPSHOT") {
        run_snapshot(&expr, PathBuf::from(path));
    } else {
        run_interactive(&expr);
    }
}

/// The scene, minus the presentation layer: the headless render plugins plus the
/// `setup` system, shared by the interactive and snapshot paths. `bevy_window`
/// is pulled in transitively by the render features, so DefaultPlugins carries a
/// WindowPlugin (and, without winit, no loop driver). Render headless — no
/// primary window, and don't exit just because there are none — and drive the
/// update loop ourselves at ~60 fps.
fn base_app(expr: &str) -> App {
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
    .insert_resource(DiceList(dice_from_expr(expr)))
    .init_resource::<ArenaImage>()
    .add_systems(Startup, setup);
    app
}

/// The real path: terminal context + input + per-frame blit.
fn run_interactive(expr: &str) {
    let mut app = base_app(expr);
    app.add_plugins(RatatuiPlugins::default())
        .add_systems(Update, (handle_input, draw_arena))
        .run();
}

/// The validation path: render a few frames, dump the arena image to `path`, exit.
fn run_snapshot(expr: &str, path: PathBuf) {
    let mut app = base_app(expr);
    app.insert_resource(Snapshot { path, frames: 0 })
        .add_systems(Update, save_snapshot)
        .run();
}

/// One die per entry, sides parsed from the expression (each term contributes
/// `count` dice); falls back to a sampler of all six solids. Capped so the row
/// of dice stays legible in the spike.
fn dice_from_expr(expr: &str) -> Vec<u32> {
    let mut sides = Vec::new();
    if let Ok(roll) = parse::parse(expr) {
        for term in &roll.terms {
            for _ in 0..term.count {
                sides.push(term.sides);
            }
        }
    }
    if sides.is_empty() {
        sides = vec![20, 12, 10, 8, 6, 4];
    }
    sides.truncate(8);
    sides
}

#[derive(Resource)]
struct DiceList(Vec<u32>);

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
    dice: Res<DiceList>,
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

    // 6. The dice, in a row on the felt at fixed, slightly-tumbled poses.
    let n = dice.0.len().max(1);
    for (i, &sides) in dice.0.iter().enumerate() {
        let mesh = meshes.add(convert::dice_mesh(&dice_geom::mesh_for(sides)));
        let material = materials.add(StandardMaterial {
            base_color: Color::srgb(0.92, 0.90, 0.84),
            perceptual_roughness: 0.4,
            ..default()
        });
        let t = if n == 1 {
            0.5
        } else {
            i as f32 / (n as f32 - 1.0)
        };
        let x = (t - 0.5) * (physics::HX * 1.4);
        let z = ((i % 2) as f32 - 0.5) * physics::HZ * 0.5;
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_xyz(x, -physics::HY + physics::DIE_R, z)
                .with_scale(Vec3::splat(physics::DIE_R))
                .with_rotation(Quat::from_rotation_y(i as f32 * 0.7) * Quat::from_rotation_x(0.4)),
        ));
    }
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

/// Wait for the render and the async GPU readback to settle, write the arena
/// image to a PNG, and exit. The validation counterpart of [`draw_arena`].
fn save_snapshot(
    mut snapshot: ResMut<Snapshot>,
    arena: Res<ArenaImage>,
    mut exit: MessageWriter<AppExit>,
) {
    snapshot.frames += 1;

    // The first readback is a few frames out (render → copy → map_async → the
    // next extract triggers ReadbackComplete); wait for pixels AND a short warmup.
    let ready = !arena.pixels.is_empty();
    if snapshot.frames < 16 || !ready {
        if snapshot.frames > 300 {
            eprintln!("tinhorn: no frame read back after 300 frames; giving up");
            exit.write_default();
        }
        return;
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
