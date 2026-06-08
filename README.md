# roll 🎲

A terminal dice roller. Type dice in common notation and watch them bounce
around the screen, tumbling through random faces until they settle and the
total is tallied. Built in Rust with [ratatui](https://ratatui.rs).

```
┌ 🎲  roll — settled ───────────────────────────────────────────────────┐
│                                                                       │
│                                                                       │
│                                        ┌───┐┌───┐       ┌───┐         │
│                                        │ 8 ││ 5 │       │ 5 │         │
│                                        └───┘└───┘       └───┘         │
└───────────────────────────────────────────────────────────────────────┘
┌ result ───────────────────────────────────────────────────────────────┐
│[8] [5] [5]                                                            │
│  Σ total  18                                                          │
└───────────────────────────────────────────────────────────────────────┘
dice ▸ d20+d6+d6█
 › Enter roll  Esc quit   try: 3d6 · d6+d8 · d6d10 · d6,d12 · 2d20-1
```

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
| type / `Backspace` | edit the dice expression        |
| `Esc` / `Ctrl-C` | quit                              |

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

## Design

Three small modules behind a 60 fps event loop:

- **`parse`** — a hand-written parser that turns notation into a `Roll`
  (a flat list of individual dice plus an integer modifier). Pure and unit-tested.
- **`app`** — the state plus a tiny physics simulation. Each die is a box with
  position/velocity; the engine applies gravity, bounces off the four walls with
  restitution, rubs off speed with floor friction and air drag, and tumbles its
  visible face every 50 ms while airborne. When a die is slow and resting on the
  floor it *snaps to its pre-rolled value* and locks. The roll result is decided
  up front by the RNG — the animation is just showing it off — so the displayed
  total always matches the real total.
- **`ui`** — ratatui rendering: a bordered arena into which die boxes are
  painted cell-by-cell at their float positions, a result panel with per-die
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
never escape the arena, and that the UI renders a full roll without panicking
(via ratatui's `TestBackend`).
