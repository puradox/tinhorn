//! Application state and the little physics engine that bounces the dice.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::parse::{self, Roll};

/// Width / height of a die "box" in terminal cells: ┌───┐ / │ N │ / └───┘
pub const DIE_W: f32 = 5.0;
pub const DIE_H: f32 = 3.0;

// Physics tuning (units are terminal cells and seconds).
const GRAVITY: f32 = 60.0;
const WALL_RESTITUTION: f32 = 0.72;
const FLOOR_RESTITUTION: f32 = 0.5;
const FLOOR_FRICTION: f32 = 0.7;
const AIR_DRAG: f32 = 0.4;
const TUMBLE_INTERVAL: f32 = 0.05; // how often a mid-air die shows a new face
const SETTLE_SPEED_SQ: f32 = 6.0; // below this (and on the floor) a die comes to rest
const MIN_BOUNCE_VY: f32 = 3.5; // smaller floor bounces are killed so dice actually stop

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

            // Come to rest.
            let on_floor = die.y >= maxy - 0.6;
            let slow = die.vx * die.vx + die.vy * die.vy < SETTLE_SPEED_SQ;
            if on_floor && slow {
                die.settled = true;
                die.shown = die.final_value;
                die.y = maxy;
                die.vx = 0.0;
                die.vy = 0.0;
            }
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
    fn tiny_arena_still_settles() {
        let mut app = App::new("3d6".to_string());
        app.arena_w = 3.0; // smaller than a die box
        app.arena_h = 2.0;
        assert!(settle(&mut app, 100).is_some());
    }
}
