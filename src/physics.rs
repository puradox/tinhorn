//! Real 3D rigid-body physics for the dice, on top of Rapier.
//!
//! The arena is a fixed-size box in world units (a shallow tray): a floor, four
//! walls, and a ceiling, all static. Each die is a dynamic body with a convex
//! collider built from its polyhedron, so it tumbles, bounces, collides with the
//! others, and comes to rest like a real die. Everything the rest of the app
//! needs is behind this small API — pose, sleeping, launch, step — so the
//! renderer and `app` never touch Rapier directly.
//!
//! Physics decides *nothing* about the dice values (those are the seeded RNG's,
//! drawn in `app`); it only decides where the dice land and how they lie.
//!
//! (Rapier 0.34 speaks glam, so its `Vec3`/`Quat` are ours — no conversion.)

use glam::{Quat, Vec3};
use rapier3d::prelude::*;

/// Half-extents of the arena box (x wide, y tall, z deep). Dice fall in −y.
pub const HX: f32 = 3.2;
pub const HY: f32 = 1.9;
/// Tray depth. 2.0 gives the felt a 1.6:1 width:depth read — a real dice tray's
/// proportions (the reference tray is ~4:3), not the letterbox slot 1.1 made.
/// Deeper still reads even more tray-like, but the overhead camera must back out
/// to fit the felt's depth and the dice shrink with it; 2.0 is the tradeoff.
/// The launch lattice, camera framing, and furniture all derive from this —
/// see `app::launch_pool`, `render3d_view::arena_camera`, and `ui::render_arena`.
pub const HZ: f32 = 2.0;
/// World radius a die occupies (its mesh is circumradius 1, scaled to this).
pub const DIE_R: f32 = 0.36;
/// THE fixed timestep. Every step advances exactly this much — [`Physics::step`]
/// takes no `dt` at all — which is what makes insta and animated rolls settle
/// bit-identically, so the `--seed` contract survives real physics.
pub const STEP: f32 = 1.0 / 60.0;

/// An impact worth a sound this step: which die, how hard, and whether it struck
/// another die (a brighter "knock") rather than a wall.
pub struct Impact {
    pub sides: u32,
    pub speed: f32,
    pub die_die: bool,
}

pub struct Physics {
    bodies: RigidBodySet,
    colliders: ColliderSet,
    gravity: Vec3,
    params: IntegrationParameters,
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: DefaultBroadPhase,
    narrow_phase: NarrowPhase,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd: CCDSolver,
}

impl Physics {
    pub fn new() -> Self {
        let mut colliders = ColliderSet::new();

        // Static arena: floor, ceiling, four walls. Thin cuboids just outside the
        // play volume so nothing tunnels out. (half-x, half-y, half-z, cx, cy, cz)
        let t = 0.2; // wall thickness
        let walls: [(f32, f32, f32, f32, f32, f32); 6] = [
            (HX, t, HZ, 0.0, -HY - t, 0.0), // floor
            (HX, t, HZ, 0.0, HY + t, 0.0),  // ceiling
            (t, HY + t, HZ, -HX - t, 0.0, 0.0),
            (t, HY + t, HZ, HX + t, 0.0, 0.0),
            (HX, HY + t, t, 0.0, 0.0, -HZ - t),
            (HX, HY + t, t, 0.0, 0.0, HZ + t),
        ];
        for (hx, hy, hz, x, y, z) in walls {
            colliders.insert(
                ColliderBuilder::cuboid(hx, hy, hz)
                    .translation(Vec3::new(x, y, z))
                    .restitution(0.45)
                    .friction(0.85)
                    .build(),
            );
        }

        let params = IntegrationParameters {
            dt: STEP,
            ..Default::default()
        };

        Self {
            bodies: RigidBodySet::new(),
            colliders,
            gravity: Vec3::new(0.0, -14.0, 0.0), // a touch heavier than earth, snappier rolls
            params,
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad_phase: DefaultBroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd: CCDSolver::new(),
        }
    }

    /// Drop a die into the world: a dynamic body with a convex collider from its
    /// mesh points (circumradius 1, scaled to [`DIE_R`]). Returns its handle.
    pub fn spawn(&mut self, mesh_points: &[Vec3], pos: Vec3) -> RigidBodyHandle {
        let body = RigidBodyBuilder::dynamic()
            .translation(pos)
            .linear_damping(0.15)
            .angular_damping(0.35)
            .can_sleep(true)
            .build();
        let handle = self.bodies.insert(body);

        // Sleep fast once a die is still, so its value burns in promptly. Rapier's
        // default is a sleepy 2 s; a fifth of that (with slightly looser velocity
        // thresholds) makes the roll feel *done* the instant the dice stop.
        if let Some(b) = self.bodies.get_mut(handle) {
            let act = b.activation_mut();
            act.time_until_sleep = 0.2;
            act.normalized_linear_threshold = 0.6;
            act.angular_threshold = 0.9;
        }

        let pts: Vec<Vec3> = mesh_points.iter().map(|p| *p * DIE_R).collect();
        let collider = ColliderBuilder::convex_hull(&pts)
            .unwrap_or_else(|| ColliderBuilder::ball(DIE_R))
            .restitution(0.4)
            .friction(0.9)
            .density(1.0)
            .build();
        self.colliders
            .insert_with_parent(collider, handle, &mut self.bodies);
        handle
    }

    /// Kick a die with a launch velocity and spin.
    pub fn launch(&mut self, h: RigidBodyHandle, linvel: Vec3, angvel: Vec3) {
        if let Some(b) = self.bodies.get_mut(h) {
            b.set_linvel(linvel, true);
            b.set_angvel(angvel, true);
            b.wake_up(true);
        }
    }

    /// Advance the simulation one fixed [`STEP`], returning impacts (for foley).
    /// An impact is a die whose speed dropped sharply this step — i.e. it hit
    /// something. `dice` carries each die's handle, `sides`, and its speed
    /// *before* the step; include settled dice too, so a strike against a
    /// resting die is still recognised as die-vs-die.
    pub fn step(&mut self, dice: &[(RigidBodyHandle, u32, f32)]) -> Vec<Impact> {
        self.pipeline.step(
            self.gravity,
            &self.params,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd,
            &(),
            &(),
        );

        let mut impacts = Vec::new();
        for &(h, sides, prev_speed) in dice {
            let (drop, sleeping, my_pos) = match self.bodies.get(h) {
                Some(b) => (
                    prev_speed - b.linvel().length(),
                    b.is_sleeping(),
                    b.translation(),
                ),
                None => continue,
            };
            if drop > 1.2 && !sleeping {
                // A die within a couple of radii was almost certainly the thing
                // it hit — voice that as a die-vs-die knock, not a wall strike.
                let die_die = dice.iter().any(|&(oh, _, _)| {
                    oh != h
                        && self
                            .bodies
                            .get(oh)
                            .map(|ob| (ob.translation() - my_pos).length() < 2.4 * DIE_R)
                            .unwrap_or(false)
                });
                impacts.push(Impact {
                    sides,
                    speed: drop,
                    die_die,
                });
            }
        }
        impacts
    }

    /// Current speed of a die's body (for the impact bookkeeping above).
    pub fn speed(&self, h: RigidBodyHandle) -> f32 {
        self.bodies
            .get(h)
            .map(|b| b.linvel().length())
            .unwrap_or(0.0)
    }

    /// A die body's sideways velocity (the tests pin throw direction with it).
    #[cfg(test)]
    pub fn velocity_x(&self, h: RigidBodyHandle) -> f32 {
        self.bodies.get(h).map(|b| b.linvel().x).unwrap_or(0.0)
    }

    /// A die's world pose: position and orientation.
    pub fn pose(&self, h: RigidBodyHandle) -> (Vec3, Quat) {
        match self.bodies.get(h) {
            Some(b) => (b.translation(), *b.rotation()),
            None => (Vec3::ZERO, Quat::IDENTITY),
        }
    }

    /// Has the die come to rest (Rapier put its body to sleep)?
    pub fn sleeping(&self, h: RigidBodyHandle) -> bool {
        self.bodies.get(h).map(|b| b.is_sleeping()).unwrap_or(true)
    }

    /// Force a die to rest where it is (the hard airborne-cap backstop): freeze
    /// it into a static body. Going through the body type keeps Rapier's island
    /// manager consistent — forcing `sleep()` directly trips its invariants.
    pub fn freeze(&mut self, h: RigidBodyHandle) {
        if let Some(b) = self.bodies.get_mut(h) {
            b.set_linvel(Vec3::ZERO, false);
            b.set_angvel(Vec3::ZERO, false);
            b.set_body_type(RigidBodyType::Fixed, false);
        }
    }

    /// Remove every die body (a new roll), leaving the static arena. Every body
    /// in the set is a die — the arena is parentless colliders — including any
    /// the airborne cap froze into `Fixed` bodies, so remove them all: filtering
    /// on `is_dynamic()` would leak each frozen die as an invisible obstacle.
    pub fn clear(&mut self) {
        let dice: Vec<RigidBodyHandle> = self.bodies.iter().map(|(h, _)| h).collect();
        for h in dice {
            self.bodies.remove(
                h,
                &mut self.islands,
                &mut self.colliders,
                &mut self.impulse_joints,
                &mut self.multibody_joints,
                true,
            );
        }
    }
}

impl Default for Physics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the airborne cap freezes a die into a `Fixed` body. `clear`
    /// must remove it like any other die — filtering on `is_dynamic()` left
    /// every frozen die behind as an invisible obstacle in all later rolls.
    #[test]
    fn clear_removes_frozen_dice_too() {
        let mut phys = Physics::new();
        let cube: Vec<Vec3> = (0..8)
            .map(|i| {
                Vec3::new(
                    if i & 1 == 0 { -1.0 } else { 1.0 },
                    if i & 2 == 0 { -1.0 } else { 1.0 },
                    if i & 4 == 0 { -1.0 } else { 1.0 },
                )
            })
            .collect();
        let live = phys.spawn(&cube, Vec3::ZERO);
        let frozen = phys.spawn(&cube, Vec3::new(1.0, 0.0, 0.0));
        phys.freeze(frozen);
        phys.clear();
        assert_eq!(
            phys.bodies.iter().count(),
            0,
            "clear() left die bodies behind"
        );
        let _ = live;
    }
}
