//! The Bevy dice arena — the default interactive renderer.
//!
//! `App` (the core sim) is the single source of truth; the Bevy entities are a
//! pure view of it (Architecture decision 2). Each frame the input system feeds
//! keys to the shared `handle_key`, `advance_sim` steps the physics,
//! `sync_dice_scene` mirrors `app.dice` into `DieView` entities, the camera and
//! lights choreograph off the sim's envelopes, and `draw_ui` composes the CPU
//! read-back of the Bevy render (blitted as half-blocks) with all of tinhorn's
//! ratatui chrome via [`ui::render_bevy`]. The render target autoresizes to the
//! arena panel so the blit is 1:1 and the burned-number overlays land on their
//! dice.
//!
//! Only the interactive/snapshot paths reach here; the one-shot CLI never
//! constructs a Bevy `App`, so scripting stays GPU-free.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::asset::RenderAssetUsages;
use bevy::camera::{Hdr, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::pbr::{DistanceFog, FogFalloff, ScreenSpaceAmbientOcclusion};
use bevy::prelude::*;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::Msaa;
use bevy::window::{ExitCondition, WindowPlugin};
use bevy_ratatui::event::KeyMessage;
use bevy_ratatui::{RatatuiContext, RatatuiPlugins};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyEventKind, KeyModifiers};
use ratatui::style::Color as TColor;

use tinhorn_core::app::{App as DiceApp, Die, SoundEvent, crit_face, fumble_face};
use tinhorn_core::{dice_geom, physics, view_math};

use crate::foley::Foley;
use crate::{Action, handle_key, ui};

mod arena;
mod convert;

/// Impact/knock sounds voiced per frame; more is mush (mirrors the legacy loop).
const MAX_CLICKS_PER_FRAME: usize = 8;

/// Fixed terminal size the headless snapshot composes into (cols × rows).
const SNAP_COLS: u16 = 100;
const SNAP_ROWS: u16 = 38;

/// Entry point (interactive or headless snapshot). Only called off the
/// interactive CLI path, never one-shot.
pub fn run(expr: String, seed: Option<u64>, muted: bool) {
    if let Some(path) = std::env::var_os("TINHORN_BEVY_SNAPSHOT") {
        run_snapshot(&expr, seed, muted, PathBuf::from(path));
    } else {
        run_interactive(&expr, seed, muted);
    }
}

/// The scene shared by both paths: headless render plugins, the sim, and the
/// systems that step it and mirror it into the scene. `bevy_window` is enabled
/// transitively, so DefaultPlugins carries a WindowPlugin and no loop driver —
/// render headless (no primary window, don't exit when there are none) and drive
/// the loop ourselves at ~60 fps.
fn base_app(expr: &str, seed: Option<u64>, muted: bool) -> App {
    let mut sim = match seed {
        Some(s) => DiceApp::with_seed(expr.to_string(), s),
        None => DiceApp::new(expr.to_string()),
    };
    sim.muted = muted;
    // `App::with_seed`/`new` already roll a non-empty expression on construction
    // (exactly as the legacy TUI's `-- 3d6` does), consuming the seed once — so we
    // must NOT roll again here, or the Bevy path would diverge from `evaluate`.
    // A sensible arena size until the first frame reports the real one.
    sim.arena_w = 64.0;
    sim.arena_h = 20.0;

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
    .insert_resource(ArenaView { w: 0, h: 0 })
    .init_resource::<ArenaImage>()
    .add_systems(Startup, setup)
    .add_systems(
        Update,
        (
            resize_arena,
            advance_sim,
            sync_dice_scene,
            sync_cup,
            choreograph,
        )
            .chain(),
    );
    app
}

/// The interactive path: terminal context, input, per-frame compose, sound.
fn run_interactive(expr: &str, seed: Option<u64>, muted: bool) {
    let mut app = base_app(expr, seed, muted);
    app.add_plugins(RatatuiPlugins::default())
        .insert_resource(Sound(None))
        .add_systems(PreUpdate, input_system)
        .add_systems(Update, (draw_ui, drain_sounds).chain().after(choreograph))
        .run();
}

/// The validation path: render until the roll settles, dump a full-frame PNG.
/// `TINHORN_SNAP_COLS`/`TINHORN_SNAP_ROWS` override the composed terminal size
/// (bigger reads more detail into the PNG).
fn run_snapshot(expr: &str, seed: Option<u64>, muted: bool, path: PathBuf) {
    let dim = |key: &str, default: u16| -> u16 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let mut app = base_app(expr, seed, muted);
    app.insert_resource(Snapshot {
        path,
        frames: 0,
        cols: dim("TINHORN_SNAP_COLS", SNAP_COLS),
        rows: dim("TINHORN_SNAP_ROWS", SNAP_ROWS),
        // `TINHORN_SNAP_FRAME=N` captures at frame N (mid-roll / establishing
        // framing) instead of waiting for the roll to settle.
        at_frame: std::env::var("TINHORN_SNAP_FRAME")
            .ok()
            .and_then(|v| v.parse().ok()),
    })
    .add_systems(Update, save_snapshot.after(choreograph))
    .run();
}

/// The core sim — the single source of truth. Only the input system writes it.
#[derive(Resource)]
struct Sim(DiceApp);

/// Tags a Bevy entity as the view of `sim.0.dice[index]`.
#[derive(Component)]
struct DieView(usize);

/// Marks the arena camera (its transform choreographs; its target resizes).
#[derive(Component)]
struct ArenaCamera;

/// Marks the tin cup (shown only while shaking).
#[derive(Component)]
struct CupView;

/// The lazily-spawned audio player; `None` until the first audible sound.
#[derive(Resource)]
struct Sound(Option<Foley>);

/// The arena panel's desired render size in pixels (cols × rows*2), reported by
/// the compose step; the render target tracks it so the blit is 1:1.
#[derive(Resource)]
struct ArenaView {
    w: u32,
    h: u32,
}

/// The CPU-side copy of the rendered arena, refreshed by the readback observer.
/// `pixels` is row-padded RGBA8 for a `w`×`h` image (see [`aligned_row_bytes`]).
#[derive(Resource, Default)]
struct ArenaImage {
    handle: Handle<Image>,
    pixels: Vec<u8>,
    w: u32,
    h: u32,
}

/// A render-target image of `w`×`h`, usable as a camera target and read back.
fn arena_image(w: u32, h: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::all(),
    );
    image.texture_descriptor.usage |=
        TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    image
}

fn setup(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut arena: ResMut<ArenaImage>,
) {
    // Render target the camera draws into and we read back each frame.
    let (w, h) = (SNAP_COLS as u32, SNAP_ROWS as u32 * 2);
    let handle = images.add(arena_image(w, h));
    arena.handle = handle.clone();
    arena.w = w;
    arena.h = h;

    // Camera → image, posed for now by the settled framing; `choreograph` moves it.
    let aspect = w as f32 / h as f32;
    let cam = view_math::arena_camera(glam::Vec3::ZERO, aspect, 1.0);
    commands.spawn((
        ArenaCamera,
        Camera3d::default(),
        RenderTarget::from(handle.clone()),
        // Realism the CPU rasterizer never had: render HDR and tonemap it
        // filmically (highlights roll off instead of clipping), 4× MSAA for clean
        // edges, and screen-space ambient occlusion to ground the dice in the
        // felt and darken the tray's inner corners.
        Hdr,
        Tonemapping::TonyMcMapface,
        Msaa::Sample4,
        ScreenSpaceAmbientOcclusion::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_4,
            ..default()
        }),
        Transform::from_translation(convert::vec3(cam.position))
            .looking_at(convert::vec3(cam.target), Vec3::Y),
        // Low, slightly cool ambient so the warm key light can carve a spotlit
        // pool on the felt (the drama the software renderer gets from per-fragment
        // falloff). The room stays visible because its floor/backdrop are *unlit*,
        // not because ambient floods everything flat.
        AmbientLight {
            color: Color::srgb(0.42, 0.48, 0.62),
            brightness: 70.0,
            ..default()
        },
        // Recede far geometry into the room; a warm colour so the floor's far
        // reaches and the drapes ease into the room rather than into black. Start
        // pushed past the mid-ground so the felt, dice, and near room stay crisp.
        DistanceFog {
            color: Color::srgb_u8(40, 28, 22),
            falloff: FogFalloff::Linear {
                start: 22.0,
                end: 55.0,
            },
            ..default()
        },
    ));

    // Read the render target back to the CPU each frame (Bevy 0.19 built-in).
    commands
        .spawn(Readback::texture(handle))
        .observe(on_readback);

    // Warm key light with shadow maps, a cool rim.
    commands.spawn((
        ArenaKeyLight,
        PointLight {
            color: Color::srgb(1.0, 0.86, 0.66),
            intensity: 9_000_000.0,
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

    // The static casino-tray furniture: felt bed, mahogany walls + rails, the
    // wood table + apron, the rug, the room floor, the gradient backdrop, the
    // curtains, and the poker-chip stacks.
    arena::spawn(&mut commands, &mut meshes, &mut materials, &mut images);

    // The tin cup, hidden until a shake begins.
    let cup_mesh = meshes.add(convert::dice_mesh(&dice_geom::cup()));
    let cup_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.72, 0.74, 0.78),
        metallic: 0.6,
        perceptual_roughness: 0.35,
        ..default()
    });
    commands.spawn((
        CupView,
        Mesh3d(cup_mesh),
        MeshMaterial3d(cup_mat),
        Transform::from_xyz(0.0, -physics::HY + 0.7, physics::HZ * 0.2)
            .with_scale(Vec3::splat(1.1)),
        Visibility::Hidden,
    ));
}

/// Marks the warm key light so its intensity can flinch on hard impacts.
#[derive(Component)]
struct ArenaKeyLight;

/// Feed keys to the shared, pure `handle_key`; quit on its `Quit` verdict.
fn input_system(
    mut keys: MessageReader<KeyMessage>,
    mut sim: ResMut<Sim>,
    mut exit: MessageWriter<AppExit>,
) {
    for key in keys.read() {
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if handle_key(&mut sim.0, key.code, ctrl) == Action::Quit {
            exit.write_default();
        }
    }
}

/// Step the core sim by real elapsed time; its fixed-step accumulator keeps the
/// physics deterministic regardless of Bevy's frame pacing.
fn advance_sim(time: Res<Time>, mut sim: ResMut<Sim>) {
    sim.0.update(time.delta_secs());
}

/// Resize the render target (and repoint the camera + readback) when the arena
/// panel's size changes, so the blit stays 1:1 and overlays align.
fn resize_arena(
    view: Res<ArenaView>,
    mut arena: ResMut<ArenaImage>,
    mut images: ResMut<Assets<Image>>,
    mut camera: Query<&mut RenderTarget, With<ArenaCamera>>,
    mut readback: Query<&mut Readback>,
) {
    if view.w == 0 || (view.w == arena.w && view.h == arena.h) {
        return;
    }
    let old = arena.handle.clone();
    let handle = images.add(arena_image(view.w, view.h));
    if let Ok(mut target) = camera.single_mut() {
        *target = RenderTarget::from(handle.clone());
    }
    if let Ok(mut rb) = readback.single_mut() {
        *rb = Readback::texture(handle.clone());
    }
    images.remove(&old);
    arena.handle = handle;
    arena.w = view.w;
    arena.h = view.h;
    arena.pixels.clear(); // stale for the new size; wait for the next readback
}

/// Mirror `sim.0.dice` into the scene: spawn a mesh+material per new die, copy
/// each existing view's pose and colour every frame, despawn cleared ones.
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
            None => commands.entity(entity).despawn(),
        }
    }

    for (i, die) in dice.iter().enumerate() {
        if has_view[i] {
            continue;
        }
        let mesh = meshes.add(convert::dice_mesh(&dice_geom::mesh_for(die.sides)));
        let material = materials.add(StandardMaterial {
            base_color: die_color(die),
            perceptual_roughness: 0.28,
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

/// Show the cup only while shaking; sway it on the shared shake clock.
fn sync_cup(sim: Res<Sim>, mut cup: Query<(&mut Visibility, &mut Transform), With<CupView>>) {
    let Ok((mut visibility, mut transform)) = cup.single_mut() else {
        return;
    };
    if sim.0.shaking() {
        *visibility = Visibility::Visible;
        let sway = sim.0.cup_offset() * physics::HX * 0.6;
        *transform = Transform::from_xyz(sway, -physics::HY + 0.7, physics::HZ * 0.2)
            .with_scale(Vec3::splat(1.1));
    } else {
        *visibility = Visibility::Hidden;
    }
}

/// Move the arena camera each frame to match the shared `live_camera` framing (so
/// the burned-number overlays, projected through the same call, stay in register),
/// and flinch the key light on hard impacts / a crit flare.
fn choreograph(
    sim: Res<Sim>,
    mut camera: Query<&mut Transform, With<ArenaCamera>>,
    mut key_light: Query<&mut PointLight, With<ArenaKeyLight>>,
) {
    let app = &sim.0;
    let aspect = view_math::arena_aspect(app.arena_w, app.arena_h);
    let cam = view_math::live_camera(
        app.camera_shake(),
        aspect,
        app.focus(),
        app.clock(),
        app.flash(),
    );
    if let Ok(mut transform) = camera.single_mut() {
        *transform = Transform::from_translation(convert::vec3(cam.position))
            .looking_at(convert::vec3(cam.target), Vec3::Y);
    }
    if let Ok(mut light) = key_light.single_mut() {
        // Brighten with hard-impact energy and a crit flare.
        let boost = 1.0 + app.impact_energy() * 0.6 + app.flash() * 1.5;
        light.intensity = 9_000_000.0 * boost;
    }
}

/// Compose the interactive frame (Bevy arena blit + all chrome) into the terminal,
/// and report the arena panel size back so the render target can track it.
fn draw_ui(
    mut context: ResMut<RatatuiContext>,
    mut sim: ResMut<Sim>,
    arena: Res<ArenaImage>,
    mut view: ResMut<ArenaView>,
) -> Result {
    let mut reported = (view.w, view.h);
    context.draw(|frame| {
        let (w, h) = ui::render_bevy(frame, &mut sim.0, &arena.pixels, arena.w, arena.h);
        if w > 0 {
            reported = (w as u32, h as u32);
        }
    })?;
    view.w = reported.0;
    view.h = reported.1;
    Ok(())
}

/// Drain the sim's queued sounds into the lazily-spawned player (capping the
/// per-frame click storm), exactly as the legacy loop does.
fn drain_sounds(mut sim: ResMut<Sim>, mut sound: ResMut<Sound>) {
    let mut clicks = 0usize;
    for ev in sim.0.take_sounds() {
        let player = sound.0.get_or_insert_with(Foley::spawn);
        if matches!(ev, SoundEvent::Impact { .. } | SoundEvent::Knock { .. }) {
            clicks += 1;
            if clicks > MAX_CLICKS_PER_FRAME {
                continue;
            }
        }
        player.play(ev);
    }
}

/// Copy each completed GPU readback into the CPU-side arena image.
fn on_readback(readback: On<ReadbackComplete>, mut arena: ResMut<ArenaImage>) {
    arena.pixels = readback.event().data.clone();
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

/// Render the full composed frame (arena + chrome) into a `TestBackend`, and once
/// the roll has settled and the render target has synced to the composed arena
/// size, encode it to a PNG and exit. Its validation counterpart is [`draw_ui`].
fn save_snapshot(
    mut snapshot: ResMut<Snapshot>,
    mut sim: ResMut<Sim>,
    arena: Res<ArenaImage>,
    mut view: ResMut<ArenaView>,
    mut exit: MessageWriter<AppExit>,
) {
    snapshot.frames += 1;

    let mut terminal = Terminal::new(TestBackend::new(snapshot.cols, snapshot.rows)).unwrap();
    let mut reported = (view.w, view.h);
    terminal
        .draw(|frame| {
            let (w, h) = ui::render_bevy(frame, &mut sim.0, &arena.pixels, arena.w, arena.h);
            if w > 0 {
                reported = (w as u32, h as u32);
            }
        })
        .unwrap();
    view.w = reported.0;
    view.h = reported.1;

    // Only capture once the render target matches the composed arena panel (so the
    // blit filled it), a readback landed, and the roll settled — or at a hard cap.
    let synced = arena.w == reported.0 && arena.h == reported.1 && !arena.pixels.is_empty();
    let done = match snapshot.at_frame {
        Some(n) => snapshot.frames >= n && synced,
        None => snapshot.frames >= 20 && synced && sim.0.all_settled(),
    };
    if !done && snapshot.frames < 600 {
        return;
    }
    if snapshot.frames >= 600 {
        eprintln!("tinhorn: roll didn't settle in 600 frames; snapshotting anyway");
    }

    // The PNG shows the arena visuals; a text dump (arena half-blocks blanked so
    // the burned numbers and chrome stand out) makes the whole UI readable in a
    // non-interactive shell.
    print_frame_text(terminal.backend().buffer());
    match save_frame_png(terminal.backend().buffer(), &snapshot.path) {
        Ok(()) => eprintln!("tinhorn: wrote snapshot {}", snapshot.path.display()),
        Err(err) => eprintln!("tinhorn: failed to write snapshot: {err}"),
    }
    exit.write_default();
}

/// Dump a composed frame as text (the `▀` arena fill blanked to spaces, so the
/// burned die numbers and the chrome read clearly) for headless validation.
fn print_frame_text(buf: &ratatui::buffer::Buffer) {
    let area = *buf.area();
    let mut out = String::from("\n");
    for y in 0..area.height {
        for x in 0..area.width {
            let sym = buf[(area.x + x, area.y + y)].symbol();
            out.push_str(if sym == "▀" { " " } else { sym });
        }
        out.push('\n');
    }
    println!("{out}");
}

#[derive(Resource)]
struct Snapshot {
    path: PathBuf,
    frames: u32,
    cols: u16,
    rows: u16,
    at_frame: Option<u32>,
}

/// Encode a composed ratatui buffer as a PNG (two stacked pixels per cell for the
/// `▀` half-block cells; a flat cell colour otherwise), so the whole UI — arena
/// and chrome — can be eyeballed from a non-interactive shell.
fn save_frame_png(
    buf: &ratatui::buffer::Buffer,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let area = *buf.area();
    let (pw, ph) = (area.width as usize, area.height as usize * 2);
    let mut px = vec![0u8; pw * ph * 4];
    for cy in 0..area.height {
        for cx in 0..area.width {
            let cell = &buf[(area.x + cx, area.y + cy)];
            let (top, bot) = if cell.symbol() == "▀" {
                (color_rgb(cell.fg), color_rgb(cell.bg))
            } else {
                (color_rgb(cell.fg), color_rgb(cell.fg))
            };
            for (row, [r, g, b]) in [(cy as usize * 2, top), (cy as usize * 2 + 1, bot)] {
                let i = (row * pw + cx as usize) * 4;
                px[i..i + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
    image::save_buffer(
        path,
        &px,
        pw as u32,
        ph as u32,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(())
}

/// Map a ratatui `Color` to RGB for the PNG (named colours to their terminal-ish
/// tones, RGB passthrough, everything else to the dark terminal background).
fn color_rgb(c: TColor) -> [u8; 3] {
    match c {
        TColor::Rgb(r, g, b) => [r, g, b],
        TColor::Black => [0, 0, 0],
        TColor::White => [230, 230, 230],
        TColor::Red => [200, 70, 70],
        TColor::Green => [70, 190, 90],
        TColor::Yellow => [220, 200, 60],
        TColor::Blue => [80, 120, 230],
        TColor::Magenta => [200, 90, 200],
        TColor::Cyan => [70, 190, 200],
        TColor::Gray => [160, 160, 160],
        TColor::DarkGray => [90, 90, 90],
        TColor::LightGreen => [130, 230, 130],
        TColor::LightMagenta => [230, 130, 230],
        TColor::LightYellow => [240, 230, 140],
        TColor::LightRed => [230, 120, 120],
        TColor::LightBlue => [130, 170, 240],
        TColor::LightCyan => [140, 220, 230],
        _ => [13, 17, 23],
    }
}
