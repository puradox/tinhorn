# roll 🎲

A terminal dice roller you actually *throw*. Type dice in common notation,
shake the cup, and release at the top of the meter — the dice burst out,
bounce around a physics arena as the 2D silhouettes of their polyhedra, and
settle into a tallied total, with the clatter synthesized live from the
physics. Stake a roll against a target (`d20+5 vs 15`) and the arena hands
down the verdict. Built in Rust with [ratatui](https://ratatui.rs). Or run it
[one-shot](#scripting-one-shot-mode) — `roll -p 3d6` — to print a result and
exit, so it works in scripts and pipes too.

![roll: typing 2d20kh1 vs 15, shaking the cup, releasing at the peak, and landing a natural 20 — SUCCESS by 5](docs/demo.gif)

*Real frames from the TUI: advantage with stakes, thrown at full power. The
release echo grades the catch, the dropped 8 dims, and the natural 20 lands
the verdict under a burst of gold.*

Shapes by die: **d4** triangle · **d6** square · **d8** diamond ·
**d10** kite · **d12** pentagon · **d20** hexagon. Other sizes fall back to a
plain box.

## Run

```sh
cargo run --release            # start empty, type an expression
cargo run --release -- 3d6     # roll immediately
cargo run --release -- "d6+d8" # quote anything with shell-special chars
cargo run --release -- --mute  # start silent (Ctrl-M toggles at runtime)
```

## The Throw

`Enter` rolls instantly, exactly as you'd expect. But a roll can also be
*thrown*: press `Tab` and the dice drop into a cup that rattles along the
arena floor while a power meter climbs and falls (~1.6 s per cycle), the cup
swaying harder as power builds. Release with `Enter` (or `Tab`) and the dice
launch out of the cup at whatever power you caught — `Esc` puts them down.

- **Timing is the game.** A release at the trough is a timid lob that flops
  out beside the cup; catch the peak and the throw rockets across the arena
  and works the far wall, shuddering the whole box on impact.
- **The release echo tells you what your thumb did**: the meter freezes at
  the top of the arena, graded — *the peak! / a rocket / a clean toss / a
  timid lob* — so every shake trains your timing.
- **The dice are fair.** Power shapes the launch trajectory and nothing else:
  faces are drawn from the same untouched, seedable RNG at the moment of
  release, and a test asserts the same seed rolls identical values whether
  you lob or rocket. The expression is locked when the cup is picked up.

## Stakes — `vs N`

End any expression with a target and the roll becomes a check: `d20+5 vs 15`
succeeds when the total *meets or beats* 15. While the dice bounce the panel
shows what's at stake; when they settle it slams **`SUCCESS by 3`** or
**`FAIL by 1`**. The statistics pane (`Ctrl-S`) shows your odds of making the
check before you commit.

Crits are for every die, not just d20s: any **kept** die that settles on its
max face bursts gold with a bright ring, and any kept 1 slumps in dust with a
low thud. All-d20 crits keep their proper name — *✦ natural 20* — while a
maxed d6 earns a *✦ crit* on its own merits. Dropped dice celebrate nothing.

## Sound

Every noise is synthesized from the physics that caused it — no samples:
impact speed sets loudness, die size sets pitch (a d4 clicks, a d20 thunks),
the cup rattle follows the sway, a crit rings, and the staked verdict gets a
two-note up or down. Sound is on by default in the TUI; `--mute` starts
silent, `Ctrl-M` toggles, and machines with no audio device (ssh, CI) just
roll quiet dice.

> `Ctrl-M` needs a terminal that speaks the enhanced keyboard protocol
> (iTerm2 3.5+, kitty, WezTerm, Ghostty, foot…). On legacy encodings Ctrl-M
> *is* the Enter key, so the help bar only advertises the chord where it can
> actually arrive. Hear the whole palette out loud with
> `cargo test audible -- --ignored`.

## Scripting (one-shot mode)

Given an expression, `roll` normally opens the animation. But with an output
flag — or whenever stdout isn't a terminal — it skips the TUI, evaluates the
roll once, prints a result, and exits. So it drops straight into scripts and
pipelines:

```sh
roll -p 3d6              # 13            (just the total)
roll 3d6 | cat           # 13            (piped stdout → one-shot automatically)
total=$(roll -p 2d20kh1) # capture it in a variable
roll --seed 42 4d6dl1    # reproducible: the same seed always rolls the same dice
roll -v 4d6dl1+2         # a full breakdown (dropped dice in [brackets])
roll --json 2d20kh1+3    # machine-readable for jq & friends

roll -p d20+4 vs 14 && echo "the potion works"   # the exit code IS the check
```

Under an explicit `-p`/`-v`, a staked roll exits 0 on success and 1 on
failure, so shell scripts branch on the saving throw itself. The implicit
piped-stdout mode and `--json` always exit 0 on clean output — `roll d20 vs
15 | tee log` in a `set -e` script won't abort just because the die came up
short, and JSON consumers read the verdict from the data instead.

```text
$ roll -v --seed 1 "d20+5 vs 15"
  d20        17  = 17
  modifier   +5
  total      22
  vs 15      success by 7
```

The `--json` output carries every die with its `kept`, `exploded`, `crit`,
and `fumble` flags, the per-term subtotals, the flat modifier, the grand
total, and — when staked — `target`, `success`, and `margin`: everything the
animation knows, in a form a script can read. A parse error goes to stderr
and exits 2, so failures are catchable.

## Keys

| Key                | Action                                             |
| ------------------ | -------------------------------------------------- |
| `Enter`            | roll / re-roll the current dice instantly          |
| `Tab`              | pick the dice up and shake; `Enter`/`Tab` throws   |
| `?`                | toggle the dice-notation help overlay              |
| `Ctrl-H`           | toggle the roll-history pane                       |
| `Ctrl-S`           | toggle the statistics pane                         |
| `Ctrl-M`           | mute / unmute (terminals with the kitty protocol)  |
| type / `Backspace` | edit the dice expression                           |
| `Esc` / `Ctrl-C`   | quit (`Esc` closes a pane or shake first)          |

The pane hotkeys use `Ctrl` chords, not bare letters, so that `h` and `s`
stay free to type — `kh`/`dh` and any expression containing those letters
would otherwise be swallowed. (Space is a notation separator, which is why
the Throw lives on `Tab`.)

The three pop-out panes (`?` / `Ctrl-H` / `Ctrl-S`) float over the running
animation — the dice keep bouncing underneath — and are mutually exclusive;
opening one closes the others. `Esc` or `q` dismisses whichever is open.

**History** lists recent rolls, newest first, with the kept faces and the
total (in-memory only — it's cleared on quit). **Statistics** shows the
theoretical odds for the expression currently in the input box — min, max,
average, success odds when staked, and a small distribution curve, estimated
by sampling the roll many times so every modifier is accounted for —
alongside a summary of the rolls of that expression you've actually made
this session.

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
  the crit particles and a queue of pure `SoundEvent`s the physics emits.
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
  out (rodio), pitch from die size and loudness from impact speed. Degrades
  silently without an audio device.

The main loop draws a frame, polls for a key for up to one frame budget,
advances the simulation by the real elapsed `dt`, then plays whatever the
physics wanted heard. Decoupling the roll result from the animation keeps the
physics free to be as chaotic as it likes.

## Tests

```sh
cargo test                                    # parser, physics, rendering, CLI, keys, foley
cargo test snapshot -- --ignored --nocapture  # print a rendered frame
cargo test audible -- --ignored               # play the sound palette out loud
```

The suite covers notation parsing (including the `vs` grammar and its
overflow guards), that dice always converge to rest under a hard frame cap,
that they never escape the arena or overlap at rest, and that the UI renders
a full roll without panicking (via ratatui's `TestBackend`). The Throw is
pinned by tests that power shapes the launch but never the values, that the
expression is locked at pickup, and that a fresh shake starts clean. Stakes
are tested across the TUI verdict, the CLI exit code, JSON fields, and the
stats odds — all backed by one shared rule. Key routing is tested so a
hotkey can never swallow a letter you need to type, and the foley synthesis
is unit-tested for range, pitch ordering, and loudness scaling.

The demo GIF above is generated from real captured frames:
`DEMO_OUT=/tmp/demo.json cargo test record_demo -- --ignored` dumps rendered
frames plus per-frame sound events, ready for re-rendering.
