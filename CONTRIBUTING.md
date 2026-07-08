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

Five small modules behind a 60 fps event loop:

- **`parse`** — a hand-written parser that turns notation into a `Roll`: a list
  of **dice terms** (each a count, a side count, and its modifiers — keep/drop,
  explode, multiply) plus an integer flat modifier and the optional `vs`
  target. Pure and unit-tested.
- **`app`** — the state plus a tiny physics simulation. Each die is an
  axis-aligned box with position/velocity; the engine applies gravity, bounces
  off the four walls with restitution, rubs off speed with floor friction and air
  drag, and tumbles its visible face every 50 ms while airborne. Dice also
  collide with **each other** — a per-frame AABB separation pass pushes
  overlapping pairs apart, and a die that lands off-centre on another *rolls
  off the edge*, so dice spread out instead of balancing in neat columns.
  A die comes to rest when it's slow and *supported*, snapping flush onto its
  support and locking to its pre-rolled value; the simulation always
  converges. Each die's value is decided up front by the RNG — the animation
  just shows it off — so the displayed total always matches the real total.
  **Exploding** is the one thing that happens *during* the animation.
  The Throw lives here too (the shake clock, the cup, the launch), along with
  the Tab-cycled roll modes — insta fast-forwards this same simulation
  between two frames — plus the crit particles and a queue of pure
  `SoundEvent`s the physics emits.
  `evaluate()` resolves a roll instantly for the one-shot CLI with **exactly
  the same semantics** as the animation, and single-source helpers (`check`,
  `verdict_text`, `crit_face`/`fumble_face`) keep the verdict and crit rules
  identical across the TUI, the CLI, and the stats sampler.
- **`ui`** — ratatui rendering: the arena (each die painted cell-by-cell at
  its float position as the 2D silhouette of its polyhedron), the swaying cup
  and its power meter, the release echo, crit and fumble particles, a result
  panel with per-die colour-coded chips and the staked verdict, an editable
  input line, and a help bar.
- **`cli`** — clap argument parsing and the one-shot output paths (bare
  total, verbose breakdown, JSON), plus the staked exit-code contract.
- **`foley`** — procedural sound synthesis: `SoundEvent`s in, sample buffers
  out (rodio), pitch from die size and loudness from impact speed. Opens
  lazily on the first audible sound; degrades silently without an audio
  device.

The main loop draws a frame, polls for a key for up to one frame budget,
advances the simulation by the real elapsed `dt`, then plays whatever the
physics wanted heard. Decoupling the roll result from the animation keeps the
physics free to be as chaotic as it likes.

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
  mode; Tab cycles the mode; Space separates notation.
- **The help overlay fits a 28-row terminal.** A test pins it — trim a line
  before adding one.
- **Audio opens the default output device only, lazily.** Never call rodio's
  `open_default_sink()` or enumerate devices — the fallback walks every
  audio device including microphones (its own way to draw the macOS mic
  prompt) and prints to stderr over the TUI. There is no input path in this
  program; if the default device won't open, go silent. Known and accepted:
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
stats odds — all backed by one shared rule. Key routing is tested so a
hotkey can never swallow a letter you need to type, and the foley synthesis
is unit-tested for range, pitch ordering, and loudness scaling.

## The demo GIF

The README's GIF is built from real captured frames:
`DEMO_OUT=/tmp/demo.json cargo test record_demo -- --ignored` dumps rendered
frames plus per-frame sound events as JSON, ready for re-rendering by the
demo player. If a change alters what the TUI shows, re-record.

## Licensing

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as MIT OR Apache-2.0, without any additional
terms or conditions.
