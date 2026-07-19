//! The arena camera and the pure world→screen math shared by the renderer and
//! the simulation.
//!
//! [`arena_camera`]/[`live_camera`] define where the eye sits and how it moves
//! through the throw; [`project_to_cell`] is the one true world→cell map. The
//! terminal renderer poses its camera here and the crit/fumble particle bursts
//! ([`crate::app`]) project through the *same* [`live_camera`], so the dice,
//! their burned numbers, and the bursts can never disagree about where a die
//! sits on screen. No renderer types cross this boundary — it is glam only.

use glam::{Mat4, Vec3, Vec4};

/// Projection type for the camera.
#[derive(Debug, Clone, Copy)]
pub enum Projection {
    Perspective { fov_y: f32, near: f32, far: f32 },
}

impl Default for Projection {
    fn default() -> Self {
        Self::Perspective {
            fov_y: std::f32::consts::FRAC_PI_4,
            near: 0.1,
            far: 100.0,
        }
    }
}

/// A camera that defines the view into the 3D scene.
#[derive(Debug, Clone)]
pub struct Camera {
    pub position: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub projection: Projection,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            position: Vec3::new(0.0, 2.0, 5.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            projection: Projection::default(),
        }
    }
}

impl Camera {
    /// Compute the view matrix.
    // tinhorn: glam 0.33 deprecated `Mat4::look_at_rh` in favour of the
    // `camera::rh::view` free functions.
    pub fn view_matrix(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.position, self.target, self.up)
    }

    /// Compute the projection matrix for a given aspect ratio.
    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        match self.projection {
            Projection::Perspective { fov_y, near, far } => {
                glam::camera::rh::proj::directx::perspective(fov_y, aspect, near, far)
            }
        }
    }
}

/// Field of view the arena camera frames the tray through.
const ARENA_FOV_Y: f32 = std::f32::consts::FRAC_PI_4;

/// The arena's view aspect for a `cols`×`rows` cell area. HalfBlock packs 1×2
/// pixels per cell, so a cell is twice as tall as wide — hence the `*2`.
pub fn arena_aspect(cols: f32, rows: f32) -> f32 {
    cols / (rows * 2.0)
}

/// The arena camera: above and in front, angled down at the felt — a dice tray
/// on a table. `shake` offsets both eye and target for the hard-throw shudder.
///
/// The distance is **derived from `aspect`** so the tray's full width always just
/// fills the frame: a wide terminal pulls the camera in for chunkier dice, a
/// narrow one backs off so a die against a wall is never clipped. `aspect` is
/// `cols/(rows*2)`, exactly what [`project_to_cell`] computes, so both agree.
///
/// One definition, shared by the live renderer (the terminal `ui`) and the
/// particle placement in [`crate::app`], so the dice, their burned numbers, and
/// the crit/fumble bursts can never disagree about where a die sits on screen.
pub fn arena_camera(shake: Vec3, aspect: f32, focus: f32) -> Camera {
    let focus = focus.clamp(0.0, 1.0);
    let tan = (ARENA_FOV_Y * 0.5).tan();
    // Frame the whole tray: fit its full width AND its height, taking whichever
    // needs more distance, so the cup, the throw's arc, and the tray are never
    // clipped. As the dice settle (`focus`→1) there's no launch arc left, so the
    // vertical framing tightens and the camera leans in over the felt.
    let want_half_w = crate::physics::HX + crate::physics::DIE_R + 0.02;
    // The vertical framing follows the tray's depth (`physics::HZ`), not a magic
    // number, so a deeper tray frames correctly at both ends of the ceremony.
    // Settled (`focus`→1) the camera looks near-straight down (~68°, sin ≈ 0.93):
    // the vertical frame maps almost directly to felt depth, so cover HZ plus a
    // whisker of rail margin. Rolling (`focus`→0) the tilt is shallow (~31°, sin ≈
    // 0.52): depth foreshortens to half and the launch arc's *height* needs the
    // frame — a fixed headroom term, since HY and the throw are depth-agnostic.
    let hz = crate::physics::HZ;
    let overhead = 0.93 * hz + 0.15; // felt depth at ~68° + a thin rail margin
    let establishing = 0.52 * hz + 1.45; // tray front at ~31° + launch-arc headroom
    let want_half_h = establishing + (overhead - establishing) * focus;
    let dist_w = want_half_w / (aspect.max(0.25) * tan);
    let dist_h = want_half_h / tan;
    let dist = dist_w.max(dist_h).clamp(3.5, 8.0);

    // Aim at the tray's mid-low centre; sit up-and-back. Rolling, the tilt is a
    // shallow ~31° "tray on a table"; as the dice rest it pitches up to ~68°, the
    // way you'd lean over the felt to read the numbers on top.
    let target = Vec3::new(0.0, -0.35, 0.0);
    let pitch = 0.55 + 0.63 * focus; // radians: ~31° rolling → ~68° overhead read
    let (pitch_sin, pitch_cos) = (pitch.sin(), pitch.cos());
    let position = target + Vec3::new(0.0, dist * pitch_sin, dist * pitch_cos);

    Camera {
        position: position + shake,
        target: target + shake,
        up: Vec3::Y,
        projection: Projection::Perspective {
            fov_y: ARENA_FOV_Y,
            near: 0.1,
            far: 100.0,
        },
    }
}

/// THE live arena camera: [`arena_camera`] plus every per-frame modifier — the
/// idle drift and the crit flash's punch-in toward the tray. Both the renderer
/// (the terminal `ui`) and the particle placement ([`crate::app`]) build their
/// camera here, so a new modifier moves the burned numbers and the crit/fumble
/// bursts with it — the two projections can never drift out of register.
pub fn live_camera(shake: Vec3, aspect: f32, focus: f32, clock: f32, flash: f32) -> Camera {
    let mut camera = arena_camera(shake, aspect, focus);
    // A slow idle drift of the eye (target fixed) so the view floats with life.
    camera.position += idle_orbit(clock);
    // A natural crit punches the camera in toward the tray, receding as it fades.
    if flash > 0.0 {
        let dir = (camera.target - camera.position).normalize_or_zero();
        camera.position += dir * (flash * 0.35);
    }
    camera
}

/// A gentle idle drift for the camera *eye* (not the target), so the view floats
/// like a slow handheld shot — near tray against far lights gives a little
/// parallax. Cosmetic; applied to `Camera::position` via [`live_camera`].
pub fn idle_orbit(time: f32) -> Vec3 {
    // A slow top-to-bottom pan of the eye (target fixed), like leaning in to read
    // the dice and easing back: the eye rises while dollying forward so the view
    // pitches down over the top faces — a vertical drift with only a whisper of
    // sideways sway. Zero at t=0 so it eases in from rest.
    let p = (time * 0.16).sin(); // ~39 s per loop
    Vec3::new(
        (time * 0.11).sin() * 0.06, // barely any sideways
        p * 0.42,                   // rise and fall...
        -p * 0.28,                  // ...easing forward as it rises, so the view tilts down
    )
}

/// Project a world point into arena *cell* coordinates for a `cols`×`rows` cell
/// area — the space the burned numbers and the 2D particle bursts both live in.
/// `None` when the point is behind the camera. This is the one true world→screen
/// map for the arena; everything that has to land on a die goes through it.
pub fn project_to_cell(camera: &Camera, p: Vec3, cols: f32, rows: f32) -> Option<(f32, f32)> {
    let aspect = arena_aspect(cols, rows);
    let clip =
        camera.projection_matrix(aspect) * camera.view_matrix() * Vec4::new(p.x, p.y, p.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let nx = clip.x / clip.w;
    let ny = clip.y / clip.w;
    Some(((nx + 1.0) * 0.5 * cols, (1.0 - ny) * 0.5 * rows))
}
