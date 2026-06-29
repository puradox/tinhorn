# roll 🎲

A terminal dice roller. Type dice in common notation and watch them bounce
around the screen, tumbling through random faces until they settle and the
total is tallied. Each die is drawn as the 2D silhouette of its polyhedron.
Built in Rust with [ratatui](https://ratatui.rs).

```
┌ 🎲  roll — settled ───────────────────────────────────────────────────┐
│                                                                       │
│        ╱╲       ┌────┐     ╱╲      ╱‾‾╲     ╱╲       ╱‾‾╲              │
│       ╱  ╲      │    │    ╱  ╲    ╱    ╲   ╱  ╲     │ 18 │             │
│      ╱ 4  ╲     │ 6  │    ╲2 ╱    ╲ 10 ╱  │ 11 │    │    │             │
│      ‾‾‾‾‾‾     └────┘     ╲╱       ╲╱    └────┘     ╲__╱              │
└───────────────────────────────────────────────────────────────────────┘
┌ result ───────────────────────────────────────────────────────────────┐
│[4] [6] [2] [10] [11] [18]                                             │
│  Σ total  51                                                          │
└───────────────────────────────────────────────────────────────────────┘
dice ▸ d4+d6+d8+d10+d12+d20█
 › Enter roll  Esc quit   try: 3d6 · 2d20kh1 · 4d6dl1 · 3d6! · 4d6*2
```

Shapes by die: **d4** triangle · **d6** square · **d8** diamond ·
**d10** kite · **d12** pentagon · **d20** hexagon. Other sizes fall back to a
plain box.

## Run

```sh
cargo run --release            # start empty, type an expression
cargo run --release -- 3d6     # roll immediately
cargo run --release -- "d6+d8" # quote anything with shell-special chars
```

## Keys

| Key            | Action                              |
| -------------- | ----------------------------------- |
| `Enter`        | roll / re-roll the current dice     |
| `?`            | toggle the dice-notation help overlay |
| `Ctrl-H`       | toggle the roll-history pane        |
| `Ctrl-S`       | toggle the statistics pane          |
| type / `Backspace` | edit the dice expression        |
| `Esc` / `Ctrl-C` | quit (`Esc` closes an open pane first) |

The history and statistics panes use `Ctrl` chords, not bare letters, so that
`h` and `s` stay free to type — `kh`/`dh` and any expression containing those
letters would otherwise be swallowed by a hotkey.

The three pop-out panes (`?` / `Ctrl-H` / `Ctrl-S`) float over the running animation —
the dice keep bouncing underneath — and are mutually exclusive; opening one
closes the others. `Esc` or `q` dismisses whichever is open.

**History** (`h`) lists recent rolls, newest first, with the kept faces and the
total (in-memory only — it's cleared on quit). **Statistics** (`s`) shows the
theoretical odds for the expression currently in the input box — min, max,
average, and a small distribution curve, estimated by sampling the roll many
times so every modifier (keep/drop, exploding, multiply) is accounted for —
alongside a summary of the rolls of that expression you've actually made this
session.

## Dice notation

A roll is a sequence of **dice terms** and optional **flat modifiers**, in any
combination. Terms can be separated by `+`, `,`, whitespace, or simply written
next to each other.

| Input      | Meaning                              |
| ---------- | ------------------------------------ |
| `3d6`      | three six-sided dice                 |
| `d6+d8`    | one d6 and one d8                    |
| `d6d10`    | adjacency works as a separator       |
| `d6,d12`   | commas work too                      |
| `2d20-1`   | two d20 with a −1 modifier           |
| `d20 + 5`  | whitespace is ignored                |

`d6` means `1d6`. Sizes are capped (≤ 60 dice, ≤ 1000 sides) so a fat-fingered
`999d99999` can't wedge the renderer.

### Per-die modifiers

A dice term can carry modifiers written right after the `dN`. They apply in
pool order — **explode → keep/drop → multiply** — and can be stacked.

| Input      | Meaning                                                  |
| ---------- | -------------------------------------------------------- |
| `2d20kh1`  | **advantage** — roll two d20, keep the highest 1         |
| `2d20kl1`  | **disadvantage** — keep the lowest 1                     |
| `4d6dl1`   | drop the lowest 1 (the classic ability-score roll)       |
| `4d6dh1`   | drop the highest 1                                        |
| `3d6!`     | **exploding** — a max face rolls another die (repeats)   |
| `d10!>8`   | explode on any face `> 8` instead of just the max        |
| `d6!=6`    | explode on exactly 6 (`>`/`<`/`=` all work)              |
| `4d6*2`    | multiply *this term's* kept sum by 2                      |
| `4d6!kh3*2`| stack them: explode, keep the best 3, then double         |

`kh`/`kl`/`dh`/`dl` default to 1 (`2d20kh` = `2d20kh1`) and clamp to the pool
size. Dropped dice are still thrown and bounce around — you watch advantage
discard the lower d20 — but they're rendered dimmed and left out of the total.
Exploding plays out live: a die that *settles* on a qualifying face drops one
more die into the arena, which can explode in turn — capped at 40 extra dice per
term so `d2!` can't grow without bound. A multiplier binds to its own term: in
`3d6*2 + d8` only the d6 sum is doubled.

## Design

Three small modules behind a 60 fps event loop:

- **`parse`** — a hand-written parser that turns notation into a `Roll`: a list
  of **dice terms** (each a count, a side count, and its modifiers — keep/drop,
  explode, multiply) plus an integer flat modifier. Pure and unit-tested.
  `app` rolls each term's base pool, applies keep/drop (flags dice out of the
  total) and multiply (scales the term's sum), and tags each die with its term's
  explode condition. The total is summed per term, so a `*N` multiplier scales
  only its own dice.
- **`app`** — the state plus a tiny physics simulation. Each die is an
  axis-aligned box with position/velocity; the engine applies gravity, bounces
  off the four walls with restitution, rubs off speed with floor friction and air
  drag, and tumbles its visible face every 50 ms while airborne. Dice also
  collide with **each other** — a per-frame AABB separation pass pushes
  overlapping pairs apart, so they bounce off one another. A die that lands
  off-centre on another converts part of the impact into sideways motion and
  *rolls off the edge*, so dice spread out instead of balancing in neat columns;
  when the arena is too narrow to spread they pile into stacks instead.
  A die comes to rest when it's slow and *supported* (by the floor or by a
  settled die beneath it), snapping flush onto its support and locking to its
  pre-rolled value; a die wedged in an over-tall stack that the arena can't hold
  settles in place after a short timeout so the simulation always converges.
  Each die's value is decided up front by the RNG — the animation just shows it
  off — so the displayed total always matches the real total. **Exploding** is
  the one thing that happens *during* the animation: when a die settles on a face
  that meets its term's condition it drops one more die into the arena, which can
  itself explode, so the chain unfolds on screen. Each die explodes at most once
  and each term is capped, so the pile always converges; the running total climbs
  as the chain plays out.
- **`ui`** — ratatui rendering: a bordered arena into which each die is painted
  cell-by-cell at its float position as the 2D silhouette of its polyhedron
  (`die_shape` maps sides → a 6×4 ASCII template), a result panel with per-die
  colour-coded chips and the running/final sum, an editable input line, and a
  help bar.

The main loop draws a frame, polls for a key for up to one frame budget, then
advances the simulation by the real elapsed `dt`. Decoupling the roll result
from the animation keeps the physics free to be as chaotic as it likes.

## Tests

```sh
cargo test                                    # parser, physics, rendering
cargo test snapshot -- --ignored --nocapture  # print a rendered frame
```

The suite covers notation parsing, that dice always converge to rest (with a
hard frame cap so a non-converging bug fails instead of hanging), that they
never escape the arena, that a crowded pool settles without any two dice
overlapping, and that the UI renders a full roll without panicking (via
ratatui's `TestBackend`).
