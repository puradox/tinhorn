//! Application state, the roll evaluator, and the glue that drives the dice
//! through the 3D rigid-body sim in [`crate::physics`] (Rapier).

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::parse::{self, DiceTerm, Goal, Roll, Stake, TermMod};
use crate::physics::{self, Physics};
use glam::{Quat, Vec3};
use rapier3d::prelude::RigidBodyHandle;

// Tuning. The rigid-body sim lives in `physics`; these are the bits `app` still
// owns: the crit/fumble particle gravity, the airborne cap, and the ceremony.
const GRAVITY: f32 = 60.0; // 2D particle-burst gravity (cells/s²), not the dice
const MAX_AIRBORNE: f32 = 8.0; // hard cap: a die tumbling this long is frozen in place
const MAX_EXPLOSIONS: usize = 40; // cap on dice an exploding term can spawn, so the pool can't run away

// The Throw: shake-the-cup tuning. Power rises from 0 the moment the cup is
// picked up and oscillates forever, so the release timing is the whole game.
const SHAKE_POWER_RATE: f32 = 3.9; // rad/s: a full 0→1→0 power cycle ≈ 1.6 s
/// rad/s: how fast the cup rattles side to side. `pub(crate)` because the
/// renderer syncs the cup's bob and lean to the same clock — one rate, so the
/// drawn shake can't drift off the audible one.
pub(crate) const CUP_SWAY_RATE: f32 = 7.0;
const MAX_HISTORY: usize = 200; // most recent rolls kept in memory for the history pane
const STAT_SAMPLES: usize = 20_000; // Monte-Carlo trials for the statistics pane's odds
const MAX_SOUNDS: usize = 64; // pending sound events; when nothing drains them, stop queuing
const KNOCK_BUDGET: usize = 8; // impact/knock sounds voiced per physics step; more is inaudible
const CRIT_PARTICLES: usize = 16; // burst size for a natural 20
const RELEASE_ECHO_SECS: f32 = 1.6; // how long the caught-power verdict lingers on screen

/// One die in the arena. Its world pose (`pos`, `rot`) is synced from the Rapier
/// body every physics step; the value fields are the seeded RNG's, decided up
/// front so the physics only decides where a die lands, never what it reads.
pub struct Die {
    pub sides: u32,
    pub final_value: u32,
    pub shown: u32,
    /// World position, synced from the physics body each step.
    pub pos: Vec3,
    /// Orientation, synced from the physics body each step.
    pub rot: Quat,
    pub settled: bool,
    pub color_idx: usize,
    /// Which dice term this die belongs to. Dice in the same term share a `mult`
    /// and are kept/dropped together; the total is summed per term.
    pub term_idx: usize,
    /// Per-term multiplier applied to this term's kept sum (1 when no `*N`).
    pub mult: i32,
    /// `false` for a die that was thrown and animated but dropped by keep/drop —
    /// it still tumbles and settles, but is rendered dimmed and left out of the total.
    pub kept: bool,
    /// The explode condition inherited from this die's term, if it explodes.
    /// When such a die settles on a matching face it spawns one more die — so
    /// explosions unfold *during* the animation rather than all up front.
    explode: Option<parse::Compare>,
    /// Set once a die has had its chance to explode, so it can't spawn twice.
    exploded: bool,
    age: f32, // seconds airborne; a hard cap forces a rest so nothing tumbles forever
    /// Handle to this die's Rapier rigid body (invalid until spawned).
    body: RigidBodyHandle,
}

/// THE statement of the staked-check rule: `(success, margin)` for a total
/// checked against a [`Stake`]. Every consumer — the TUI verdict, the headless
/// [`evaluate`], the stats pane's success odds — must call this, never restate
/// the comparison, so the rule can only ever change in one place.
///
/// `margin` is "how much you made it by": positive on success, negative on
/// failure, whichever way the stake runs. Meet-or-beat measures `total −
/// target`; roll-under measures `target − total`. Either way `success` is
/// `margin >= 0`, so [`verdict_text`] reads the same for both directions. The
/// subtraction is done in i64 so an absurd-but-parseable target (i32::MIN)
/// can't overflow.
pub fn check(total: i32, stake: Stake) -> (bool, i32) {
    let raw = match stake.goal {
        Goal::Over => total as i64 - stake.target as i64,
        Goal::Under => stake.target as i64 - total as i64,
    };
    let margin = raw.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    (margin >= 0, margin)
}

/// The staked verdict, spelled out. Shared by the TUI chip and the CLI's
/// verbose breakdown (which lowercases it) so the two paths can't drift.
pub fn verdict_text(success: bool, margin: i32) -> String {
    match (success, margin) {
        (true, 0) => "SUCCESS — exactly".to_string(),
        (true, m) => format!("SUCCESS by {m}"),
        (false, m) => format!("FAIL by {}", -m),
    }
}

/// The crit rule, one place: a die of `sides` showing `value` is a crit on
/// its max face. A d1 is excluded — it is always at its max (and its min)
/// and deserves neither celebration nor pity. Shared by the animated path
/// ([`App::crit_dice`]) and the headless evaluator so `--json` consumers see
/// the same call the arena makes.
pub fn crit_face(sides: u32, value: u32) -> bool {
    sides >= 2 && value == sides
}

/// The fumble rule: a 1 on any die of 2+ sides. See [`crit_face`].
pub fn fumble_face(sides: u32, value: u32) -> bool {
    sides >= 2 && value == 1
}

/// A cosmetic up-face number for an airborne die, derived from its orientation
/// so it flickers as the die tumbles and stills as it slows. No RNG is drawn —
/// the seed-identical roll contract is untouched — and it never counts toward
/// anything: the value that matters burns in from `final_value` the instant the
/// die settles. This is what keeps the outcome a secret until the dice stop.
fn tumbling_face(rot: Quat, sides: u32) -> u32 {
    if sides <= 1 {
        return 1;
    }
    let bits = rot.x.to_bits() ^ rot.y.to_bits().rotate_left(11) ^ rot.z.to_bits().rotate_left(22);
    1 + (bits % sides)
}

/// The convex-hull points for a die of `sides` — its polyhedron's vertices
/// (circumradius 1; the physics scales them to the collider size).
fn mesh_points(sides: u32) -> Vec<Vec3> {
    crate::render3d::dice::mesh_for(sides)
        .vertices
        .iter()
        .map(|v| v.position)
        .collect()
}

/// Something the physics did that deserves a noise. `App` only *emits* these —
/// pure data, so the simulation stays testable — and the foley module turns
/// them into sound. When nothing drains the queue it simply stops filling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SoundEvent {
    /// A die struck a wall, the ceiling, or the floor at `speed`.
    Impact {
        sides: u32,
        speed: f32,
    },
    /// Two dice knocked together with the given closing speed.
    Knock {
        sides: u32,
        speed: f32,
    },
    /// A die came to rest.
    Settle {
        sides: u32,
    },
    /// The cup crossed a sway peak while shaking (one tick of the rattle).
    Rattle {
        power: f32,
    },
    /// The shake was released.
    Throw {
        power: f32,
    },
    /// A kept d20 settled on 20 / on 1.
    Crit,
    Fumble,
    /// A staked roll resolved.
    Success,
    Failure,
}

/// A short-lived celebratory glyph in the arena (nat-20 burst, fumble dust).
pub struct Particle {
    pub x: f32,
    pub y: f32,
    vx: f32,
    vy: f32,
    age: f32,
    life: f32,
    pub glyph: char,
    /// Gold burst (crit) vs dim dust (fumble).
    pub bright: bool,
}

impl Particle {
    /// 0.0 fresh → 1.0 expired; the UI dims the glyph as it dies.
    pub fn fade(&self) -> f32 {
        (self.age / self.life).clamp(0.0, 1.0)
    }
}

/// A shake in progress: its clock plus the expression locked in at pickup.
/// The throw rolls the snapshot — so no future path that edits the input
/// mid-shake can change what was validated and shown when the cup was lifted.
struct Shake {
    t: f32,
    expr: String,
}

/// The most recent release: the power caught and how long ago. Bundling the
/// age inside the Option makes "age only means something after a throw"
/// unrepresentable instead of a rule every consumer must remember.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Throw {
    pub power: f32,
    pub age: f32,
}

/// Which pop-out pane, if any, is currently overlaid on the UI. They're mutually
/// exclusive — opening one closes the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    None,
    Help,
    History,
    Stats,
}

/// One completed roll, recorded for the history pane when the dice settle.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// The expression as typed, e.g. "4d6dl1".
    pub expr: String,
    /// Each kept die's face value, in pool order (dropped dice excluded).
    pub values: Vec<u32>,
    /// The final total, modifiers and all.
    pub total: i32,
}

/// How Enter rolls the dice. Tab cycles these, in the order the ceremony
/// shrinks: the full cup ritual, a plain animated roll, or the result already
/// at rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RollMode {
    Shake,
    Roll,
    Insta,
}

impl RollMode {
    /// The cycle Tab walks.
    pub fn next(self) -> RollMode {
        match self {
            RollMode::Shake => RollMode::Roll,
            RollMode::Roll => RollMode::Insta,
            RollMode::Insta => RollMode::Shake,
        }
    }

    /// The word shown next to Enter in the help bar.
    pub fn label(self) -> &'static str {
        match self {
            RollMode::Shake => "shake",
            RollMode::Roll => "roll",
            RollMode::Insta => "insta",
        }
    }
}

pub struct App {
    pub input: String,
    /// Byte offset of the edit caret within `input`. Left/Right walk it,
    /// typing and Backspace/Delete act at it. Kept on a char boundary and
    /// never past the end by [`Self::cursor_byte`], which every reader goes
    /// through, so a stale offset (e.g. after the input is reassigned) can't
    /// panic.
    pub cursor: usize,
    /// Vertical scroll offset for whichever pop-out pane is open, in lines.
    /// Up/Down nudge it; the renderer clamps it to the content that overflows
    /// the pane and hands the clamped value back. Reset to 0 on every pane
    /// change so each pane opens at its top.
    pub pane_scroll: u16,
    pub dice: Vec<Die>,
    pub modifier: i32,
    /// The stakes from the current roll (`vs N`), when it's staked.
    pub stake: Option<Stake>,
    pub error: Option<String>,
    /// Completed rolls, newest last. Capped so a long session can't grow without
    /// bound; the history pane shows the most recent slice.
    pub history: Vec<HistoryEntry>,
    /// Which pop-out pane is showing (help / history / stats / none).
    pub pane: Pane,
    /// What Enter does (shake / roll / insta); Tab cycles it.
    pub mode: RollMode,
    /// Set the first frame after every die settles, so the roll is recorded into
    /// `history` exactly once rather than every frame thereafter.
    recorded: bool,
    pub arena_w: f32,
    pub arena_h: f32,
    pub spawned: bool,
    /// The shake in progress, if any (the Throw). Power and cup sway are both
    /// functions of its one clock; the throw rolls its locked expression.
    shake: Option<Shake>,
    /// The most recent release: caught power plus seconds since. Drives the
    /// arena title, the release echo, and the brief impact screen-shake.
    pub last_throw: Option<Throw>,
    /// Celebration glyphs in flight (crit burst, fumble dust).
    pub particles: Vec<Particle>,
    /// Noises the simulation wants made, oldest first. The event loop drains
    /// this every frame; headless callers can ignore it (it self-caps).
    pub sounds: Vec<SoundEvent>,
    /// Foley gate, toggled with Ctrl-Q (and seeded by `--mute`). Enforced by
    /// [`Self::take_sounds`] so every drain site inherits the rule.
    pub muted: bool,
    /// The last computed stats, keyed by (expression, history length) — the
    /// two values the sampler is seeded from, so a hit is exact.
    stats_cache: Option<(String, usize, Stats)>,
    /// Count of dice an exploding term has spawned so far this roll, indexed by
    /// term. Explosions happen over the course of the animation, so the cap has
    /// to be enforced across frames rather than in one up-front loop.
    explosions: Vec<usize>,
    rng: StdRng,
    /// The 3D rigid-body world. Physics decides only where dice land, never the
    /// values. Stepped on a fixed timestep so insta and animated rolls settle
    /// bit-identically — the `--seed` contract survives real physics.
    physics: Physics,
    /// Leftover real time not yet consumed by a fixed physics step.
    phys_accum: f32,
    /// Wall-clock seconds since launch, ticked every `update`. Purely cosmetic —
    /// drives the arena's gentle idle camera drift; never touches the RNG or sim.
    clock: f32,
    /// Crit celebration level (1 → 0), set when a natural crit settles and decayed
    /// each `update`. Cosmetic: drives the gold light flare and camera punch-in.
    flash: f32,
    /// A short envelope of recent die-impact energy (0..~1.5), bumped on each hard
    /// contact and decayed each `update`. Cosmetic: kicks the overhead key light so
    /// the table light "flinches" with the physics. Reads impacts, never changes them.
    impact_energy: f32,
    /// Camera "reading" focus (0 → 1), keyed to the ceremony: eased toward 1 the
    /// moment a roll is launched and held through the flight and the settle, back
    /// to 0 while the cup is shaking. Cosmetic: leans the camera down over the
    /// tray so you watch the dice come to a readable stop. Never touches the RNG
    /// or sim.
    focus: f32,
}

impl App {
    /// Start with a fresh, entropy-seeded RNG.
    pub fn new(initial: String) -> Self {
        Self::with_rng(initial, StdRng::from_entropy())
    }

    /// Start with a fixed seed, so the animation produces a reproducible roll
    /// (the `--seed` flag flows through here).
    pub fn with_seed(initial: String, seed: u64) -> Self {
        Self::with_rng(initial, StdRng::seed_from_u64(seed))
    }

    fn with_rng(initial: String, rng: StdRng) -> Self {
        let mut app = App {
            input: String::new(),
            cursor: 0,
            pane_scroll: 0,
            dice: Vec::new(),
            modifier: 0,
            stake: None,
            error: None,
            history: Vec::new(),
            pane: Pane::None,
            mode: RollMode::Shake,
            recorded: false,
            arena_w: 0.0,
            arena_h: 0.0,
            spawned: false,
            shake: None,
            last_throw: None,
            particles: Vec::new(),
            sounds: Vec::new(),
            muted: false,
            stats_cache: None,
            explosions: Vec::new(),
            rng,
            physics: Physics::new(),
            phys_accum: 0.0,
            clock: 0.0,
            flash: 0.0,
            impact_energy: 0.0,
            focus: 0.0,
        };
        if !initial.trim().is_empty() {
            app.input = initial.trim().to_string();
            app.cursor = app.input.len();
            app.roll();
        }
        app
    }

    /// The edit caret as a byte index that's safe to slice `input` at: never
    /// past the end, always on a char boundary. Every insert/delete/move
    /// helper and the renderer read the caret through here, so reassigning
    /// `input` out from under a larger offset can never panic.
    pub fn cursor_byte(&self) -> usize {
        let mut c = self.cursor.min(self.input.len());
        while !self.input.is_char_boundary(c) {
            c -= 1;
        }
        c
    }

    /// Insert a typed character at the caret and step over it.
    pub fn input_insert(&mut self, c: char) {
        let at = self.cursor_byte();
        self.input.insert(at, c);
        self.cursor = at + c.len_utf8();
    }

    /// Delete the character before the caret (Backspace): step the caret left,
    /// then delete what's now under it.
    pub fn input_backspace(&mut self) {
        if self.cursor_byte() == 0 {
            return;
        }
        self.cursor_left();
        self.input_delete();
    }

    /// Delete the character under the caret (Delete); the caret stays put.
    pub fn input_delete(&mut self) {
        let at = self.cursor_byte();
        if at < self.input.len() {
            self.input.remove(at);
        }
        self.cursor = at;
    }

    /// Move the caret one character left, stopping at the start.
    pub fn cursor_left(&mut self) {
        let at = self.cursor_byte();
        self.cursor = self.input[..at]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    /// Move the caret one character right, stopping at the end.
    pub fn cursor_right(&mut self) {
        let at = self.cursor_byte();
        self.cursor = self.input[at..]
            .chars()
            .next()
            .map(|c| at + c.len_utf8())
            .unwrap_or(at);
    }

    /// Jump the caret to the start / end of the expression.
    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.input.len();
    }

    /// Switch the visible pane and rewind its scroll to the top. Every pane
    /// change routes through here so `pane` and `pane_scroll` can't drift apart
    /// (a new pane-opening path can't forget to reset the scroll).
    pub fn set_pane(&mut self, pane: Pane) {
        self.pane = pane;
        self.pane_scroll = 0;
    }

    /// Parse the current input and, on success, build a fresh pool of dice.
    /// Actual spawn positions are assigned later, once the arena size is known.
    pub fn roll(&mut self) {
        if self.build_pool() {
            self.launch_pool(None); // a plain roll rains the dice from the top
        }
    }

    /// Parse the input and build the dice pool — values decided up front. No
    /// bodies are spawned yet; [`Self::launch_pool`] does that. Returns `false`
    /// on a parse error (stored in `self.error`).
    fn build_pool(&mut self) -> bool {
        match parse::parse(&self.input) {
            Ok(Roll {
                terms,
                modifier,
                stake,
            }) => {
                self.error = None;
                self.modifier = modifier;
                self.stake = stake;
                self.explosions = vec![0; terms.len()];
                let mut dice: Vec<Die> = Vec::new();
                for (ti, term) in terms.iter().enumerate() {
                    self.roll_term(ti, term, &mut dice);
                }
                self.dice = dice;
                self.spawned = false;
                self.recorded = false;
                self.particles.clear();
                self.last_throw = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    /// Roll with the theater skipped: same parse, same RNG draws, same
    /// physics and explosions — the arena just runs to rest between two
    /// frames, so insta totals are bit-identical to animated ones under the
    /// same seed. The mid-air racket is dropped; the landing still speaks
    /// (one settle tick, plus any crit/fumble/verdict).
    pub fn insta_roll(&mut self) {
        self.roll();
        if self.error.is_some() {
            return;
        }
        // The snapshot test's budget: explosions settle one at a time, so a
        // chain needs room. A convergence bug still ends, just unsettled.
        for _ in 0..40_000 {
            self.update(1.0 / 60.0);
            if self.all_settled() {
                break;
            }
        }
        self.particles.clear();
        self.sounds.retain(|s| {
            matches!(
                s,
                SoundEvent::Crit | SoundEvent::Fumble | SoundEvent::Success | SoundEvent::Failure
            )
        });
        if let Some(d) = self.dice.first() {
            self.sounds.insert(0, SoundEvent::Settle { sides: d.sides });
        }
    }

    /// Roll one dice term's *base* pool and append it to `out`. Keep/drop and
    /// multiply are applied here; exploding is deferred to the animation — each
    /// die carries its term's explode condition and spawns a sibling when it
    /// settles on a matching face (see [`Self::settle_supported`]), so the chain
    /// unfolds on screen instead of all at once. Every die ever thrown stays in
    /// the pool; `kept`/`mult` decide what reaches the total. `color_idx`
    /// continues across terms so each die gets its own palette colour.
    fn roll_term(&mut self, term_idx: usize, term: &DiceTerm, out: &mut Vec<Die>) {
        let start = out.len();
        let base_color = start;
        let explode = explode_condition(term);

        // Base pool, each tagged with the term's explode condition.
        for _ in 0..term.count {
            let color = base_color + (out.len() - start);
            out.push(self.new_die(term.sides, term_idx, color, explode));
        }

        // Keep/drop flags discarded dice out of the total. It runs on the base
        // pool only: dice that explode later always count, which keeps the live
        // total monotonic as the chain unfolds (a dropped die never un-drops).
        apply_keep_drop(&mut out[start..], term);

        // Multiply tags every die in the term — including ones spawned later,
        // which inherit `mult` from this same term via the settle-time spawn.
        let mult = term_multiplier(term);
        for die in &mut out[start..] {
            die.mult = mult;
        }
    }

    /// Build one freshly-rolled die (value decided up front; the animation just
    /// reveals it). `kept`/`mult` default to "counts at face value"; `explode`
    /// is the term's condition so the die can spawn a sibling when it settles.
    fn new_die(
        &mut self,
        sides: u32,
        term_idx: usize,
        color_idx: usize,
        explode: Option<parse::Compare>,
    ) -> Die {
        let final_value = self.rng.gen_range(1..=sides);
        Die {
            sides,
            final_value,
            // The decoy face for the frames before the first physics step runs
            // `tumbling_face` — anything but `final_value`, which would flash the
            // real outcome on the launch frame. Derived, not drawn, so the seed
            // contract is untouched.
            shown: 1 + (color_idx as u32 * 7 + 2) % sides,
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            settled: false,
            color_idx,
            term_idx,
            mult: 1,
            kept: true,
            explode,
            exploded: false,
            age: 0.0,
            body: RigidBodyHandle::invalid(),
        }
    }

    /// Pick the dice up and start shaking the cup (the Throw). Validates the
    /// expression immediately so a typo surfaces now, not at release — and
    /// locks it: the eventual throw rolls this snapshot, whatever happens to
    /// the input in between.
    pub fn start_shake(&mut self) {
        if self.shake.is_some() {
            return;
        }
        match parse::parse(&self.input) {
            Ok(_) => {
                self.error = None;
                self.shake = Some(Shake {
                    t: 0.0,
                    expr: self.input.clone(),
                });
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn shaking(&self) -> bool {
        self.shake.is_some()
    }

    /// Seconds spent shaking so far (0 when not shaking). The UI keys the cup's
    /// rattle animation off this clock.
    pub fn shake_t(&self) -> f32 {
        self.shake.as_ref().map(|s| s.t).unwrap_or(0.0)
    }

    /// Put the dice down without throwing. Nothing is rolled or consumed.
    pub fn cancel_shake(&mut self) {
        self.shake = None;
    }

    /// Where the throw's power sits right now: 0..=1, rising from 0 the moment
    /// the cup is picked up, then oscillating. Catching the peak is the game.
    /// Power only shapes the launch — never the values, which stay pure RNG.
    pub fn power(&self) -> f32 {
        if self.shake.is_some() {
            0.5 - 0.5 * (self.shake_t() * SHAKE_POWER_RATE).cos()
        } else {
            0.0
        }
    }

    /// The cup's centre x in arena cells. It sways side to side, harder as
    /// power builds, so the throw's origin is part of the theater too.
    pub fn cup_x(&self) -> f32 {
        let centre = self.arena_w / 2.0;
        let amp = (self.arena_w * 0.18) * (0.35 + 0.65 * self.power());
        centre + (self.shake_t() * CUP_SWAY_RATE).sin() * amp
    }

    /// THE cell→normalised map for the cup's sway: its position as −1..1 across
    /// the arena. The renderer places the 3D cup with this and the throw aims
    /// away from it, so where the dice fly from can never drift off where the
    /// cup is drawn.
    pub fn cup_offset(&self) -> f32 {
        (self.cup_x() / self.arena_w.max(1.0)) * 2.0 - 1.0
    }

    /// Release the shake: roll the expression locked at pickup and launch the
    /// dice out of the cup with the power caught at this instant.
    pub fn throw(&mut self) {
        let power = self.power();
        let cup = self.cup_offset();
        let Some(shake) = self.shake.take() else {
            return;
        };
        self.input = shake.expr;
        if self.build_pool() {
            self.launch_pool(Some((power, cup)));
            self.last_throw = Some(Throw { power, age: 0.0 });
            self.emit(SoundEvent::Throw { power });
        }
    }

    /// Queue a sound for the event loop to play. Self-capping so headless
    /// runs (tests, the stats sampler) never grow the queue without bound.
    fn emit(&mut self, ev: SoundEvent) {
        if self.sounds.len() < MAX_SOUNDS {
            self.sounds.push(ev);
        }
    }

    /// Queue a sound that must survive a full queue — the once-per-roll
    /// climax events (crit ring, verdict). Evicts the oldest queued noise
    /// instead of dropping the new one, so 64 wall knocks can never silence
    /// the natural 20 they preceded.
    fn emit_priority(&mut self, ev: SoundEvent) {
        if self.sounds.len() >= MAX_SOUNDS {
            self.sounds.remove(0);
        }
        self.sounds.push(ev);
    }

    /// Hand over (and clear) the queued sounds — empty when muted. The mute
    /// rule lives here so every drain site inherits it instead of each
    /// remembering its own gate.
    pub fn take_sounds(&mut self) -> Vec<SoundEvent> {
        let events = std::mem::take(&mut self.sounds);
        if self.muted {
            Vec::new()
        } else {
            events
        }
    }

    /// The release echo: the most recent throw while it's still fresh enough
    /// to show (the frozen caught-power meter). Purely descriptive — the
    /// physics already happened.
    pub fn release_echo(&self) -> Option<Throw> {
        self.last_throw.filter(|t| t.age < RELEASE_ECHO_SECS)
    }

    /// How hard the arena should tremble right now (0 = still). A hard throw
    /// rattles the box for its first instants; a lob doesn't move it.
    pub fn tremor(&self) -> f32 {
        match self.last_throw {
            Some(t) if t.power > 0.6 && t.age < 0.35 => (t.power - 0.6) / 0.4,
            _ => 0.0,
        }
    }

    /// The staked verdict, once everything is settled: [`check`]'s
    /// `(success, margin)` for the current total against the `vs` stake.
    pub fn verdict(&self) -> Option<(bool, i32)> {
        let stake = self.stake?;
        if !self.all_settled() {
            return None;
        }
        Some(check(self.total(), stake))
    }

    /// Kept dice that settled on their proudest face — [`crit_face`] on any
    /// die type (a 6 on a d6 counts as much as a 20 on a d20).
    pub fn crit_dice(&self) -> impl Iterator<Item = &Die> {
        self.dice
            .iter()
            .filter(|d| d.kept && d.settled && crit_face(d.sides, d.final_value))
    }

    /// Kept dice of any type that settled on a 1 ([`fumble_face`]).
    pub fn fumble_dice(&self) -> impl Iterator<Item = &Die> {
        self.dice
            .iter()
            .filter(|d| d.kept && d.settled && fumble_face(d.sides, d.final_value))
    }

    /// Spawn every die's rigid body into the physics world and launch it. A
    /// plain roll rains them from the top (`power` = None); a released throw
    /// fires them with the caught power (`Some((0..1, cup_offset))`, the cup's
    /// normalised −1..1 position from [`Self::cup_offset`]). Only the launch is
    /// power's to shape — the faces were rolled in [`Self::roll`].
    /// The RNG draws here are identical for insta and animated rolls, and the
    /// physics is fixed-step deterministic, so the two settle bit-identically.
    fn launch_pool(&mut self, power: Option<(f32, f32)>) {
        self.physics.clear();
        self.phys_accum = 0.0;
        let count = self.dice.len();
        let cols = count.clamp(1, 6);
        for i in 0..count {
            let sides = self.dice[i].sides;
            let pts = mesh_points(sides);

            // Stagger spawn spots on a 3D lattice — columns across the width,
            // then depth slots, then layers stacked DOWNWARD from near the top
            // (going up would push dice through the ceiling and eject them).
            // Every spot sits a die-diameter (2·DIE_R = 0.72) from its
            // neighbours, so even the 60-die cap never spawns two dice deeply
            // interpenetrating — Rapier's penetration recovery would hurl such
            // a pair apart hard enough to eject dice from the tray. The lattice
            // is 6 × 3 × 4 = 72 spots, comfortably over the cap.
            let col = i % cols;
            let zi = (i / cols) % 3;
            let layer = i / (cols * 3);
            let fx = if cols > 1 {
                col as f32 / (cols - 1) as f32
            } else {
                0.5
            };
            let x = (fx * 2.0 - 1.0) * physics::HX * 0.7 + self.rng.gen_range(-0.05..=0.05);
            // Depth slots spread across the tray: centre plus one slot near each
            // z wall (die radius + the ±0.02 jitter margin inside it), so the
            // lattice uses the whole depth. Slot spacing must stay ≥ a die
            // diameter (2·DIE_R = 0.72) to keep spawns non-interpenetrating —
            // `zs ≥ 0.72` holds for any HZ ≥ 1.1, and at HZ = 1.1 this derives
            // exactly the old hand-tuned 0.72 slots.
            let zs = physics::HZ - physics::DIE_R - 0.02;
            let z = [0.0, -zs, zs][zi] + self.rng.gen_range(-0.02..=0.02);
            let y =
                (physics::HY * 0.6 - layer as f32 * 0.8).max(-physics::HY + physics::DIE_R + 0.25); // backstop: never below the floor
            let pos = Vec3::new(x, y, z);
            let body = self.physics.spawn(&pts, pos);
            self.dice[i].body = body;
            self.dice[i].pos = pos;

            let (linvel, angvel) = match power {
                Some((p, cx)) => {
                    // Force and direction are separate dials. The caught power
                    // sets only the speed; the aim leans away from wherever the
                    // cup ended up swaying (`cx`, −1..1) plus a real per-die
                    // spray. Power and sway both ride the same shake clock, so
                    // a bare sign-of-`cx` aim made every same-power release
                    // fly the same way — a full-power throw always broke right.
                    let speed = 4.0 + 9.0 * p;
                    let aim = (-cx + self.rng.gen_range(-0.6..=0.6)).clamp(-1.0, 1.0);
                    let lin = Vec3::new(
                        aim * speed * self.rng.gen_range(0.45..=0.65),
                        speed * self.rng.gen_range(0.4..=0.7),
                        self.rng.gen_range(-1.0..=1.0),
                    );
                    let ang = Vec3::new(
                        self.rng.gen_range(-22.0..=22.0),
                        self.rng.gen_range(-22.0..=22.0),
                        self.rng.gen_range(-22.0..=22.0),
                    );
                    (lin, ang)
                }
                None => {
                    let lin = Vec3::new(
                        self.rng.gen_range(-2.0..=2.0),
                        self.rng.gen_range(-7.0..=-3.0),
                        self.rng.gen_range(-1.5..=1.5),
                    );
                    let ang = Vec3::new(
                        self.rng.gen_range(-18.0..=18.0),
                        self.rng.gen_range(-18.0..=18.0),
                        self.rng.gen_range(-18.0..=18.0),
                    );
                    (lin, ang)
                }
            };
            self.physics.launch(body, linvel, angvel);
        }
        self.spawned = true;
    }

    /// Cosmetic wall-clock (seconds), driving the arena's idle camera drift.
    pub fn clock(&self) -> f32 {
        self.clock
    }

    /// Crit celebration level (1 → 0): the gold flare + camera punch on a natural
    /// crit, decaying to nothing. Cosmetic.
    pub fn flash(&self) -> f32 {
        self.flash
    }

    /// Recent die-impact energy (0..~1.5), for the key light's flinch. Cosmetic.
    pub fn impact_energy(&self) -> f32 {
        self.impact_energy
    }

    /// Camera "reading" focus (0 → 1), keyed to the ceremony: eased toward 1 the
    /// moment a roll is launched — held through the flight and the settle, the
    /// lean-in over the felt — and back to 0 while the cup is shaking. Cosmetic.
    pub fn focus(&self) -> f32 {
        self.focus
    }

    /// The hard-throw camera shudder, as a world-space eye offset — the 3D heir
    /// to the 2D screen-shake. Fed into `render3d_view::live_camera` by the
    /// renderer and the particle projection alike, so the shake can't knock the
    /// two out of register.
    pub fn camera_shake(&self) -> Vec3 {
        let tr = self.tremor();
        match self.last_throw {
            Some(t) if tr > 0.0 => {
                let (amp, phase) = (tr * 0.055, t.age * 38.0);
                Vec3::new(phase.sin() * amp, phase.cos() * amp, 0.0)
            }
            _ => Vec3::ZERO,
        }
    }

    /// Advance the simulation by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        let dt = dt.min(0.05); // clamp so a stalled frame doesn't teleport dice
        self.clock += dt; // cosmetic wall-clock for the idle camera drift
        self.flash = (self.flash - dt * 2.2).max(0.0); // crit flare fades over ~0.45 s
        self.impact_energy = (self.impact_energy - dt * 4.0).max(0.0); // flinch fades fast

        // Ease the reading-focus by ceremony, not by rest: lean IN from the moment a
        // throw is launched (and hold through the flight and the settle, so you watch
        // the dice come to a readable stop), and lean back OUT the instant the next
        // shake begins — the cup coming up is the cue to pull back to the wide view.
        let want_focus = if self.shaking() || self.dice.is_empty() {
            0.0
        } else {
            1.0
        };
        self.focus += (want_focus - self.focus) * (dt * 2.2).min(1.0);

        // The shake clock ticks whether or not any dice are in flight — the cup
        // rattles over whatever the last roll left on the floor. Deriving both
        // cycle indices from the clock means a fresh shake can't inherit stale state.
        if let Some(shake) = &mut self.shake {
            let before = (shake.t * CUP_SWAY_RATE / std::f32::consts::PI) as i32;
            shake.t += dt;
            let after = (shake.t * CUP_SWAY_RATE / std::f32::consts::PI) as i32;
            if after != before {
                let power = self.power();
                self.emit(SoundEvent::Rattle { power });
            }
        }
        if let Some(throw) = &mut self.last_throw {
            throw.age += dt;
        }
        self.update_particles(dt);

        if !self.spawned {
            return;
        }

        // Advance the physics on a FIXED timestep (physics::STEP, the one true
        // 1/60 s), accumulating real time. An insta roll (one step per call) and
        // an animated roll (several steps per frame) run the identical simulation
        // and settle bit-identically — the seeded-roll contract survives real
        // physics. Once every die is at rest nothing can move again until the
        // next launch, so skip the stepping entirely rather than churn Rapier
        // over a world of sleepers for as long as the result sits on screen.
        if self.all_settled() {
            self.phys_accum = 0.0;
        } else {
            self.phys_accum += dt;
            let mut budget = 8; // cap steps per frame so a stall can't spiral
            while self.phys_accum >= physics::STEP && budget > 0 {
                self.phys_accum -= physics::STEP;
                budget -= 1;
                self.physics_step();
            }
        }

        // The frame the roll finishes, record it once for the history pane.
        if !self.recorded && self.all_settled() {
            self.record_roll();
            self.recorded = true;
        }
    }

    /// One fixed physics step: advance Rapier, voice any impacts, sync each die's
    /// pose from its body, settle the sleepers, and spawn explosion dice.
    fn physics_step(&mut self) {
        // Every die's speed before the step, so a sharp slowdown (a bounce) can
        // be reported as an impact for the foley. Settled dice can't produce an
        // impact (their speed is already 0) but stay in the list so a moving die
        // striking one is voiced as a die-vs-die knock, not a wall strike.
        let pre: Vec<(RigidBodyHandle, u32, f32)> = self
            .dice
            .iter()
            .map(|d| (d.body, d.sides, self.physics.speed(d.body)))
            .collect();
        let mut voiced = 0;
        for imp in self.physics.step(&pre) {
            if imp.speed > 1.5 {
                // A hard contact kicks the key light (cosmetic; capped and decayed).
                self.impact_energy = (self.impact_energy + imp.speed * 0.12).min(1.5);
                // A dense pool landing at once is dozens of near-identical clicks
                // in one step — more than anyone can hear. Voice a frame's worth.
                if voiced >= KNOCK_BUDGET {
                    continue;
                }
                voiced += 1;
                let (sides, speed) = (imp.sides, imp.speed * 12.0);
                if imp.die_die {
                    self.emit(SoundEvent::Knock { sides, speed });
                } else {
                    self.emit(SoundEvent::Impact { sides, speed });
                }
            }
        }
        self.sync_and_settle(physics::STEP);
    }

    /// Sync poses from the bodies, settle any that have gone to sleep (or run out
    /// the airborne clock), and spawn the dice that exploding faces call for.
    fn sync_and_settle(&mut self, dt: f32) {
        let mut to_explode: Vec<(u32, usize, i32, parse::Compare)> = Vec::new();
        for i in 0..self.dice.len() {
            if self.dice[i].settled {
                continue;
            }
            let (pos, rot) = self.physics.pose(self.dice[i].body);
            self.dice[i].pos = pos;
            self.dice[i].rot = rot;
            self.dice[i].age += dt;

            // While it's still moving, the up-face number flickers with the spin —
            // a secret still forming. The settle branch below overwrites it with
            // the real value (the burn) on the step the die comes to rest.
            self.dice[i].shown = tumbling_face(rot, self.dice[i].sides);

            // Hard cap: a die tumbling past MAX_AIRBORNE is frozen where it lies,
            // a backstop so an over-packed pile can't agitate itself forever.
            let overdue = self.dice[i].age >= MAX_AIRBORNE;
            if overdue {
                self.physics.freeze(self.dice[i].body);
            }
            if overdue || self.physics.sleeping(self.dice[i].body) {
                self.dice[i].settled = true;
                // The burn: the RNG-decided value locks onto the up-face.
                self.dice[i].shown = self.dice[i].final_value;
                let sides = self.dice[i].sides;
                self.emit(SoundEvent::Settle { sides });

                // Exploding: a die that rests on a matching face spawns one more,
                // once, while its term is under the cap. Read the fields out first
                // so the `self.dice` borrow doesn't overlap `self.explosions`.
                let (exploded, explode, final_value, term, mult) = {
                    let d = &self.dice[i];
                    (d.exploded, d.explode, d.final_value, d.term_idx, d.mult)
                };
                if !exploded {
                    self.dice[i].exploded = true;
                    if let Some(cmp) = explode {
                        if cmp.matches(final_value) && self.explosions[term] < MAX_EXPLOSIONS {
                            self.explosions[term] += 1;
                            to_explode.push((sides, term, mult, cmp));
                        }
                    }
                }
            }
        }

        // Drop the explosion dice from the top, exactly like a fresh throw.
        for (sides, term_idx, mult, cmp) in to_explode {
            let color = self.dice.len();
            let mut die = self.new_die(sides, term_idx, color, Some(cmp));
            die.mult = mult;
            let x = self.rng.gen_range(-physics::HX * 0.6..=physics::HX * 0.6);
            let z = self.rng.gen_range(-physics::HZ * 0.5..=physics::HZ * 0.5);
            let pos = Vec3::new(x, physics::HY * 0.6, z);
            let pts = mesh_points(sides);
            die.body = self.physics.spawn(&pts, pos);
            die.pos = pos;
            self.physics.launch(
                die.body,
                Vec3::new(
                    self.rng.gen_range(-2.0..=2.0),
                    self.rng.gen_range(-6.0..=-3.0),
                    self.rng.gen_range(-1.0..=1.0),
                ),
                Vec3::new(
                    self.rng.gen_range(-18.0..=18.0),
                    self.rng.gen_range(-18.0..=18.0),
                    self.rng.gen_range(-18.0..=18.0),
                ),
            );
            self.dice.push(die);
        }
    }

    /// Append the just-settled roll to the history (newest last), capped —
    /// and let the moment land: crit bursts, fumble dust, the staked verdict.
    fn record_roll(&mut self) {
        let values: Vec<u32> = self
            .dice
            .iter()
            .filter(|d| d.kept)
            .map(|d| d.final_value)
            .collect();
        self.history.push(HistoryEntry {
            expr: self.input.trim().to_string(),
            values,
            total: self.total(),
        });
        if self.history.len() > MAX_HISTORY {
            let overflow = self.history.len() - MAX_HISTORY;
            self.history.drain(0..overflow);
        }

        // Every maxed die bursts gold and every 1 slumps, but the ring and the
        // thud play once per roll however many dice earned them — a fistful of
        // sixes is one chord, not a bell choir. Positions are copied out first
        // so the burst can borrow the RNG mutably.
        let crit_pos: Vec<Vec3> = self.crit_dice().map(|d| d.pos).collect();
        let fumble_pos: Vec<Vec3> = self.fumble_dice().map(|d| d.pos).collect();
        // Fire the flash *before* projecting the bursts: the next frames render
        // through the punched-in camera, so the bursts must be placed by it too
        // or the gold would erupt beside the die instead of from it.
        if !crit_pos.is_empty() {
            self.flash = 1.0; // fire the gold flare + camera punch
        }
        let crits: Vec<(f32, f32)> = crit_pos.iter().map(|&p| self.world_to_cell(p)).collect();
        let fumbles: Vec<(f32, f32)> = fumble_pos.iter().map(|&p| self.world_to_cell(p)).collect();
        for &(x, y) in &crits {
            self.burst(x, y, true);
        }
        for &(x, y) in &fumbles {
            self.burst(x, y, false);
        }
        if !crits.is_empty() {
            self.emit_priority(SoundEvent::Crit);
        }
        if !fumbles.is_empty() {
            self.emit_priority(SoundEvent::Fumble);
        }
        match self.verdict() {
            Some((true, _)) => self.emit_priority(SoundEvent::Success),
            Some((false, _)) => self.emit_priority(SoundEvent::Failure),
            None => {}
        }
    }

    /// Spray celebration glyphs from a die's centre: a gold radial burst for a
    /// crit, a small grey slump of dust for a fumble.
    fn burst(&mut self, x: f32, y: f32, bright: bool) {
        // `x`/`y` are already the die's projected centre in cells.
        let (cx, cy) = (x, y);
        let n = if bright { CRIT_PARTICLES } else { 6 };
        for i in 0..n {
            let angle =
                (i as f32 / n as f32) * std::f32::consts::TAU + self.rng.gen_range(-0.2..=0.2);
            // Fumble dust barely rises; crit sparks fly.
            let speed = if bright {
                self.rng.gen_range(9.0..=20.0)
            } else {
                self.rng.gen_range(2.0..=5.0)
            };
            let glyph = if bright {
                ['✦', '*', '·'][i % 3]
            } else {
                '·'
            };
            self.particles.push(Particle {
                x: cx,
                y: cy,
                vx: angle.cos() * speed * 1.8, // terminal cells are tall; widen the spray
                vy: angle.sin() * speed - if bright { 6.0 } else { 0.0 },
                age: 0.0,
                life: if bright {
                    self.rng.gen_range(0.7..=1.2)
                } else {
                    self.rng.gen_range(0.4..=0.8)
                },
                glyph,
                bright,
            });
        }
    }

    /// Drift, fall, and expire the celebration glyphs. Light gravity so a crit
    /// burst blooms and rains instead of dropping like rocks.
    fn update_particles(&mut self, dt: f32) {
        let (maxx, maxy) = (self.arena_w.max(1.0), self.arena_h.max(1.0));
        for p in &mut self.particles {
            p.age += dt;
            p.vy += GRAVITY * 0.25 * dt;
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            // Out of the arena = done, a touch early is fine.
            if p.x < -1.0 || p.x > maxx || p.y > maxy {
                p.age = p.life;
            }
        }
        self.particles.retain(|p| p.age < p.life);
    }

    pub fn all_settled(&self) -> bool {
        self.spawned && !self.dice.is_empty() && self.dice.iter().all(|d| d.settled)
    }

    /// World→arena-cell mapping for the 2D particle overlays (crit gold, fumble
    /// dust). It projects through the *same* [`live_camera`](crate::render3d_view::live_camera)
    /// the dice are drawn with — drift, punch-in, shudder and all — so a burst
    /// erupts exactly from the die that earned it.
    fn world_to_cell(&self, pos: Vec3) -> (f32, f32) {
        if self.arena_w < 1.0 || self.arena_h < 1.0 {
            return (self.arena_w / 2.0, self.arena_h / 2.0);
        }
        let aspect = crate::render3d_view::arena_aspect(self.arena_w, self.arena_h);
        let cam = crate::render3d_view::live_camera(
            self.camera_shake(),
            aspect,
            self.focus,
            self.clock,
            self.flash,
        );
        crate::render3d_view::project_to_cell(&cam, pos, self.arena_w, self.arena_h)
            .unwrap_or((self.arena_w / 2.0, self.arena_h / 2.0))
    }

    /// Final total (only meaningful once settled, but always well-defined).
    pub fn total(&self) -> i32 {
        self.summed(|d| d.final_value as i32)
    }

    /// Sum each term's kept dice (via `value`), scale by the term's multiplier,
    /// and add the flat modifier once. Dropped dice (`!kept`) contribute nothing.
    /// A per-term multiplier means the total can't be a flat sum over all dice —
    /// a `×2` term's dice must be summed and *then* doubled, so we group by term.
    fn summed(&self, value: impl Fn(&Die) -> i32) -> i32 {
        let mut sum = 0i32;
        let mut i = 0;
        while i < self.dice.len() {
            let term = self.dice[i].term_idx;
            let mult = self.dice[i].mult;
            let mut term_sum = 0i32;
            while i < self.dice.len() && self.dice[i].term_idx == term {
                if self.dice[i].kept {
                    term_sum += value(&self.dice[i]);
                }
                i += 1;
            }
            sum += term_sum * mult;
        }
        sum + self.modifier
    }

    /// Compute the statistics pane's contents for the current input: the
    /// theoretical distribution (by Monte-Carlo sampling, so every modifier is
    /// handled) plus a summary of the session's history. Returns `Err` (with the
    /// parse error) if the expression doesn't parse.
    ///
    /// Memoized on (expression, history length): the render loop calls this
    /// every frame while the stats pane is open, and 20 000 samples per frame
    /// is a core of wasted heat for bit-identical numbers — the sample RNG is
    /// seeded from exactly the values in the key.
    pub fn stats(&mut self) -> Result<Stats, String> {
        let expr = self.input.trim().to_string();
        if let Some((k_expr, k_rolls, stats)) = &self.stats_cache {
            if *k_expr == expr && *k_rolls == self.history.len() {
                return Ok(stats.clone());
            }
        }

        let roll = parse::parse(&expr)?;

        // Sample the expression many times to estimate its distribution.
        let mut sample_rng = StdRng::seed_from_u64(self.history.len() as u64 ^ 0x5715_d1ce);
        let mut totals = Vec::with_capacity(STAT_SAMPLES);
        let mut sum = 0i64;
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for _ in 0..STAT_SAMPLES {
            let t = sample_total(&roll, &mut sample_rng);
            totals.push(t);
            sum += t as i64;
            lo = lo.min(t);
            hi = hi.max(t);
        }
        let mean = sum as f64 / STAT_SAMPLES as f64;

        // A staked expression also gets its odds of succeeding, judged by the
        // same check() as the real verdict — so roll-under odds fall as the
        // target rises, just as the arena's verdict would.
        let success_odds = roll.stake.map(|s| {
            totals.iter().filter(|&&v| check(v, s).0).count() as f64 / STAT_SAMPLES as f64
        });

        // A coarse histogram over the observed range for the little curve.
        let dist = histogram(&totals, lo, hi);

        // Session history for the current expression (and overall).
        let here: Vec<&HistoryEntry> = self.history.iter().filter(|e| e.expr == expr).collect();
        let session = SessionStats::from(&here);

        let stats = Stats {
            expr: expr.clone(),
            samples: STAT_SAMPLES,
            min: lo,
            max: hi,
            mean,
            stake: roll.stake,
            success_odds,
            dist,
            session,
            total_rolls: self.history.len(),
        };
        self.stats_cache = Some((expr, self.history.len(), stats.clone()));
        Ok(stats)
    }
}

/// One bucket of the sampled distribution: a total value and how often it came up.
#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    pub total: i32,
    pub fraction: f64, // 0.0..=1.0
}

/// Theoretical odds for an expression plus a summary of the session so far.
#[derive(Debug, Clone)]
pub struct Stats {
    pub expr: String,
    pub samples: usize,
    pub min: i32,
    pub max: i32,
    pub mean: f64,
    /// The stakes (`vs N`), when the expression is staked.
    pub stake: Option<Stake>,
    /// Estimated chance the staked roll succeeds (present iff `stake` is).
    pub success_odds: Option<f64>,
    /// Up to a handful of buckets spanning min..=max, for a sparkline-ish curve.
    pub dist: Vec<Bucket>,
    /// Stats over the rolls of *this* expression actually made this session.
    pub session: SessionStats,
    /// How many rolls are in the whole session history.
    pub total_rolls: usize,
}

/// Aggregates of the actual rolls made this session for one expression.
#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    pub count: usize,
    pub min: i32,
    pub max: i32,
    pub mean: f64,
}

impl SessionStats {
    fn from(entries: &[&HistoryEntry]) -> Self {
        if entries.is_empty() {
            return SessionStats::default();
        }
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        let mut sum = 0i64;
        for e in entries {
            lo = lo.min(e.total);
            hi = hi.max(e.total);
            sum += e.total as i64;
        }
        SessionStats {
            count: entries.len(),
            min: lo,
            max: hi,
            mean: sum as f64 / entries.len() as f64,
        }
    }
}

/// Bucket `totals` into at most `MAX_BUCKETS` evenly-spaced bins spanning
/// `lo..=hi`, returning each bin's representative value and its share of the
/// samples. A single-value distribution collapses to one full bucket.
fn histogram(totals: &[i32], lo: i32, hi: i32) -> Vec<Bucket> {
    const MAX_BUCKETS: usize = 11;
    if totals.is_empty() {
        return Vec::new();
    }
    let span = (hi - lo).max(0) as usize + 1;
    let bins = span.min(MAX_BUCKETS);
    let width = (span as f64 / bins as f64).max(1.0);

    let mut counts = vec![0usize; bins];
    for &t in totals {
        let b = (((t - lo) as f64) / width) as usize;
        counts[b.min(bins - 1)] += 1;
    }
    let n = totals.len() as f64;
    counts
        .iter()
        .enumerate()
        .map(|(b, &c)| Bucket {
            // Label each bucket with the rounded centre of the range it covers,
            // so a coarsened curve still reads as "roughly this total".
            total: lo + (b as f64 * width + width / 2.0).floor() as i32,
            fraction: c as f64 / n,
        })
        .collect()
}

/// Evaluate a parsed roll once, instantly, returning its total. This is the
/// non-animated twin of [`App::roll`] + the settle-time explosion: same rules
/// (explode → keep/drop on the base pool → per-term multiply → flat modifier),
/// just resolved in a tight loop. Used to Monte-Carlo a roll's distribution for
/// the statistics pane, where running the physics thousands of times is absurd.
fn sample_total(roll: &Roll, rng: &mut StdRng) -> i32 {
    let mut total = roll.modifier;
    for term in &roll.terms {
        let explode = explode_condition(term);
        let mult = term_multiplier(term);

        // Base pool: (value, kept). Explosions append more dice, always kept.
        let mut pool: Vec<(u32, bool)> = (0..term.count)
            .map(|_| (rng.gen_range(1..=term.sides), true))
            .collect();

        if let Some(cmp) = explode {
            let mut spawned = 0usize;
            let mut i = 0;
            while i < pool.len() {
                if cmp.matches(pool[i].0) && spawned < MAX_EXPLOSIONS {
                    pool.push((rng.gen_range(1..=term.sides), true));
                    spawned += 1;
                }
                i += 1;
            }
        }

        // Keep/drop applies to the base pool only (exploded dice always count),
        // matching the animation. Flag the discarded base dice out.
        let base = term.count as usize;
        for m in &term.mods {
            let (high, n) = match *m {
                TermMod::KeepHigh(n) => (true, n as usize),
                TermMod::DropLow(n) => (true, base.saturating_sub(n as usize)),
                TermMod::KeepLow(n) => (false, n as usize),
                TermMod::DropHigh(n) => (false, base.saturating_sub(n as usize)),
                _ => continue,
            };
            keep_n_values(&mut pool[..base], n, high);
        }

        let term_sum: i32 = pool
            .iter()
            .filter(|&&(_, k)| k)
            .map(|&(v, _)| v as i32)
            .sum();
        total += term_sum * mult;
    }
    total
}

/// One rolled die in a headless evaluation.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct OutcomeDie {
    pub value: u32,
    /// Whether this die counts toward the total (false ⇒ dropped by keep/drop).
    pub kept: bool,
    /// Whether this die was spawned by an explosion (vs. a base-pool die).
    pub exploded: bool,
    /// A kept die on its max face ([`crit_face`]) — the same call the arena
    /// celebrates, so JSON consumers never re-derive the rule.
    pub crit: bool,
    /// A kept die on a 1 ([`fumble_face`]).
    pub fumble: bool,
}

/// The result of evaluating one dice term headlessly: its dice and subtotal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OutcomeTerm {
    /// The term as written, e.g. "4d6dl1" or "3d6!".
    pub notation: String,
    pub sides: u32,
    pub dice: Vec<OutcomeDie>,
    /// The per-term multiplier (`*N`), 1 when absent.
    pub multiplier: i32,
    /// Sum of this term's kept dice, after the multiplier.
    pub subtotal: i32,
}

/// A complete headless roll: every term's dice plus the grand total. This is the
/// data behind the plain / verbose / `--json` CLI output and is the non-animated
/// equivalent of a full settled roll in the TUI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Outcome {
    /// The whole expression as typed.
    pub expression: String,
    pub terms: Vec<OutcomeTerm>,
    /// The flat `+N`/`-N` modifier.
    pub modifier: i32,
    pub total: i32,
    /// The `vs N` target, when the roll was staked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<i32>,
    /// The stake direction: `over` (meet or beat) or `under` (roll under).
    /// Present iff `target` is, so JSON consumers read the direction of the
    /// check rather than guess it from the sign of the margin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<Goal>,
    /// Whether the roll made its stake (present iff `target` is).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    /// How far the check succeeded (≥ 0) or failed (< 0) by, per [`check`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin: Option<i32>,
}

/// Evaluate a parsed roll once into a full breakdown (every die, kept/exploded
/// flags, per-term subtotals, grand total). Mirrors the animation's semantics
/// exactly — explode → keep/drop on the base pool → per-term multiply → flat
/// modifier — but resolves instantly. Used by the one-shot CLI path; [`App`]'s
/// animated roll and this share the same rules so a die shown bouncing and a die
/// printed to stdout always agree.
pub fn evaluate(expression: &str, roll: &Roll, rng: &mut StdRng) -> Outcome {
    let mut terms = Vec::with_capacity(roll.terms.len());
    let mut total = roll.modifier;

    for term in &roll.terms {
        let explode = explode_condition(term);
        let mult = term_multiplier(term);
        let base = term.count as usize;

        // Base pool, then explosions appended (always kept). Track which dice
        // were spawned by explosions so the output can flag them.
        let mut dice: Vec<OutcomeDie> = (0..base)
            .map(|_| OutcomeDie {
                value: rng.gen_range(1..=term.sides),
                kept: true,
                exploded: false,
                crit: false,
                fumble: false,
            })
            .collect();

        if let Some(cmp) = explode {
            let mut spawned = 0usize;
            let mut i = 0;
            while i < dice.len() {
                if cmp.matches(dice[i].value) && spawned < MAX_EXPLOSIONS {
                    dice.push(OutcomeDie {
                        value: rng.gen_range(1..=term.sides),
                        kept: true,
                        exploded: true,
                        crit: false,
                        fumble: false,
                    });
                    spawned += 1;
                }
                i += 1;
            }
        }

        // Keep/drop on the base pool only (exploded dice always count).
        for m in &term.mods {
            let (high, n) = match *m {
                TermMod::KeepHigh(n) => (true, n as usize),
                TermMod::DropLow(n) => (true, base.saturating_sub(n as usize)),
                TermMod::KeepLow(n) => (false, n as usize),
                TermMod::DropHigh(n) => (false, base.saturating_sub(n as usize)),
                _ => continue,
            };
            keep_n_outcome(&mut dice[..base], n, high);
        }

        // Crit/fumble is judged after keep/drop — a dropped 20 celebrates
        // nothing — with the same face rules the arena uses.
        for d in &mut dice {
            d.crit = d.kept && crit_face(term.sides, d.value);
            d.fumble = d.kept && fumble_face(term.sides, d.value);
        }

        let kept_sum: i32 = dice.iter().filter(|d| d.kept).map(|d| d.value as i32).sum();
        let subtotal = kept_sum * mult;
        total += subtotal;

        terms.push(OutcomeTerm {
            notation: term_notation(term),
            sides: term.sides,
            dice,
            multiplier: mult,
            subtotal,
        });
    }

    // The staked verdict: the same check() App::verdict calls.
    let checked = roll.stake.map(|s| check(total, s));
    Outcome {
        expression: expression.to_string(),
        terms,
        modifier: roll.modifier,
        total,
        target: roll.stake.map(|s| s.target),
        goal: roll.stake.map(|s| s.goal),
        success: checked.map(|(s, _)| s),
        margin: checked.map(|(_, m)| m),
    }
}

/// Keep the `n` highest/lowest-valued kept dice, flagging the rest out. The
/// [`OutcomeDie`] twin of [`keep_n_values`].
fn keep_n_outcome(dice: &mut [OutcomeDie], n: usize, high: bool) {
    let mut live: Vec<usize> = (0..dice.len()).filter(|&i| dice[i].kept).collect();
    live.sort_by_key(|&i| dice[i].value);
    if high {
        live.reverse();
    }
    for &i in live.iter().skip(n) {
        dice[i].kept = false;
    }
}

/// Reconstruct a term's notation from its parsed form, e.g. "4d6dl1*2". Used to
/// label terms in the breakdown output.
fn term_notation(term: &DiceTerm) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    if term.count != 1 {
        let _ = write!(s, "{}", term.count);
    }
    let _ = write!(s, "d{}", term.sides);
    for m in &term.mods {
        match *m {
            TermMod::KeepHigh(n) => {
                let _ = write!(s, "kh{n}");
            }
            TermMod::KeepLow(n) => {
                let _ = write!(s, "kl{n}");
            }
            TermMod::DropHigh(n) => {
                let _ = write!(s, "dh{n}");
            }
            TermMod::DropLow(n) => {
                let _ = write!(s, "dl{n}");
            }
            TermMod::Explode(None) => s.push('!'),
            TermMod::Explode(Some(parse::Compare::Eq(n))) => {
                let _ = write!(s, "!={n}");
            }
            TermMod::Explode(Some(parse::Compare::Gt(n))) => {
                let _ = write!(s, "!>{n}");
            }
            TermMod::Explode(Some(parse::Compare::Lt(n))) => {
                let _ = write!(s, "!<{n}");
            }
            TermMod::Mul(n) => {
                let _ = write!(s, "*{n}");
            }
        }
    }
    s
}

/// Keep the `n` highest- (or lowest-) valued kept entries in `pool`, flagging
/// the rest out. The value-slice twin of [`keep_n`].
fn keep_n_values(pool: &mut [(u32, bool)], n: usize, high: bool) {
    let mut live: Vec<usize> = (0..pool.len()).filter(|&i| pool[i].1).collect();
    live.sort_by_key(|&i| pool[i].0);
    if high {
        live.reverse();
    }
    for &i in live.iter().skip(n) {
        pool[i].1 = false;
    }
}

/// The explode condition for a term, if it has one. A bare `!` (no comparison)
/// means "explode on the max face", resolved here against the die's sides.
fn explode_condition(term: &DiceTerm) -> Option<parse::Compare> {
    term.mods.iter().find_map(|m| match m {
        TermMod::Explode(Some(c)) => Some(*c),
        TermMod::Explode(None) => Some(parse::Compare::Eq(term.sides)),
        _ => None,
    })
}

/// The product of all `*N` multipliers on a term (1 if none — the empty product).
fn term_multiplier(term: &DiceTerm) -> i32 {
    term.mods
        .iter()
        .filter_map(|m| match m {
            TermMod::Mul(n) => Some(*n),
            _ => None,
        })
        .product()
}

/// Flag dice out of a term's pool per its keep/drop modifiers. Operates on the
/// dice's `final_value` (the result is decided up front), so the displayed
/// running total already reflects the discard the whole way down. Multiple
/// keep/drop mods compose: each one further narrows what's already kept.
fn apply_keep_drop(dice: &mut [Die], term: &DiceTerm) {
    for m in &term.mods {
        let (keep_high, n) = match *m {
            TermMod::KeepHigh(n) => (true, n as usize),
            TermMod::DropLow(n) => (true, dice.len().saturating_sub(n as usize)),
            TermMod::KeepLow(n) => (false, n as usize),
            TermMod::DropHigh(n) => (false, dice.len().saturating_sub(n as usize)),
            _ => continue,
        };
        keep_n(dice, n, keep_high);
    }
}

/// Keep exactly `n` of the currently-kept dice — the highest `n` if `high`, the
/// lowest `n` otherwise — flagging the rest out. Ties break by position, which
/// is fine: equal faces are interchangeable for the total.
fn keep_n(dice: &mut [Die], n: usize, high: bool) {
    // Indices of dice still in play, ordered by value (desc to keep-high).
    let mut live: Vec<usize> = (0..dice.len()).filter(|&i| dice[i].kept).collect();
    live.sort_by_key(|&i| dice[i].final_value);
    if high {
        live.reverse();
    }
    for &i in live.iter().skip(n) {
        dice[i].kept = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step the simulation until everything settles, with a hard frame cap so a
    /// non-converging physics bug fails the test instead of hanging forever.
    fn settle(app: &mut App, max_frames: usize) -> Option<usize> {
        for f in 0..max_frames {
            app.update(1.0 / 60.0);
            if app.all_settled() {
                return Some(f);
            }
        }
        None
    }

    /// A die's centre is inside the physics tray (a small margin for its radius).
    fn in_box(p: Vec3) -> bool {
        p.x.abs() <= physics::HX + 0.25
            && p.y.abs() <= physics::HY + 0.25
            && p.z.abs() <= physics::HZ + 0.25
    }

    /// A deterministic App for testing roll semantics: same input, same RNG,
    /// same dice every time. The arena is sized so dice can settle.
    fn seeded(input: &str, seed: u64) -> App {
        let mut app = App {
            input: input.to_string(),
            cursor: input.len(),
            pane_scroll: 0,
            dice: Vec::new(),
            modifier: 0,
            stake: None,
            error: None,
            arena_w: 60.0,
            arena_h: 20.0,
            spawned: false,
            shake: None,
            last_throw: None,
            particles: Vec::new(),
            sounds: Vec::new(),
            muted: false,
            stats_cache: None,
            explosions: Vec::new(),
            history: Vec::new(),
            pane: Pane::None,
            mode: RollMode::Shake,
            recorded: false,
            rng: StdRng::seed_from_u64(seed),
            physics: Physics::new(),
            phys_accum: 0.0,
            clock: 0.0,
            flash: 0.0,
            impact_energy: 0.0,
            focus: 0.0,
        };
        app.roll();
        app
    }

    /// Step `app` for `secs` of simulated time without asserting anything.
    fn tick(app: &mut App, secs: f32) {
        let frames = (secs * 60.0) as usize;
        for _ in 0..frames {
            app.update(1.0 / 60.0);
        }
    }

    #[test]
    fn insta_rolls_the_same_dice_as_the_animation_under_the_same_seed() {
        // Exploding included: explosions draw from the RNG mid-flight, so
        // this only passes if insta runs the very same simulation.
        let expr = "4d6!kh3+2";

        let mut animated = seeded(expr, 9);
        for _ in 0..40_000 {
            animated.update(1.0 / 60.0);
            if animated.all_settled() {
                break;
            }
        }
        assert!(animated.all_settled());

        // Same seed, untouched by the empty-input parse error at build time.
        let mut insta = seeded("", 9);
        insta.input = expr.to_string();
        insta.insta_roll();

        assert!(insta.all_settled(), "insta must land settled");
        let a: Vec<u32> = animated.dice.iter().map(|d| d.final_value).collect();
        let b: Vec<u32> = insta.dice.iter().map(|d| d.final_value).collect();
        assert_eq!(a, b, "insta must roll exactly the animation's dice");
    }

    #[test]
    fn power_starts_at_zero_and_stays_in_unit_range() {
        let mut app = seeded("", 1);
        app.input = "3d6".into();
        app.start_shake();
        assert!(app.shaking());
        assert_eq!(app.power(), 0.0, "power starts from a standstill");
        // Sample a few seconds of shaking: power must stay in 0..=1 and move.
        let mut peak = 0.0f32;
        for _ in 0..300 {
            app.update(1.0 / 60.0);
            let p = app.power();
            assert!((0.0..=1.0).contains(&p), "power {p} escaped 0..=1");
            peak = peak.max(p);
        }
        assert!(peak > 0.95, "power never came near its peak (max {peak})");
    }

    #[test]
    fn shaking_rolls_nothing_until_released() {
        let mut app = seeded("", 2);
        app.input = "3d6".into();
        app.start_shake();
        tick(&mut app, 1.0);
        assert!(app.dice.is_empty(), "dice appeared before the throw");
        assert!(app.history.is_empty());

        // Cancelling puts the dice down: still nothing rolled.
        app.cancel_shake();
        assert!(!app.shaking());
        assert!(app.dice.is_empty(), "cancelling a shake must not roll");
    }

    #[test]
    fn a_bad_expression_errors_at_pickup_not_at_release() {
        let mut app = seeded("", 3);
        app.input = "nonsense".into();
        app.start_shake();
        assert!(!app.shaking(), "a bad expression must not start a shake");
        assert!(app.error.is_some(), "the typo surfaces immediately");
    }

    #[test]
    fn a_throw_launches_from_the_cup_and_settles_in_bounds() {
        for seed in 0..20 {
            let mut app = seeded("", seed);
            app.input = "4d6".into();
            app.start_shake();
            tick(&mut app, 0.4); // release on the rise
            app.throw();
            assert!(!app.shaking());
            assert_eq!(app.dice.len(), 4);

            // The thrown pool converges inside the tray like any other roll.
            settle(&mut app, 20000).expect("thrown dice never settled");
            for d in &app.dice {
                assert!(
                    in_box(d.pos),
                    "seed {seed}: die escaped the tray at {:?}",
                    d.pos
                );
            }
            assert_eq!(
                app.history.len(),
                1,
                "a thrown roll is recorded like any other"
            );
        }
    }

    #[test]
    fn throw_power_shapes_the_launch_but_not_the_values() {
        // Same seed: the faces must be identical however hard you throw,
        // because power only feeds the trajectory, never the RNG draw order.
        let faces = |shake_secs: f32| -> (Vec<u32>, f32) {
            let mut app = seeded("", 42);
            app.input = "5d20".into();
            app.start_shake();
            tick(&mut app, shake_secs);
            let power = app.power();
            app.throw();
            let mut vals: Vec<u32> = app.dice.iter().map(|d| d.final_value).collect();
            vals.sort_unstable();
            (vals, power)
        };
        // ~0.8s is near the power peak; ~1.55s is near the trough.
        let (hard_vals, hard_power) = faces(0.8);
        let (soft_vals, soft_power) = faces(1.55);
        assert!(
            hard_power > 0.9,
            "expected a near-peak release, got {hard_power}"
        );
        assert!(
            soft_power < 0.2,
            "expected a near-trough release, got {soft_power}"
        );
        assert_eq!(
            hard_vals, soft_vals,
            "throw power leaked into the roll values"
        );
    }

    #[test]
    fn a_full_power_throw_leaves_the_cup_faster_than_a_lob() {
        // The other half of the power contract: the caught power must actually
        // shape the launch, or the whole release-timing game is a no-op. Read
        // each die's body speed straight after the throw, before any step.
        let launch_speed = |shake_secs: f32, seed: u64| -> f32 {
            let mut app = seeded("", seed);
            app.input = "3d6".into();
            app.start_shake();
            tick(&mut app, shake_secs);
            app.throw();
            app.dice
                .iter()
                .map(|d| app.physics.speed(d.body))
                .fold(0.0, f32::max)
        };
        for seed in 0..10 {
            let rocket = launch_speed(0.8, seed); // near the power peak
            let lob = launch_speed(1.55, seed); // near the trough
            assert!(
                rocket > lob * 1.5,
                "seed {seed}: rocket {rocket:.1} not clearly faster than lob {lob:.1}"
            );
        }
    }

    #[test]
    fn same_power_throws_spray_both_directions() {
        // Force and direction are separate dials: power rides the shake clock,
        // so the cup's sway phase is pinned at any given release time — but the
        // aim must still vary die to die and seed to seed. If direction ever
        // collapses back to sign-of-cup-side, every full-power throw breaks the
        // same way and this catches it.
        let (mut left, mut right) = (0, 0);
        for seed in 0..30 {
            let mut app = seeded("", seed);
            app.input = "3d6".into();
            app.start_shake();
            tick(&mut app, 0.8); // near the power peak — the same phase every time
            app.throw();
            for d in &app.dice {
                match app.physics.velocity_x(d.body) {
                    vx if vx < 0.0 => left += 1,
                    _ => right += 1,
                }
            }
        }
        assert!(
            left > 0 && right > 0,
            "full-power throws all broke one way ({left} left / {right} right)"
        );
    }

    #[test]
    fn staked_roll_reaches_a_verdict_only_once_settled() {
        for seed in 0..30 {
            let mut app = seeded("d20+2 vs 12", seed);
            assert_eq!(app.stake.map(|s| s.target), Some(12));
            assert_eq!(app.verdict(), None, "no verdict while dice are falling");
            settle(&mut app, 6000).expect("never settled");
            let (success, margin) = app.verdict().expect("settled roll must have a verdict");
            assert_eq!(margin, app.total() - 12);
            assert_eq!(success, app.total() >= 12, "seed {seed}");
        }
    }

    #[test]
    fn a_roll_under_verdict_flips_the_comparison() {
        // Same target, opposite direction: roll-under succeeds when the total
        // comes in at or below it, and the margin is how far under it landed.
        for seed in 0..30 {
            let mut app = seeded("d20 < 10", seed);
            assert_eq!(
                app.stake,
                Some(Stake {
                    target: 10,
                    goal: Goal::Under
                })
            );
            settle(&mut app, 6000).expect("never settled");
            let (success, margin) = app.verdict().expect("settled roll must have a verdict");
            assert_eq!(success, app.total() <= 10, "seed {seed}");
            assert_eq!(margin, 10 - app.total(), "seed {seed}");
        }
    }

    #[test]
    fn a_natural_20_bursts_gold_and_rings() {
        // Find a seed whose d20 lands a 20, and one that lands a 1.
        let mut found = (false, false);
        for seed in 0..300 {
            let mut app = seeded("d20", seed);
            let v = app.dice[0].final_value;
            if v != 20 && v != 1 {
                continue;
            }
            settle(&mut app, 6000).expect("never settled");
            if v == 20 {
                assert!(app.crit_dice().count() == 1);
                assert!(
                    app.particles.iter().any(|p| p.bright),
                    "seed {seed}: no gold burst for a natural 20"
                );
                // The burst is placed by projecting the die through the real
                // arena camera (same map the renderer draws it with), so it
                // erupts from the die — never NaN, never an orthographic guess.
                let (cx, cy) = app.world_to_cell(app.dice[0].pos);
                assert!(
                    cx.is_finite() && cy.is_finite(),
                    "burst position not finite"
                );
                assert!(app.sounds.contains(&SoundEvent::Crit), "no crit ring");
                found.0 = true;
            } else {
                assert!(app.fumble_dice().count() == 1);
                assert!(
                    app.particles.iter().any(|p| !p.bright),
                    "seed {seed}: no dust for a natural 1"
                );
                assert!(app.sounds.contains(&SoundEvent::Fumble), "no fumble thud");
                found.1 = true;
            }
            if found == (true, true) {
                return;
            }
        }
        panic!("seed range produced neither a 20 nor a 1: {found:?}");
    }

    #[test]
    fn any_die_type_crits_on_its_max_and_fumbles_on_one() {
        // A d6 landing a 6 earns the same gold as a d20 landing a 20…
        let mut found = (false, false);
        for seed in 0..200 {
            let mut app = seeded("d6", seed);
            let v = app.dice[0].final_value;
            if v != 6 && v != 1 {
                continue;
            }
            settle(&mut app, 6000).expect("never settled");
            if v == 6 {
                assert_eq!(
                    app.crit_dice().count(),
                    1,
                    "seed {seed}: a maxed d6 is a crit"
                );
                assert!(
                    app.particles.iter().any(|p| p.bright),
                    "no gold for a maxed d6"
                );
                assert!(app.sounds.contains(&SoundEvent::Crit));
                found.0 = true;
            } else {
                assert_eq!(
                    app.fumble_dice().count(),
                    1,
                    "seed {seed}: a d6 on 1 fumbles"
                );
                assert!(app.sounds.contains(&SoundEvent::Fumble));
                found.1 = true;
            }
            if found == (true, true) {
                return;
            }
        }
        panic!("seed range produced neither a 6 nor a 1 on a d6: {found:?}");
    }

    #[test]
    fn dropped_dice_never_crit() {
        // Disadvantage where the *dropped* die maxed: no crit, no gold.
        for seed in 0..500 {
            let mut app = seeded("2d20kl1", seed);
            let dropped_max = app.dice.iter().any(|d| !d.kept && d.final_value == 20);
            let kept_max = app.dice.iter().any(|d| d.kept && d.final_value == 20);
            if !dropped_max || kept_max {
                continue;
            }
            settle(&mut app, 6000).expect("never settled");
            assert_eq!(
                app.crit_dice().count(),
                0,
                "seed {seed}: a dropped 20 must not crit"
            );
            assert!(
                !app.sounds.contains(&SoundEvent::Crit),
                "seed {seed}: dropped 20 rang"
            );
            return;
        }
        panic!("no seed dropped a 20 under disadvantage");
    }

    #[test]
    fn many_crits_burst_per_die_but_ring_once() {
        // A pool with several maxed dice: one gold burst per die, one chord.
        for seed in 0..300 {
            let mut app = seeded("8d4", seed);
            let maxes = app.dice.iter().filter(|d| d.final_value == 4).count();
            if maxes < 2 {
                continue;
            }
            settle(&mut app, 12000).expect("never settled");
            assert_eq!(app.crit_dice().count(), maxes);
            let bursts = app.particles.iter().filter(|p| p.bright).count();
            assert!(
                bursts >= maxes * 6,
                "seed {seed}: {maxes} crits but only {bursts} spark glyphs"
            );
            let rings = app
                .sounds
                .iter()
                .filter(|s| **s == SoundEvent::Crit)
                .count();
            assert_eq!(rings, 1, "seed {seed}: the ring plays once per roll");
            return;
        }
        panic!("no seed rolled two maxed d4s");
    }

    #[test]
    fn particles_expire_rather_than_accumulate() {
        for seed in 0..300 {
            let mut app = seeded("d20", seed);
            if app.dice[0].final_value != 20 {
                continue;
            }
            settle(&mut app, 6000).expect("never settled");
            assert!(!app.particles.is_empty());
            tick(&mut app, 3.0); // long past every particle's life
            assert!(app.particles.is_empty(), "particles never died");
            return;
        }
        panic!("no natural 20 in seed range");
    }

    #[test]
    fn a_staked_settle_emits_the_matching_verdict_sound() {
        for seed in 0..30 {
            let mut app = seeded("d20 vs 10", seed);
            settle(&mut app, 6000).expect("never settled");
            let (success, _) = app.verdict().unwrap();
            let expect = if success {
                SoundEvent::Success
            } else {
                SoundEvent::Failure
            };
            let opposite = if success {
                SoundEvent::Failure
            } else {
                SoundEvent::Success
            };
            assert!(
                app.sounds.contains(&expect),
                "seed {seed}: verdict sound missing"
            );
            assert!(
                !app.sounds.contains(&opposite),
                "seed {seed}: wrong verdict sound"
            );
        }
    }

    #[test]
    fn physics_makes_noise_and_the_queue_self_caps() {
        // A big undrained roll: impacts/knocks/settles pile up but never
        // exceed the cap (headless users don't drain).
        let mut app = seeded("20d6", 4);
        settle(&mut app, 20000).expect("never settled");
        assert!(!app.sounds.is_empty(), "a full roll should make some noise");
        assert!(app.sounds.len() <= MAX_SOUNDS, "queue exceeded its cap");
        assert!(
            app.sounds
                .iter()
                .any(|s| matches!(s, SoundEvent::Settle { .. })),
            "no settle knock recorded"
        );
    }

    #[test]
    fn a_fresh_shake_does_not_inherit_a_stale_rattle() {
        // Regression: the sway half-cycle used to be remembered across shakes,
        // so every pickup after the first opened with a phantom rattle tick.
        let mut app = seeded("", 6);
        app.input = "3d6".into();
        app.start_shake();
        tick(&mut app, 1.0);
        app.cancel_shake();
        app.sounds.clear();

        app.start_shake();
        app.update(1.0 / 60.0);
        assert!(
            !app.sounds
                .iter()
                .any(|s| matches!(s, SoundEvent::Rattle { .. })),
            "a fresh shake must not open with a leftover rattle tick"
        );
    }

    #[test]
    fn muting_empties_the_sound_queue_at_the_source() {
        let mut app = seeded("3d6", 1);
        settle(&mut app, 6000).expect("never settled");
        assert!(!app.sounds.is_empty(), "a roll should queue some noise");

        // Muted: the queue is cleared but nothing is handed over.
        app.muted = true;
        assert!(
            app.take_sounds().is_empty(),
            "muted take_sounds must return nothing"
        );
        assert!(
            app.sounds.is_empty(),
            "muted take_sounds must still clear the queue"
        );

        // Unmuted: events flow again.
        app.muted = false;
        app.input = "3d6".into();
        app.roll();
        settle(&mut app, 6000).expect("never settled");
        assert!(
            !app.take_sounds().is_empty(),
            "unmuted take_sounds must hand events over"
        );
    }

    #[test]
    fn the_throw_rolls_the_expression_locked_at_pickup() {
        // The shake snapshots the input: whatever mutates it mid-shake, the
        // throw rolls what was validated when the cup was lifted.
        let mut app = seeded("", 8);
        app.input = "2d6".into();
        app.start_shake();
        app.input = "9d9nonsense".into(); // bypasses the key router on purpose
        app.throw();
        assert!(app.error.is_none(), "the locked expression was valid");
        assert_eq!(
            app.dice.len(),
            2,
            "the throw must roll the snapshot, not the mutation"
        );
        assert_eq!(
            app.input, "2d6",
            "the input reverts to what was actually thrown"
        );
    }

    #[test]
    fn shaking_rattles_and_release_echo_fades() {
        let mut app = seeded("", 5);
        app.input = "3d6".into();
        app.start_shake();
        tick(&mut app, 0.8);
        assert!(
            app.sounds
                .iter()
                .any(|s| matches!(s, SoundEvent::Rattle { .. })),
            "a shaken cup must rattle"
        );
        app.throw();
        assert!(app.sounds.contains(&SoundEvent::Throw {
            power: app.last_throw.unwrap().power
        }));

        // The echo (and, for a hard catch, the tremor) is up right after…
        assert!(
            app.release_echo().is_some(),
            "no release echo after a throw"
        );
        assert!(
            app.tremor() > 0.0,
            "a near-peak throw should shake the arena"
        );
        // …and both die down.
        tick(&mut app, 2.0);
        assert_eq!(app.release_echo(), None, "the echo must fade");
        assert_eq!(app.tremor(), 0.0, "the tremor must stop");
    }

    #[test]
    fn advantage_keeps_the_higher_die() {
        // Try a spread of seeds; for every one, the kept d20 is the larger.
        for seed in 0..50 {
            let app = seeded("2d20kh1", seed);
            assert_eq!(app.dice.len(), 2, "both dice are still thrown");
            let kept: Vec<&Die> = app.dice.iter().filter(|d| d.kept).collect();
            assert_eq!(kept.len(), 1, "advantage keeps exactly one");
            let dropped = app.dice.iter().find(|d| !d.kept).unwrap();
            assert!(
                kept[0].final_value >= dropped.final_value,
                "kept {} < dropped {}",
                kept[0].final_value,
                dropped.final_value
            );
            // The total is just the kept die.
            assert_eq!(app.total(), kept[0].final_value as i32);
        }
    }

    #[test]
    fn disadvantage_keeps_the_lower_die() {
        for seed in 0..50 {
            let app = seeded("2d20kl1", seed);
            let kept = app.dice.iter().find(|d| d.kept).unwrap();
            let dropped = app.dice.iter().find(|d| !d.kept).unwrap();
            assert!(kept.final_value <= dropped.final_value);
        }
    }

    #[test]
    fn drop_lowest_sums_the_top_three() {
        for seed in 0..50 {
            let app = seeded("4d6dl1", seed);
            assert_eq!(app.dice.len(), 4);
            let mut vals: Vec<u32> = app.dice.iter().map(|d| d.final_value).collect();
            vals.sort_unstable();
            let expected: i32 = vals[1..].iter().map(|&v| v as i32).sum(); // top 3
            assert_eq!(app.total(), expected);
            // Exactly the single lowest die is dropped.
            assert_eq!(app.dice.iter().filter(|d| !d.kept).count(), 1);
        }
    }

    #[test]
    fn exploding_happens_during_the_animation_not_up_front() {
        // roll() builds only the base pool — no dice are pre-spawned...
        for seed in 0..30 {
            let app = seeded("6d6!", seed);
            assert_eq!(app.dice.len(), 6, "seed {seed}: roll() pre-spawned dice");
        }
        // ...and the pool only grows once the simulation runs and a max settles.
        // Find a seed whose base roll contains a six, then prove it grows.
        let mut grew = false;
        for seed in 0..200 {
            let mut app = seeded("6d6!", seed);
            let base_max = app.dice.iter().filter(|d| d.final_value == 6).count();
            settle(&mut app, 20000).expect("exploding pool never settled");
            if base_max > 0 {
                assert!(
                    app.dice.len() > 6,
                    "seed {seed}: base roll had a six but pool never grew"
                );
                grew = true;
                break;
            }
        }
        assert!(grew, "no seed in range rolled a six to explode");
    }

    #[test]
    fn every_settled_max_die_spawned_exactly_one_more() {
        // The defining invariant of the settle-time mechanic: once everything is
        // at rest, the number of dice that rolled a matching face equals the base
        // pool plus the number of explosions (each max spawns exactly one), up to
        // the per-term cap.
        for seed in 0..30 {
            let mut app = seeded("6d6!", seed);
            settle(&mut app, 20000).expect("never settled");
            let sixes = app.dice.iter().filter(|d| d.final_value == 6).count();
            let spawned = app.dice.len() - 6;
            // Every six spawned a die unless the term hit the cap.
            assert_eq!(
                spawned,
                sixes.min(MAX_EXPLOSIONS),
                "seed {seed}: {sixes} sixes but {spawned} spawned"
            );
            // Once settled, every die has had (and used up) its one explosion chance.
            assert!(
                app.dice.iter().all(|d| d.exploded),
                "a settled die never got its explosion check"
            );
        }
    }

    #[test]
    fn frequent_explosions_hit_the_cap_and_still_converge() {
        // d3 exploding on >1 fires roughly two times in three, so a chain races
        // to the per-term cap. It must still terminate and stay in the arena.
        for seed in 0..20 {
            let mut app = seeded("4d3!>1", seed);
            settle(&mut app, 40000).expect("capped explosion chain never settled");
            assert!(app.dice.len() <= 4 + MAX_EXPLOSIONS, "blew past the cap");
            for d in &app.dice {
                assert!(in_box(d.pos), "die escaped the tray at {:?}", d.pos);
            }
        }
    }

    #[test]
    fn exploding_stays_capped_and_total_is_the_full_sum() {
        for seed in 0..30 {
            let mut app = seeded("6d6!", seed);
            settle(&mut app, 20000).expect("never settled");
            assert!(
                app.dice.len() <= 6 + MAX_EXPLOSIONS,
                "explosion count is capped"
            );
            // `!` keeps everything, so the total is just the sum of every die.
            let expected: i32 = app.dice.iter().map(|d| d.final_value as i32).sum();
            assert_eq!(app.total(), expected);
        }
    }

    #[test]
    fn multiply_scales_only_its_term() {
        for seed in 0..50 {
            let app = seeded("3d6*2+d8", seed);
            // Term 0 is the three d6 (×2); term 1 is the lone d8 (×1).
            let t0: i32 = app
                .dice
                .iter()
                .filter(|d| d.term_idx == 0)
                .map(|d| d.final_value as i32)
                .sum();
            let t1: i32 = app
                .dice
                .iter()
                .filter(|d| d.term_idx == 1)
                .map(|d| d.final_value as i32)
                .sum();
            assert_eq!(app.total(), t0 * 2 + t1);
        }
    }

    #[test]
    fn exploding_pool_still_converges_and_stays_in_arena() {
        let mut app = seeded("6d6!", 3);
        let mut settled = false;
        for _ in 0..20000 {
            app.update(1.0 / 60.0);
            for d in &app.dice {
                assert!(in_box(d.pos), "die escaped the tray at {:?}", d.pos);
            }
            if app.all_settled() {
                settled = true;
                break;
            }
        }
        assert!(settled, "exploding pool never settled");
    }

    #[test]
    fn dice_settle_and_total_is_consistent() {
        let mut app = App::new("4d6+2".to_string());
        app.arena_w = 60.0;
        app.arena_h = 20.0;

        let frame = settle(&mut app, 6000).expect("dice never came to rest");
        // A few seconds of sim time at most.
        assert!(frame < 6000);

        // Once settled, every die's shown face is the burned-in rolled value...
        for d in &app.dice {
            assert_eq!(d.shown, d.final_value);
            assert!((1..=d.sides).contains(&d.final_value));
        }
        // ...and the total is in range.
        let t = app.total();
        assert!((4 + 2..=24 + 2).contains(&t), "total {t} out of range");
    }

    #[test]
    fn dice_stay_inside_the_arena() {
        let mut app = App::new("6d8".to_string());
        for _ in 0..20000 {
            app.update(1.0 / 60.0);
            for d in &app.dice {
                assert!(in_box(d.pos), "die escaped the tray at {:?}", d.pos);
            }
            if app.all_settled() {
                break;
            }
        }
        assert!(app.all_settled());
    }

    #[test]
    #[ignore]
    fn debug_converge() {
        let mut app = App::new("6d8".to_string());
        for f in 0..20000 {
            app.update(1.0 / 60.0);
            if f % 500 == 0 || app.all_settled() {
                let settled = app.dice.iter().filter(|d| d.settled).count();
                eprintln!("f={f:5} settled={settled}/{}", app.dice.len());
                if app.all_settled() {
                    break;
                }
            }
        }
        for (i, d) in app.dice.iter().enumerate() {
            eprintln!("  die {i}: pos={:?} settled={}", d.pos, d.settled);
        }
    }

    #[test]
    fn settled_dice_do_not_overlap() {
        // A crowded pool must pile up without two dice occupying the same spot —
        // Rapier's contacts enforce it, and the centres stay meaningfully apart.
        let mut app = App::new("8d6".to_string());
        settle(&mut app, 20000).expect("crowded pool never settled");

        // Non-overlap bound: each die contains a ball of its inradius, so two
        // non-overlapping dice keep their centres at least two d6-inradii apart
        // (2·DIE_R/√3 ≈ 0.416; the tightest rest is two cubes face-to-face). A
        // hair under that allows Rapier's contact slop without hiding a real
        // interpenetration, which would land well below it.
        let min_gap = 2.0 * physics::DIE_R / 3.0_f32.sqrt() * 0.96;
        for i in 0..app.dice.len() {
            for j in (i + 1)..app.dice.len() {
                let gap = (app.dice[i].pos - app.dice[j].pos).length();
                assert!(
                    gap > min_gap,
                    "dice {i} and {j} overlap: centres {gap:.2} apart (< {min_gap:.2})"
                );
            }
        }
    }

    #[test]
    fn settled_dice_stay_in_bounds_when_cramped() {
        // A big pool crowds the tray and stacks up, but must still converge with
        // no die ending up outside the box. 60d6 is the parser's cap — the worst
        // case the spawn lattice must place without interpenetration (co-spawned
        // overlapping dice get hurled apart by Rapier's penetration recovery,
        // hard enough to eject one through a wall).
        for spec in ["12d6", "16d6", "20d6", "60d6"] {
            for _ in 0..15 {
                let mut app = App::new(spec.to_string());
                settle(&mut app, 20000).expect("cramped pool never settled");
                for (i, d) in app.dice.iter().enumerate() {
                    assert!(
                        in_box(d.pos),
                        "{spec} die {i} settled out of bounds at {:?}",
                        d.pos
                    );
                }
            }
        }
    }

    #[test]
    fn a_plain_roll_comes_to_rest() {
        let mut app = App::new("3d6".to_string());
        assert!(
            settle(&mut app, 20000).is_some(),
            "a plain roll must come to rest"
        );
    }

    #[test]
    fn a_completed_roll_is_recorded_in_history_exactly_once() {
        let mut app = seeded("3d6+1", 5);
        assert!(app.history.is_empty(), "nothing recorded before it settles");
        settle(&mut app, 6000).expect("never settled");

        // Keep stepping after settling — the entry must not be duplicated.
        for _ in 0..200 {
            app.update(1.0 / 60.0);
        }
        assert_eq!(app.history.len(), 1, "recorded once and only once");

        let e = &app.history[0];
        assert_eq!(e.expr, "3d6+1");
        assert_eq!(e.values.len(), 3); // three kept dice
                                       // total = sum of faces + 1, and matches what the entry stored.
        let face_sum: i32 = e.values.iter().map(|&v| v as i32).sum();
        assert_eq!(e.total, face_sum + 1);
        assert_eq!(e.total, app.total());
    }

    #[test]
    fn history_records_only_kept_dice() {
        // Advantage: two thrown, one kept — history stores the single kept value.
        let mut app = seeded("2d20kh1", 9);
        settle(&mut app, 6000).expect("never settled");
        assert_eq!(app.history.len(), 1);
        assert_eq!(
            app.history[0].values.len(),
            1,
            "only the kept die is stored"
        );
        assert_eq!(app.history[0].total, app.history[0].values[0] as i32);
    }

    #[test]
    fn history_is_capped() {
        let mut app = seeded("d6", 1);
        // Forge more than the cap of entries, then roll one more for real.
        for n in 0..(MAX_HISTORY + 50) {
            app.history.push(HistoryEntry {
                expr: "d6".into(),
                values: vec![1],
                total: n as i32,
            });
        }
        app.input = "d6".into();
        app.roll();
        settle(&mut app, 6000).expect("never settled");
        assert!(app.history.len() <= MAX_HISTORY, "history exceeded its cap");
    }

    #[test]
    fn sampled_stats_match_known_dice_ranges() {
        // 3d6: exactly 3..=18, average 10.5. Sampling should pin the bounds and
        // land close on the mean.
        let mut app = seeded("3d6", 1);
        let s = app.stats().expect("3d6 parses");
        assert_eq!(s.min, 3);
        assert_eq!(s.max, 18);
        assert!((s.mean - 10.5).abs() < 0.3, "mean {} far from 10.5", s.mean);
        // The distribution fractions sum to ~1.
        let total: f64 = s.dist.iter().map(|b| b.fraction).sum();
        assert!((total - 1.0).abs() < 1e-6, "fractions sum to {total}");
    }

    #[test]
    fn sampled_stats_reflect_modifiers() {
        // Advantage shifts the average of a single d20 up from 10.5 toward ~13.8.
        let adv = App::new("2d20kh1".to_string()).stats().unwrap();
        assert_eq!(adv.min, 1);
        assert_eq!(adv.max, 20);
        assert!(
            adv.mean > 12.0,
            "advantage mean {} should beat a flat d20",
            adv.mean
        );

        // A flat *2 multiplier doubles the achievable range.
        let doubled = App::new("1d6*2".to_string()).stats().unwrap();
        assert_eq!(doubled.min, 2);
        assert_eq!(doubled.max, 12);
    }

    #[test]
    fn session_stats_summarize_matching_rolls() {
        let mut app = seeded("3d6", 2);
        settle(&mut app, 6000).expect("settle 1");
        // Roll the same expression a second time.
        app.input = "3d6".into();
        app.roll();
        settle(&mut app, 6000).expect("settle 2");

        let s = app.stats().unwrap();
        assert_eq!(s.session.count, 2, "both 3d6 rolls counted");
        assert_eq!(s.total_rolls, 2);
        assert!(s.session.min <= s.session.max);
        // Mean of the session sits within the achievable 3..=18 range.
        assert!(s.session.mean >= 3.0 && s.session.mean <= 18.0);
    }

    #[test]
    fn stats_error_surfaces_for_bad_input() {
        let mut app = App::new(String::new());
        app.input = "garbage".into();
        assert!(app.stats().is_err());
    }
}
