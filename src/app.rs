//! Application state and the little physics engine that bounces the dice.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::parse::{self, Roll};

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
    tumble_acc: f32,
    still_for: f32, // seconds spent slow-but-unsettled; triggers the stuck fallback
    age: f32,       // seconds since thrown; a hard cap so nothing tumbles forever
}

pub struct App {
    pub input: String,
    pub dice: Vec<Die>,
    pub modifier: i32,
    pub error: Option<String>,
    pub arena_w: f32,
    pub arena_h: f32,
    pub spawned: bool,
    needs_spawn: bool,
    rng: StdRng,
}

impl App {
    pub fn new(initial: String) -> Self {
        let mut app = App {
            input: String::new(),
            dice: Vec::new(),
            modifier: 0,
            error: None,
            arena_w: 0.0,
            arena_h: 0.0,
            spawned: false,
            needs_spawn: false,
            rng: StdRng::from_entropy(),
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
            Ok(Roll { dice, modifier }) => {
                self.error = None;
                self.modifier = modifier;
                self.dice = dice
                    .iter()
                    .enumerate()
                    .map(|(i, spec)| {
                        let final_value = self.rng.gen_range(1..=spec.sides);
                        Die {
                            sides: spec.sides,
                            final_value,
                            shown: self.rng.gen_range(1..=spec.sides),
                            x: 0.0,
                            y: 0.0,
                            vx: 0.0,
                            vy: 0.0,
                            settled: false,
                            color_idx: i,
                            tumble_acc: 0.0,
                            still_for: 0.0,
                            age: 0.0,
                        }
                    })
                    .collect();
                self.spawned = false;
                self.needs_spawn = true;
            }
            Err(e) => {
                self.error = Some(e);
            }
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
        }
    }

    pub fn all_settled(&self) -> bool {
        self.spawned && !self.dice.is_empty() && self.dice.iter().all(|d| d.settled)
    }

    /// Final total (only meaningful once settled, but always well-defined).
    pub fn total(&self) -> i32 {
        self.dice.iter().map(|d| d.final_value as i32).sum::<i32>() + self.modifier
    }

    /// Running total of whatever faces are currently showing.
    pub fn live_total(&self) -> i32 {
        self.dice.iter().map(|d| d.shown as i32).sum::<i32>() + self.modifier
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
}

