# Contributing to tinhorn

Glad you pulled up a chair. The whole works sits on the table; the notes
below tell you where everything is and which walls are load-bearing.

## Getting started

```sh
git clone https://github.com/puradox/tinhorn
cd tinhorn
cargo run --release            # the TUI (release build — the animation wants LTO)
cargo run --release -- 3d6     # rolling 3d6 immediately
cargo run --release -- -p 3d6  # one-shot: print the total and exit
```

On Linux, building sound needs the ALSA headers: `libasound2-dev`
(Debian/Ubuntu) or `alsa-lib-devel` (Fedora), plus `pkg-config`. macOS and
Windows need nothing extra.

There is no lint or fmt config beyond the defaults. Before sending a PR:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings   # CI holds PRs to this
cargo test
```

If a change alters what the user sees or how rolls behave, update the
README in the same PR.

## Design

A handful of modules behind a 60 fps event loop — `parse`, `app`, `physics`,
`ui`, `cli`, `foley`, plus the vendored `render3d` rasterizer and its
`render3d_view` bridge:

- **`parse`** — a hand-written parser that turns notation into a `Roll`: a list
  of **dice terms** (each a count, a side count, and its modifiers — keep/drop,
  explode, multiply) plus an integer flat modifier and the optional `Stake` (a
  target with a `Goal`: meet-or-beat `> N` / `vs N`, or roll-under `< N`). Pure
  and unit-tested.
- **`app`** — the state, the roll evaluator, and the glue that drives the dice
  through the rigid-body sim. Each die is a real 3D body in the `physics`
  world (Rapier): `app` decides its value up front with the seeded RNG, spawns
  and launches its body, syncs its pose (`pos`/`rot`) back every step, and
  settles it when the body sleeps — or freezes it in place at the hard
  airborne cap, so the simulation always converges. The physics advances on a
  **fixed 1/60 s timestep** (`physics::STEP`, fed by an accumulator), which is
  what makes insta and animated rolls settle bit-identically. While a die
  tumbles its number is a spin-derived decoy (no RNG draw); the real value
  burns in the instant it settles — the animation just shows off a total that
  was already decided, so the displayed total always matches the real one.
  **Exploding** is the one thing that happens *during* the animation.
  The Throw lives here too (the shake clock, the cup, the launch), along with
  the Tab-cycled roll modes — insta fast-forwards this same simulation
  between two frames — plus the crit particles and a queue of pure
  `SoundEvent`s the physics emits.
  `evaluate()` resolves a roll instantly for the one-shot CLI with **exactly
  the same semantics** as the animation, and single-source helpers (`check`,
  `verdict_text`, `crit_face`/`fumble_face`) keep the verdict and crit rules
  identical across the TUI, the CLI, and the stats sampler.
- **`physics`** — the 3D rigid-body world on **Rapier**: a fixed box tray
  (floor, walls, ceiling) and one dynamic body per die with a convex-hull
  collider from its polyhedron. A small API (`spawn`/`launch`/`step`/`pose`/
  `sleeping`/`freeze`/`clear`) keeps Rapier out of every other module. It
  decides only where dice land — never their values.
- **`ui`** — ratatui rendering: the 3D arena — a rendered dice tray (a textured
  felt floor inside a solid mahogany lip — a back wall and two side walls with a
  top rail, under a warm overhead point light with a cool rim behind) sitting on a
  small wood table over a dark-red casino rug in a dark
  casino room, heavy oxblood stage curtains — free-hanging drapes with
  scallop-edged real corrugated folds, velvet-textured — flanking the
  background, the whole
  scene fading into
  **depth fog** so the far floor recedes,
  with tumbling polyhedra straight
  from the physics and a number riding each die's frontmost face (`dice::face_geometry`
  finds it from the die's orientation) — a dim decoy that ducks out as the die
  rolls edge-on, burning to the real value on settle, and scaling uniformly across
  the roll from a single cell to a dark-outlined half-block glyph — centred on and
  contained within the die — as a wide terminal renders the dice large — an open tin cup that sways
  while shaking with its power meter above, the release echo, crit and fumble
  particles, a result panel with per-die colour-coded chips and the staked
  verdict, an editable input line, and a help bar. The result panel **holds the
  outcome back until the dice stop** — each chip a dim decoy until its die
  settles, the total a dim `…` until every die has landed — so the animation
  never spoils the number it's building toward.
- **`cli`** — clap argument parsing and the one-shot output paths (bare
  total, verbose breakdown, JSON), plus the staked exit-code contract.
- **`foley`** — procedural sound synthesis: `SoundEvent`s in, sample buffers
  out (rodio), pitch from die size and loudness from impact speed. Runs on a
  dedicated audio thread that spawns lazily on the first audible sound (so the
  render loop never blocks on device start-up); degrades silently without an
  audio device.

The main loop draws a frame, polls for a key for up to one frame budget,
advances the simulation by the real elapsed `dt` (which `app` consumes in
fixed 1/60 s physics steps), then plays whatever the physics wanted heard.
Decoupling the roll result from the animation keeps the physics free to be as
chaotic as it likes.

## House rules (the invariants)

Tests guard most of these, but know them before you lean on a wall:

- **The RNG stays untouched.** Every die's value is decided up front by the
  seedable RNG; throw power, verdicts, particles, and sound are all
  downstream of it. Tests pin that the same seed rolls the same dice whether
  you lob, rocket, or insta.
- **Roll semantics live in two places that must agree**: the animated path in
  `app` and the instant `evaluate()`. Editing roll rules (explode → keep/drop
  → multiply order) means editing both together. Cross-cutting rules
  (verdict, crit/fumble) live exactly once, in the shared helpers — never
  restate those comparisons inline.
- **No plain-letter hotkeys.** Bare letters must stay typeable — `kh`/`dh`
  need their `h`. Pane keys are chords or `?`; Enter rolls in the current
  mode; Tab cycles the mode; Space separates notation. The arrow keys are
  reserved for editing and scrolling, never a roll action: on the prompt
  `←`/`→` (and `Home`/`End`) walk the caret — `App.cursor` is a byte offset,
  kept valid by `cursor_byte()`, which every insert/delete/move and the
  renderer read through; while a pane is open the same arrows drive
  `App.pane_scroll` instead.
- **The help overlay fits a 28-row terminal.** A test pins it — trim a line
  before adding one. Taller panes (or a short frame) scroll: every overlay
  lays out its whole content (history included — it no longer trims to the
  frame) and `overlay_panel` takes a scroll offset, clamps it to the overflow,
  and returns the corrected value for `render` to store back into `pane_scroll`
  (so an over-scroll self-corrects next frame). `App::set_pane` owns the
  `pane` ↔ `pane_scroll` pairing — it's the only way pane changes should be
  made, so a new pane-opening path can't forget to rewind the scroll.
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
cargo test                                    # parser, physics, rendering, CLI, keys, foley
cargo test <name>                             # one test by substring, e.g. `cargo test typing_kh`
cargo test snapshot -- --ignored --nocapture  # print a rendered frame to eyeball layout
SNAP="4d6!kh3" cargo test snapshot -- --ignored --nocapture   # snapshot another expression
cargo test audible -- --ignored               # play the whole sound palette out loud
```

The suite covers notation parsing (including the `vs` grammar and its
overflow guards), that dice always converge to rest under a hard frame cap,
that they never escape the arena or overlap at rest, and that the UI renders
a full roll without panicking (via ratatui's `TestBackend`). The Throw is
pinned by tests that power shapes the launch but never the values, that the
expression is locked at pickup, that a fresh shake starts clean, and that
insta mode rolls the same dice as the animation under the same seed. Stakes
are tested across the TUI verdict, the CLI exit code, JSON fields, and the
stats odds — all backed by one shared rule — in both directions, meet-or-beat
(`> N` / `vs N`) and roll-under (`< N`). Key routing is tested so a
hotkey can never swallow a letter you need to type, and the foley synthesis
is unit-tested for range, pitch ordering, and loudness scaling.

## Releasing

Releases are automated by [release-plz](https://release-plz.dev) across two
workflows; a maintainer's only manual act is merging a PR.

- **`.github/workflows/release-plz.yml`** (on push to `main`) keeps a standing
  **Release PR** that bumps `Cargo.toml` and updates `CHANGELOG.md`. Merge it to
  cut a release: release-plz publishes to crates.io, pushes the `vX.Y.Z` tag, and
  creates the GitHub Release from the changelog.
- **`.github/workflows/cd.yml`** (on that release being *published*) builds
  `tinhorn` for four targets and attaches the archives plus `sha256` sums.

Two repository secrets are required:

- **`RELEASE_PLZ_TOKEN`** — a Personal Access Token with **Contents** and
  **Pull requests** read/write (fine-grained, scoped to this repo; or classic
  `repo`). release-plz runs under it so the release it publishes can trigger
  `cd.yml`: the default `GITHUB_TOKEN` cannot trigger further workflow runs. As
  a bonus it also lets CI run on the Release PR.
- **`CARGO_REGISTRY_TOKEN`** — a crates.io API token, for the publish.

Two things worth knowing:

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
