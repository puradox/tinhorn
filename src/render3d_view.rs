//! Glue between the pure `render3d` rasterizer and tinhorn's ratatui UI.
//!
//! `render3d` renders a [`Scene`] into an RGB [`Framebuffer`] and knows nothing
//! about ratatui. This module is the bridge: it packs that framebuffer's pixels
//! into terminal cells inside a ratatui [`Buffer`], so any widget can draw a 3D
//! scene into a [`Rect`] with one call. Kept out of `render3d` itself so the
//! renderer core stays dependency-light (glam only). It also owns the shared
//! [`arena_camera`]/[`project_to_cell`] so the arena, the burned numbers, and
//! the crit/fumble bursts all agree on where a die sits on screen.
//!
//! The Braille/ASCII render paths are only exercised by tests for now, so a few
//! items are dead outside `#[cfg(test)]`.
#![allow(dead_code)]

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::render3d::camera::Camera;
use crate::render3d::color::Rgb;
use crate::render3d::math::{Vec3, Vec4};
use crate::render3d::pipeline::framebuffer::Framebuffer;
use crate::render3d::pipeline::render;
use crate::render3d::scene::Scene;

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
/// fills the frame: a wide terminal (more horizontal room) pulls the camera in
/// for chunkier dice instead of wasting the space, and a narrow one backs off so
/// a die resting against a wall is never clipped. `aspect` is `cols/(rows*2)`,
/// exactly what [`project_to_cell`] computes, so both agree on the framing.
///
/// One definition, shared by the live renderer ([`crate::ui`]) and the particle
/// placement in [`crate::app`], so the dice, their burned numbers, and the
/// crit/fumble bursts can never disagree about where a die sits on screen.
pub fn arena_camera(shake: Vec3, aspect: f32, focus: f32) -> Camera {
    use crate::render3d::camera::Projection;

    let focus = focus.clamp(0.0, 1.0);
    let tan = (ARENA_FOV_Y * 0.5).tan();
    // Frame the whole tray: fit its full width AND its height — the floor, the
    // walls, and up past the height a die launches from — taking whichever needs
    // more distance, so the cup, the throw's arc, and the tray are never clipped.
    // As the dice settle (`focus`→1) there's no launch arc left to frame, so the
    // vertical framing tightens a touch and the camera leans in over the felt.
    let want_half_w = crate::physics::HX + crate::physics::DIE_R + 0.25;
    // The vertical framing follows the tray's depth (`physics::HZ`) rather than a
    // magic number, so a deeper tray automatically frames correctly at both ends
    // of the ceremony. Settled (`focus`→1) the camera looks near-straight down
    // (~68°, sin ≈ 0.93): the vertical frame maps almost directly to the felt's
    // depth, so cover HZ at that projection plus a whisker of rail margin.
    // Rolling (`focus`→0) the tilt is shallow (~31°, sin ≈ 0.52): depth
    // foreshortens to half, and the launch arc's *height* is what needs the
    // frame — a fixed headroom term, since HY and the throw are depth-agnostic.
    // (At the old HZ = 1.1 these derive the previous hand-tuned 2.4 → 1.45.)
    let hz = crate::physics::HZ;
    let overhead = 0.93 * hz + 0.42; // felt depth at ~68° + rail margin
    let establishing = 0.52 * hz + 1.83; // tray front at ~31° + launch-arc headroom
    let want_half_h = establishing + (overhead - establishing) * focus;
    let dist_w = want_half_w / (aspect.max(0.25) * tan);
    let dist_h = want_half_h / tan;
    let dist = dist_w.max(dist_h).clamp(3.5, 8.0);

    // Aim at the tray's mid-low centre; sit up-and-back. While rolling the tilt is
    // a shallow ~31° "tray on a table"; as the dice come to rest the camera pitches
    // up and over to ~68°, looking down the way you'd lean over the felt to read
    // the numbers on top — which is also the angle the floorboards read best from.
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
/// idle drift, and the crit flash's punch-in toward the tray. Both the renderer
/// ([`crate::ui`]) and the particle placement ([`crate::app`]) build their
/// camera here, so a new modifier added for the scene automatically moves the
/// burned numbers and the crit/fumble bursts with it — the two projections can
/// never drift out of register.
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
/// parallax and life. Cosmetic and slow; applied to `Camera::position` after
/// [`arena_camera`] (via [`live_camera`]).
pub fn idle_orbit(time: f32) -> Vec3 {
    // A slow top-to-bottom pan of the eye (target fixed), like someone leaning in
    // to read the dice and easing back: the eye rises while dollying gently forward
    // so the view pitches down over the top faces, then settles — a vertical drift
    // with only a whisper of sideways sway. Zero at t=0 so it eases in from rest.
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

/// Braille dot bits, indexed `[row 0..4][col 0..2]` — the Unicode 2×4 layout.
const BRAILLE: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
/// Luminance ramp, dark → light.
const RAMP: &[u8] = b" .:-=+*#%@";

/// How framebuffer pixels are packed into terminal cells. Each choice trades
/// spatial resolution against colour (see the render-types lesson).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderMode {
    /// `▀` with fg = upper pixel, bg = lower pixel: full colour, 1×2 px per cell.
    #[default]
    HalfBlock,
    /// Braille dots: 2×4 px per cell — crisp shape, one (averaged) colour per cell.
    Braille,
    /// Luminance ramp: one glyph per cell, most portable, lowest resolution.
    Ascii,
}

impl RenderMode {
    /// The framebuffer resolution needed to fill `area` at this cell density.
    pub fn pixel_size(self, area: Rect) -> (u32, u32) {
        let (w, h) = (area.width as u32, area.height as u32);
        match self {
            RenderMode::HalfBlock | RenderMode::Ascii => (w, h * 2),
            RenderMode::Braille => (w * 2, h * 4),
        }
    }
}

/// Supersampling factor: the scene is rasterised at this multiple of the target
/// resolution, then box-downsampled, so dice and wall edges come out smooth
/// instead of stair-stepped. (The arena framebuffer is tiny, so 2× is cheap.)
const SUPERSAMPLE: u32 = 2;

/// Render `scene` through `camera` and paint it into `area` of `buf`, in one
/// call. Rasterises at [`SUPERSAMPLE`]× and averages down for anti-aliasing; a
/// no-op on an empty `area`.
///
/// The two framebuffers persist across calls (the render loop is here every
/// frame; reallocating hundreds of kilobytes of colour/depth/alpha 60× a second
/// is pure allocator churn): `render` clears them, and `resize` only
/// reallocates when the area actually changes.
pub fn draw(buf: &mut Buffer, area: Rect, scene: &Scene, camera: &Camera, mode: RenderMode) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (w, h) = mode.pixel_size(area);
    let ss = SUPERSAMPLE.max(1);
    thread_local! {
        static BUFFERS: std::cell::RefCell<(Framebuffer, Framebuffer)> =
            std::cell::RefCell::new((Framebuffer::new(0, 0), Framebuffer::new(0, 0)));
    }
    BUFFERS.with(|b| {
        let (hi, lo) = &mut *b.borrow_mut();
        hi.resize(w * ss, h * ss);
        render(scene, camera, hi);
        let fb = if ss == 1 {
            hi
        } else {
            downsample_into(hi, ss, lo);
            lo
        };
        vignette(fb, 0.32);
        blit(fb, area, buf, mode);
    });
}

/// Darken the frame gently toward its edges so the void recedes and the eye is
/// pulled into the lit tray, and apply a subtle **warm grade** (lift red, ease
/// blue) so the whole frame reads as a warm room. Radial and subtle; the centre
/// keeps its brightness, only the corners fall off (toward a warm ember, not
/// pure black, because red is lifted before the darken).
fn vignette(fb: &mut Framebuffer, strength: f32) {
    let (cx, cy) = (fb.width as f32 * 0.5, fb.height as f32 * 0.5);
    let inv = 1.0 / (cx * cx + cy * cy); // normalise the corner distance² to 1
    for y in 0..fb.height {
        for x in 0..fb.width {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            let d2 = (dx * dx + dy * dy) * inv; // 0 at the centre → 1 at a corner
            let f = 1.0 - strength * d2 * d2; // quartic: bites only near the edges
            let i = fb.index(x, y);
            let c = fb.color[i];
            let (fr, fg, fb_) = (f * 1.04, f, f * 0.95); // warm grade woven into the vignette
            fb.color[i] = Rgb(
                (c.0 as f32 * fr).min(255.0) as u8,
                (c.1 as f32 * fg).min(255.0) as u8,
                (c.2 as f32 * fb_) as u8,
            );
        }
    }
}

/// Box-downsample `hi` by integer factor `ss` into `out`, averaging each
/// `ss`×`ss` block's colour and coverage (alpha) into one output pixel — cheap
/// anti-aliasing. Writes into a caller-owned buffer so the per-frame path
/// allocates nothing.
fn downsample_into(hi: &Framebuffer, ss: u32, out: &mut Framebuffer) {
    let (w, h) = (hi.width / ss, hi.height / ss);
    out.resize(w, h);
    let n = (ss * ss).max(1);
    for y in 0..h {
        for x in 0..w {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..ss {
                for dx in 0..ss {
                    let i = hi.index(x * ss + dx, y * ss + dy);
                    let c = hi.color[i];
                    r += c.0 as u32;
                    g += c.1 as u32;
                    b += c.2 as u32;
                    a += hi.alpha[i] as u32;
                }
            }
            let oi = out.index(x, y);
            out.color[oi] = Rgb((r / n) as u8, (g / n) as u8, (b / n) as u8);
            out.alpha[oi] = (a / n) as u8;
        }
    }
}

/// Paint an already-rendered framebuffer into `area` of `buf`. Split out from
/// [`draw`] so a caller that reuses one framebuffer across frames (the render
/// loop) can skip the per-frame allocation.
pub fn blit(fb: &Framebuffer, area: Rect, buf: &mut Buffer, mode: RenderMode) {
    match mode {
        RenderMode::HalfBlock => blit_half_block(fb, area, buf),
        RenderMode::Braille => blit_braille(fb, area, buf),
        RenderMode::Ascii => blit_ascii(fb, area, buf),
    }
}

/// Framebuffer pixel, or black outside its bounds.
fn px(fb: &Framebuffer, x: u32, y: u32) -> Rgb {
    if x < fb.width && y < fb.height {
        fb.get_pixel(x, y)
    } else {
        Rgb::BLACK
    }
}

fn blit_half_block(fb: &Framebuffer, area: Rect, buf: &mut Buffer) {
    for row in 0..area.height {
        for col in 0..area.width {
            let x = col as u32;
            let upper = px(fb, x, row as u32 * 2);
            let lower = px(fb, x, row as u32 * 2 + 1);
            let cell = &mut buf[(area.x + col, area.y + row)];
            cell.set_char('▀');
            cell.set_style(
                Style::default()
                    .fg(Color::Rgb(upper.0, upper.1, upper.2))
                    .bg(Color::Rgb(lower.0, lower.1, lower.2)),
            );
        }
    }
}

fn blit_braille(fb: &Framebuffer, area: Rect, buf: &mut Buffer) {
    for row in 0..area.height {
        for col in 0..area.width {
            let (base_x, base_y) = (col as u32 * 2, row as u32 * 4);
            let mut bits: u8 = 0;
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..4u32 {
                for dx in 0..2u32 {
                    let (x, y) = (base_x + dx, base_y + dy);
                    if x < fb.width && y < fb.height {
                        let i = fb.index(x, y);
                        if fb.alpha[i] != 0 {
                            bits |= BRAILLE[dy as usize][dx as usize];
                            let c = fb.color[i];
                            r += c.0 as u32;
                            g += c.1 as u32;
                            b += c.2 as u32;
                            n += 1;
                        }
                    }
                }
            }
            let cell = &mut buf[(area.x + col, area.y + row)];
            if bits == 0 {
                cell.set_char(' ');
            } else {
                let n = n.max(1);
                cell.set_char(char::from_u32(0x2800 + bits as u32).unwrap_or(' '));
                cell.set_style(Style::default().fg(Color::Rgb(
                    (r / n) as u8,
                    (g / n) as u8,
                    (b / n) as u8,
                )));
            }
        }
    }
}

fn blit_ascii(fb: &Framebuffer, area: Rect, buf: &mut Buffer) {
    for row in 0..area.height {
        for col in 0..area.width {
            let x = col as u32;
            let upper = px(fb, x, row as u32 * 2);
            let lower = px(fb, x, row as u32 * 2 + 1);
            let lum = (upper.luminance() + lower.luminance()) * 0.5;
            let ramp = (lum * (RAMP.len() - 1) as f32).round() as usize;
            let ch = RAMP[ramp.min(RAMP.len() - 1)] as char;
            let color = Rgb(
                ((upper.0 as u16 + lower.0 as u16) / 2) as u8,
                ((upper.1 as u16 + lower.1 as u16) / 2) as u8,
                ((upper.2 as u16 + lower.2 as u16) / 2) as u8,
            );
            let cell = &mut buf[(area.x + col, area.y + row)];
            cell.set_char(ch);
            cell.set_style(Style::default().fg(Color::Rgb(color.0, color.1, color.2)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render3d::light::Light;
    use crate::render3d::material::Material;
    use crate::render3d::math::Vec3;
    use crate::render3d::object::SceneObject;
    use crate::render3d::primitives;

    fn cube_scene() -> Scene {
        let mut s = Scene::new();
        s.add_object(
            SceneObject::new(primitives::cube())
                .with_material(Material::default().with_color(Rgb(200, 120, 60))),
        );
        s.add_light(Light::ambient(Rgb::WHITE, 0.4));
        s.add_light(Light::directional(Vec3::new(-1.0, -1.0, -1.0), Rgb::WHITE));
        s
    }

    // The whole path — render3d Scene → Framebuffer → blit → ratatui Buffer —
    // runs and lands geometry in real ratatui cells. In ASCII mode background
    // pixels map to ' ', so a non-space centre cell means the cube is really there.
    #[test]
    fn draws_a_scene_into_a_ratatui_buffer() {
        let area = Rect::new(0, 0, 48, 24);
        let mut buf = Buffer::empty(area);
        draw(
            &mut buf,
            area,
            &cube_scene(),
            &Camera::default(),
            RenderMode::Ascii,
        );

        assert_ne!(buf[(24, 12)].symbol(), " ", "cube should cover the centre");
        let filled = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() != " ")
            .count();
        assert!(filled > 80, "cube should fill many cells, filled {filled}");
    }

    // Every mode paints into a same-sized ratatui area without panicking, and
    // HalfBlock fills every cell with the upper-half-block glyph.
    #[test]
    fn every_mode_paints_the_area() {
        let area = Rect::new(0, 0, 32, 16);
        for mode in [
            RenderMode::HalfBlock,
            RenderMode::Braille,
            RenderMode::Ascii,
        ] {
            let mut buf = Buffer::empty(area);
            draw(&mut buf, area, &cube_scene(), &Camera::default(), mode);
            if mode == RenderMode::HalfBlock {
                assert_eq!(buf[(0, 0)].symbol(), "▀");
            }
        }
    }

    // Eyeball the full pipeline as text:
    //   cargo test render3d_view::tests::print_cube -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_cube() {
        let area = Rect::new(0, 0, 60, 26);
        let mut buf = Buffer::empty(area);
        draw(
            &mut buf,
            area,
            &cube_scene(),
            &Camera::default(),
            RenderMode::Ascii,
        );
        println!("\ncube rendered through render3d → blit → ratatui Buffer:");
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            println!("{line}");
        }
    }
}
