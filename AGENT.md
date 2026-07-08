# AGENT.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tinhorn` is a terminal dice roller. Type dice in standard notation, shake the
cup, and watch each die bounce around a physics arena as the 2D silhouette of
its polyhedron until it settles and the total is tallied. (A *tinhorn* was a
small-time gambler, named for the tin cup they rattled their dice in.) It has
two modes from a single binary:

- **Interactive TUI** (default): the bouncing-dice animation, built on ratatui.
- **One-shot CLI**: given an expression plus an output flag — or whenever stdout
  isn't a terminal — it skips the TUI, evaluates once, prints, and exits, so it
  drops into scripts and pipes (`tinhorn 3d6 | cat`).

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

Five modules behind a ~60 fps event loop (`main.rs`). The loop draws a frame,
polls for a key for up to one frame budget, advances the physics by the real
elapsed `dt`, then plays whatever sounds the physics queued.

- **`parse`** — hand-written parser: notation → `Roll` (a `Vec<DiceTerm>` + flat
  `i32` modifier + optional `vs` target for staked rolls). Each `DiceTerm` is
  count, sides, and modifiers (`TermMod`: keep/drop, explode with a `Compare`,
  multiply). Pure and unit-tested. Sizes are capped (≤ 60 dice, ≤ 1000 sides) so
  a huge expression can't wedge the renderer; the `vs` target must come last and
  is range-checked into `i32`.

- **`app`** — state plus the physics simulation and the roll evaluator.
  - `App` holds the dice, input line, `Pane` (Help/History/Stats overlays),
    history, session stats, the Throw state (shake clock, last release), crit
    particles, the `RollMode` (what Enter does; Tab cycles Shake → Roll →
    Insta), and the pending `SoundEvent` queue. `App::roll()` starts an
    animated roll; `start_shake()`/`throw()` are the Throw — the default mode
    (Enter shakes the cup, a second Enter releases; release timing shapes only
    the launch); `insta_roll()` fast-forwards the same simulation to rest
    between two frames (same RNG draws, so totals are seed-identical across
    all three modes); `update(dt)` steps the physics; `all_settled()` reports
    convergence.
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
  - Single-source rule helpers back both paths: `check()` (the `vs` meet-or-beat
    verdict, also used by the stats pane's success odds), `verdict_text()` (the
    SUCCESS/FAIL wording shared by TUI chip and CLI), and `crit_face()`/
    `fumble_face()` (any die maxing / rolling 1; drives particles, sounds, and
    the `crit`/`fumble` flags in JSON). Never restate these comparisons inline.

- **`ui`** — ratatui rendering: `render(frame, app)` paints the arena (each die
  drawn cell-by-cell at its float position via `die_shape`, which maps sides → a
  6×4 ASCII template), the shaking cup with its power meter, the release echo,
  crit/fumble particles, a result panel with colour-coded chips and the staked
  verdict, the input line, a help bar, and the Help/History/Stats overlays.

- **`cli`** — clap `Cli` and `run_one_shot`. Three output shapes: bare total
  (default), verbose breakdown (`-v`, dropped dice in `[brackets]`, exploded
  marked `!`), and `--json`. `--seed N` gives reproducible rolls in both modes.
  Under explicit `-p`/`-v`, a staked roll's exit code is the verdict (0/1); the
  implicit piped-stdout mode and `--json` always exit 0 on clean output.

- **`foley`** — procedural sound. `App` emits pure `SoundEvent`s (impacts,
  knocks, settles, cup rattle, crit ring, verdicts); `synth()` renders them from
  physics parameters (die size → pitch, impact speed → loudness) with no assets;
  `Foley` plays them via rodio, degrading silently with no audio device. On by
  default; `--mute` starts muted, Ctrl-Q toggles. Audio initializes **lazily**
  in `main.rs::run` on the first audible sound — a muted session never touches
  audio APIs at all (macOS raises a one-time microphone prompt for playback on
  duplex output devices; that's the OS, even `afplay` draws it — don't chase
  it).

## Conventions worth knowing

- **Key routing is deliberately constrained** (`main.rs::handle_key`, kept pure
  so it's unit-tested). Pane hotkeys use chords/`?` (Ctrl-H history, Ctrl-S stats,
  `?` help) specifically so bare `h`/`s` stay typeable for notation like `kh`/`dh`.
  Don't add a plain-letter hotkey — it will eat characters users need to type.
  Enter rolls in the current mode; Tab cycles the mode (shake → roll →
  insta); Space stays a notation separator. Mute is Ctrl-Q ("quiet") — never
  move it to Ctrl-M: on legacy encodings (e.g. Apple Terminal) Ctrl-M *is*
  Enter (ASCII CR), so it can't be a hotkey anywhere, which is also why there
  is no enhanced-keyboard machinery in the codebase.
- **Roll semantics live in two places that must stay in lockstep**: the animated
  path in `app` and `evaluate`. A test would fail if they diverge, but keep them
  together when editing rules (explode/keep-drop/multiply order). Cross-cutting
  rules (verdict, crit/fumble) live once in the shared helpers listed above.
- **The RNG stays untouched**: throw power, verdicts, particles, and sound are
  all downstream of the same seedable RNG; a test asserts the same seed rolls
  identical values however the cup is thrown.
- Tests assert the simulation **always converges** under a hard frame cap (so a
  non-converging bug fails instead of hanging) and that no die escapes the arena
  or overlaps another at rest.
- The help overlay must fit a 28-row terminal (a test pins this) — trim before
  adding lines to it.
- Demo recordings: `DEMO_OUT=/tmp/d.json cargo test record_demo -- --ignored`
  dumps real rendered frames + per-frame sound events as JSON for the HTML demo
  player; `cargo test audible -- --ignored` plays the foley palette out loud.
- Audio opens the **default output device only** (`foley.rs::Foley::new`).
  Never call rodio's `open_default_sink()` or enumerate devices: the fallback
  walks every audio device including microphones, which is its own way to
  draw the macOS mic prompt, and it prints to stderr over the TUI. There is
  no input path in this program; if the default device won't open, go silent.
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
Tagline register: *all rattle, honest dice.* Four rules for all user-facing
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
