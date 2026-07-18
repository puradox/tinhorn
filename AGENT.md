# AGENT.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tinhorn` is a terminal dice roller. Type dice in standard notation, shake the
cup, and watch each die tumble as a real 3D polyhedron in a rendered dice tray
until it settles and the total is tallied. (A *tinhorn* was a
small-time gambler, named for the tin cup they rattled their dice in.) It has
two modes from a single binary:

- **Interactive TUI** (default): the bouncing-dice animation. The arena is a
  **headless Bevy** scene rendered on the GPU, read back to the CPU, and blitted
  into a ratatui frame as half-blocks; ratatui paints the chrome (result panel,
  input line, help, panes) around it.
- **One-shot CLI**: given an expression plus an output flag — or whenever stdout
  isn't a terminal — it skips the TUI, evaluates once, prints, and exits, so it
  drops into scripts and pipes (`tinhorn 3d6 | cat`). This path never constructs
  a Bevy `App`, so scripting stays GPU-free.

## Commands

```sh
cargo run --release            # start the TUI empty
cargo run --release -- 3d6     # TUI, rolling 3d6 immediately
cargo run --release -- -p 3d6  # one-shot: print total and exit
cargo build --release          # release build (LTO on)
cargo run --features bevy/dynamic_linking   # dev iteration: fast Bevy link, unoptimized

cargo test                     # parser, physics, evaluator, CLI, key routing, chrome, foley
cargo test <name>              # run a single test by substring, e.g. `cargo test typing_kh`
cargo test -p tinhorn-core     # the ~64 GPU-free sim/ceremony tests, no Bevy

# Validate the Bevy arena without a TTY: render it headless to a PNG + a readable
# text dump of the composed frame (the arena half-blocks blanked so the burned
# numbers and chrome stand out). wgpu runs headless, so this works in CI / ssh.
TINHORN_BEVY_SNAPSHOT=/tmp/arena.png cargo run -- 4d6!kh3
TINHORN_SNAP_COLS=120 TINHORN_SNAP_ROWS=44 TINHORN_BEVY_SNAPSHOT=/tmp/a.png cargo run -- d20
TINHORN_SNAP_FRAME=8 TINHORN_BEVY_SNAPSHOT=/tmp/a.png cargo run -- 3d6   # mid-roll, not settled
```

The three GPU render smoke tests in the binary are `#[ignore]`d (they need a real
adapter) — validate the renderer with the snapshot path above instead. There is
no lint/fmt config beyond defaults; use `cargo fmt` and `cargo clippy`.

## Architecture

A **virtual Cargo workspace** of two crates. The seed contract, roll evaluator,
and physics live in the library; only the terminal binary knows about Bevy,
ratatui, rodio, or clap:

- **`crates/tinhorn-core`** — the renderer-agnostic library (`parse`, `app`,
  `physics`, `dice_geom`, `view_math`). Deps are `rand`/`serde`/`glam`/`rapier3d`
  and *nothing else* — no ratatui, no Bevy, no rodio, no clap — so its ~64
  sim/ceremony tests run GPU- and terminal-free.
- **`crates/tinhorn`** — the binary: the Bevy `scene`, the ratatui `ui` chrome
  and arena overlays, `cli`, `foley`, `paint`, and the vendored `term` terminal
  integration. Re-exports `tinhorn_core::{app, parse, physics}` so its modules
  refer to `crate::app` etc.

The interactive arena is a Bevy `App` driven by a `ScheduleRunnerPlugin` at
~60 fps (no window — headless render target). The core `App` (the sim) is the
**single source of truth**, held in a `Sim` resource; the Bevy entities are a
pure view of it. Each frame: `input_system` feeds keys to the shared
`handle_key`, `advance_sim` steps `app.update(dt)`, `sync_dice_scene` mirrors
`app.dice` into entities, `choreograph` moves the camera and lights off the sim's
envelopes, `draw_ui` composes the CPU read-back of the render with the chrome,
and `drain_sounds` plays whatever the physics queued.

- **`parse`** (core) — hand-written parser: notation → `Roll` (a `Vec<DiceTerm>`
  + flat `i32` modifier + optional `Stake` for staked rolls). Each `DiceTerm` is
  count, sides, and modifiers (`TermMod`: keep/drop, explode with a `Compare`,
  multiply). Pure and unit-tested. Sizes are capped (≤ 60 dice, ≤ 1000 sides) so
  a huge expression can't wedge the renderer; the `vs` target must come last and
  is range-checked into `i32`. A `Stake` bundles that target with a `Goal`
  (`Over` for `>` and its word alias `vs`, `Under` for the roll-under `<`) so a
  direction can only exist alongside a target; both comparisons are inclusive.

- **`app`** (core) — state plus the physics simulation and the roll evaluator.
  - `App` holds the dice, input line, `Pane` (Help/History/Stats overlays),
    history, session stats, the Throw state (shake clock, last release), crit
    particles, the `RollMode` (what Enter does; Tab cycles Shake → Roll →
    Insta), and the pending `SoundEvent` queue. `App::new`/`App::with_seed`
    **roll a non-empty initial expression on construction** (consuming the seed
    once), which is how `tinhorn -- 3d6` rolls immediately — the Bevy path must
    NOT roll again. `App::roll()` starts an animated roll; `start_shake()`/
    `throw()` are the Throw — the default mode (Enter shakes the cup, a second
    Enter releases; release timing shapes only the launch); `insta_roll()`
    fast-forwards the same simulation to rest between two frames (same RNG draws,
    so totals are seed-identical across all three modes); `update(dt)` steps the
    physics; `all_settled()` reports convergence.
  - Physics: real 3D rigid bodies via **Rapier** (see the `physics` module). Each
    `Die` holds a body handle and a cached pose (`pos`/`rot`) synced from Rapier
    every step; `update(dt)` advances the sim on a **fixed 1/60 s timestep**
    (`physics::STEP`, fed by an accumulator), which is what makes insta and
    animated rolls settle bit-identically. A die is "settled" when its body
    sleeps (or hits the hard airborne cap, which freezes it into a static body).
    The step also voices contacts as foley and spawns explosion dice.
  - **Every die's value is decided up front by the RNG** — the animation only
    shows it off — so the displayed total always matches the real total. The
    on-die number (`Die::shown`) is a cosmetic decoy while the die tumbles
    (`tumbling_face`, keyed off the spin, drawing no RNG so the seed contract
    holds); it locks to `final_value` on settle. The one exception to "decided up
    front" is **exploding**, which plays out *during* the animation: a die that
    settles on a qualifying face drops another die into the arena (capped per term
    so it always converges).
  - `evaluate(expr, &Roll, &mut StdRng) -> Outcome` resolves a roll *instantly*
    into a full breakdown (`Outcome`/`OutcomeTerm`/`OutcomeDie`, all `serde`). It
    **mirrors the animation's semantics exactly** — explode → keep/drop on the
    base pool → per-term multiply → flat modifier. This is the shared contract:
    the one-shot CLI and the TUI must agree, so changes to roll rules belong here
    and in the animated path together.
  - Single-source rule helpers back both paths: `check()` (the `vs` verdict for
    either `Goal` — meet-or-beat or roll-under — returning a direction-aware
    margin, also used by the stats pane's success odds), `Stake::label()` (the
    `vs`/`vs ≤` chip text shared by TUI and CLI), `verdict_text()` (the
    SUCCESS/FAIL wording shared by TUI chip and CLI), and `crit_face()`/
    `fumble_face()` (any die maxing / rolling 1; drives particles, sounds, and
    the `crit`/`fumble` flags in JSON). Never restate these comparisons inline.

- **`physics`** (core) — the real 3D rigid-body world, on **Rapier** (`rapier3d`,
  which speaks glam natively, so no conversion). A fixed box tray (floor + walls +
  ceiling) and one dynamic body per die with a convex-hull collider from its
  polyhedron, so dice tumble, bounce, collide, and sleep at rest. Small API
  (`spawn`/`launch`/`step`/`pose`/`sleeping`/`freeze`/`clear`) so `app` and the
  scene never touch Rapier. It decides *only* where dice land — the seeded
  RNG owns the values (drawn in `app`, up front). `HX/HY/HZ` are the tray
  half-extents — 3.2×1.9×2.0, a ~1.6:1 width:depth felt, near a real dice
  tray's proportions — `DIE_R` the die world-radius, and `STEP` THE fixed
  timestep (`Physics::step` takes no `dt`). The camera framing, the spawn
  lattice's depth slots, and the table under the tray all derive from `HZ`, so it
  stays a one-line retune.

- **`dice_geom`** (core) — the six standard polyhedra (d4, d6, d8, d10, d12, d20)
  as plain glam data, the **single source of the dice shapes**: the physics
  collider is the convex hull of `mesh_for(sides)`'s vertices, and the Bevy
  renderer builds its `Mesh` from the same data (via `scene::convert::dice_mesh`),
  so the die you see is the die the sim tumbles. One `polyhedron` face-builder
  handles all six (d12 the icosahedron's dual, d10 a pentagonal bipyramid);
  `face_geometry(sides)` returns each face's `(centroid, outward_normal)` in
  unit-mesh space, so the number overlay can ride the die's frontmost face.
  `cup()` builds the open, hollow tin tumbler shown while shaking.

- **`view_math`** (core) — the arena camera and the pure world→screen math, glam
  only (no renderer types cross it). `arena_camera()`/`live_camera()` define where
  the eye sits and how it moves through the throw (`live_camera` folds in the idle
  drift and crit punch-in); `project_to_cell()` is the one true world→cell map.
  **Both the Bevy scene's `choreograph` and the `ui` overlays pose their camera
  here**, and the crit/fumble particle bursts project through the *same*
  `live_camera`, so the dice, their burned numbers, and the bursts can never
  disagree about where a die sits on screen. `arena_camera` derives its distance
  from the view aspect (fitting the tray's full width and height) and its vertical
  framing from `physics::HZ`, and pitches from a shallow ~31° establishing angle
  to a near-overhead ~68° read as `focus`→1.

- **`scene`** (bin) — the Bevy dice arena, the default interactive renderer. The
  `Sim(App)` resource is the single source of truth; the entities are a pure view.
  - Systems (all reading `Sim`): `input_system` (PreUpdate) feeds each
    `term::event::KeyMessage` to the shared, pure `handle_key`, exiting on its
    `Quit` verdict; `advance_sim` steps `app.update(time.delta_secs())` (core's
    fixed-step accumulator keeps the physics deterministic regardless of Bevy's
    frame pacing); `sync_dice_scene` diffs `app.dice` → `DieView` entities
    (spawn a mesh+material per new die, copy each pose+colour every frame, despawn
    cleared ones); `sync_cup` shows the tin cup only while shaking and sways it on
    the shared shake clock; `choreograph` moves the `ArenaCamera` to match
    `view_math::live_camera` (so the overlays stay in register) and flinches the
    key light on hard impacts / a crit flare; `draw_ui` composes the frame via
    `ui::render_bevy_mode` (the resolved `Graphics(mode)` resource selects
    half-blocks vs kitty) and reports the arena panel size back — and in **kitty
    mode**, *strictly after* the draw (ratatui owns stdout during it), emits the
    arena image as kitty-graphics APCs (`graphics::emit`) over the panel origin,
    gated by a `KittyState` that deletes/re-places while a pane covers the arena;
    `drain_sounds` feeds `app.take_sounds()` to the lazily-spawned `Foley` (capping
    the per-frame click storm). `kitty_cleanup` (PostUpdate) deletes the image on
    quit, before `CleanupPlugin` leaves the alt screen.
  - Rendering: a headless `Camera3d` draws into an offscreen render-target
    `Image`, which is **read back to the CPU each frame via Bevy 0.19's built-in
    `bevy_render::gpu_readback`** (`Readback` + a `ReadbackComplete` observer);
    in the `blocks` path `ui::blit_bevy_arena` blits the RGBA into half-blocks (the
    `kitty` path in `graphics` transmits the same readback as a real image instead).
    Realism the old CPU
    rasterizer never had: **HDR + filmic tonemapping** (TonyMcMapface),
    **4× MSAA**, **screen-space ambient occlusion**, **shadow maps** (the key
    `PointLight`), and **`DistanceFog`**, plus a **2× supersample** (`ARENA_SS`,
    box-downsampled in the blit) and a warm-graded radial **vignette**. The render
    target **autoresizes** to the arena panel (`resize_arena`) so the blit is 1:1
    and the burned-number overlays land on their dice.
  - `scene/arena.rs` builds the static casino furniture — the green-baize felt
    bed, mahogany tray walls + rails, the wood table + apron, the dark-red rug,
    the oak-floorboard room floor, the gradient backdrop, the free-hanging oxblood
    stage curtains (real corrugated fold geometry), and the poker-chip stacks —
    reusing `ui`'s `ArenaStyle` palette and its procedural texture generators
    (wrapped as Bevy `Image`s). Shadow maps replace the software renderer's baked
    contact shadows.
  - `scene/convert.rs` is **the one sanctioned crossing** between core's glam 0.33
    and Bevy's own (distinct) glam — `vec3`/`quat` component-wise, plus the core
    mesh → Bevy `Mesh` build — so a stray direct assignment is a compile error,
    not a silent unit bug.

- **`ui`** (bin) — no longer a software renderer: it's the ratatui **chrome** plus
  the **shared arena overlays**. `render_bevy_mode(frame, app, pixels, img_w, img_h,
  mode) -> ArenaReport` lays out the four rows (arena, result panel, input line,
  help bar) and branches only on the arena panel: in `blocks` mode it blits the
  Bevy read-back into the arena and composites the numbers; in `kitty` mode it
  clears the arena cells (the scene places a real image behind them) and *returns*
  the numbers as `NumberBurn`s (`ArenaReport.kitty`) for the scene to rasterise into
  the pixels (`burn_numbers`). It returns the render-target size to request.
  `render_bevy` is the thin `blocks` wrapper kept at its old signature so the ≈30
  chrome tests (and the three `#[ignore]` GPU tests) compile untouched. The
  overlays: the **die numbers** (each riding the frontmost read-face, a spin-derived
  decoy while airborne that ducks out edge-on and burns to `final_value` on settle,
  scaled uniformly across the roll from a single cell up to a blocky half-block
  glyph — `DIGIT_FONT`/`GlyphRaster`/`plan_die_number`/`draw_big_number`/
  `number_scale`/`read_face`, the placement + glyph raster both render paths share),
  the crit/fumble **particles**, the **power meter** and **release echo**. It also
  owns the
  **procedural texture generators** (`felt_texture`, `floor_texture`,
  `velvet_texture`, `grain_texture`, `backdrop_texture` — colour-keyed bake
  cache), the `ArenaStyle` palette (`DEFAULT`), and the result/help/pane panels.
  The result panel **withholds the outcome until the dice stop**: each chip stays
  a dim decoy until its die settles, then locks in bold; the Σ total is a dim `…`
  until every die has landed. (The animation must never spoil the total it's
  building toward.)

- **`graphics`** (bin) — the **kitty graphics protocol** arena (the pixel-perfect
  output path), a plain module *outside* the vendored `term/` (the PROVENANCE
  re-sync contract), and named `graphics` not `kitty` so it never collides with
  `term/crossterm_context/kitty.rs` (the keyboard protocol). `resolve(--graphics)`
  picks a `GraphicsMode`: `auto` env-sniffs the terminal (`kitty_capable`: kitty /
  Ghostty / WezTerm via `TERM`/`TERM_PROGRAM`/`KITTY_WINDOW_ID`, **never under
  tmux/screen**, which inherit `KITTY_WINDOW_ID` yet eat the graphics APC), else
  `blocks`; `kitty`/`blocks` force it. The per-frame payload pipeline: `pack_rgb`
  strips wgpu's 256-byte row padding, drops alpha (`f=24`), and applies the shared
  `ui::vignette`; `compress` runs zlib-fast (`o=z`) and `encode_apc` does base64 →
  4096-byte APC chunks (`a=T`, a **fixed** `i=1,p=1` so a same-id retransmit is kitty's
  flicker-free in-place replace, a deep negative `z` under non-default backgrounds,
  `C=1` so the cursor doesn't move, and `q=2` so no response ever lands in
  crossterm's input stream). `emit`/`emit_raw` write to stdout; the delete escapes
  clean up on pane-open and on quit. `scale_for` maps the cell pixel height to the
  render scale; `MAX_IMG_W` caps the transmitted width. Everything is pure and
  unit-tested bar the two impure edges (`resolve`, `emit`).

- **`paint`** (bin) — the small CPU `Rgb` (8-bit tint/palette) and `Texture`
  (row-major RGBA) types the overlays and the procedural generators use. They
  outlived the deleted `render3d` rasterizer they came from; the Bevy renderer
  wraps the baked `Texture`s as `Image`s.

- **`cli`** (bin) — clap `Cli` and `run_one_shot`. Three output shapes: bare total
  (default), verbose breakdown (`-v`, dropped dice in `[brackets]`, exploded
  marked `!`), and `--json`. `--seed N` gives reproducible rolls in both modes.
  Under explicit `-p`/`-v`, a staked roll's exit code is the verdict (0/1); the
  implicit piped-stdout mode and `--json` always exit 0 on clean output.

- **`foley`** (bin) — procedural sound. `App` emits pure `SoundEvent`s (impacts,
  knocks, settles, cup rattle, crit ring, verdicts); `synth()` renders them from
  physics parameters (die size → pitch, impact speed → loudness) with no assets;
  `Foley` plays them via rodio on a dedicated audio thread, degrading silently
  with no audio device. On by default; `--mute` starts muted, Ctrl-Q toggles.
  The thread spawns **lazily** in `scene::drain_sounds` on the first audible sound
  (opening the device blocks for tens of ms — off the render loop so it isn't a
  frame hitch), and a muted session never spawns it, so it never touches audio
  APIs at all (macOS raises a one-time microphone prompt for playback on duplex
  output devices; that's the OS, even `afplay` draws it — don't chase it).

- **`term`** (bin) — the terminal integration, a **vendored in-tree snapshot** of
  `puradox/bevy_ratatui` (a fork of `ratatui/bevy_ratatui` plus apekros's PR #98,
  the Bevy-0.19 port; see `src/term/PROVENANCE.md`). It lives as a plain module
  (`src/term/`), **not** a path dependency, so `cargo publish` works. It's trimmed
  to the **crossterm** path the scene drives — the winit/`soft_ratatui`
  `windowed` backend is dropped and the crate's `crossterm`/`keyboard`/`mouse`
  cargo features are resolved to always-on — and provides the `RatatuiContext`,
  `RatatuiPlugins`, and the `KeyMessage` input the scene reads. `#![allow(dead_code,
  unused_imports)]` keeps the upstream surface verbatim under the binary's
  `-D warnings`. tinhorn uses **Bevy 0.19's built-in `gpu_readback`** for readback
  rather than the `bevy_ratatui_camera` crate. To sync: recopy the fork's
  crossterm `src/`, re-apply the trims, update the SHA in `PROVENANCE.md`.

## Conventions worth knowing

- **Key routing is deliberately constrained** (`main.rs::handle_key`, kept pure
  so it's unit-tested and shared verbatim by `scene::input_system`). Pane hotkeys
  use chords/`?` (Ctrl-H history, Ctrl-S stats, `?` help) specifically so bare
  `h`/`s` stay typeable for notation like `kh`/`dh`. Don't add a plain-letter
  hotkey — it will eat characters users need to type. Enter rolls in the current
  mode; Tab cycles the mode (shake → roll → insta); Space stays a notation
  separator. The arrows are for editing and scrolling only, never a roll action:
  on the prompt ←/→ (and Home/End) move the caret (`App.cursor`, a byte offset
  kept valid by `cursor_byte()`, which the insert/delete/move helpers and the
  renderer all read through); while a pane is open the same arrows scroll it
  (`App.pane_scroll`, clamped to the overflow in `ui::overlay_panel`;
  `App::set_pane` owns the `pane` ↔ `pane_scroll` pairing and is the only way to
  change panes). Mute is Ctrl-Q ("quiet") — never move it to Ctrl-M: on legacy
  encodings (e.g. Apple Terminal) Ctrl-M *is* Enter (ASCII CR), so it can't be a
  hotkey anywhere, which is also why there is no enhanced-keyboard machinery in
  the codebase.
- **Roll semantics live in two places that must stay in lockstep**: the animated
  path in `app` and `evaluate`. A test would fail if they diverge, but keep them
  together when editing rules (explode/keep-drop/multiply order). Cross-cutting
  rules (verdict, crit/fumble) live once in the shared helpers listed above.
- **The RNG stays untouched**: throw power, verdicts, particles, and sound are
  all downstream of the same seedable RNG; a test asserts the same seed rolls
  identical values however the cup is thrown, and identically across shake / roll
  / insta / one-shot. `App::with_seed` rolls once on construction and the Bevy
  path deliberately does not re-roll, so `--seed` survives all four routes.
- **The sim is the single source of truth; the scene is a pure view.** Bevy
  entities never own state the sim needs — they mirror `app.dice` each frame.
  New per-frame camera behaviour goes into `view_math::live_camera` so the render
  and the overlays move together; new glam-crossing goes through `scene::convert`.
- Tests assert the simulation **always converges** under a hard frame cap (so a
  non-converging bug fails instead of hanging) and that no die escapes the arena
  or overlaps another at rest — all GPU-free in `tinhorn-core`.
- The help overlay must fit a 28-row terminal (a test pins this) — trim before
  adding lines to it.
- **The renderer is validated headless via `TINHORN_BEVY_SNAPSHOT`** (a PNG plus
  a text dump; `TINHORN_SNAP_COLS/ROWS/FRAME` tune it). The main-loop's three GPU
  render smoke tests are `#[ignore]`d because they need a real adapter; the chrome
  and overlays are covered GPU-free with ratatui's `TestBackend`. Eyeball the
  foley palette with `cargo test audible -- --ignored`. `TINHORN_BEVY_SNAPSHOT`
  **always forces `blocks`** (it composes to a `TestBackend`/PNG and has no TTY),
  so kitty never affects the snapshot or the chrome tests.
- **The kitty arena is an output-path feature only** — no changes to the sim,
  physics, RNG contract, or camera math (a test pins `--seed` identical across
  kitty / blocks / one-shot). Load-bearing rules: the **"cols × 2·rows" image
  shape is the single aspect source** — kitty raises only the *scale*, so
  `view_math::arena_aspect`/`project_to_cell` stay untouched and any cell-ratio
  mismatch is the same mild stretch half-blocks already have. `q=2` is **always**
  on every APC (a kitty response would land in crossterm's input stream and read as
  a keypress). Emission happens **strictly after `context.draw()`** (ratatui owns
  stdout during a draw). `auto` **never picks kitty under tmux/screen** (they eat
  the APC). All kitty code lives in `graphics`, never the vendored `term/` (the
  PROVENANCE re-sync contract) — its own `term/…/kitty.rs` is the unrelated
  keyboard protocol.
- Audio opens the **default output device only** (`foley.rs::open_sink`, on the
  audio thread). Never call rodio's `open_default_sink()` or enumerate devices:
  the fallback walks every audio device including microphones, which is its own
  way to draw the macOS mic prompt, and it prints to stderr over the TUI. There
  is no input path in this program; if the default device won't open, go silent.
  Note: on macOS with a duplex default output (USB interface, headset), the
  OS raises a one-time microphone prompt for ANY playback — even `afplay`.
  That is not fixable in code (a bespoke AudioUnit backend was tried and
  reverted); the README documents it. Don't reinvestigate.
- The README is user-facing only; design notes, test docs, and the invariant
  list for humans live in `CONTRIBUTING.md`. When a module description or
  invariant changes, update CONTRIBUTING.md alongside this file.
- **Commits follow [Conventional Commits](https://www.conventionalcommits.org)**:
  `type(optional scope): description`, where type is one of
  `feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert` (append `!`
  before the colon for a breaking change). release-plz reads these to choose
  the version bump and group the changelog, so they earn their keep. A
  PreToolUse hook (`.claude/hooks/conventional-commit.sh`) blocks any
  `git commit -m` whose subject doesn't match — write the type in from the start.

## Voice & tone — "the honest tinhorn"

The name is self-deprecating (a tinhorn talks big at small stakes); the brand
inverts it: talks like a small-time hustler, delivers scrupulous fairness.
Tagline: *a dice cup for your terminal* — the name doesn't say what the app
does, so the tagline must. Four rules for all user-facing
copy (README, help overlay, arena titles, release notes, launch posts):

1. **Wry, never cosplay.** A dusty phrase as seasoning — "the arena hands
   down the verdict", "a timid lob" — never cowboy dialect.
2. **Sensory, not abstract.** Everything is heard and felt: clatter, thunk,
   shudder. Never "generates random numbers".
3. **Every brag is checkable.** A fairness or fidelity claim ships with its
   mechanism (the seedable RNG, the test that pins it, `--seed`). Flavor
   with no mechanism behind it gets cut.
4. **Dry where it's plumbing.** stderr, `--help`, JSON, and exit codes stay
   deadpan and terse. The contrast with the TUI's theater is the charm —
   don't spend flavor in the scripting surface.
