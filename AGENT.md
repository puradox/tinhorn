# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`roll` is a terminal dice roller. Type dice in standard notation and watch each
die bounce around a physics arena as the 2D silhouette of its polyhedron until it
settles and the total is tallied. It has two modes from a single binary:

- **Interactive TUI** (default): the bouncing-dice animation, built on ratatui.
- **One-shot CLI**: given an expression plus an output flag — or whenever stdout
  isn't a terminal — it skips the TUI, evaluates once, prints, and exits, so it
  drops into scripts and pipes (`roll 3d6 | cat`).

## Commands

```sh
cargo run --release            # start the TUI empty
cargo run --release -- 3d6     # TUI, rolling 3d6 immediately
cargo run --release -- -p 3d6  # one-shot: print total and exit
cargo build --release          # release build (LTO on; the animation wants it)

cargo test                     # parser, physics, rendering, CLI, key routing
cargo test <name>              # run a single test by substring, e.g. `cargo test typing_kh`
cargo test snapshot -- --ignored --nocapture   # print a rendered frame to eyeball layout
SNAP="4d6!kh3" cargo test snapshot -- --ignored --nocapture  # override the snapshot expression
```

There is no lint/fmt config beyond defaults; use `cargo fmt` and `cargo clippy`.

## Architecture

Four modules behind a ~60 fps event loop (`main.rs`). The loop draws a frame,
polls for a key for up to one frame budget, then advances the physics by the real
elapsed `dt`.

- **`parse`** — hand-written parser: notation → `Roll` (a `Vec<DiceTerm>` + flat
  `i32` modifier). Each `DiceTerm` is count, sides, and modifiers (`TermMod`:
  keep/drop, explode with a `Compare`, multiply). Pure and unit-tested. Sizes are
  capped (≤ 60 dice, ≤ 1000 sides) so a huge expression can't wedge the renderer.

- **`app`** — state plus the physics simulation and the roll evaluator.
  - `App` holds the dice, input line, `Pane` (Help/History/Stats overlays),
    history, and session stats. `App::roll()` starts an animated roll; `update(dt)`
    steps the physics; `all_settled()` reports convergence.
  - Physics: each `Die` is an AABB with position/velocity under gravity, wall
    bounces with restitution, friction/drag, per-frame die-vs-die AABB
    separation, and roll-off so dice spread instead of stacking neatly.
  - **Every die's value is decided up front by the RNG** — the animation only
    shows it off — so the displayed total always matches the real total. The one
    exception is **exploding**, which plays out *during* the animation: a die that
    settles on a qualifying face drops another die into the arena (capped per term
    so it always converges).
  - `evaluate(expr, &Roll, &mut StdRng) -> Outcome` resolves a roll *instantly*
    into a full breakdown (`Outcome`/`OutcomeTerm`/`OutcomeDie`, all `serde`). It
    **mirrors the animation's semantics exactly** — explode → keep/drop on the
    base pool → per-term multiply → flat modifier. This is the shared contract:
    the one-shot CLI and the TUI must agree, so changes to roll rules belong here
    and in the animated path together.

- **`ui`** — ratatui rendering: `render(frame, app)` paints the arena (each die
  drawn cell-by-cell at its float position via `die_shape`, which maps sides → a
  6×4 ASCII template), a result panel with colour-coded chips, the input line, a
  help bar, and the Help/History/Stats overlays.

- **`cli`** — clap `Cli` and `run_one_shot`. Three output shapes: bare total
  (default), verbose breakdown (`-v`, dropped dice in `[brackets]`, exploded
  marked `!`), and `--json`. `--seed N` gives reproducible rolls in both modes.

## Conventions worth knowing

- **Key routing is deliberately constrained** (`main.rs::handle_key`, kept pure
  so it's unit-tested). Pane hotkeys use chords/`?` (Ctrl-H history, Ctrl-S stats,
  `?` help) specifically so bare `h`/`s` stay typeable for notation like `kh`/`dh`.
  Don't add a plain-letter hotkey — it will eat characters users need to type.
- **Roll semantics live in two places that must stay in lockstep**: the animated
  path in `app` and `evaluate`. A test would fail if they diverge, but keep them
  together when editing rules (explode/keep-drop/multiply order).
- Tests assert the simulation **always converges** under a hard frame cap (so a
  non-converging bug fails instead of hanging) and that no die escapes the arena
  or overlaps another at rest.
