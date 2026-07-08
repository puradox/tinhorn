<p align="center">
  <img src="docs/logo.svg" width="360" alt="tinhorn — a pixel tin cup throwing dice">
</p>

# tinhorn

<p align="center">
  <a href="https://crates.io/crates/tinhorn"><img src="https://img.shields.io/crates/v/tinhorn.svg?logo=rust&logoColor=white" alt="crates.io"></a>
  <a href="https://crates.io/crates/tinhorn"><img src="https://img.shields.io/crates/d/tinhorn.svg" alt="downloads"></a>
  <a href="https://github.com/puradox/tinhorn/actions/workflows/ci.yml"><img src="https://github.com/puradox/tinhorn/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="#license"><img src="https://img.shields.io/crates/l/tinhorn.svg" alt="license: MIT OR Apache-2.0"></a>
</p>

Step right up: a terminal dice roller with a genuine tin-cup shake. Type
your dice in the usual notation, rattle the cup, and let 'em fly — real
physics on every bounce, real racket off every wall, and the dice land how
they land. Nothing up these sleeves, friend: seed the roll (`--seed 42`)
and watch the very same throw land twice.

![tinhorn: typing 2d20kh1 vs 15, shaking the cup, releasing at the peak, and landing a natural 20 — SUCCESS by 5](docs/demo.gif)

> **Why the name?** A *tinhorn* is a small-time gambler, named for the tin
> shaker chuck-a-luck dealers rattled their dice in — small stakes, big noise.
> That shaker is this program: all rattle, honest dice.

And what'll it cost you to see all this? Not one thin dime:

- **A real physics arena.** Six silhouettes — d4 triangle to d20 hexagon —
  tossed, bounced, knocked together, and rolled off each other's backs at
  sixty frames a second, painted by [ratatui](https://ratatui.rs).
- **The Throw.** Shake the cup, catch the meter at its peak, and put some
  arm into it. Power shapes the launch and *never* the dice — there's a
  test that swears to it.
- **Stakes.** Call your number — `d20+5 vs 15` — and the arena hands down
  the verdict, margin and all. The stats pane quotes you fair odds before
  you take the bet.
- **Sound from thin air.** Every click, knock, and thunk synthesized live
  from the very impact that made it. No samples anywhere on the premises.
- **[The fancy notation.](#dice-notation)** Advantage, drop-the-lowest,
  exploding dice, multipliers — the works.
- **[One-shot mode](#scripting-one-shot-mode)** for scripts and pipes: asks
  no questions, prints a number, gets out of the way. Even the exit code
  carries the verdict.

House rules: [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), your pick —
the whole works sits on the table for inspection.

## Install

You'll need a [Rust toolchain](https://rustup.rs). On Linux, the sound needs
the ALSA headers to build (macOS and Windows need nothing extra):

```sh
sudo apt install libasound2-dev pkg-config   # Debian/Ubuntu
sudo dnf install alsa-lib-devel              # Fedora
```

Then install from crates.io:

```sh
cargo install tinhorn
```

For the latest unreleased code, install from the repository instead:

```sh
cargo install --git https://github.com/puradox/tinhorn
```

If your fingers insist on the old ways: `alias roll=tinhorn`.

## Run

```sh
tinhorn                  # start empty, type an expression
tinhorn 3d6              # roll 3d6 the moment it opens
tinhorn "d6+d8"          # quote anything with shell-special characters
tinhorn --mute           # start silent (Ctrl-Q toggles at runtime)
```

> **macOS asked about the microphone?** Recent macOS raises that prompt for
> *any* app playing audio through an output device that also carries mic
> inputs (a USB interface, a headset) — even Apple's `afplay` trips it.
> tinhorn never records and opens the default output device only, so deny it
> freely; `--mute` skips audio entirely and never asks.

## Keys

| Key                | Action                                             |
| ------------------ | -------------------------------------------------- |
| `Enter`            | roll, per the mode (shake: press again to throw)   |
| `Tab`              | cycle the mode — shake → roll → insta              |
| type / `Backspace` | edit the dice expression                           |
| `←` `→` (`Home`/`End`) | move the caret in the expression (jump to ends) |
| `↑` `↓`            | scroll an open pane that's taller than the screen  |
| `?`                | toggle the dice-notation help overlay              |
| `Ctrl-H`           | toggle the roll-history pane                       |
| `Ctrl-S`           | toggle the statistics pane                         |
| `Ctrl-Q`           | mute / unmute — Q for quiet                        |
| `Esc` / `Ctrl-C`   | quit (`Esc` closes a pane or shake first)          |

Three roll modes cycle on `Tab`: **shake** (drop into the cup and catch the
power meter), **roll** (dice tumble straight in), and **insta** (landed and
tallied at once).

`?`, `Ctrl-H`, and `Ctrl-S` open the notation help, roll history, and
statistics panes; they float over the animation and close on `Esc`.

## Dice notation

A roll is a sequence of **dice terms** and optional **flat modifiers**, in any
combination. Terms can be separated by `+`, `,`, whitespace, or simply written
next to each other.

| Input      | Meaning                              |
| ---------- | ------------------------------------ |
| `3d6`      | three six-sided dice                 |
| `d%`       | percentile — shorthand for `d100`    |
| `d6+d8`    | one d6 and one d8                    |
| `d6d10`    | adjacency works as a separator       |
| `d6,d12`   | commas work too                      |
| `2d20-1`   | two d20 with a −1 modifier           |
| `d20 + 5`  | whitespace is ignored                |
| `d20+5 vs 15` | staked: succeed on a total ≥ 15   |

`d6` means `1d6`. Sizes are capped (≤ 60 dice, ≤ 1000 sides) so a fat-fingered
`999d99999` can't wedge the renderer. A `vs` target must come last — `d20 vs
4d6` is an error, not a surprise — and there's at most one per roll.

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

## Scripting (one-shot mode)

With an output flag — or whenever stdout isn't a terminal — `tinhorn` skips
the animation, evaluates the roll once, prints a result, and exits, so it
drops straight into scripts and pipelines:

```sh
tinhorn -p 3d6              # 13            (just the total)
tinhorn 3d6 | cat           # 13            (piped stdout → one-shot automatically)
total=$(tinhorn -p 2d20kh1) # capture it in a variable
tinhorn --seed 42 4d6dl1    # reproducible: the same seed always rolls the same dice
tinhorn -v 4d6dl1+2         # a full breakdown (dropped dice in [brackets])
tinhorn --json 2d20kh1+3    # machine-readable for jq & friends

tinhorn -p d20+4 vs 14 && echo "the potion works"   # the exit code IS the check
```

Under `-p`/`-v`, a staked roll exits 0 on success and 1 on failure, so scripts
branch on the check itself; `--json` and piped output always exit 0, and a
parse error goes to stderr and exits 2.

```text
$ tinhorn -v --seed 1 "d20+5 vs 15"
  d20        17  = 17
  modifier   +5
  total      22
  vs 15      success by 7
```

The `--json` output carries every die and its flags, the per-term subtotals,
the flat modifier, the total, and — when staked — `target`, `success`, and
`margin`.

## Contributing

Want a look behind the table? The design notes, the test suite, and the
house rules all live in [CONTRIBUTING.md](CONTRIBUTING.md) — pull up a
chair. Built in Rust; the dice, the physics, and every sound are made
from scratch on the premises.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
