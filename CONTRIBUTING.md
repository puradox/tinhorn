# Contributing to tinhorn

Glad you pulled up a chair. The whole works sits on the table; the notes
below tell you where everything is and which walls are load-bearing.

## Getting started

```sh
git clone https://github.com/puradox/tinhorn
cd tinhorn
cargo run --release            # the TUI (release build — LTO on)
cargo run --release -- 3d6     # rolling 3d6 immediately
cargo run --release -- -p 3d6  # one-shot: print the total and exit
```

The interactive arena renders on the GPU through **Bevy** (headless, read back to
the terminal). A first release build pulls Bevy in and takes a while; for
iteration, `cargo run --features bevy/dynamic_linking` links Bevy dynamically and
compiles fast. (The workspace already gives dependencies real optimization in dev
via `profile.dev.package."*"`, so the animation isn't a slideshow unoptimized.)

On Linux, building sound needs the ALSA headers: `libasound2-dev`
(Debian/Ubuntu) or `alsa-lib-devel` (Fedora), plus `pkg-config`. Bevy's usual
Linux build deps apply too (see the Bevy book). macOS and Windows need nothing
extra for our code.

There is no lint or fmt config beyond the defaults. Before sending a PR:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings   # CI holds PRs to this
cargo test
```

If a change alters what the user sees or how rolls behave, update the
README in the same PR.

## The workspace

tinhorn is a **virtual Cargo workspace** of two crates, with one vendored fork
kept out of it:

- **`crates/tinhorn-core`** — the library: everything renderer-agnostic. The
  hand-written parser, the seeded roll evaluator and the ceremony (`app`), the
  Rapier `physics` world, the dice `dice_geom`etry (also the physics hulls), and
  the `view_math` camera. Its only dependencies are `rand`, `serde`, `glam`, and
  `rapier3d` — **no ratatui, no Bevy, no rodio, no clap** — so the crate stays GPU-
  and terminal-free and its ~64 sim/ceremony tests run without a display.
- **`crates/tinhorn`** — the binary: the Bevy `scene`, the ratatui `ui` chrome and
  arena overlays, `cli`, `foley`, the small `paint` colour/texture types, and the
  vendored `term` terminal integration. It re-exports
  `tinhorn_core::{app, parse, physics}` so its modules keep saying `crate::app`.
- **`src/term/`** — the terminal integration, a **vendored in-tree snapshot** of the
  `bevy_ratatui` fork (see `src/term/PROVENANCE.md`), trimmed to the crossterm path.
  A plain module, not a path dependency, so the crate publishes.

## Design

The roll rules and the 3D sim live in `tinhorn-core`; the terminal, the GPU, and
the audio live in the binary. The interactive arena is a headless **Bevy** app on
a `ScheduleRunnerPlugin` at ~60 fps — the core `App` is the single source of
truth, and the Bevy entities are a pure view of it.

- **`parse`** (core) — a hand-written parser that turns notation into a `Roll`: a
  list of **dice terms** (each a count, a side count, and its modifiers — keep/drop,
  explode, multiply) plus an integer flat modifier and the optional `Stake` (a
  target with a `Goal`: meet-or-beat `> N` / `vs N`, or roll-under `< N`). Pure
  and unit-tested.
- **`app`** (core) — the state, the roll evaluator, and the glue that drives the
  dice through the rigid-body sim. Each die is a real 3D body in the `physics`
  world (Rapier): `app` decides its value up front with the seeded RNG, spawns
  and launches its body, syncs its pose (`pos`/`rot`) back every step, and
  settles it when the body sleeps — or freezes it in place at the hard
  airborne cap, so the simulation always converges. The physics advances on a
  **fixed 1/60 s timestep** (`physics::STEP`, fed by an accumulator), which is
  what makes insta and animated rolls settle bit-identically. `App::new`/
  `with_seed` **roll a non-empty initial expression on construction** (this is
  how `-- 3d6` rolls straight away), so the Bevy path must not re-roll. While a
  die tumbles its number is a spin-derived decoy (no RNG draw); the real value
  burns in the instant it settles — the animation just shows off a total that
  was already decided, so the displayed total always matches the real one.
  **Exploding** is the one thing that happens *during* the animation.
  The Throw lives here too (the shake clock, the cup sway, the launch), along
  with the Tab-cycled roll modes — insta fast-forwards this same simulation
  between two frames — plus the crit particles and a queue of pure
  `SoundEvent`s the physics emits.
  `evaluate()` resolves a roll instantly for the one-shot CLI with **exactly
  the same semantics** as the animation, and single-source helpers (`check`,
  `verdict_text`, `crit_face`/`fumble_face`) keep the verdict and crit rules
  identical across the TUI, the CLI, and the stats sampler.
- **`physics`** (core) — the 3D rigid-body world on **Rapier**: a fixed box tray
  (floor, walls, ceiling) and one dynamic body per die with a convex-hull
  collider from its polyhedron. A small API (`spawn`/`launch`/`step`/`pose`/
  `sleeping`/`freeze`/`clear`) keeps Rapier out of every other module. It
  decides only where dice land — never their values.
- **`dice_geom`** (core) — the six standard polyhedra as plain glam data, the
  single source of the dice shapes: the physics collider is the convex hull of a
  die's mesh, and the Bevy renderer builds its `Mesh` from the same data, so the
  die you see is the die the sim bounces. `face_geometry` gives each face's
  centroid + outward normal so the number can ride the frontmost face; `cup()`
  is the open tin tumbler.
- **`view_math`** (core) — the arena camera and the pure world→screen map, glam
  only. `arena_camera`/`live_camera` pose the eye and move it through the throw;
  `project_to_cell` places a world point in a terminal cell. The Bevy scene and
  the `ui` overlays both build their camera here, and the crit/fumble bursts
  project through the *same* `live_camera`, so the dice, their burned numbers,
  and the bursts can never disagree about where a die is.
- **`scene`** (binary) — the Bevy dice arena, the default renderer. `Sim(App)` is
  the single source of truth; systems mirror it into entities each frame:
  `input_system` feeds keys to the shared `handle_key`, `advance_sim` steps the
  sim, `sync_dice_scene` diffs `app.dice` into `DieView` entities, `sync_cup`
  shows/sways the cup while shaking, `choreograph` moves the camera to match
  `view_math::live_camera` (and flinches the key light on impacts / a crit),
  `draw_ui` composes the frame, and `drain_sounds` plays what the physics queued.
  A headless `Camera3d` renders into an offscreen image that's **read back to the
  CPU with Bevy 0.19's built-in `gpu_readback`** (`Readback` + `ReadbackComplete`)
  and blitted as half-blocks — with HDR + filmic tonemapping, MSAA, screen-space
  ambient occlusion, shadow maps, distance fog, a 2× supersample, and a warm
  vignette. `scene/arena.rs` builds the static casino furniture (felt, mahogany
  walls + rails, table, rug, oak floor, gradient backdrop, oxblood curtains,
  poker chips) from `ui`'s `ArenaStyle` palette and its procedural textures;
  `scene/convert.rs` is the *one* place core's glam 0.33 crosses into Bevy's glam.
- **`ui`** (binary) — ratatui rendering, now the **chrome and the arena overlays**
  rather than a 3D renderer. `render_bevy` lays out the arena panel, the result
  panel, the input line, and the help/Help/History/Stats panes, blits the Bevy
  read-back into the arena, and draws the overlays: the number riding each die's
  frontmost face (a dim decoy that ducks out edge-on, burning to the real value
  on settle and scaling from a single cell to a dark-outlined half-block glyph as
  a wide terminal renders the dice large), the power meter, the release echo, and
  the crit/fumble particles. It also owns the `ArenaStyle` palette and the
  procedural texture generators (felt, floorboards, velvet, wood grain, backdrop)
  the Bevy furniture wraps as images. The result panel **holds the outcome back
  until the dice stop** — each chip a dim decoy until its die settles, the total a
  dim `…` until every die has landed — so the animation never spoils the number
  it's building toward.
- **`paint`** (binary) — the small `Rgb`/`Texture` types the overlays and the
  procedural textures use; they outlived the deleted software rasterizer.
- **`cli`** (binary) — clap argument parsing and the one-shot output paths (bare
  total, verbose breakdown, JSON), plus the staked exit-code contract.
- **`foley`** (binary) — procedural sound synthesis: `SoundEvent`s in, sample
  buffers out (rodio), pitch from die size and loudness from impact speed. Runs on
  a dedicated audio thread that spawns lazily on the first audible sound (so the
  render loop never blocks on device start-up); degrades silently without an
  audio device.

Every frame, the Bevy schedule reads keys into the shared `handle_key`, steps the
sim by the real elapsed `dt` (which `app` consumes in fixed 1/60 s physics steps),
renders the scene on the GPU, reads it back, composes it under the ratatui chrome,
and plays whatever the physics wanted heard. Decoupling the roll result from the
animation keeps the physics free to be as chaotic as it likes.

## House rules (the invariants)

Tests guard most of these, but know them before you lean on a wall:

- **The RNG stays untouched.** Every die's value is decided up front by the
  seedable RNG; throw power, verdicts, particles, and sound are all
  downstream of it. Tests pin that the same seed rolls the same dice whether
  you lob, rocket, insta, or print — and `App::with_seed` rolls once on
  construction while the Bevy path deliberately doesn't re-roll, so `--seed`
  survives every route.
- **The sim is the single source of truth; the Bevy scene is a pure view.** The
  entities mirror `app.dice` each frame and own no state the sim needs. New
  per-frame camera behaviour belongs in `view_math::live_camera` (so the render
  and the overlays move together), and the only glam-0.33 → Bevy-glam crossing
  goes through `scene::convert` — a stray direct assignment is a compile error,
  not a silent unit bug.
- **Roll semantics live in two places that must agree**: the animated path in
  `app` and the instant `evaluate()`. Editing roll rules (explode → keep/drop
  → multiply order) means editing both together. Cross-cutting rules
  (verdict, crit/fumble) live exactly once, in the shared helpers — never
  restate those comparisons inline.
- **No plain-letter hotkeys.** Bare letters must stay typeable — `kh`/`dh`
  need their `h`. Pane keys are chords or `?`; Enter rolls in the current
  mode; Tab cycles the mode; Space separates notation. Mute is Ctrl-Q, never
  Ctrl-M (on legacy encodings Ctrl-M *is* Enter/CR). `handle_key` is pure and
  unit-tested, and the Bevy `input_system` calls it verbatim, so the routing is
  proven once and shared. The arrow keys are reserved for editing and scrolling,
  never a roll action: on the prompt `←`/`→` (and `Home`/`End`) walk the caret —
  `App.cursor` is a byte offset, kept valid by `cursor_byte()`, which every
  insert/delete/move and the renderer read through; while a pane is open the same
  arrows drive `App.pane_scroll` instead.
- **The one-shot CLI never touches the GPU.** `main()` runs the headless-snapshot
  short-circuit first (an explicit `TINHORN_BEVY_SNAPSHOT`, which *wants* no TTY),
  then the one-shot path (an output flag or a non-terminal stdout) which calls
  `cli::run_one_shot` and returns *before* any Bevy `App` is constructed. So
  `tinhorn 3d6 | cat`, `-p`, `-v`, and `--json` stay pure and GPU-free.
- **The help overlay fits a 28-row terminal.** A test pins it — trim a line
  before adding one. Taller panes (or a short frame) scroll: every overlay
  lays out its whole content (history included) and `overlay_panel` takes a
  scroll offset, clamps it to the overflow, and returns the corrected value for
  `render_bevy` to store back into `pane_scroll` (so an over-scroll self-corrects
  next frame). `App::set_pane` owns the `pane` ↔ `pane_scroll` pairing — it's the
  only way pane changes should be made, so a new pane-opening path can't forget to
  rewind the scroll.
- **Audio opens the default output device only, lazily, on the audio thread.**
  Opening it blocks for tens of ms, so it lives on a dedicated thread that
  spawns on the first audible sound — never on the render loop, where it would
  be a one-frame hitch. Never call rodio's `open_default_sink()` or enumerate
  devices — the fallback walks every audio device including microphones (its
  own way to draw the macOS mic prompt) and prints to stderr over the TUI.
  There is no input path in this program; if the default device won't open, go
  silent. Known and accepted:
  on macOS a duplex default output (USB interface, headset) draws a one-time
  microphone prompt for any playback, even `afplay` — that's the OS, not
  fixable in code; the README documents it.
- **User-facing copy follows the voice rules** in [AGENT.md](AGENT.md)
  ("the honest tinhorn"): wry never cosplay, sensory not abstract, every brag
  checkable, dry where it's plumbing.

## Tests

```sh
cargo test                                    # parser, physics, evaluator, CLI, keys, chrome, foley
cargo test -p tinhorn-core                    # just the ~64 GPU-free sim/ceremony tests
cargo test <name>                             # one test by substring, e.g. `cargo test typing_kh`
cargo test audible -- --ignored               # play the whole sound palette out loud
```

`cargo test` compiles Bevy (the binary depends on it) but runs GPU-free: the
three arena render smoke tests are `#[ignore]`d because they need a real adapter.
The chrome and overlays are exercised through ratatui's `TestBackend` — the caret
and scrolling, the withheld outcome, the staked verdict, the dropped-die dimming,
the number ducking out edge-on and scaling with the die — none of which touch the
GPU. To actually see the rendered arena without a TTY, render it headless:

```sh
TINHORN_BEVY_SNAPSHOT=/tmp/arena.png cargo run -- 4d6!kh3   # PNG + a text frame dump
TINHORN_SNAP_COLS=120 TINHORN_SNAP_ROWS=44 …                # compose at a larger size
TINHORN_SNAP_FRAME=8 …                                      # capture mid-roll, not settled
```

wgpu runs headless, so the snapshot works in CI and over ssh. Beyond the chrome,
the suite covers notation parsing (including the `vs` grammar and its overflow
guards), that dice always converge to rest under a hard frame cap, and that they
never escape the arena or overlap at rest. The Throw is pinned by tests that power
shapes the launch but never the values, that the expression is locked at pickup,
that a fresh shake starts clean, and that insta mode rolls the same dice as the
animation under the same seed. Stakes are tested across the TUI verdict, the CLI
exit code, JSON fields, and the stats odds — all backed by one shared rule — in
both directions, meet-or-beat (`> N` / `vs N`) and roll-under (`< N`). Key routing
is tested so a hotkey can never swallow a letter you need to type, and the foley
synthesis is unit-tested for range, pitch ordering, and loudness scaling.

## Releasing

Releases are automated by [release-plz](https://release-plz.dev) across two
workflows; a maintainer's only manual act is merging a PR.

- **`.github/workflows/release-plz.yml`** (on push to `main`) keeps a standing
  **Release PR** that bumps the crate versions and updates `CHANGELOG.md`. Merge
  it to cut a release: release-plz publishes to crates.io, pushes the `vX.Y.Z`
  tag, and creates the GitHub Release from the changelog.
- **`.github/workflows/cd.yml`** (on that release being *published*) builds
  `tinhorn` for four targets and attaches the archives plus `sha256` sums.

Two repository secrets are required:

- **`RELEASE_PLZ_TOKEN`** — a Personal Access Token with **Contents** and
  **Pull requests** read/write (fine-grained, scoped to this repo; or classic
  `repo`). release-plz runs under it so the release it publishes can trigger
  `cd.yml`: the default `GITHUB_TOKEN` cannot trigger further workflow runs. As
  a bonus it also lets CI run on the Release PR.
- **`CARGO_REGISTRY_TOKEN`** — a crates.io API token, for the publish.

Three things worth knowing:

- **Publish `tinhorn-core` before `tinhorn`.** The binary depends on the library
  by version, so release-plz (or a manual publish) must push `tinhorn-core` to
  crates.io first, then `tinhorn` — the normal two-crate workspace order. There is
  no longer a git-fork blocker: the old `bevy_ratatui` path dependency is vendored
  in-tree as `src/term/`, so both crates publish with only crates.io dependencies.
- release-plz reads **conventional-commit** prefixes (`feat:`, `fix:`) to choose
  the version bump and group the changelog. Commits without them still release,
  but as a patch bump under a flat changelog.
- `cd.yml` covers the four targets that build natively on GitHub runners.
  `aarch64-unknown-linux-gnu` is left out on purpose: rodio links ALSA, so
  cross-compiling to another Linux arch needs that C library for the target too.

## Licensing

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as MIT OR Apache-2.0, without any additional
terms or conditions.
