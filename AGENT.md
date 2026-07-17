# AGENT.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tinhorn` is a terminal dice roller. Type dice in standard notation, shake the
cup, and watch each die tumble as a real 3D polyhedron in a rendered dice tray
until it settles and the total is tallied. (A *tinhorn* was a
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

A handful of modules behind a ~60 fps event loop (`main.rs`): `parse`, `app`,
`ui`, `cli`, `foley`, the vendored `render3d` + its `render3d_view` bridge, and
`physics` (the Rapier 3D sim). The loop draws a frame, polls for a key for up to
one frame budget, advances the physics by the real elapsed `dt` (which `app`
consumes in fixed steps), then plays whatever sounds the physics queued.

- **`parse`** — hand-written parser: notation → `Roll` (a `Vec<DiceTerm>` + flat
  `i32` modifier + optional `Stake` for staked rolls). Each `DiceTerm` is
  count, sides, and modifiers (`TermMod`: keep/drop, explode with a `Compare`,
  multiply). Pure and unit-tested. Sizes are capped (≤ 60 dice, ≤ 1000 sides) so
  a huge expression can't wedge the renderer; the `vs` target must come last and
  is range-checked into `i32`. A `Stake` bundles that target with a `Goal`
  (`Over` for `>` and its word alias `vs`, `Under` for the roll-under `<`) so a
  direction can only exist alongside a target; both comparisons are inclusive.

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
  - Physics: real 3D rigid bodies via **Rapier** (see the `physics` module). Each
    `Die` holds a body handle and a cached pose (`pos`/`rot`) synced from Rapier
    every step; `update(dt)` advances the sim on a **fixed 1/60 s timestep**
    (accumulator), which is what makes insta and animated rolls settle
    bit-identically. A die is "settled" when its body sleeps (or hits the hard
    airborne cap, which freezes it into a static body). `physics_step` also voices
    contacts as foley and spawns explosion dice.
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

- **`ui`** — ratatui rendering: `render(frame, app)` paints the 3D arena (see
  `render_arena` below), the power meter while shaking, the release echo,
  crit/fumble particles, a result panel with colour-coded chips and the staked
  verdict, the input line, a help bar, and the Help/History/Stats overlays. The
  result panel **withholds the outcome until the dice stop**: each chip stays a
  dim decoy until *its* die settles, then locks in bold; the Σ total is a dim
  `…` until every die has landed, then the green figure. (The animation must
  never spoil the total it's building toward.)

- **`cli`** — clap `Cli` and `run_one_shot`. Three output shapes: bare total
  (default), verbose breakdown (`-v`, dropped dice in `[brackets]`, exploded
  marked `!`), and `--json`. `--seed N` gives reproducible rolls in both modes.
  Under explicit `-p`/`-v`, a staked roll's exit code is the verdict (0/1); the
  implicit piped-stdout mode and `--json` always exit 0 on clean output.

- **`foley`** — procedural sound. `App` emits pure `SoundEvent`s (impacts,
  knocks, settles, cup rattle, crit ring, verdicts); `synth()` renders them from
  physics parameters (die size → pitch, impact speed → loudness) with no assets;
  `Foley` plays them via rodio on a dedicated audio thread, degrading silently
  with no audio device. On by default; `--mute` starts muted, Ctrl-Q toggles.
  The thread spawns **lazily** in `main.rs::run` on the first audible sound
  (opening the device blocks for tens of ms — off the render loop so it isn't a
  frame hitch), and a muted session never spawns it, so it never touches audio
  APIs at all (macOS raises a one-time microphone prompt for playback on duplex
  output devices; that's the OS, even `afplay` draws it — don't chase it).

- **`render3d`** — a vendored, ratatui-free software rasterizer (adapted from
  limlabs/ratatui-3d, MIT; see `src/render3d/LICENSE-render3d`). It renders a
  `Scene` of meshes into an RGB `Framebuffer`: `pipeline::{vertex,rasterize,
  fragment,framebuffer}` (perspective-correct barycentrics — a local `// tinhorn:`
  fix — and **near-plane clipping**: `vertex` transforms to clip space and
  `clip_near` cuts each triangle at `clip.w >= near` before the divide, so a mesh
  straddling the camera is trimmed, not dropped — the upstream code rejected any
  such triangle, which made huge ground planes vanish at shallow angles; and
  **optional depth fog** — `Scene::fog: Option<(f32, f32)>` (start/end distances,
  `None` by default so the other render3d callers and tests are untouched); when
  set, the `fragment` stage lerps each shaded fragment toward `Scene::background`
  by a smoothstepped camera-distance factor, so far geometry recedes into the room.
  The hot path skips it entirely when `None`),
  `dice::mesh_for(sides)` (all six polyhedra, built by one `polyhedron`
  face-builder; d12 is the icosahedron's dual, d10 a pentagonal bipyramid stand-in
  for the trapezohedron; `dice::face_geometry(sides)` returns each face's
  `(centroid, outward_normal)` in that same unit-mesh space — extracted from the
  built mesh by grouping vertices on their shared flat normal — so the number
  overlay can ride the die's frontmost face) and `dice::cup()` (an **open, hollow tin tumbler** —
  flared outer wall, rolled lip, rim, inner wall, raised inner floor, every face
  double-wound so the bowl shows through the mouth; the shaded hollow against
  the lit lip is what reads "cup" and not "cylinder"), `camera`/`math` (glam). **`render3d_view`**
  is the glue: it rasterises at **2× and box-downsamples** (`draw`/`downsample`,
  cheap anti-aliasing) then applies a gentle radial **vignette**, and blits the
  `Framebuffer` into a ratatui `Buffer` as half-block/braille/ASCII cells. It also
  owns the shared `arena_camera()`/`live_camera()` (the latter folds in every
  per-frame modifier — idle drift, crit punch-in, throw shudder) +
  `project_to_cell()` so the dice, their burned numbers, and the crit/fumble
  bursts all agree on where a die sits on screen. `arena_camera` derives its
  distance from the view **aspect**, fitting the tray's full width *and* height
  (floor through the launch height) so the whole box, the throw's arc, and the
  cup stay in frame — dice read large on a wide terminal, a narrow one backs the
  camera off so nothing clips. Its vertical framing is **derived from
  `physics::HZ`** (the overhead read covers the felt's full depth at the ~68°
  projection; the establishing view adds fixed launch-arc headroom), so
  re-proportioning the tray is a one-constant edit that the camera follows.
  Together they **are the arena** — there is no 2D
  sprite renderer.

- **`physics`** — the real 3D rigid-body world, on **Rapier** (`rapier3d`, which
  speaks glam natively, so no conversion). A fixed box tray (floor + walls +
  ceiling) and one dynamic body per die with a convex-hull collider from its
  polyhedron, so dice tumble, bounce, collide, and sleep at rest. Small API
  (`spawn`/`launch`/`step`/`pose`/`sleeping`/`freeze`/`clear`) so `app` and the
  renderer never touch Rapier. It decides *only* where dice land — the seeded
  RNG owns the values (drawn in `app`, up front). `HX/HY/HZ` are the tray
  half-extents — 3.2×1.9×2.0, a ~1.6:1 width:depth felt, near a real dice
  tray's proportions (deeper reads squarer but forces the overhead camera out
  and shrinks the dice; see the `HZ` comment) — and `DIE_R` the die
  world-radius. The camera framing, the spawn lattice's depth slots, and the
  table under the tray all derive from `HZ`, so it stays a one-line retune.

- **`ui::render_arena`** draws the roll as tumbling polyhedra straight from the
  physics: each `Die`'s `pos`/`rot` (synced from its Rapier body) place and
  orient a mesh in the tray. The **tray itself is rendered** as a warm casino
  table: a **green-baize** felt floor (a dedicated `felt_texture` — a plush pile
  mottle with a **recess ambient-occlusion** baked in, darkening the felt toward
  the three walls so it reads as a sunken bed, not a flat plane, plus a soft radial
  **dice-traffic sheen** brightening the felt's centre a few percent where the dice
  land) with a
  **back wall and two side walls**, each a solid lip with real depth — an inner
  face flaring up from the floor edge, a flat **top rail** you look down onto, an
  outer face, and (corners mitred per-vertex) an **end cap** closing each side
  wall where it meets the open front, so the lip reads as a solid frame instead
  of a hollow shell. The front is left open so you look straight in, where the
  tray shows **real floor thickness**: the walls' outer faces and the open-front
  edge drop past the felt line down to the table, and that front edge reads as a
  thin **felt lip on a wooden base**, so the felt is a pad set into a solid tray
  rather than paint on the tabletop. (That exposed felt cut edge — `felt_edge_mat`
  — is kept deliberately dark and dim: brighter, it caught the near-overhead light
  and read as a floating bright-green stripe, so it's toned down to read as the
  felt's own shadowed thickness.) The walls are
  warm **mahogany** with a lighter wood **rail** along the top, lit by
  a low warm-**tungsten point light** (with a **near-subliminal slow sway** of its
  x/z off `app.clock()`, so the felt's hotspot breathes like a lamp on a chain)
  whose per-fragment falloff gives the felt a
  spotlit pool falling to shadow, plus a dim **cool blue-white rim light** from high
  behind — warm key against cool rim shapes the dice — to pop
  the die silhouettes. Each near-floor die casts a soft **contact shadow** (its
  own silhouette flattened onto the felt, a darker core under a wider penumbra)
  and the dice wear a broad matte sheen. The tray and the **poker-chip stacks**
  (clustered at each open-front corner, their heights and per-chip colour order
  **hash-varied by stack index** so they read as casually stacked, not tidy towers)
  rest on a small raised **wood
  table** — a slab with a visible front and side **apron** so it reads as furniture,
  itself grounded on a dark-red **casino rug** (two flat quads — a deeper oxblood
  border band under a lighter crimson field; pure-ambient plain tones, no thin
  borders that would shimmer at this frame). The rug is generous toward the
  viewer and the sides — in the wide establishing view it peeks out beside the
  table's flanks and in front of its apron, so it reads before and during a roll,
  not just at the settle — but its **far edge is capped** by the narrow floor band
  the near-overhead read shows behind the tray, so in both camera framings the
  floorboards stay clearly visible beyond the rug there and it reads as furniture
  dressing, never the room's floor colour
  — and beyond the rug a broad **room floor** of wooden **floorboards** (a
  `floor_texture` of long planks running front-to-back, their seams converging into
  the distance — the bold seam lines are what read as "a floor" at this tiny frame,
  and running them front-to-back keeps them from bunching into a blur at the
  shallow grazing angle behind the tray). It's lit by pure ambient (no diffuse, so
  no key-light pool streaks a bright patch across it) and kept a **genuinely bright**
  lit oak so it shows at every angle rather than reading as a dark void. The
  **depth fog** (set on the scene here, applied in the pipeline) respects that: its
  start is tuned past the mid-ground so the near and middle boards keep their full
  lit-oak warmth and only the floor's far reaches recede toward the backdrop —
  depth at the horizon, no dimming of the room. It's a plain
  quad; its near edge sits *behind* the camera, so its triangles straddle the near
  plane — which used to make the rasterizer drop the whole floor at shallow angles
  (a true background gap, the bug that made it "partially show"). The fix lives in
  the pipeline (near-plane clipping, below), not here. Far
  behind, a warm emissive **backdrop** wall is a **vertical gradient — bright at the
  floor seam (its bottom tone == the lit floor, so the floor→wall horizon is one
  continuous surface, no dark band reading as missing floor) fading up to a dark
  ceiling**. Heavy **oxblood stage curtains** flank the background, built to be
  *identified* at this resolution, not just seen: each drape **hangs free** —
  width near-constant down the drop with a whisker of outward relaxation at the
  floor, its inner edge not a ruled line but a **gentle scallop at the fold
  amplitude** (on a free hang, the innermost fold's crest/valley profile IS the
  fabric's edge; the earlier straight-edged band version read as flat dark
  walls) over **real corrugated fold geometry** (wide, evenly spaced vertical
  waves, flat-shaded facets the key and rim genuinely light and shade),
  finished with a subtle
  baked **velvet streak texture** (`velvet_texture`, cached). Lit materials
  tied to the rug's tones, hung *near* (z = −5, falling to the boards just past
  the rug's far edge; deeper, perspective shrinks them to corner patches).
  The middle between them stays deliberately bare: gradient wall, wainscot,
  fog, nothing else. (Backdrop bake-off history, so nobody retreads it: the
  bokeh light-field, a lit doorway, and billboard lamps were rejected — flat
  emissive billboards read as 2D cutouts at this resolution; only real shaded
  geometry like the cup and chips reads as an object, so don't regress to
  painted glows or light cones. A follow-up tried *real-geometry* pendant lamps
  — lathed shades with exposed emissive bulbs — and they modelled fine, but a
  cold viewer read cone-over-glowing-ball as **mushrooms**: the right kind of
  geometry, the wrong silhouette. The background stays lamp-free. A **tied-back
  cinch** silhouette for the drapes was also tried and pulled: with no visible
  tie geometry the pinch read as unnatural — an untied curtain just hangs.)
  A thick dark **wainscoting/chair-rail band** is baked
  across the wall, well above the seam, with a subtly deeper panelled tone below it
  — both faded out before the seam so the bottom tone still matches the floor (the
  horizon invariant is untouched); in the two tight framings the receding
  floorboards occlude most of the wall, so it reads only faintly. (The camera's
  `want_half_h` is set a little tall so this casino room is actually visible over
  the tray rail.) The
  camera **leans in to read the dice**: an eased `App::focus`, keyed to the
  ceremony (0 while the cup is shaking → 1 the moment a throw is launched, held
  through the flight and the settle until the next shake begins) pitches it from a
  shallow ~31° establishing angle up and over to a near-overhead ~68° and tightens
  `want_half_h` hard to zoom in over the felt — the way you'd lean over the tray to
  read the top faces, which is also the one angle the **floorboards read reliably
  from** (grazing/shallow, their seams blur into a flat "blank"; looked down on,
  they're clearly a floor); on top of
  that a slow **vertical
  reading pan** (`render3d_view::idle_orbit` off `App::clock`, target fixed → the
  eye rises and eases forward so the view drifts top-to-bottom) gives it life; a
  natural crit fires a gold **flare** light +
  camera punch-in (`App::flash`) — the flare catches every lit surface: the
  felt, the dice, the rails, and the curtains' inner folds — and hard bounces
  **flinch** the key light (`App::impact_energy`) — all decayed each `update`. The
  final frame gets a 2× box-downsample and a warm-graded vignette in
  `render3d_view`. The whole palette lives in one `ArenaStyle` (`DEFAULT`; tests
  override it to render design mock-ups). **A
  number rides every die on the face that points at us** — the read-face, picked
  each frame as the one whose world normal (`Die::rot` × the face normal from
  `dice::face_geometry`) most faces the camera — drawn at *that face's* projected
  centre (not the die's middle) on a dark plate so it stays legible over the die's
  colour. While airborne it's a dim decoy (`Die::shown`, a face derived from the
  die's spin — no RNG, so the seed contract holds) that **ducks out** as the die
  rolls edge/corner-on: when two faces tie for frontmost the read-face's lead over
  the runner-up nears zero, so the digit blinks off there and reads as ink on the
  tumbling solid rather than a fixed label. The instant a die settles it **burns**
  to the RNG-decided `final_value` (crit hot, fumble red, dropped grey) and always
  shows in full — the payoff must stay legible. The digit also **scales to the
  dice's on-screen size, uniformly across the roll** (`number_scale`, sized from a
  reference die at the felt centre so every die reads the same — never a big number
  on the nearest die and single cells on the rest): a narrow terminal keeps the
  crisp single terminal cell on a dark plate, but once a wide terminal renders the
  dice large enough the number grows into a blocky **half-block** glyph
  (`DIGIT_FONT`, drawn by `draw_big_number`) centred on the die and sized to sit
  *within* its face (`FACE_FRAC_*`; tuned so the glyph appears on a modern
  ~120-column terminal). That big glyph carries a thin **outline** dilated around
  its strokes on every side (the draw loop pads one cell past the glyph box so the
  top/left/right border isn't clipped) — tinted a **dark shade of that die's own
  colour**, so on a small die (where the digits cover most of it) the number's
  surround still carries the die's hue and you can read which number belongs to
  which die. It leaves the rest transparent, so the die shows through and the
  number reads as ink **on the face** rather than a plate covering it; it
  composites per half-block sub-pixel (ink / outline / the die pixel already in the
  buffer). While
  shaking, the cup is the open tin tumbler (`dice::cup()`), and its whole act
  rides the one shake clock (`CUP_SWAY_RATE`, shared from `app`): the sway, a
  bob that hops at each direction flip, a lean into the swing, and a
  high-frequency rattle jitter, all scaled by the building power; the power
  meter is the one text overlay left. While shaking the **dice, their numbers, and
  their contact shadows are all withheld** — they're gathered in the cup, so the
  arena shows only the tumbler and its meter; without those guards the previous
  roll's settled dice linger on the felt and their number overlays paint in front
  of the cup. The throw it releases separates **force
  from direction**: power sets only the speed, the aim leans away from the
  cup's sway plus a per-die spray (`launch_pool`), so same-power throws don't
  all break the same way (a test pins both directions occurring). It reuses the release echo and crit/fumble particles as
  overlays — bursts projected through the shared `live_camera` so they erupt
  from the die that earned them — shudders the camera off `tremor()` on a hard
  throw, and hands `arena_w/h` to the sim. (Known cosmetic nit: an open pane's
  wide-emoji title can keep one stray half-block from the arena behind it.)

## Conventions worth knowing

- **Key routing is deliberately constrained** (`main.rs::handle_key`, kept pure
  so it's unit-tested). Pane hotkeys use chords/`?` (Ctrl-H history, Ctrl-S stats,
  `?` help) specifically so bare `h`/`s` stay typeable for notation like `kh`/`dh`.
  Don't add a plain-letter hotkey — it will eat characters users need to type.
  Enter rolls in the current mode; Tab cycles the mode (shake → roll →
  insta); Space stays a notation separator. The arrows are for editing and
  scrolling only, never a roll action: on the prompt ←/→ (and Home/End) move
  the caret (`App.cursor`, a byte offset kept valid by `cursor_byte()`, which
  the insert/delete/move helpers and the renderer all read through); while a
  pane is open the same arrows scroll it (`App.pane_scroll`, clamped to the
  overflow in `ui::overlay_panel`; `App::set_pane` owns the `pane` ↔
  `pane_scroll` pairing and is the only way to change panes). Mute
  is Ctrl-Q ("quiet") — never
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
