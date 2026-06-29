//! Application state and the little physics engine that bounces the dice.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::parse::{self, DiceTerm, Roll, TermMod};

/// Width / height of a die "box" in terminal cells: ┌───┐ / │ N │ / └───┘
pub const DIE_W: f32 = 6.0;
pub const DIE_H: f32 = 4.0;

// Physics tuning (units are terminal cells and seconds).
const GRAVITY: f32 = 60.0;
const WALL_RESTITUTION: f32 = 0.72;
const FLOOR_RESTITUTION: f32 = 0.5;
const FLOOR_FRICTION: f32 = 0.7;
const AIR_DRAG: f32 = 0.4;
const TUMBLE_INTERVAL: f32 = 0.05; // how often a mid-air die shows a new face
const SETTLE_SPEED_SQ: f32 = 6.0; // below this (and supported) a die comes to rest
const MIN_BOUNCE_VY: f32 = 3.5; // smaller floor bounces are killed so dice actually stop
const DIE_RESTITUTION: f32 = 0.2; // bounciness when two dice strike each other (dice aren't bouncy)
const CONTACT_EPS: f32 = 0.7; // vertical gap within which a die counts as resting on another
const COLLISION_ITERS: usize = 8; // separation passes per frame (stacks need a few)
const STUCK_TIMEOUT: f32 = 0.4; // a die stopped this long with no proper rest settles in place
const TOPPLE_FACTOR: f32 = 1.4; // how strongly an off-centre landing rolls a die off the one below
const TOPPLE_MAX: f32 = 20.0; // cap on a single topple kick so a hard slam can't fling a die away
const MIN_SLIDE_VX: f32 = 2.5; // sideways speed below this is killed on contact so piles settle
const MAX_AIRBORNE: f32 = 8.0; // hard cap: a die tumbling this long settles in place no matter what
const MAX_EXPLOSIONS: usize = 40; // cap on dice an exploding term can spawn, so the pool can't run away
const MAX_HISTORY: usize = 200; // most recent rolls kept in memory for the history pane
const STAT_SAMPLES: usize = 20_000; // Monte-Carlo trials for the statistics pane's odds

/// One die in flight.
pub struct Die {
    pub sides: u32,
    pub final_value: u32,
    pub shown: u32,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub settled: bool,
    pub color_idx: usize,
    /// Which dice term this die belongs to. Dice in the same term share a `mult`
    /// and are kept/dropped together; the total is summed per term.
    pub term_idx: usize,
    /// Per-term multiplier applied to this term's kept sum (1 when no `*N`).
    pub mult: i32,
    /// `false` for a die that was thrown and animated but dropped by keep/drop —
    /// it still bounces and settles, but is rendered dimmed and left out of the total.
    pub kept: bool,
    /// The explode condition inherited from this die's term, if it explodes.
    /// When such a die settles on a matching face it spawns one more die — so
    /// explosions unfold *during* the animation rather than all up front.
    explode: Option<parse::Compare>,
    /// Set once a die has had its chance to explode, so it can't spawn twice.
    exploded: bool,
    tumble_acc: f32,
    still_for: f32, // seconds spent slow-but-unsettled; triggers the stuck fallback
    age: f32,       // seconds since thrown; a hard cap so nothing tumbles forever
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

pub struct App {
    pub input: String,
    pub dice: Vec<Die>,
    pub modifier: i32,
    pub error: Option<String>,
    /// Completed rolls, newest last. Capped so a long session can't grow without
    /// bound; the history pane shows the most recent slice.
    pub history: Vec<HistoryEntry>,
    /// Which pop-out pane is showing (help / history / stats / none).
    pub pane: Pane,
    /// Set the first frame after every die settles, so the roll is recorded into
    /// `history` exactly once rather than every frame thereafter.
    recorded: bool,
    pub arena_w: f32,
    pub arena_h: f32,
    pub spawned: bool,
    needs_spawn: bool,
    /// Count of dice an exploding term has spawned so far this roll, indexed by
    /// term. Explosions happen over the course of the animation, so the cap has
    /// to be enforced across frames rather than in one up-front loop.
    explosions: Vec<usize>,
    rng: StdRng,
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
            dice: Vec::new(),
            modifier: 0,
            error: None,
            history: Vec::new(),
            pane: Pane::None,
            recorded: false,
            arena_w: 0.0,
            arena_h: 0.0,
            spawned: false,
            needs_spawn: false,
            explosions: Vec::new(),
            rng,
        };
        if !initial.trim().is_empty() {
            app.input = initial.trim().to_string();
            app.roll();
        }
        app
    }

    /// Parse the current input and, on success, build a fresh pool of dice.
    /// Actual spawn positions are assigned later, once the arena size is known.
    pub fn roll(&mut self) {
        match parse::parse(&self.input) {
            Ok(Roll { terms, modifier }) => {
                self.error = None;
                self.modifier = modifier;
                self.explosions = vec![0; terms.len()];
                let mut dice: Vec<Die> = Vec::new();
                for (ti, term) in terms.iter().enumerate() {
                    self.roll_term(ti, term, &mut dice);
                }
                self.dice = dice;
                self.spawned = false;
                self.needs_spawn = true;
                self.recorded = false;
            }
            Err(e) => {
                self.error = Some(e);
            }
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
        Die {
            sides,
            final_value: self.rng.gen_range(1..=sides),
            shown: self.rng.gen_range(1..=sides),
            x: 0.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            settled: false,
            color_idx,
            term_idx,
            mult: 1,
            kept: true,
            explode,
            exploded: false,
            tumble_acc: 0.0,
            still_for: 0.0,
            age: 0.0,
        }
    }

    fn max_xy(&self) -> (f32, f32) {
        (
            (self.arena_w - DIE_W).max(0.0),
            (self.arena_h - DIE_H).max(0.0),
        )
    }

    /// Toss every die from a random spot near the top with a random velocity.
    fn spawn(&mut self) {
        let (maxx, maxy) = self.max_xy();
        // Borrow the rng separately from the dice to satisfy the borrow checker.
        let rng = &mut self.rng;
        for die in &mut self.dice {
            die.x = rng.gen_range(0.0..=maxx.max(0.01));
            die.y = rng.gen_range(0.0..=(maxy * 0.35).max(0.01));
            die.vx = rng.gen_range(-42.0..=42.0);
            die.vy = rng.gen_range(-22.0..=18.0);
            die.settled = false;
            die.tumble_acc = 0.0;
            die.still_for = 0.0;
            die.age = 0.0;
        }
        self.spawned = true;
        self.needs_spawn = false;
    }

    /// Advance the simulation by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        if self.needs_spawn && self.arena_w > 0.0 {
            self.spawn();
        }
        if !self.spawned {
            return;
        }

        let dt = dt.min(0.05); // clamp so a stalled frame doesn't teleport dice
        let (maxx, maxy) = self.max_xy();
        let drag = (1.0 - AIR_DRAG * dt).max(0.0);

        for die in &mut self.dice {
            if die.settled {
                continue;
            }

            // Degenerate arena (too small for a die): just settle in place.
            if maxy < 0.5 {
                die.settled = true;
                die.shown = die.final_value;
                die.x = die.x.clamp(0.0, maxx);
                die.y = die.y.clamp(0.0, maxy);
                continue;
            }

            // Integrate.
            die.vy += GRAVITY * dt;
            die.vx *= drag;
            die.x += die.vx * dt;
            die.y += die.vy * dt;

            // Side walls.
            if die.x < 0.0 {
                die.x = 0.0;
                die.vx = -die.vx * WALL_RESTITUTION;
            } else if die.x > maxx {
                die.x = maxx;
                die.vx = -die.vx * WALL_RESTITUTION;
            }

            // Ceiling.
            if die.y < 0.0 {
                die.y = 0.0;
                die.vy = -die.vy * WALL_RESTITUTION;
            }

            // Floor: bounce, rub off horizontal speed, and kill tiny hops.
            if die.y > maxy {
                die.y = maxy;
                die.vy = -die.vy * FLOOR_RESTITUTION;
                die.vx *= FLOOR_FRICTION;
                if die.vy.abs() < MIN_BOUNCE_VY {
                    die.vy = 0.0;
                }
            }

            // Tumble the visible face while airborne.
            die.tumble_acc += dt;
            if die.tumble_acc >= TUMBLE_INTERVAL {
                die.tumble_acc = 0.0;
                die.shown = self.rng.gen_range(1..=die.sides);
            }
        }

        // Keep dice from overlapping, then let any that are slow and supported
        // (by the floor or by an already-settled die) come to rest. Order
        // matters: settling reads the post-separation resting positions.
        self.resolve_collisions(maxx, maxy);
        self.settle_supported(dt, maxx, maxy);

        // The frame the roll finishes, record it once for the history pane.
        if !self.recorded && self.all_settled() {
            self.record_roll();
            self.recorded = true;
        }
    }

    /// Append the just-settled roll to the history (newest last), capped.
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
    }

    /// Push apart any overlapping pairs of dice (axis-aligned boxes). Settled
    /// dice act as immovable obstacles so piles build from the bottom up.
    fn resolve_collisions(&mut self, maxx: f32, maxy: f32) {
        let n = self.dice.len();
        for _ in 0..COLLISION_ITERS {
            for i in 0..n {
                for j in (i + 1)..n {
                    let (left, right) = self.dice.split_at_mut(j);
                    resolve_pair(&mut left[i], &mut right[0]);
                }
            }
        }
        // Separation can shove a die through a wall; clamp everything back in.
        for die in &mut self.dice {
            if !die.settled {
                die.x = die.x.clamp(0.0, maxx);
                die.y = die.y.clamp(0.0, maxy);
            }
        }
    }

    /// Settle each slow die that rests on the floor or on a settled die below it.
    /// A settling die is snapped flush onto whatever supports it; if that resting
    /// spot would still overlap a die that came to rest this same frame, settling
    /// is deferred so the collision pass can pry them apart first. This is what
    /// stops two co-settling dice from freezing on top of each other.
    ///
    /// As a last resort, a die that has sat still for [`STUCK_TIMEOUT`] without
    /// finding a clean rest (wedged in an over-tall stack the arena can't hold)
    /// settles wherever it is — accepting a little overlap beats bouncing forever.
    fn settle_supported(&mut self, dt: f32, maxx: f32, maxy: f32) {
        // Boxes already at rest. Grows as we accept more dice this frame, so each
        // candidate is checked against the ones settled just before it.
        let mut resting: Vec<(f32, f32)> = self
            .dice
            .iter()
            .filter(|d| d.settled)
            .map(|d| (d.x, d.y))
            .collect();

        // Explosions discovered this frame: a die that settles on a matching face
        // spawns one more. Collected here and tossed in after the loop so we don't
        // grow `self.dice` (and shift `resting`) mid-iteration.
        let mut to_explode: Vec<(u32, usize, i32, parse::Compare)> = Vec::new();

        for i in 0..self.dice.len() {
            if self.dice[i].settled {
                continue;
            }
            self.dice[i].age += dt;
            // Hard cap: anything still tumbling after MAX_AIRBORNE settles in
            // place, even if it's fast — a backstop so a chaotically over-packed
            // pile can't agitate itself forever. Real rolls settle long before.
            let overdue = self.dice[i].age >= MAX_AIRBORNE;

            let slow = {
                let d = &self.dice[i];
                d.vx * d.vx + d.vy * d.vy < SETTLE_SPEED_SQ
            };
            if slow {
                self.dice[i].still_for += dt;
            } else {
                self.dice[i].still_for = 0.0;
                if !overdue {
                    continue; // moving and not overdue: keep simulating
                }
            }
            let stuck = overdue || self.dice[i].still_for >= STUCK_TIMEOUT;
            let die = &self.dice[i];

            // Where would this die come to rest? On the floor, or flush on the
            // highest settled die directly beneath it (it may straddle two).
            let on_floor = die.y >= maxy - 0.5;
            let bottom = die.y + DIE_H;
            let support_top = resting
                .iter()
                .filter(|&&(rx, ry)| {
                    let x_overlap = die.x < rx + DIE_W && die.x + DIE_W > rx;
                    x_overlap && ry > die.y && (bottom - ry).abs() < CONTACT_EPS
                })
                .map(|&(_, ry)| ry)
                .fold(f32::INFINITY, f32::min);

            let (rest_x, rest_y) = if on_floor {
                (die.x, maxy)
            } else if support_top.is_finite() {
                (die.x, support_top - DIE_H)
            } else if stuck {
                (die.x, die.y) // wedged with nowhere to go: rest in place
            } else {
                continue; // unsupported: keep falling
            };
            // A stack taller than the arena would push the top die past the
            // ceiling; keep every resting spot inside the bounds.
            let rest_x = rest_x.clamp(0.0, maxx);
            let rest_y = rest_y.clamp(0.0, maxy);

            // Defer if this resting spot still clashes with one already taken —
            // unless we're stuck, in which case a clash is unavoidable.
            let clashes = !stuck
                && resting.iter().any(|&(rx, ry)| {
                    let (px, py) = penetration(rest_x, rest_y, rx, ry);
                    px.min(py) > 0.5
                });
            if clashes {
                continue;
            }

            let die = &mut self.dice[i];
            die.settled = true;
            die.shown = die.final_value;
            die.x = rest_x;
            die.y = rest_y;
            die.vx = 0.0;
            die.vy = 0.0;
            resting.push((rest_x, rest_y));

            // Exploding: a die that comes to rest on a face meeting its condition
            // spawns one more die — but only once, and only while its term is
            // under the cap. The new die is built and tossed after the loop.
            if !die.exploded {
                die.exploded = true;
                if let Some(cmp) = die.explode {
                    let term = die.term_idx;
                    if cmp.matches(die.final_value) && self.explosions[term] < MAX_EXPLOSIONS {
                        self.explosions[term] += 1;
                        to_explode.push((die.sides, term, die.mult, cmp));
                    }
                }
            }
        }

        // Toss any dice the just-settled dice exploded into. They drop from the
        // top with a random velocity, exactly like the opening throw, and carry
        // their term's multiplier and explode condition so chains keep going.
        for (sides, term_idx, mult, cmp) in to_explode {
            let color = self.dice.len();
            let mut die = self.new_die(sides, term_idx, color, Some(cmp));
            die.mult = mult;
            self.launch(&mut die, maxx, maxy);
            self.dice.push(die);
        }
    }

    /// Toss a single die from a random spot near the top, like the opening
    /// throw. Used for explosion spawns that appear mid-animation.
    fn launch(&mut self, die: &mut Die, maxx: f32, maxy: f32) {
        die.x = self.rng.gen_range(0.0..=maxx.max(0.01));
        die.y = self.rng.gen_range(0.0..=(maxy * 0.35).max(0.01));
        die.vx = self.rng.gen_range(-42.0..=42.0);
        die.vy = self.rng.gen_range(-22.0..=18.0);
        die.settled = false;
    }

    pub fn all_settled(&self) -> bool {
        self.spawned && !self.dice.is_empty() && self.dice.iter().all(|d| d.settled)
    }

    /// Final total (only meaningful once settled, but always well-defined).
    pub fn total(&self) -> i32 {
        self.summed(|d| d.final_value as i32)
    }

    /// Running total of whatever faces are currently showing.
    pub fn live_total(&self) -> i32 {
        self.summed(|d| d.shown as i32)
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
    /// handled) plus a summary of the session's history. Returns `None` (with the
    /// parse error) if the expression doesn't parse.
    pub fn stats(&self) -> Result<Stats, String> {
        let roll = parse::parse(self.input.trim())?;

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

        // A coarse histogram over the observed range for the little curve.
        let dist = histogram(&totals, lo, hi);

        // Session history for the current expression (and overall).
        let expr = self.input.trim().to_string();
        let here: Vec<&HistoryEntry> = self.history.iter().filter(|e| e.expr == expr).collect();
        let session = SessionStats::from(&here);

        Ok(Stats {
            expr,
            samples: STAT_SAMPLES,
            min: lo,
            max: hi,
            mean,
            dist,
            session,
            total_rolls: self.history.len(),
        })
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

        let term_sum: i32 = pool.iter().filter(|&&(_, k)| k).map(|&(v, _)| v as i32).sum();
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

    Outcome {
        expression: expression.to_string(),
        terms,
        modifier: roll.modifier,
        total,
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

/// Per-axis overlap depth of two die-sized boxes whose top-left corners are at
/// the given coordinates. A component is positive only when the boxes intrude on
/// that axis; both positive means a real overlap, and `min` of the two is the
/// penetration that matters for separation.
fn penetration(ax: f32, ay: f32, bx: f32, by: f32) -> (f32, f32) {
    (
        (DIE_W - (ax - bx).abs()).max(0.0),
        (DIE_H - (ay - by).abs()).max(0.0),
    )
}

/// Separate two overlapping dice and damp their velocities along the contact
/// normal. Dice are equal-size axis-aligned boxes, so this is plain AABB:
/// resolve along the axis of least penetration. A settled die is immovable, so
/// moving dice get pushed off it and piles build from the bottom up.
fn resolve_pair(a: &mut Die, b: &mut Die) {
    if a.settled && b.settled {
        return; // two resting dice: never nudge them
    }

    // Equal extents, so the centre offset equals the top-left offset.
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let (px, py) = penetration(a.x, a.y, b.x, b.y);
    if px <= 0.0 || py <= 0.0 {
        return; // not touching
    }

    // A settled die yields nothing; otherwise split the correction evenly.
    let (a_share, b_share) = if a.settled {
        (0.0, 1.0)
    } else if b.settled {
        (1.0, 0.0)
    } else {
        (0.5, 0.5)
    };

    if px < py {
        // Horizontal contact: n is the unit normal from b toward a.
        let n = if dx >= 0.0 { 1.0 } else { -1.0 };
        a.x += n * px * a_share;
        b.x -= n * px * b_share;
        resolve_velocity(&mut a.vx, &mut b.vx, n, a.settled, b.settled);
    } else {
        // Vertical contact: bounce and kill tiny hops so stacks can come to rest.
        let n = if dy >= 0.0 { 1.0 } else { -1.0 };
        a.y += n * py * a_share;
        b.y -= n * py * b_share;

        // How hard they meet, captured before the bounce damps it.
        let closing = -(a.vy - b.vy) * n;
        resolve_velocity(&mut a.vy, &mut b.vy, n, a.settled, b.settled);

        // Topple: real dice don't balance on each other. The upper die (smaller
        // y) converts part of the impact into sideways motion, proportional to
        // how far its centre overhangs the die below, so it rolls off the edge.
        // (overhang/DIE_W is the signed overhang fraction.) The kick is capped so
        // a hard, far-overhanging slam can't fling a die across the arena.
        let (upper, lower) = if dy < 0.0 { (&mut *a, &*b) } else { (&mut *b, &*a) };
        if closing > 0.0 && !upper.settled {
            let kick = (upper.x - lower.x) / DIE_W * closing * TOPPLE_FACTOR;
            upper.vx += kick.clamp(-TOPPLE_MAX, TOPPLE_MAX);
        }

        // Quiet a die that's effectively at rest on another: kill tiny vertical
        // hops and tiny sideways drift. A real topple kick is far larger than
        // MIN_SLIDE_VX so it still rolls; this only stops the micro-jitter that
        // would otherwise keep a crowded pile from ever settling.
        for d in [&mut *a, &mut *b] {
            if d.settled {
                continue;
            }
            if d.vy.abs() < MIN_BOUNCE_VY {
                d.vy = 0.0;
            }
            if d.vx.abs() < MIN_SLIDE_VX {
                d.vx = 0.0;
            }
        }
    }
}

/// One-dimensional collision response along a contact normal `n` (±1). For two
/// equal-mass movable dice this reverses their relative velocity (scaled by
/// restitution); against an immovable settled die the mover simply reflects.
/// Only fires when the pair is actually approaching, so resting contacts don't
/// gain energy.
fn resolve_velocity(va: &mut f32, vb: &mut f32, n: f32, a_static: bool, b_static: bool) {
    let e = DIE_RESTITUTION;
    match (a_static, b_static) {
        (false, false) => {
            if (*va - *vb) * n < 0.0 {
                let (a0, b0) = (*va, *vb);
                *va = 0.5 * ((1.0 - e) * a0 + (1.0 + e) * b0);
                *vb = 0.5 * ((1.0 + e) * a0 + (1.0 - e) * b0);
            }
        }
        (true, false) => {
            if *vb * n > 0.0 {
                *vb = -e * *vb;
            }
        }
        (false, true) => {
            if *va * n < 0.0 {
                *va = -e * *va;
            }
        }
        (true, true) => {}
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

    /// A deterministic App for testing roll semantics: same input, same RNG,
    /// same dice every time. The arena is sized so dice can settle.
    fn seeded(input: &str, seed: u64) -> App {
        let mut app = App {
            input: input.to_string(),
            dice: Vec::new(),
            modifier: 0,
            error: None,
            arena_w: 60.0,
            arena_h: 20.0,
            spawned: false,
            needs_spawn: false,
            explosions: Vec::new(),
            history: Vec::new(),
            pane: Pane::None,
            recorded: false,
            rng: StdRng::seed_from_u64(seed),
        };
        app.roll();
        app
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
            assert!(app.dice.iter().all(|d| d.exploded), "a settled die never got its explosion check");
        }
    }

    #[test]
    fn frequent_explosions_hit_the_cap_and_still_converge() {
        // d3 exploding on >1 fires roughly two times in three, so a chain races
        // to the per-term cap. It must still terminate and stay in the arena.
        for seed in 0..20 {
            let mut app = seeded("4d3!>1", seed);
            let (maxx, maxy) = app.max_xy();
            settle(&mut app, 40000).expect("capped explosion chain never settled");
            assert!(app.dice.len() <= 4 + MAX_EXPLOSIONS, "blew past the cap");
            for d in &app.dice {
                assert!(d.x >= -0.01 && d.x <= maxx + 0.01);
                assert!(d.y >= -0.01 && d.y <= maxy + 0.01);
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
    fn live_total_excludes_dropped_dice_mid_roll() {
        // Before settling, the running total already ignores the dropped die.
        let app = seeded("2d20kh1", 7);
        let kept = app.dice.iter().find(|d| d.kept).unwrap();
        // live_total uses `shown`, but dropped dice never contribute regardless.
        let by_hand: i32 = app
            .dice
            .iter()
            .filter(|d| d.kept)
            .map(|d| d.shown as i32)
            .sum();
        assert_eq!(app.live_total(), by_hand);
        let _ = kept; // (named for clarity)
    }

    #[test]
    fn exploding_pool_still_converges_and_stays_in_arena() {
        let mut app = seeded("6d6!", 3);
        let (maxx, maxy) = app.max_xy();
        let mut settled = false;
        for _ in 0..12000 {
            app.update(1.0 / 60.0);
            for d in &app.dice {
                assert!(d.x >= -0.001 && d.x <= maxx + 0.001);
                assert!(d.y >= -0.001 && d.y <= maxy + 0.001);
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

        // Once settled, every die shows its rolled value...
        for d in &app.dice {
            assert_eq!(d.shown, d.final_value);
            assert!((1..=d.sides).contains(&d.final_value));
        }
        // ...and live/final totals agree.
        assert_eq!(app.total(), app.live_total());
        let t = app.total();
        assert!((4 + 2..=24 + 2).contains(&t), "total {t} out of range");
    }

    #[test]
    fn dice_stay_inside_the_arena() {
        let mut app = App::new("6d8".to_string());
        app.arena_w = 40.0;
        app.arena_h = 15.0;
        let (maxx, maxy) = app.max_xy();
        for _ in 0..6000 {
            app.update(1.0 / 60.0);
            for d in &app.dice {
                assert!(d.x >= -0.001 && d.x <= maxx + 0.001, "x={} escaped", d.x);
                assert!(d.y >= -0.001 && d.y <= maxy + 0.001, "y={} escaped", d.y);
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
        app.arena_w = 40.0;
        app.arena_h = 15.0;
        for f in 0..6000 {
            app.update(1.0 / 60.0);
            if f % 500 == 0 || app.all_settled() {
                let settled = app.dice.iter().filter(|d| d.settled).count();
                let max_sp = app
                    .dice
                    .iter()
                    .map(|d| (d.vx * d.vx + d.vy * d.vy).sqrt())
                    .fold(0.0f32, f32::max);
                eprintln!("f={f:5} settled={settled}/6 max_speed={max_sp:6.2}");
                if app.all_settled() {
                    break;
                }
            }
        }
        for (i, d) in app.dice.iter().enumerate() {
            eprintln!("  die {i}: x={:6.2} y={:6.2} settled={}", d.x, d.y, d.settled);
        }
    }

    #[test]
    fn settled_dice_do_not_overlap() {
        // Narrow but tall: forces real stacking (~3 per row) while leaving enough
        // height that no die ever gets crushed, so zero overlap is achievable.
        let mut app = App::new("8d6".to_string());
        app.arena_w = 22.0;
        app.arena_h = 40.0;

        settle(&mut app, 12000).expect("crowded pool never settled");

        for i in 0..app.dice.len() {
            for j in (i + 1)..app.dice.len() {
                let (a, b) = (&app.dice[i], &app.dice[j]);
                // Allow a hair of penetration from the position-correction slop.
                let (px, py) = penetration(a.x, a.y, b.x, b.y);
                let pen = px.min(py);
                assert!(
                    pen < 0.5,
                    "dice {i} and {j} overlap by {pen:.2} at ({},{}) / ({},{})",
                    a.x, a.y, b.x, b.y
                );
            }
        }
    }

    #[test]
    fn settled_dice_stay_in_bounds_when_cramped() {
        // Arenas a bit too small to hold every die cleanly (~1.5× capacity):
        // stacks reach the ceiling and the stuck/over-tall paths kick in, but the
        // pool still converges. No settled die may end up outside the arena
        // (a regression guard for the once-unclamped rest position).
        for (spec, w, h) in [("12d6", 20.0, 14.0), ("12d6", 18.0, 14.0), ("16d6", 22.0, 16.0)] {
            for _ in 0..40 {
                let mut app = App::new(spec.to_string());
                app.arena_w = w;
                app.arena_h = h;
                let (maxx, maxy) = app.max_xy();
                settle(&mut app, 15000).expect("cramped pool never settled");
                for (i, d) in app.dice.iter().enumerate() {
                    assert!(
                        d.x >= -0.01 && d.x <= maxx + 0.01 && d.y >= -0.01 && d.y <= maxy + 0.01,
                        "{spec} die {i} settled out of bounds at ({}, {})",
                        d.x, d.y
                    );
                }
            }
        }
    }

    #[test]
    fn tiny_arena_still_settles() {
        let mut app = App::new("3d6".to_string());
        app.arena_w = 3.0; // smaller than a die box
        app.arena_h = 2.0;
        assert!(settle(&mut app, 100).is_some());
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
        assert_eq!(app.history[0].values.len(), 1, "only the kept die is stored");
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
        let app = seeded("3d6", 1);
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
        assert!(adv.mean > 12.0, "advantage mean {} should beat a flat d20", adv.mean);

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

