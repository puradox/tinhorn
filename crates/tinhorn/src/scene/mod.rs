//! The Bevy dice arena — the default interactive renderer.
//!
//! `App` (the core sim) is the single source of truth; the Bevy entities are a
//! pure view of it (Architecture decision 2). Each frame the input system feeds
//! keys to the shared `handle_key`, `advance_sim` steps the physics,
//! `sync_dice_scene` mirrors `app.dice` into `DieView` entities, the camera and
//! lights choreograph off the sim's envelopes, and `draw_ui` composes the CPU
//! read-back of the Bevy render (blitted as quadrant glyphs, or a real kitty image)
//! with all of tinhorn's ratatui chrome via [`ui::render_bevy`]. The render target
//! autoresizes to the
//! arena panel so the blit is 1:1 and the burned-number overlays land on their
//! dice.
//!
//! Only the interactive/snapshot paths reach here; the one-shot CLI never
//! constructs a Bevy `App`, so scripting stays GPU-free.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::term::event::KeyMessage;
use crate::term::{RatatuiContext, RatatuiPlugins};
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
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyEventKind, KeyModifiers};
use ratatui::style::Color as TColor;

use tinhorn_core::app::{App as DiceApp, Die, SoundEvent};
use tinhorn_core::{dice_geom, physics, view_math};

use crate::foley::Foley;
use crate::graphics::{self, GraphicsArg, GraphicsMode};
use crate::{Action, handle_key, ui};

mod arena;
mod convert;

/// Impact/knock sounds voiced per frame; more is mush (mirrors the legacy loop).
const MAX_CLICKS_PER_FRAME: usize = 8;

/// Fixed terminal size the headless snapshot composes into (cols × rows).
const SNAP_COLS: u16 = 100;
const SNAP_ROWS: u16 = 38;

/// A tracing span for a hot block, compiled only under the `profiling` /
/// `profiling-tracy` features so ordinary builds pay nothing — it gives our
/// non-system work (compose, kitty emit) its own bars next to Bevy's per-system
/// spans.
macro_rules! profile_span {
    ($name:expr) => {
        #[cfg(any(feature = "profiling", feature = "profiling-tracy"))]
        let _profile_guard = bevy::log::info_span!($name).entered();
    };
}

/// Entry point (interactive or headless snapshot). Only called off the
/// interactive CLI path, never one-shot. `arg` is the `--graphics` flag; the
/// snapshot path forces blocks (no TTY to resolve against), the interactive
/// path resolves it against the terminal.
pub fn run(expr: String, seed: Option<u64>, muted: bool, arg: GraphicsArg) {
    if let Some(path) = std::env::var_os("TINHORN_BEVY_SNAPSHOT") {
        run_snapshot(&expr, seed, muted, PathBuf::from(path));
    } else {
        run_interactive(&expr, seed, muted, graphics::resolve(arg));
    }
}

/// The scene shared by both paths: headless render plugins, the sim, and the
/// systems that step it and mirror it into the scene. `bevy_window` is enabled
/// transitively, so DefaultPlugins carries a WindowPlugin and no loop driver —
/// render headless (no primary window, don't exit when there are none) and drive
/// the loop ourselves at ~60 fps.
fn base_app(expr: &str, seed: Option<u64>, muted: bool, mode: GraphicsMode) -> App {
    let mut sim = match seed {
        Some(s) => DiceApp::with_seed(expr.to_string(), s),
        None => DiceApp::new(expr.to_string()),
    };
    sim.muted = muted;
    // `App::with_seed`/`new` already roll a non-empty expression on construction,
    // consuming the seed once — so we must NOT roll again here, or the Bevy path
    // would diverge from `evaluate`.
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
    .insert_resource(Graphics(mode))
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

/// Are we running over SSH? A file-transmitted kitty image (`t=f`) can't work then —
/// the file is written on the remote host but read on the local one, so the emitter
/// falls back to base64 through the pty (the only medium an SSH hop forwards).
/// `SSH_CONNECTION`/`SSH_TTY` are set by sshd on the session.
fn over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

/// The interactive path: terminal context, input, per-frame compose, sound. In
/// kitty mode `draw_ui` also emits the image after the draw, `KittyState` gates the
/// pane-open placement delete, and `kitty_cleanup` removes the image on quit.
fn run_interactive(expr: &str, seed: Option<u64>, muted: bool, mode: GraphicsMode) {
    let mut app = base_app(expr, seed, muted, mode);
    app.add_plugins(RatatuiPlugins::default())
        .insert_resource(Sound(None))
        .insert_resource(KittyState {
            // File transmission (`t=f`) is used whenever it can be. It can't cross an
            // SSH hop (see `over_ssh`), so fall back to base64-through-the-pty there;
            // `TINHORN_KITTY_DIRECT` forces base64 too, for a local terminal that
            // restricts which files it will read.
            file: (!over_ssh() && std::env::var_os("TINHORN_KITTY_DIRECT").is_none())
                .then(|| std::env::temp_dir().join(format!("tinhorn-{}.rgb", std::process::id()))),
            ..default()
        })
        .insert_resource(FrameStats {
            show: cfg!(debug_assertions) || std::env::var_os("TINHORN_FPS").is_some(),
            ..default()
        })
        .add_systems(PreUpdate, input_system)
        .add_systems(Update, (draw_ui, drain_sounds).chain().after(choreograph))
        .add_systems(PostUpdate, kitty_cleanup);
    // In kitty mode, chain a teardown onto `term`'s restore panic hook so a crash
    // mid-roll cleans up the arena image + scratch file too (the graceful path is
    // `kitty_cleanup`). PostStartup, so `term`'s Startup hook is already installed.
    if let GraphicsMode::Kitty { .. } = mode {
        app.add_systems(PostStartup, install_kitty_panic_hook);
    }
    app.run();
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
    // The snapshot path always composes to a TestBackend/PNG: force half-blocks.
    let mut app = base_app(expr, seed, muted, GraphicsMode::Blocks);
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

/// The resolved arena renderer for this session, fixed at startup.
#[derive(Resource, Clone, Copy)]
struct Graphics(GraphicsMode);

/// Kitty emission state. `placed` gates the delete / re-place when a pane opens over
/// the arena (its `Clear` + default-bg text would show a placed image through at any
/// z). `file` selects the transmit path: `Some(temp path)` sends each frame as a
/// `t=f` file reference (raw pixels on disk, a tiny pty write) — the default whenever
/// it can work; `None` falls back to base64-in-escape (SSH, or `TINHORN_KITTY_DIRECT`).
#[derive(Resource, Default)]
struct KittyState {
    placed: bool,
    file: Option<PathBuf>,
}

/// The debug frame-rate readout. `fps` is an EMA of the real per-frame `dt`; `stage`
/// holds EMA'd per-stage kitty-transmit times (ms). Both are drawn in the arena's
/// top border with the render-target size. `show` is fixed at startup: on in a debug
/// build, or whenever `TINHORN_FPS` is set (so a release build can be profiled too).
#[derive(Resource, Default)]
struct FrameStats {
    fps: f32,
    show: bool,
    stage: StageMs,
}

/// EMA'd wall-clock times (ms) for the per-frame kitty transmit stages, so the
/// overlay can show where the frame budget goes: `prep` = pack + number burn,
/// `zip` = zlib, `b64` = base64 + APC chunking, `wr` = the stdout write + flush
/// (which also catches terminal backpressure). Whatever's left of the frame time is
/// the Bevy render + GPU→CPU readback.
#[derive(Clone, Copy, Default)]
struct StageMs {
    prep: f32,
    zip: f32,
    b64: f32,
    wr: f32,
}

/// The overlay's exponential moving average: 0.9/0.1 smoothing, seeded on the
/// first sample so a stat isn't dragged up from zero. Shared by the fps readout
/// and every per-stage transmit time.
fn ema(old: f32, new: f32) -> f32 {
    if old > 0.0 {
        old * 0.9 + new * 0.1
    } else {
        new
    }
}

impl FrameStats {
    /// Fold this frame's stage times into the EMAs.
    fn record(&mut self, prep: f32, zip: f32, b64: f32, wr: f32) {
        self.stage.prep = ema(self.stage.prep, prep);
        self.stage.zip = ema(self.stage.zip, zip);
        self.stage.b64 = ema(self.stage.b64, b64);
        self.stage.wr = ema(self.stage.wr, wr);
    }
}

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
        // A warm, smoky haze thickening with distance so the room dissolves into
        // amber murk beyond the lit tray — the cigar-smoke air of a saloon back
        // room. Exponential falloff reads softer than a hard linear band; the near
        // tray and dice stay crisp.
        DistanceFog {
            color: Color::srgb_u8(50, 33, 20),
            falloff: FogFalloff::Exponential { density: 0.06 },
            ..default()
        },
    ));

    // Read the render target back to the CPU each frame (Bevy 0.19 built-in).
    commands
        .spawn(Readback::texture(handle))
        .observe(on_readback);

    // Warm key light living inside the overhead pendant shade (see `arena`), hung
    // low and centred over the tray so it pools a bright circle of lamplight on
    // the felt and falls off into the smoky room, a cool rim for separation.
    commands.spawn((
        ArenaKeyLight,
        PointLight {
            color: Color::srgb(1.0, 0.84, 0.6),
            intensity: KEY_LIGHT_INTENSITY,
            range: 40.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(0.0, -physics::HY + 2.7, 0.1),
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

/// The pendant key light's resting intensity — hung low over the tray, so it's
/// dimmer than a high fill would be. `choreograph` scales this on impact and
/// crit, so the spawn and the flinch must read the same baseline.
const KEY_LIGHT_INTENSITY: f32 = 4_500_000.0;

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
        // Glossy resin: low roughness + lifted reflectance so each die catches a
        // crisp specular highlight, reading as moulded plastic. A faint
        // colour-matched emissive keeps the dice vivid in the dim edges of the
        // lamp-pool instead of dulling to mud in the shadow.
        let material = materials.add(StandardMaterial {
            base_color: die_color(die),
            emissive: die_emissive(die),
            perceptual_roughness: 0.2,
            reflectance: 0.62,
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
        light.intensity = KEY_LIGHT_INTENSITY * boost;
    }
}

/// Compose the interactive frame (Bevy arena blit + all chrome) into the terminal,
/// and report the arena panel size back so the render target can track it.
#[allow(clippy::too_many_arguments)] // a Bevy system's params are its dependencies
fn draw_ui(
    mut context: ResMut<RatatuiContext>,
    mut sim: ResMut<Sim>,
    arena: Res<ArenaImage>,
    mut view: ResMut<ArenaView>,
    gfx: Res<Graphics>,
    mut kitty: ResMut<KittyState>,
    time: Res<Time>,
    mut stats: ResMut<FrameStats>,
) -> Result {
    let mode = gfx.0;

    // Smooth the real frame rate for the debug overlay: 1/dt, EMA-filtered. `dt` is
    // the wall-clock gap between Update runs, so this is the *actual* loop rate — if
    // the encode/readback overruns the 1/60 s budget, the number falls.
    let dt = time.delta_secs();
    if dt > 0.0 {
        stats.fps = ema(stats.fps, 1.0 / dt);
    }
    // The overlay shows the *previous* frame's stage times (emit runs after this
    // draw); one frame stale is invisible against the EMA.
    let (show_fps, fps, stage) = (stats.show, stats.fps, stats.stage);

    let mut reported = (view.w, view.h);
    let mut panel = None;
    {
        profile_span!("compose");
        context.draw(|frame| {
            let report =
                ui::render_bevy_mode(frame, &mut sim.0, &arena.pixels, arena.w, arena.h, mode);
            if report.view.0 > 0 {
                reported = (report.view.0 as u32, report.view.1 as u32);
            }
            panel = report.kitty;
            if show_fps {
                draw_fps_overlay(frame, fps, stage, mode, arena.w, arena.h);
            }
        })?;
    }
    view.w = reported.0;
    view.h = reported.1;

    // Kitty emission runs strictly AFTER the draw (ratatui owns stdout during it):
    // write the image APC over the arena origin, or delete it while a pane covers it.
    if let (GraphicsMode::Kitty { .. }, Some(panel)) = (mode, panel) {
        let pane_open = sim.0.pane != tinhorn_core::app::Pane::None;
        emit_kitty(&panel, &arena, pane_open, &mut kitty, &mut stats);
    }
    Ok(())
}

/// The scratch sibling a `t=f` frame is staged in before being renamed over the
/// terminal-referenced path. Same directory ⇒ same filesystem ⇒ the rename is
/// atomic. Also removed on cleanup, in case a write died between stage and rename.
fn scratch_tmp(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

/// Write `data` to `path` atomically: stage it in a sibling `.tmp`, then rename that
/// over `path`. The terminal, reading `path` for a `t=f` transmit, then always sees
/// one whole frame (the old inode or the new) — never the truncate-in-progress a
/// plain `std::fs::write` to the reused path would expose to a concurrent read.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = scratch_tmp(path);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Place — or, while a pane covers the arena, remove — the kitty image for this
/// frame. A pane renders with `Clear` + default-bg text, so a placed image would
/// show through it at any z; delete the placement once and re-place on close. An
/// empty/stale readback (startup, or just after `resize_arena` clears `pixels`) is
/// skipped, leaving the previous placement up.
fn emit_kitty(
    panel: &ui::KittyPanel,
    arena: &ArenaImage,
    pane_open: bool,
    state: &mut KittyState,
    stats: &mut FrameStats,
) {
    if pane_open {
        if state.placed {
            let _ = graphics::emit_raw(&graphics::delete_placement_apc());
            state.placed = false;
        }
        return;
    }
    profile_span!("kitty_emit");
    // Time each transmit stage (ms) so the overlay can show where the frame goes.
    let ms = |t: Instant| t.elapsed().as_secs_f32() * 1000.0;

    let t0 = Instant::now();
    let Some(mut rgb) = graphics::pack_rgb(&arena.pixels, arena.w, arena.h) else {
        return;
    };
    ui::burn_numbers(
        &mut rgb,
        arena.w,
        arena.h,
        panel.inner.width,
        panel.inner.height,
        &panel.burns,
    );
    let prep = ms(t0);
    let (w, h) = (panel.inner.width, panel.inner.height);

    // `zip` and `b64` are timed for whichever transmit path is live: for the direct
    // path they're zlib and base64+chunk; for the file path `zip` is the raw-pixel
    // file write and `b64` the (tiny) path-APC build.
    let (zip, b64, apc) = if let Some(path) = &state.file {
        let t1 = Instant::now();
        let wrote = write_atomic(path, &rgb).is_ok();
        let zip = ms(t1);
        let t2 = Instant::now();
        let apc = if wrote {
            graphics::encode_apc_path(&path.to_string_lossy(), arena.w, arena.h, w, h)
        } else {
            // File write failed — fall back to the direct path for this frame.
            graphics::encode_apc(&graphics::compress(&rgb), arena.w, arena.h, w, h)
        };
        (zip, ms(t2), apc)
    } else {
        let t1 = Instant::now();
        let compressed = graphics::compress(&rgb);
        let zip = ms(t1);
        let t2 = Instant::now();
        let apc = graphics::encode_apc(&compressed, arena.w, arena.h, w, h);
        (zip, ms(t2), apc)
    };

    let t3 = Instant::now();
    let ok = graphics::emit(panel.inner.x, panel.inner.y, &apc).is_ok();
    let wr = ms(t3);

    if ok {
        state.placed = true;
    }
    stats.record(prep, zip, b64, wr);
}

/// Draw the debug FPS readout in the arena's top border (right-aligned), with the
/// render-target size. In kitty mode it also breaks the frame down by transmit
/// stage — `rest` (Bevy render + GPU→CPU readback + the rest), `pack`, `zip`
/// (zlib), `b64`, `wr` (stdout write) — so the bottleneck is legible at a glance;
/// it falls back to the short form when the border is too narrow for the breakdown.
fn draw_fps_overlay(
    frame: &mut ratatui::Frame,
    fps: f32,
    stage: StageMs,
    mode: GraphicsMode,
    img_w: u32,
    img_h: u32,
) {
    let area = frame.area();
    let short = |tag: &str| format!(" {fps:>4.0} fps · {tag} {img_w}×{img_h} ");
    let label = match mode {
        GraphicsMode::Blocks => short("blocks"),
        GraphicsMode::Kitty { .. } => {
            let frame_ms = if fps > 0.0 { 1000.0 / fps } else { 0.0 };
            let sum = stage.prep + stage.zip + stage.b64 + stage.wr;
            let rest = (frame_ms - sum).max(0.0);
            let full = format!(
                " {fps:.0}fps {img_w}×{img_h}  rest {rest:.0} pack {p:.0} zip {z:.0} b64 {b:.0} wr {w:.0} ms ",
                p = stage.prep,
                z = stage.zip,
                b = stage.b64,
                w = stage.wr,
            );
            if full.chars().count() as u16 + 2 <= area.width {
                full
            } else {
                short("kitty")
            }
        }
    };
    let w = label.chars().count() as u16;
    if area.width <= w + 2 {
        return; // too narrow — don't corrupt the border
    }
    frame.buffer_mut().set_string(
        area.width - w - 1,
        0,
        label,
        ratatui::style::Style::default()
            .fg(ratatui::style::Color::Black)
            .bg(ratatui::style::Color::Yellow),
    );
}

/// Tear down the kitty image and its scratch files: delete our `i=1` image from the
/// terminal (so it isn't stranded in the scrollback) and remove the `t=f` scratch
/// frame plus its `.tmp` sibling. Shared by the graceful `kitty_cleanup` and the
/// panic hook. Targeted to `i=1` so it can't disturb anything else on screen, and
/// idempotent, so running it twice is harmless.
fn kitty_teardown(file: &Option<PathBuf>) {
    let _ = graphics::emit_raw(&graphics::delete_all_apc());
    if let Some(path) = file {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(scratch_tmp(path));
    }
}

/// On quit, delete our kitty image so nothing is left behind in the scrollback.
/// Runs in PostUpdate — after `input_system` writes `AppExit` in PreUpdate, but
/// before `CleanupPlugin` drops `RatatuiContext` in `Last` and leaves the alt
/// screen — so the escape lands while the session is still live. A no-op outside
/// kitty mode. The panic path is covered separately by [`install_kitty_panic_hook`].
fn kitty_cleanup(mut exits: MessageReader<AppExit>, gfx: Res<Graphics>, state: Res<KittyState>) {
    if exits.is_empty() {
        return;
    }
    exits.clear();
    if let GraphicsMode::Kitty { .. } = gfx.0 {
        kitty_teardown(&state.file);
    }
}

/// Chain the kitty teardown onto the terminal-restore panic hook the vendored `term`
/// installs in `Startup`. Runs in `PostStartup` (once that hook is in place, so
/// `take_hook` captures it) and only in kitty mode: on a panic we delete the image
/// and remove the scratch file *before* deferring to `term`'s restore, so a crash
/// mid-roll doesn't strand the image in the scrollback. Hooks fire even under
/// `panic = "abort"`, where a Drop guard would not.
fn install_kitty_panic_hook(state: Res<KittyState>) {
    let file = state.file.clone();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        kitty_teardown(&file);
        prev(info);
    }));
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

/// Die colour: the SAME per-die palette (`ui::die_rgb`) the number overlay tints
/// its outline with, so a die and its burned value read as the same colour — you
/// can tell which number belongs to which die when several land at once. Dropped
/// dice grey out; crit/fumble are signalled by the number's ink (gold / red), not
/// the die.
fn die_color(die: &Die) -> Color {
    if !die.kept {
        return Color::srgb_u8(90, 90, 90);
    }
    let c = ui::die_rgb(die.color_idx);
    Color::srgb_u8(c.0, c.1, c.2)
}

/// A faint self-glow in the die's own colour, so it stays vivid in the dim edges
/// of the lamp-pool instead of muddying into shadow. Dropped dice don't glow.
fn die_emissive(die: &Die) -> LinearRgba {
    if !die.kept {
        return LinearRgba::BLACK;
    }
    die_color(die).to_linear() * 0.18
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

/// Dump a composed frame as text (the arena's quadrant-glyph fill blanked to
/// spaces, so the burned die numbers and the chrome read clearly) for headless
/// validation.
fn print_frame_text(buf: &ratatui::buffer::Buffer) {
    let area = *buf.area();
    let mut out = String::from("\n");
    for y in 0..area.height {
        for x in 0..area.width {
            let sym = buf[(area.x + x, area.y + y)].symbol();
            let is_arena = sym
                .chars()
                .next()
                .is_some_and(|c| ui::quad_pattern(c).is_some());
            out.push_str(if is_arena { " " } else { sym });
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

/// Encode a composed ratatui buffer as a PNG — **2×2 pixels per cell**, expanding
/// each arena quadrant glyph back into its four sub-pixels (`fg`/`bg` per the glyph
/// mask) and painting chrome/digit cells as a flat colour — so the whole UI, arena
/// and chrome, can be eyeballed from a non-interactive shell.
fn save_frame_png(
    buf: &ratatui::buffer::Buffer,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let area = *buf.area();
    let (pw, ph) = (area.width as usize * 2, area.height as usize * 2);
    let mut px = vec![0u8; pw * ph * 4];
    for cy in 0..area.height {
        for cx in 0..area.width {
            let cell = &buf[(area.x + cx, area.y + cy)];
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            // A quadrant glyph expands to its 2×2 sub-pixels; anything else (chrome,
            // a scale-0 digit) fills the block with its fg colour.
            let sub: [[u8; 3]; 4] = match ui::quad_pattern(ch) {
                Some(mask) => {
                    let (fg, bg) = (color_rgb(cell.fg), color_rgb(cell.bg));
                    [0, 1, 2, 3].map(|i| if mask & (1 << i) != 0 { fg } else { bg })
                }
                None => [color_rgb(cell.fg); 4],
            };
            // Sub-pixel order 0=TL, 1=TR, 2=BL, 3=BR → offsets in the 2×2 block.
            for (i, (dx, dy)) in [(0, (0, 0)), (1, (1, 0)), (2, (0, 1)), (3, (1, 1))] {
                let (x, y) = (cx as usize * 2 + dx, cy as usize * 2 + dy);
                let o = (y * pw + x) * 4;
                let [r, g, b] = sub[i];
                px[o..o + 4].copy_from_slice(&[r, g, b, 255]);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn top_row(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area().width)
            .map(|x| buf[(x, 0)].symbol())
            .collect()
    }

    #[test]
    fn fps_overlay_shows_kitty_stage_breakdown() {
        // The perf diagnostic: on a wide-enough border, kitty mode carries the frame
        // rate, the render size, and the per-stage times — so a dominant stage (here
        // zip=78ms) is legible at a glance.
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let stage = StageMs {
            prep: 3.0,
            zip: 78.0,
            b64: 4.0,
            wr: 5.0,
        };
        terminal
            .draw(|f| {
                draw_fps_overlay(
                    f,
                    11.0,
                    stage,
                    GraphicsMode::Kitty { scale: 12 },
                    1440,
                    1080,
                )
            })
            .unwrap();
        let row = top_row(&terminal);
        assert!(row.contains("11fps"), "frame rate missing: {row:?}");
        assert!(row.contains("1440×1080"), "render size missing: {row:?}");
        assert!(
            row.contains("zip 78"),
            "the zlib stage time must show: {row:?}"
        );
        assert!(
            row.contains("wr 5"),
            "the write stage time must show: {row:?}"
        );
    }

    #[test]
    fn fps_overlay_falls_back_and_skips() {
        // Blocks short form fits a modest width…
        let mut wide = Terminal::new(TestBackend::new(40, 6)).unwrap();
        wide.draw(|f| {
            draw_fps_overlay(f, 60.0, StageMs::default(), GraphicsMode::Blocks, 240, 180)
        })
        .unwrap();
        assert!(top_row(&wide).contains("60 fps"), "{:?}", top_row(&wide));
        // …but a tiny frame is skipped, not panicking or corrupting the row.
        let mut tiny = Terminal::new(TestBackend::new(10, 6)).unwrap();
        tiny.draw(|f| {
            draw_fps_overlay(f, 30.0, StageMs::default(), GraphicsMode::Blocks, 200, 160)
        })
        .unwrap();
        assert!(
            !top_row(&tiny).contains("fps"),
            "should skip when too narrow"
        );
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_scratch() {
        // The `t=f` frame is staged in a `.tmp` and renamed over the reused path, so a
        // concurrent terminal read never catches a truncated file. Assert the whole
        // frame lands, a rewrite fully replaces it, and no `.tmp` is left behind.
        let path = std::env::temp_dir().join(format!("tinhorn-test-{}.rgb", std::process::id()));
        let tmp = scratch_tmp(&path);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&tmp);

        write_atomic(&path, &[1, 2, 3]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
        assert!(
            !tmp.exists(),
            "the .tmp sibling is renamed away, not left behind"
        );

        // A rewrite (the every-frame case) atomically swaps in the whole new frame.
        write_atomic(&path, &[9, 9]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [9, 9]);
        assert!(!tmp.exists());

        let _ = std::fs::remove_file(&path);
    }
}
