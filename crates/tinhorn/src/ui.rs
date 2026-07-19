//! All ratatui rendering lives here.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};

use crate::app::{App, Die, Pane, Particle, Stats};
use crate::graphics::GraphicsMode;
use crate::paint::Rgb;

/// The arena's palette in one place: the felt, and the mahogany tray lip with
/// its wall height. The Bevy renderer reads these directly (`ArenaStyle::DEFAULT`)
/// so a re-theme stays a one-line change.
#[derive(Clone, Copy)]
pub(crate) struct ArenaStyle {
    pub(crate) floor: Rgb,   // the felt
    pub(crate) wall: Rgb,    // the mahogany tray lip (back + two side walls)
    pub(crate) lip_top: f32, // wall height above the floor (shorter → more room shows)
}

impl ArenaStyle {
    pub(crate) const DEFAULT: ArenaStyle = ArenaStyle {
        floor: Rgb(22, 64, 42), // deep green baize
        wall: Rgb(66, 40, 28),  // dark warm mahogany
        lip_top: 0.85,          // a shallow tray lip — dice tray, not a deep box
    };
}

/// A cheap integer hash — the seed of every baked texture's grain. One
/// definition, so a tweak to the noise character lands in every texture.
fn hash32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// Value noise in [-1, 1] at a texel coordinate; a couple of frequencies of it
/// read as grain, plush pile, or wood depending on the weights.
fn noise2(x: u32, y: u32) -> f32 {
    let h = hash32(
        x.wrapping_mul(1973)
            .wrapping_add(y.wrapping_mul(9277))
            .wrapping_add(26699),
    );
    (h as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Smooth Hermite step, so baked shading eases in rather than banding.
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The colour-keyed bake cache all the procedural textures share. Textures are
/// asked for every frame but the palette rarely changes, so each look is baked at
/// most once. Bounded, and one implementation so the evict policy can't drift.
type TexCache =
    std::sync::OnceLock<std::sync::Mutex<Vec<([u8; 3], std::sync::Arc<crate::paint::Texture>)>>>;
fn cached_texture(
    cache: &TexCache,
    base: Rgb,
    bake: impl FnOnce() -> crate::paint::Texture,
) -> std::sync::Arc<crate::paint::Texture> {
    let key = [base.0, base.1, base.2];
    let mut slots = cache
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap();
    if let Some((_, tex)) = slots.iter().find(|(c, _)| *c == key) {
        return tex.clone();
    }
    let tex = std::sync::Arc::new(bake());
    slots.push((key, tex.clone()));
    if slots.len() > 6 {
        slots.remove(0); // bound the cache if many palettes are exercised
    }
    tex
}

/// A procedural grain texture: `base` colour with a soft fibrous grain baked in
/// (coarse blotches plus fine speckle), so a surface reads as fabric/painted
/// rather than a flat plastic plane.
pub(crate) fn grain_texture(base: Rgb) -> std::sync::Arc<crate::paint::Texture> {
    use crate::paint::Texture;

    static CACHE: TexCache = TexCache::new();
    cached_texture(&CACHE, base, || {
        const W: u32 = 192;
        const H: u32 = 80;
        let bc = [base.0 as f32, base.1 as f32, base.2 as f32];
        let mut data = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let n = 0.7 * noise2(x / 5, y / 5) + 0.3 * noise2(x, y);
                let f = 1.0 + 0.10 * n; // ±10% grain
                let i = ((y * W + x) * 4) as usize;
                for c in 0..3 {
                    data[i + c] = (bc[c] * f).clamp(0.0, 255.0) as u8;
                }
                data[i + 3] = 255;
            }
        }
        Texture::from_rgba(W, H, data)
    })
}

/// The felt of the tray floor, baked as a texture. Real trays recess plush felt
/// below the lip, so it reads as velvet and falls into shadow at the walls — so we
/// bake two things a flat grain can't: a soft **pile mottle** and an
/// **ambient-occlusion band** darkening the felt toward the three walls (the open
/// front stays lit). Cached by colour. UVs run `u` across the width, `v` from the
/// back wall (0) to the open front (1).
pub(crate) fn felt_texture(base: Rgb) -> std::sync::Arc<crate::paint::Texture> {
    use crate::paint::Texture;

    static CACHE: TexCache = TexCache::new();
    cached_texture(&CACHE, base, || {
        const W: u32 = 200;
        const H: u32 = 128; // ≈ square texels over the felt's 6.4×4 world span
        let bc = [base.0 as f32, base.1 as f32, base.2 as f32];
        let mut data = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            let v = y as f32 / (H - 1) as f32;
            for x in 0..W {
                let u = x as f32 / (W - 1) as f32;
                // Plush pile: broad soft blotches plus a whisper of tooth.
                let mottle = 0.06 * noise2(x / 9, y / 9) + 0.02 * noise2(x / 2, y / 2);
                // Recess AO: distance to the nearest wall (left u=0, right u=1, back
                // v=0) — the open front (v=1) casts no shadow. Dark in the recess,
                // full brightness a wall-band in. The back band's width is pinned in
                // *world* units (0.2 UV was tuned on the old 2.2-deep felt → 0.44
                // world), so a deeper tray doesn't stretch the recess shadow into a
                // smear down the felt.
                let au = smoothstep(0.0, 0.2, u.min(1.0 - u));
                let av = smoothstep(0.0, 0.44 / (2.0 * crate::physics::HZ), v);
                let ao = 0.5 + 0.5 * au.min(av);
                // Dice-traffic sheen: a soft radial brightening toward the felt's
                // centre (a few percent) where the dice land and scuff the pile.
                let dc = ((u - 0.5) * (u - 0.5) + (v - 0.5) * (v - 0.5)).sqrt();
                let sheen = 1.0 + 0.05 * (1.0 - smoothstep(0.0, 0.55, dc));
                let f = (1.0 + mottle) * ao * sheen;
                let i = ((y * W + x) * 4) as usize;
                for c in 0..3 {
                    data[i + c] = (bc[c] * f).clamp(0.0, 255.0) as u8;
                }
                data[i + 3] = 255;
            }
        }
        Texture::from_rgba(W, H, data)
    })
}

/// The stage curtains' velvet, baked as a texture: soft vertical streak variation
/// (varies across `u`, smeared down `v`) under a subtle darkening toward the top
/// header. Kept subtle on purpose — the drape's *geometry* (scalloped edge +
/// corrugated folds) does the talking; this only keeps the cloth from reading
/// flat. Cached by colour.
pub(crate) fn velvet_texture(base: Rgb) -> std::sync::Arc<crate::paint::Texture> {
    use crate::paint::Texture;

    static CACHE: TexCache = TexCache::new();
    cached_texture(&CACHE, base, || {
        const W: u32 = 128;
        const H: u32 = 128;
        let bc = [base.0 as f32, base.1 as f32, base.2 as f32];
        let mut data = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            let v = y as f32 / (H - 1) as f32;
            // Header shadow: the top of the drape falls into the rod's shade.
            let header = 1.0 - 0.16 * (1.0 - smoothstep(0.0, 0.2, v));
            for x in 0..W {
                // Streaks: quick variation across the width, long down the drop.
                let streak = 0.09 * noise2(x / 3, y / 24) + 0.04 * noise2(x, y / 7);
                let f = (1.0 + streak) * header;
                let i = ((y * W + x) * 4) as usize;
                for c in 0..3 {
                    data[i + c] = (bc[c] * f).clamp(0.0, 255.0) as u8;
                }
                data[i + 3] = 255;
            }
        }
        Texture::from_rgba(W, H, data)
    })
}

/// The room floor, baked as **wooden floorboards**: long planks with a dark groove
/// between them and a little shade variation plank to plank. At this tiny frame the
/// bold seam lines are what read as "a floor" rather than a flat brown field.
/// Planks run along the texture's `v`; seams sit at regular `u`. Cached by colour.
pub(crate) fn floor_texture(base: Rgb) -> std::sync::Arc<crate::paint::Texture> {
    use crate::paint::Texture;

    static CACHE: TexCache = TexCache::new();
    cached_texture(&CACHE, base, || {
        const W: u32 = 192;
        const H: u32 = 96;
        const PLANKS: f32 = 4.0; // planks across one texture tile
        let bc = [base.0 as f32, base.1 as f32, base.2 as f32];
        let mut data = vec![0u8; (W * H * 4) as usize];
        for x in 0..W {
            let u = x as f32 / W as f32;
            let fp = u * PLANKS;
            let idx = fp.floor();
            let frac = fp - idx; // position across this plank, 0..1
            // Dark groove where planks butt: 0 at each seam, 1 across the board
            // face. A wide-ish groove so the seam survives minification into the
            // grazing distance instead of averaging away to a flat "blank" tone.
            let groove = smoothstep(0.0, 0.09, frac) * smoothstep(0.0, 0.09, 1.0 - frac);
            // Each plank a slightly different wood shade.
            let shade = 0.82
                + 0.30
                    * (hash32((idx as u32).wrapping_mul(2_654_435_761)) as f32 / u32::MAX as f32);
            for y in 0..H {
                // Lengthwise grain streaks along the board (vary faster across u).
                let grain = 0.05 * noise2(x / 2, y / 10) + 0.03 * noise2(x, y);
                let f = shade * (0.38 + 0.62 * groove) * (1.0 + grain);
                let i = ((y * W + x) * 4) as usize;
                for c in 0..3 {
                    data[i + c] = (bc[c] * f).clamp(0.0, 255.0) as u8;
                }
                data[i + 3] = 255;
            }
        }
        Texture::from_rgba(W, H, data)
    })
}

/// The far room, baked as a texture: a warm vertical haze with a scatter of soft,
/// out-of-focus glowing **light blobs** — a defocused casino floor, impressionistic
/// rather than a hard row of machines. Emissive; the 3D bokeh add crisper glows in
/// front. Cached once; it never changes.
pub(crate) fn backdrop_texture() -> std::sync::Arc<crate::paint::Texture> {
    use crate::paint::Texture;
    use std::sync::{Arc, OnceLock};
    static CACHE: OnceLock<Arc<Texture>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            const W: u32 = 192;
            const H: u32 = 128;
            fn h(n: u32) -> f32 {
                let mut x = n.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
                x ^= x >> 16;
                x = x.wrapping_mul(0x7feb_352d);
                x ^= x >> 15;
                (x % 100_000) as f32 / 100_000.0
            }
            // The far wall is a vertical gradient: **bright at the bottom**, matched
            // to the lit floorboards so the floor→wall horizon is seamless (no dark
            // band reading as "missing floor"), fading up to a **dark ceiling** where
            // the bokeh hang as lights. `horizon` is the floor's own rendered tone.
            let horizon = [134.0, 100.0, 69.0]; // warm, == the lit floor at the seam
            let ceiling = 0.12; // ceiling brightness as a fraction of `horizon`
            let cols: [[f32; 3]; 8] = [
                [255.0, 182.0, 96.0],
                [255.0, 122.0, 80.0],
                [255.0, 220.0, 140.0],
                [150.0, 190.0, 255.0],
                [240.0, 110.0, 150.0],
                [130.0, 240.0, 210.0],
                [255.0, 160.0, 70.0],
                [200.0, 140.0, 255.0],
            ];
            // Pre-place soft light blobs high on the wall — the dark ceiling band —
            // so they read as distant/overhead lights, not signage down at the floor.
            let blobs: Vec<(f32, f32, f32, [f32; 3], f32)> = (0..34u32)
                .map(|i| {
                    let bx = h(i * 13 + 1) * W as f32;
                    let by = (0.04 + h(i * 13 + 2) * 0.34) * H as f32;
                    let br = 6.0 + h(i * 13 + 3) * 24.0;
                    let bc = cols[i as usize % cols.len()];
                    let bi = 0.4 + h(i * 13 + 5) * 0.6;
                    (bx, by, br, bc, bi)
                })
                .collect();
            // Bottom-to-top brightness: full at the floor seam (v→1), easing to the
            // dark ceiling (v→0). Smoothstep so most of the visible wall behind the
            // tray stays lit and only the top falls away.
            let mut data = vec![0u8; (W * H * 4) as usize];
            for y in 0..H {
                let v = y as f32 / (H - 1) as f32; // 0 = ceiling, 1 = floor seam
                let base_bri = ceiling + (1.0 - ceiling) * smoothstep(0.0, 0.6, v);
                // Wainscoting: a thick dark chair-rail band across the wall, above the
                // floor seam (near v≈0.8), with subtly deeper, warmer panelled paint
                // just below the rail. Both fade out before the seam, so the wall's
                // bottom tone still equals the lit floorboards — the horizon invariant
                // stays intact. Band only — a thin line would shimmer at this frame.
                let vb = 0.6; // chair-rail centre
                let half = 0.045; // band half-height — thick, not a line
                let rail = 1.0 - 0.4 * (1.0 - smoothstep(half, half * 2.2, (v - vb).abs()));
                let below = smoothstep(vb, vb + 2.0 * half, v) * (1.0 - smoothstep(0.66, 0.8, v));
                let bri = base_bri * rail;
                let wall = [
                    horizon[0] * (1.0 + 0.06 * below), // panelled wall reads a touch
                    horizon[1] * (1.0 - 0.03 * below), // deeper and warmer below the
                    horizon[2] * (1.0 - 0.12 * below), // rail than the plaster above
                ];
                for x in 0..W {
                    let mut col = [wall[0] * bri, wall[1] * bri, wall[2] * bri];
                    for &(bx, by, br, bc, bi) in &blobs {
                        let (dx, dy) = (x as f32 - bx, y as f32 - by);
                        let d2 = (dx * dx + dy * dy) / (br * br);
                        if d2 < 1.0 {
                            let f = (1.0 - d2) * (1.0 - d2) * bi; // soft quadratic falloff
                            for c in 0..3 {
                                col[c] += bc[c] * f;
                            }
                        }
                    }
                    let i = ((y * W + x) * 4) as usize;
                    for c in 0..3 {
                        data[i + c] = col[c].clamp(0.0, 255.0) as u8;
                    }
                    data[i + 3] = 255;
                }
            }
            Arc::new(Texture::from_rgba(W, H, data))
        })
        .clone()
}

/// THE per-die colour palette; dice cycle through it by index. The 3D dice
/// paint with these `Rgb`s directly ([`die_rgb`]) and the result chips derive
/// their ratatui colour from the same slot ([`die_color`]), so a chip and the
/// die it stands for can never disagree about a colour.
const PALETTE: [Rgb; 8] = [
    Rgb(24, 214, 230),  // cyan
    Rgb(248, 206, 20),  // amber
    Rgb(36, 214, 74),   // green
    Rgb(228, 44, 228),  // magenta
    Rgb(244, 40, 40),   // red
    Rgb(48, 108, 255),  // blue
    Rgb(120, 246, 120), // lime
    Rgb(246, 108, 246), // pink
];

/// The colour for die palette slot `idx`.
pub(crate) fn die_rgb(idx: usize) -> Rgb {
    PALETTE[idx % PALETTE.len()]
}

/// The ratatui colour for die palette slot `idx` — [`die_rgb`] as a cell colour.
fn die_color(idx: usize) -> Color {
    let Rgb(r, g, b) = die_rgb(idx);
    Color::Rgb(r, g, b)
}

/// The one tier ladder for a throw's power. The arena title and the release
/// echo both read it, so the two labels on screen can never disagree about
/// the same release.
#[derive(Clone, Copy, PartialEq)]
enum ThrowTier {
    Lob,
    Toss,
    Rocket,
    Peak,
}

fn throw_tier(power: f32) -> ThrowTier {
    if power >= 0.92 {
        ThrowTier::Peak
    } else if power >= 0.70 {
        ThrowTier::Rocket
    } else if power >= 0.33 {
        ThrowTier::Toss
    } else {
        ThrowTier::Lob
    }
}

/// A dark slate inset drawn under a die's number so the digits read over any die
/// colour — a single cell behind the small single-cell overlay.
const NUMBER_PLATE: Color = Color::Rgb(20, 24, 30);

/// The big number is sized to sit within the die's read-face, not its whole
/// silhouette: the fractions of the die's on-screen box the glyph must fit inside.
/// The top face reads near-full width but foreshortens short, so width is roomier.
const FACE_FRAC_W: f32 = 0.92;
const FACE_FRAC_H: f32 = 0.78;

/// The ink a die's number is drawn in — its colour and weight — or `None` to hide
/// it this frame. Size-agnostic, so the single-cell overlay and the scaled
/// block-digit both style the digits the same way.
///
/// While tumbling the number is dim and colourless, and **ducks out** whenever no
/// face squarely fronts the camera: `clarity` is how far the read-face's facing
/// leads the runner-up's, dropping toward zero edge- or corner-on (two faces tie),
/// so the digit blinks off and reads as ink on the tumbling solid. On settle the
/// burned value shows in full — hot on a crit, red on a fumble, grey if dropped by
/// keep/drop, white otherwise.
fn face_ink(die: &Die, clarity: f32) -> Option<(Color, Modifier)> {
    /// Below this read-face dominance no single face clearly fronts the camera,
    /// so the airborne decoy hides — a brief wink at each edge/corner crossing.
    const FACE_CLARITY_HIDE: f32 = 0.15;
    if !die.settled {
        if clarity < FACE_CLARITY_HIDE {
            return None;
        }
        return Some((Color::Rgb(150, 158, 152), Modifier::DIM));
    }
    if !die.kept {
        return Some((Color::DarkGray, Modifier::empty()));
    }
    Some(if crate::app::crit_face(die.sides, die.final_value) {
        (Color::Yellow, Modifier::BOLD)
    } else if crate::app::fumble_face(die.sides, die.final_value) {
        (Color::Red, Modifier::BOLD)
    } else {
        (Color::White, Modifier::BOLD)
    })
}

/// The face of a die that points most at the eye, for placing its number: the
/// face's `centroid` (unit-mesh space, to anchor the digit on) and its `clarity` —
/// how far that face's facing leads the runner-up's. Clarity nears zero when two
/// faces tie for frontmost (die rolled edge- or corner-on), where the airborne
/// number ducks out; it's large when one face squarely presents, and that holds for
/// a cube or a d20 alike, so one threshold fits every die. `to_cam` is the unit eye
/// direction in world space (die → camera), `rot` the die's orientation.
pub(crate) fn read_face(
    faces: &[tinhorn_core::dice_geom::FaceGeom],
    rot: glam::Quat,
    to_cam: glam::Vec3,
) -> (glam::Vec3, f32) {
    let (mut best, mut second) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut centroid = glam::Vec3::ZERO;
    for &(c, normal) in faces {
        let facing = (rot * normal).dot(to_cam);
        if facing > best {
            second = best;
            best = facing;
            centroid = c;
        } else if facing > second {
            second = facing;
        }
    }
    (centroid, best - second)
}

/// A 3×5 pixel font for the ten digits, one `u8` per row (top → bottom), the low
/// three bits a row's pixels with **bit 2 = leftmost column**. Scaled up and
/// blitted so a die's number stays proportional when a wide terminal renders the
/// dice large — otherwise a single fixed cell looks like a speck on a big die.
#[rustfmt::skip]
const DIGIT_FONT: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b010, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];

/// A die's on-screen size as `(width, height)` in cells, from projecting a
/// die-radius offset along the camera's right and up axes. Cells are twice as
/// tall as wide, so width and height are measured separately rather than assumed
/// equal — this is what decides how large the die's number can be drawn.
fn die_screen_extent(
    camera: &tinhorn_core::view_math::Camera,
    center: glam::Vec3,
    cols: f32,
    rows: f32,
) -> (f32, f32) {
    use tinhorn_core::view_math::project_to_cell;
    let r = crate::physics::DIE_R;
    let forward = (camera.target - camera.position).normalize_or_zero();
    let right = forward.cross(camera.up).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    let span = |axis: glam::Vec3| -> f32 {
        match (
            project_to_cell(camera, center - axis * r, cols, rows),
            project_to_cell(camera, center + axis * r, cols, rows),
        ) {
            (Some(a), Some(b)) => (b.0 - a.0).hypot(b.1 - a.1),
            _ => 0.0,
        }
    };
    (span(right), span(up))
}

/// The largest [`DIGIT_FONT`] scale whose glyph fits inside a die's on-screen box
/// of `die_w`×`die_h` cells, for an `n_digits`-digit number — or `0` to fall back
/// to the single-cell overlay when even scale 1 overflows (a small or edge-on
/// die). Terminal cells are twice as tall as wide, so a die spans only ~3–6 rows
/// even when it reads large; the block font renders in **half-blocks** to stay
/// compact, making a glyph `(4·n − 1)·s` cells wide and `⌈5s/2⌉` cells tall.
pub(crate) fn number_scale(die_w: f32, die_h: f32, n_digits: i32) -> i32 {
    let mut scale = 0;
    for s in 1..=4 {
        let w = ((4 * n_digits - 1) * s) as f32;
        let h = ((5 * s + 1) / 2) as f32;
        if w <= die_w && h <= die_h {
            scale = s;
        }
    }
    scale
}

/// One die's number, resolved to everything the two render paths need to place a
/// digit — computed once by [`plan_die_number`] so the cell overlay and the kitty
/// pixel burn can never disagree. `center` is in arena *cell* coordinates with the
/// inner origin NOT folded in (so both painters place from it); `scale` is the
/// shared [`number_scale`] size (0 = single-cell overlay, ≥1 = block glyph);
/// `ink`/`mods` come from [`face_ink`]; `outline` is the die-tinted glyph surround.
/// The scale-0 overlay's dark inset is the shared [`NUMBER_PLATE`].
pub(crate) struct NumberBurn {
    pub(crate) label: String,
    pub(crate) center: (f32, f32),
    pub(crate) scale: i32,
    pub(crate) ink: Color,
    pub(crate) mods: Modifier,
    pub(crate) outline: Color,
}

/// Resolve one die's number for the frame's shared `scale`. Carries the front half
/// of the overlay — [`read_face`] → [`face_ink`] → the anchor projected through
/// [`project_to_cell`] — so both render paths share it verbatim. `None` when the
/// number ducks out (edge-on) or projects behind the eye. Scale 0 anchors on the
/// read-face centroid (a small label rides the top face); ≥1 anchors on the die
/// centre (near-overhead the silhouette *is* the top face, so centring keeps the
/// digits contained rather than sliding off a small facet on a d20).
pub(crate) fn plan_die_number(
    camera: &tinhorn_core::view_math::Camera,
    die: &Die,
    cols: f32,
    rows: f32,
    scale: i32,
) -> Option<NumberBurn> {
    use tinhorn_core::view_math::project_to_cell;
    let to_cam = (camera.position - die.pos).normalize_or_zero();
    let (read_centroid, clarity) = read_face(
        tinhorn_core::dice_geom::face_geometry(die.sides),
        die.rot,
        to_cam,
    );
    let (ink, mods) = face_ink(die, clarity)?;
    let label = die.shown.to_string();

    let center = if scale < 1 {
        let anchor = die.pos + die.rot * (read_centroid * crate::physics::DIE_R);
        project_to_cell(camera, anchor, cols, rows)?
    } else {
        project_to_cell(camera, die.pos, cols, rows)?
    };

    // Outline the number in a dark tint of *this die's* colour rather than generic
    // black, so on a small die the surround still carries the die's hue — that's how
    // you tell which number belongs to which die when the die is mostly hidden.
    let base = if die.kept {
        die_rgb(die.color_idx)
    } else {
        Rgb(120, 120, 120)
    };
    let outline = Color::Rgb(
        (base.0 as f32 * 0.42) as u8,
        (base.1 as f32 * 0.42) as u8,
        (base.2 as f32 * 0.42) as u8,
    );

    Some(NumberBurn {
        label,
        center,
        scale,
        ink,
        mods,
        outline,
    })
}

/// Paint a resolved [`NumberBurn`] into the ratatui buffer (the half-block cell
/// path). Scale 0 is the crisp single centred cell per digit on a dark plate; ≥1
/// composites the outlined [`DIGIT_FONT`] glyph as `▀` half-blocks. `center` is
/// arena-local, so the inner origin is added here.
fn paint_die_number(frame: &mut Frame, inner: Rect, burn: &NumberBurn) {
    let (cx, cy) = burn.center;
    if burn.scale < 1 {
        let label = &burn.label;
        let style = Style::default()
            .bg(NUMBER_PLATE)
            .fg(burn.ink)
            .add_modifier(burn.mods);
        let x = (inner.x as f32 + cx - label.len() as f32 / 2.0).round() as i32;
        let y = (inner.y as f32 + cy).round() as i32;
        let max_x = (inner.right() as i32 - label.len() as i32).max(inner.x as i32);
        let x = x.clamp(inner.x as i32, max_x) as u16;
        let y = y.clamp(inner.y as i32, inner.bottom() as i32 - 1) as u16;
        frame.buffer_mut().set_string(x, y, label, style);
    } else {
        let center = (inner.x as f32 + cx, inner.y as f32 + cy);
        draw_big_number(
            frame,
            inner,
            center,
            &burn.label,
            burn.scale,
            burn.ink,
            burn.outline,
        );
    }
}

/// Plan and paint one die's number into the arena (the cell path). Splitting plan
/// from paint lets the kitty path collect the plans and burn them into pixels
/// instead ([`plan_die_number`] + `burn_numbers`), sharing every placement rule.
fn draw_die_number(
    frame: &mut Frame,
    inner: Rect,
    camera: &tinhorn_core::view_math::Camera,
    die: &Die,
    cols: f32,
    rows: f32,
    scale: i32,
) {
    if let Some(burn) = plan_die_number(camera, die, cols, rows, scale) {
        paint_die_number(frame, inner, &burn);
    }
}

/// One glyph sub-pixel's role in a number raster: a lit font stroke, its dark
/// outline dilation, or clear (the die shows through). Shared by the two render
/// paths — the cell path composites it into `▀` half-blocks ([`draw_big_number`]),
/// the kitty path fills image rects from it (`burn_numbers`).
#[derive(Debug, PartialEq)]
pub(crate) enum GlyphPx {
    Ink,
    Outline,
    Clear,
}

/// The rasteriser for a number `label` at `scale`, resolving each sub-pixel of the
/// [`DIGIT_FONT`] to a [`GlyphPx`]. THE single source of the digit shapes: both
/// render paths ask it the same question ([`GlyphRaster::px`]) so a cell glyph and
/// a burned-in pixel glyph can never differ. The glyph box is
/// [`width_sub`](Self::width_sub)×[`height_sub`](Self::height_sub) sub-pixels; a
/// half-block cell packs two sub-rows.
pub(crate) struct GlyphRaster {
    digits: Vec<u8>,
    scale: i32,
    gw: i32,    // glyph-box width in sub-pixels: n digits × 3 + (n−1) gaps, scaled
    h_sub: i32, // glyph-box height in sub-pixels
}

impl GlyphRaster {
    pub(crate) fn new(label: &str, scale: i32) -> Self {
        let digits: Vec<u8> = label.bytes().filter(u8::is_ascii_digit).collect();
        let n = digits.len() as i32;
        Self {
            digits,
            scale,
            gw: (4 * n - 1) * scale,
            h_sub: 5 * scale,
        }
    }

    /// Glyph-box width in sub-pixels (negative when the label had no digits).
    pub(crate) fn width_sub(&self) -> i32 {
        self.gw
    }

    /// Glyph-box height in sub-pixels.
    pub(crate) fn height_sub(&self) -> i32 {
        self.h_sub
    }

    /// Is sub-pixel `(x cell column, sub row)` a lit font stroke? Off outside the
    /// grid and in the one-column gap between digits.
    fn lit(&self, x: i32, sub: i32) -> bool {
        if x < 0 || sub < 0 || x >= self.gw || sub >= self.h_sub {
            return false;
        }
        let n = self.digits.len() as i32;
        let span = 4 * self.scale; // 3 glyph columns + 1 gap, scaled
        let di = x / span;
        let local_x = x - di * span;
        if di >= n || local_x >= 3 * self.scale {
            return false;
        }
        let (fx, fy) = ((local_x / self.scale) as usize, (sub / self.scale) as usize);
        (DIGIT_FONT[(self.digits[di as usize] - b'0') as usize][fy] >> (2 - fx)) & 1 == 1
    }

    /// Classify sub-pixel `(x, sub)`. The outline is a *tight* one-sub-pixel
    /// dilation of the strokes on every side — enough to separate the number from any
    /// die colour, but thin so the die still shows around and between the digits (that
    /// colour is how you tell dice apart), not a solid tile.
    pub(crate) fn px(&self, x: i32, sub: i32) -> GlyphPx {
        if self.lit(x, sub) {
            GlyphPx::Ink
        } else if (-1..=1).any(|dx| (-1..=1).any(|ds| self.lit(x + dx, sub + ds))) {
            GlyphPx::Outline
        } else {
            GlyphPx::Clear
        }
    }
}

/// Blit `label` as [`DIGIT_FONT`] glyphs centred on cell `center`, scaled `scale`×
/// and drawn in **half-blocks** (two sub-rows per cell) so it stays compact on a
/// short die. Lit strokes are `ink`, sub-pixels *touching* a stroke get `outline`,
/// everything else stays transparent — so the die shows through and the number
/// reads as ink *on the face* rather than a plate over it. Compositing is per
/// sub-pixel via [`GlyphRaster`]: a cell becomes `▀` with its halves coloured
/// independently (ink, outline, or the die pixel already in the buffer). Cells
/// outside `area` clip cleanly.
fn draw_big_number(
    frame: &mut Frame,
    area: Rect,
    center: (f32, f32),
    label: &str,
    scale: i32,
    ink: Color,
    outline: Color,
) {
    let (cx, cy) = center;
    let raster = GlyphRaster::new(label, scale);
    let (gw, h_sub) = (raster.width_sub(), raster.height_sub());
    if gw <= 0 {
        return; // no digits in the label
    }
    let gh = (h_sub + 1) / 2; // cells tall (two sub-rows per cell, last may be half)

    let x0 = (cx - gw as f32 / 2.0).round() as i32;
    let y0 = (cy - gh as f32 / 2.0).round() as i32;
    let buf = frame.buffer_mut();
    // Iterate one cell *beyond* the glyph box on every side: the outline dilates
    // outward, so its top/left/right border lives outside the glyph's own bounds and
    // would go undrawn if the loop stopped at the box edge.
    for row in -1..=gh {
        for col in -1..=gw {
            let (x, y) = (x0 + col, y0 + row);
            if x < area.x as i32
                || x >= area.right() as i32
                || y < area.y as i32
                || y >= area.bottom() as i32
            {
                continue;
            }
            let (up, lo) = (raster.px(col, 2 * row), raster.px(col, 2 * row + 1));
            if up == GlyphPx::Clear && lo == GlyphPx::Clear {
                continue; // leave the die pixel untouched — the number isn't here
            }
            let cell = &mut buf[(x as u16, y as u16)];
            // The die's own sub-pixels are already in the buffer: a HalfBlock `▀`
            // cell holds fg = upper pixel, bg = lower pixel. Clear keeps them.
            let (die_up, die_lo) = (cell.fg, cell.bg);
            let paint = |p: &GlyphPx, die: Color| match p {
                GlyphPx::Ink => ink,
                GlyphPx::Outline => outline,
                GlyphPx::Clear => die,
            };
            cell.set_char('▀');
            cell.set_style(
                Style::default()
                    .fg(paint(&up, die_up))
                    .bg(paint(&lo, die_lo)),
            );
        }
    }
}

/// Rasterise the resolved [`NumberBurn`]s into the packed RGB frame — the kitty
/// path's equivalent of [`draw_big_number`]'s half-block compositing. One glyph
/// sub-pixel maps to `sx × sy` image pixels, derived from the **actual** image dims
/// so a mid-resize frame still burns each digit in the right spot. Ink/outline
/// sub-pixels fill a rect; clear ones leave the die pixels showing through. Scale 0
/// has no half-block glyph, so it rasters at scale 1 with a half-size sub-pixel over
/// a dark [`NUMBER_PLATE`] backing rect. `rgb` is tight (3 bytes/pixel, no row
/// padding); everything clips to the image bounds.
pub(crate) fn burn_numbers(
    rgb: &mut [u8],
    img_w: u32,
    img_h: u32,
    inner_w: u16,
    inner_h: u16,
    burns: &[NumberBurn],
) {
    if img_w == 0 || img_h == 0 || inner_w == 0 || inner_h == 0 {
        return;
    }
    let sx = img_w as f32 / inner_w as f32;
    let sy = img_h as f32 / (inner_h as f32 * 2.0);
    for burn in burns {
        // Scale 0 has no block glyph: raster at scale 1 with a half-size sub-pixel
        // over a plate, so a small crisp number still rides the die.
        let (scale, sub_w, sub_h) = if burn.scale < 1 {
            (1, sx * 0.5, sy * 0.5)
        } else {
            (burn.scale, sx, sy)
        };
        let raster = GlyphRaster::new(&burn.label, scale);
        let (gw, h_sub) = (raster.width_sub(), raster.height_sub());
        if gw <= 0 {
            continue;
        }
        let (glyph_w, glyph_h) = (gw as f32 * sub_w, h_sub as f32 * sub_h);
        // Centre on the die: a cell's x maps to image x as `× sx`, its y as `× 2·sy`
        // (a cell is two half-block sub-rows tall).
        let x_left = burn.center.0 * sx - glyph_w * 0.5;
        let y_top = burn.center.1 * 2.0 * sy - glyph_h * 0.5;

        let ink = ink_rgb(burn.ink);
        let outline = ink_rgb(burn.outline);
        if burn.scale < 1 {
            // A dark backing plate a sub-pixel proud of the digits on every side, so
            // the tiny number reads over any die colour.
            fill_px_rect(
                rgb,
                img_w,
                img_h,
                (
                    x_left - sub_w,
                    y_top - sub_h,
                    glyph_w + 2.0 * sub_w,
                    glyph_h + 2.0 * sub_h,
                ),
                ink_rgb(NUMBER_PLATE),
            );
        }
        // Iterate one sub-pixel *beyond* the glyph box on every side, exactly as the
        // half-block path does: the outline dilates outward, so its ring lives outside
        // the box and would go unburned if the loop stopped at the edge — leaving kitty
        // digits without the halo the cell path draws (they must never differ).
        for s in -1..=h_sub {
            for c in -1..=gw {
                let color = match raster.px(c, s) {
                    GlyphPx::Ink => ink,
                    GlyphPx::Outline => outline,
                    GlyphPx::Clear => continue,
                };
                fill_px_rect(
                    rgb,
                    img_w,
                    img_h,
                    (
                        x_left + c as f32 * sub_w,
                        y_top + s as f32 * sub_h,
                        sub_w,
                        sub_h,
                    ),
                    color,
                );
            }
        }
    }
}

/// Fill an axis-aligned rect `(x, y, w, h)` of the tight RGB image with `color`.
/// Edges round so adjacent sub-pixel cells tile without gaps or overlap; clipped to
/// the image.
fn fill_px_rect(
    rgb: &mut [u8],
    img_w: u32,
    img_h: u32,
    rect: (f32, f32, f32, f32),
    color: (u8, u8, u8),
) {
    let (x, y, w, h) = rect;
    let x0 = (x.round() as i64).clamp(0, img_w as i64);
    let x1 = ((x + w).round() as i64).clamp(0, img_w as i64);
    let y0 = (y.round() as i64).clamp(0, img_h as i64);
    let y1 = ((y + h).round() as i64).clamp(0, img_h as i64);
    for py in y0..y1 {
        for px in x0..x1 {
            let i = (py as usize * img_w as usize + px as usize) * 3;
            if i + 2 < rgb.len() {
                rgb[i] = color.0;
                rgb[i + 1] = color.1;
                rgb[i + 2] = color.2;
            }
        }
    }
}

/// The RGB a number's ink / outline / plate colour burns as: the named colours
/// [`face_ink`] produces mapped to vivid tones (the kitty image is real colour, not
/// a 16-colour cell), and `Rgb` passed straight through.
fn ink_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::White => (236, 236, 236),
        Color::Yellow => (248, 216, 60),
        Color::Red => (232, 72, 72),
        Color::DarkGray => (120, 120, 120),
        _ => (236, 236, 236),
    }
}

/// The arena: the actual roll as tumbling polyhedra, rendered from the Bevy scene.
/// Each die spins airborne and freezes on settle, when its RNG-decided value is
/// "burned" onto the face pointing at you. Position comes from the sim; the values
/// and total are untouched — the renderer only shows them off.
fn arena_title(app: &App) -> String {
    if app.shaking() {
        " 🎲  tinhorn — shaking… ".to_string()
    } else if app.all_settled() {
        " 🎲  tinhorn — settled ".to_string()
    } else if app.spawned {
        // Name the throw for what it was; a plain Tab roll just "rolls".
        match app.last_throw.map(|t| throw_tier(t.power)) {
            Some(ThrowTier::Lob) => " 🎲  tinhorn — a timid lob… ".to_string(),
            Some(ThrowTier::Toss) => " 🎲  tinhorn — a clean toss… ".to_string(),
            Some(ThrowTier::Rocket | ThrowTier::Peak) => {
                " 🎲  tinhorn — a rocket throw… ".to_string()
            }
            None => " 🎲  tinhorn — rolling… ".to_string(),
        }
    } else {
        " 🎲  tinhorn ".to_string()
    }
}

/// The 2D ceremony that rides on top of the rendered arena: a burned number on
/// every die (riding the face that points at us; skipped while shaking, dice in the
/// cup), crit/fumble particles, the shake power meter, the release echo, and the
/// idle hint.
///
/// The die numbers are the one overlay coupled to the render model: in **Blocks**
/// mode they composite into the cell buffer here; in **Kitty** mode they're planned
/// and *returned* for the scene to burn into the pixels. The shake gate and shared
/// `number_scale` sizing live here so both modes size the digits identically.
/// Everything else — particles, meter, echo, hint — is pure cell chrome, the same
/// in both.
fn draw_arena_overlays(
    frame: &mut Frame,
    app: &App,
    inner: Rect,
    camera: &tinhorn_core::view_math::Camera,
    mode: GraphicsMode,
) -> Vec<NumberBurn> {
    let mut burns = Vec::new();
    if !app.shaking() {
        let (cols, rows) = (inner.width as f32, inner.height as f32);
        // One number size for the whole roll, so the dice read the same — never a big
        // number on the nearest die and single cells on the rest. Size it from a
        // reference die at the felt centre, fit within the read-face, and reserve room
        // for the widest value any die can show, so a two-digit d20 lands at the same
        // scale a one-digit d6 uses.
        let ref_center = glam::Vec3::new(0.0, -crate::physics::HY + crate::physics::DIE_R, 0.0);
        let (ref_w, ref_h) = die_screen_extent(camera, ref_center, cols, rows);
        let max_digits = app
            .dice
            .iter()
            .map(|d| d.sides.to_string().len() as i32)
            .max()
            .unwrap_or(1);
        let num_scale = number_scale(ref_w * FACE_FRAC_W, ref_h * FACE_FRAC_H, max_digits);
        for die in &app.dice {
            match mode {
                GraphicsMode::Blocks => {
                    draw_die_number(frame, inner, camera, die, cols, rows, num_scale)
                }
                GraphicsMode::Kitty { .. } => {
                    if let Some(burn) = plan_die_number(camera, die, cols, rows, num_scale) {
                        burns.push(burn);
                    }
                }
            }
        }
    }

    {
        let buf = frame.buffer_mut();
        for p in &app.particles {
            draw_particle(buf, inner, p);
        }
        if app.shaking() {
            draw_power_meter(buf, inner, app);
        }
        if let Some(throw) = app.release_echo() {
            draw_release_echo(buf, inner, throw);
        }
    }

    // Truly idle (nothing rolled, no shake in progress): a gentle hint over the felt.
    if app.dice.is_empty() && !app.shaking() && app.release_echo().is_none() {
        let hint = Paragraph::new(Line::from(
            " roll something — the dice tumble in 3D ".dark_gray(),
        ))
        .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
    }

    burns
}

/// The composed arena's report to the scene: the render-target size to request,
/// and — in kitty mode only — the panel geometry and the per-die number burns the
/// scene will rasterise into the pixels and place. `kitty` is `None` in half-block
/// mode, where the numbers are already composited into the frame.
pub struct ArenaReport {
    pub view: (u16, u16),
    pub kitty: Option<KittyPanel>,
}

/// Where the kitty image lands and what to burn into it: the arena inner `Rect`
/// (origin + cell size, so the scene knows where to place the image and how the
/// burn maps cells to pixels) and the resolved [`NumberBurn`]s (empty while
/// shaking, when the dice are gathered in the cup).
pub struct KittyPanel {
    pub inner: Rect,
    pub burns: Vec<NumberBurn>,
}

/// Compose the full interactive frame for a given [`GraphicsMode`]: a four-row
/// layout (arena, result panel, input line, help bar) with all of tinhorn's
/// chrome. The arena panel is the only branch between modes:
///
/// - **Blocks** — the CPU-read Bevy render blitted as **quadrant** glyphs (2×2
///   sub-pixels per cell, twice a half-block's resolution), supersampled `QUAD_SS×`
///   and box-downsampled; the die numbers composite on top. `pixels` is the
///   row-padded RGBA8 readback of an `img_w`×`img_h` image.
/// - **Kitty** — the arena cells are cleared to default-bg (the scene places a real
///   image behind them at native `scale`× — no SSAA, 4× MSAA already smooths edges)
///   and the die numbers are returned as burns for the scene to rasterise. `pixels`
///   is unused (the scene owns the readback).
///
/// The cell-space overlays (particles, meter, echo, hint) draw identically either
/// way. Returns the render-target size and, in kitty mode, the panel + burns.
pub fn render_bevy_mode(
    frame: &mut Frame,
    app: &mut App,
    pixels: &[u8],
    img_w: u32,
    img_h: u32,
    mode: GraphicsMode,
) -> ArenaReport {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let arena_area = chunks[0];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(arena_title(app).bold());
    let inner = block.inner(arena_area);
    frame.render_widget(block, arena_area);

    let mut view = (0u16, 0u16);
    let mut kitty = None;
    if inner.width >= 4 && inner.height >= 3 {
        // Feed the arena size to the sim (launch/particle geometry reads it) and
        // report it back so the render target can track it.
        app.arena_w = inner.width as f32;
        app.arena_h = inner.height as f32;

        // The one mode branch: how the arena panel is filled and how big a render
        // the target should be. The "cols × 2·rows" image shape is load-bearing —
        // kitty only raises the scale, so `arena_aspect`/`project_to_cell` stay the
        // single source of the framing.
        match mode {
            GraphicsMode::Blocks => {
                // Supersample (QUAD_SS× the quadrant sub-pixel grid); the blit
                // box-downsamples it into 2×2-per-cell quadrant glyphs — twice the
                // resolution of half-blocks, and the same "cols × 2·rows" aspect.
                view = (
                    inner.width * QUAD_SS as u16,
                    inner.height * 2 * QUAD_SS as u16,
                );
                quadrant_blit(frame.buffer_mut(), inner, pixels, img_w, img_h);
            }
            GraphicsMode::Kitty { scale } => {
                let s = kitty_scale(scale, inner.width);
                view = (inner.width * s as u16, inner.height * 2 * s as u16);
                clear_arena(frame.buffer_mut(), inner);
            }
        }

        let aspect = tinhorn_core::view_math::arena_aspect(inner.width as f32, inner.height as f32);
        let camera = tinhorn_core::view_math::live_camera(
            app.camera_shake(),
            aspect,
            app.focus(),
            app.clock(),
            app.flash(),
        );
        let burns = draw_arena_overlays(frame, app, inner, &camera, mode);
        if matches!(mode, GraphicsMode::Kitty { .. }) {
            kitty = Some(KittyPanel { inner, burns });
        }
    }

    render_results(frame, app, chunks[1]);
    render_input(frame, app, chunks[2]);
    render_help(frame, app, chunks[3]);

    let scroll = app.pane_scroll;
    match app.pane {
        Pane::None => {}
        Pane::Help => app.pane_scroll = render_help_overlay(frame, area, scroll),
        Pane::History => app.pane_scroll = render_history_overlay(frame, app, area, scroll),
        Pane::Stats => app.pane_scroll = render_stats_overlay(frame, app, area, scroll),
    }

    ArenaReport { view, kitty }
}

/// Compose the interactive frame in blocks mode (the universal quadrant-glyph
/// fallback). Kept at its exact original signature so the ≈30 chrome tests and the
/// three `#[ignore]` GPU tests compile untouched; returns just the render size.
pub fn render_bevy(
    frame: &mut Frame,
    app: &mut App,
    pixels: &[u8],
    img_w: u32,
    img_h: u32,
) -> (u16, u16) {
    render_bevy_mode(frame, app, pixels, img_w, img_h, GraphicsMode::Blocks).view
}

/// Knock the requested kitty scale down until the transmitted image width stays
/// under [`MAX_IMG_W`](crate::graphics::MAX_IMG_W), so a fullscreen hi-DPI arena
/// can't ask for a multi-thousand-pixel frame every frame (readback + encode is the
/// frame-budget bottleneck). Never below 1.
fn kitty_scale(scale: u32, inner_w: u16) -> u32 {
    let cap = (crate::graphics::MAX_IMG_W / (inner_w.max(1) as u32)).max(1);
    scale.min(cap).max(1)
}

/// Clear the arena inner cells to blank default-bg cells. In kitty mode the scene
/// places a real image behind them (a deep negative z), so the felt shows through
/// the terminal's default background and the chrome/overlays draw on top.
fn clear_arena(buf: &mut ratatui::buffer::Buffer, inner: Rect) {
    for y in inner.top()..inner.bottom() {
        for x in inner.left()..inner.right() {
            buf[(x, y)].reset();
        }
    }
}

/// Supersample factor: the Bevy arena is rendered at this multiple of the arena's
/// **quadrant** sub-pixel grid (`cols*2 × rows*2`) and box-downsampled in the blit,
/// for cheap anti-aliasing. 4 gives a 2×4 render block per sub-pixel — square-ish,
/// since a quadrant sub-pixel is ~1:2 (tall).
const QUAD_SS: u32 = 4;

/// The sixteen Unicode quadrant block glyphs, indexed by a 4-bit sub-pixel mask
/// (bit 0 = top-left, 1 = top-right, 2 = bottom-left, 3 = bottom-right) of which
/// sub-pixels are the foreground colour. These carve each cell into a 2×2 grid —
/// **double the resolution of a half-block** — and live in Unicode's block-elements
/// range, so every monospace font has them (unlike the newer sextants).
const QUAD_GLYPHS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// The 4-bit sub-pixel mask of a quadrant glyph (see [`QUAD_GLYPHS`]), or `None` if
/// `ch` isn't one — lets the headless snapshot re-expand each arena cell back into
/// its 2×2 pixels. A plain space maps to `0` (all background), so blank cells count
/// as arena glyphs and get blanked in the text dump.
pub(crate) fn quad_pattern(ch: char) -> Option<u8> {
    QUAD_GLYPHS.iter().position(|&g| g == ch).map(|i| i as u8)
}

/// Fit a cell's four sub-pixels (`[TL, TR, BL, BR]`) to a 2-colour quadrant glyph:
/// split them on the colour channel with the widest spread, average each side into
/// `fg`/`bg`, and pick the glyph whose lit quadrants are the `fg` side. This is how
/// `chafa`/`notcurses` pack more than two colours into a cell — 2× a half-block's
/// resolution, on glyphs every monospace font carries.
fn quad_cell(sub: [(u8, u8, u8); 4]) -> (char, Color, Color) {
    let chan = |c: (u8, u8, u8), k: usize| [c.0, c.1, c.2][k] as i32;
    // Split on the channel with the widest spread across the four sub-pixels.
    let (mut axis, mut spread, mut thresh) = (0usize, -1i32, 0i32);
    for k in 0..3 {
        let (mut lo, mut hi) = (255i32, 0i32);
        for &c in &sub {
            lo = lo.min(chan(c, k));
            hi = hi.max(chan(c, k));
        }
        if hi - lo > spread {
            (spread, axis, thresh) = (hi - lo, k, (lo + hi) / 2);
        }
    }
    let (mut mask, mut f, mut fg_n, mut b, mut bg_n) = (0u8, [0u32; 3], 0u32, [0u32; 3], 0u32);
    for (i, &c) in sub.iter().enumerate() {
        if chan(c, axis) >= thresh {
            mask |= 1 << i;
            (f[0], f[1], f[2], fg_n) = (
                f[0] + c.0 as u32,
                f[1] + c.1 as u32,
                f[2] + c.2 as u32,
                fg_n + 1,
            );
        } else {
            (b[0], b[1], b[2], bg_n) = (
                b[0] + c.0 as u32,
                b[1] + c.1 as u32,
                b[2] + c.2 as u32,
                bg_n + 1,
            );
        }
    }
    let avg =
        |s: [u32; 3], n: u32| Color::Rgb((s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8);
    let fg = if fg_n > 0 { avg(f, fg_n) } else { Color::Reset };
    let bg = if bg_n > 0 { avg(b, bg_n) } else { fg };
    (QUAD_GLYPHS[mask as usize], fg, bg)
}

/// The warm radial vignette applied to the arena: darkens toward the corners and
/// warms the tint a touch (a whisper more red, a whisper less blue). Returns the
/// per-channel multipliers `(fr, fg, fb)` for a normalised offset-from-centre
/// `(nx, ny)` in `[-0.5, 0.5]`. Shared by the half-block blit and the kitty pixel
/// pack (`graphics::pack_rgb`) so both paths grade the picture identically.
pub(crate) fn vignette(nx: f32, ny: f32) -> (f32, f32, f32) {
    let d2 = ((nx * nx + ny * ny) / 0.5).min(1.0);
    let f = 1.0 - 0.34 * d2 * d2;
    (f * 1.04, f, f * 0.95)
}

/// Blit a row-padded RGBA8 Bevy render into `inner` as **quadrant** cells: each
/// cell is a 2×2 grid of sub-pixels (`cols*2 × rows*2` overall — double a
/// half-block's resolution), box-downsampled from the `QUAD_SS×` supersampled
/// render, warm-graded by the radial vignette, and fitted to a 2-colour quadrant
/// glyph ([`quad_cell`]). A no-op until the first readback of the right size lands.
fn quadrant_blit(
    buf: &mut ratatui::buffer::Buffer,
    inner: Rect,
    pixels: &[u8],
    img_w: u32,
    img_h: u32,
) {
    let stride = crate::graphics::readback_stride(img_w); // wgpu 256-byte row pad
    if img_w == 0 || img_h == 0 || pixels.len() < stride * img_h as usize {
        return;
    }
    // Render pixels per sub-pixel in each axis: the sub-pixel grid is 2× the cell
    // grid on both axes, so `ssx`/`ssy` are `QUAD_SS/2` and `QUAD_SS` respectively.
    let (subw, subh) = (inner.width as u32 * 2, inner.height as u32 * 2);
    let ssx = (img_w / subw.max(1)).max(1);
    let ssy = (img_h / subh.max(1)).max(1);
    let (fw, fh) = (subw as f32, subh as f32);
    // Box-average the `ssx`×`ssy` render block for sub-pixel `(sx, sy)`, warm-graded.
    let subpx = |sx: u32, sy: u32| -> (u8, u8, u8) {
        let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
        for dy in 0..ssy {
            for dx in 0..ssx {
                let px = (sx * ssx + dx).min(img_w - 1) as usize;
                let py = (sy * ssy + dy).min(img_h - 1) as usize;
                let i = py * stride + px * 4;
                r += pixels[i] as u32;
                g += pixels[i + 1] as u32;
                b += pixels[i + 2] as u32;
            }
        }
        let n = (ssx * ssy).max(1);
        let (fr, fg, fb) = vignette((sx as f32 + 0.5) / fw - 0.5, (sy as f32 + 0.5) / fh - 0.5);
        (
            ((r / n) as f32 * fr).min(255.0) as u8,
            ((g / n) as f32 * fg).min(255.0) as u8,
            ((b / n) as f32 * fb).min(255.0) as u8,
        )
    };
    for row in 0..inner.height {
        for col in 0..inner.width {
            let (sx, sy) = (col as u32 * 2, row as u32 * 2);
            let (ch, fg, bg) = quad_cell([
                subpx(sx, sy),         // TL
                subpx(sx + 1, sy),     // TR
                subpx(sx, sy + 1),     // BL
                subpx(sx + 1, sy + 1), // BR
            ]);
            let cell = &mut buf[(inner.x + col, inner.y + row)];
            cell.set_char(ch);
            cell.set_style(Style::default().fg(fg).bg(bg));
        }
    }
}

/// Width of the power meter in cells — the live one in the cup and the frozen
/// release echo, which must stay pixel-identical to the meter it echoes.
const METER_WIDTH: usize = 14;

/// The meter itself: `power` as filled-vs-empty cells.
fn power_bar(power: f32) -> String {
    let filled = ((power * METER_WIDTH as f32).round() as usize).min(METER_WIDTH);
    "▓".repeat(filled) + &"░".repeat(METER_WIDTH - filled)
}

/// The frozen caught-power readout shown right after a release: the meter as
/// it was the instant you let go, graded and named. Fades before it vanishes.
fn draw_release_echo(buf: &mut ratatui::buffer::Buffer, inner: Rect, throw: crate::app::Throw) {
    let (word, color) = release_grade(throw.power);
    let label = format!("caught {} {word}", power_bar(throw.power));
    let w = label.chars().count() as u16;
    if inner.width < w || inner.height < 2 {
        return;
    }
    let x = inner.x + (inner.width - w) / 2;
    // Old (dying) echo dims; a fresh one is bold.
    let mut style = Style::default().fg(color);
    if throw.age > 0.9 {
        style = style.add_modifier(Modifier::DIM);
    } else {
        style = style.add_modifier(Modifier::BOLD);
    }
    buf.set_string(x, inner.y, label, style);
}

/// How the release reads on the meter: the wording of the catch. The peak is
/// the prize; a lob is its own reward.
fn release_grade(power: f32) -> (&'static str, Color) {
    match throw_tier(power) {
        ThrowTier::Peak => ("— the peak!", Color::Yellow),
        ThrowTier::Rocket => ("— a rocket", Color::Red),
        ThrowTier::Toss => ("— a clean toss", Color::Cyan),
        ThrowTier::Lob => ("— a timid lob", Color::DarkGray),
    }
}

/// One celebration glyph: gold sparks for a crit, grey dust for a fumble,
/// dimming as it dies.
fn draw_particle(buf: &mut ratatui::buffer::Buffer, inner: Rect, p: &Particle) {
    let x = inner.x as i32 + p.x.round() as i32;
    let y = inner.y as i32 + p.y.round() as i32;
    if x < inner.x as i32
        || x >= inner.right() as i32
        || y < inner.y as i32
        || y >= inner.bottom() as i32
    {
        return;
    }
    let mut style = Style::default().fg(if p.bright {
        Color::Yellow
    } else {
        Color::DarkGray
    });
    if p.fade() > 0.55 {
        style = style.add_modifier(Modifier::DIM);
    } else if p.bright {
        style = style.add_modifier(Modifier::BOLD);
    }
    buf.set_string(x as u16, y as u16, p.glyph.to_string(), style);
}

/// The throw's power meter, drawn while shaking (the Throw). The cup is a real 3D
/// tumbler in the arena scene (`tinhorn_core::dice_geom::cup`); this is just the
/// read-out above it — fixed and centred so your eye can time the release against
/// it, colour-coding the power you'd catch right now.
fn draw_power_meter(buf: &mut ratatui::buffer::Buffer, inner: Rect, app: &App) {
    let power = app.power();

    // Skipped when it wouldn't fit — it must never spill over the arena border.
    if inner.height >= 5 {
        let label = format!("power {} throw ↵", power_bar(power));
        let w = label.chars().count() as u16;
        if inner.width < w {
            return;
        }
        let x = inner.x + (inner.width - w) / 2;
        let y = inner.bottom() - 4;
        let bar_color = if power < 0.5 {
            Color::Green
        } else if power < 0.85 {
            Color::Yellow
        } else {
            Color::Red
        };
        buf.set_string(x, y, label, Style::default().fg(bar_color));
    }
}

fn render_results(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" result ".bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Error state takes over the panel.
    if let Some(err) = &app.error {
        let p = Paragraph::new(Line::from(vec![
            Span::styled("⚠ ", Style::default().fg(Color::Red)),
            Span::styled(err.clone(), Style::default().fg(Color::Red)),
        ]));
        frame.render_widget(p, inner);
        return;
    }

    if app.dice.is_empty() {
        let p = Paragraph::new(Span::styled(
            "type a dice expression below — Enter does the rest",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(p, inner);
        return;
    }

    let settled = app.all_settled();

    // Line 1: one chip per die. Each locks in when *its* die comes to rest —
    // colourless and dim while tumbling (the face on show is a flickering decoy),
    // then bold in the die's colour once it settles. A dropped die stays greyed.
    let mut chips: Vec<Span> = Vec::new();
    for (i, die) in app.dice.iter().enumerate() {
        if i > 0 {
            chips.push(Span::raw(" "));
        }
        // `shown` IS the display rule: a decoy until this die settles, then the
        // burned-in final value — the chip never needs to restate the burn.
        let val = die.shown;
        let style = if !die.kept {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        } else if die.settled {
            Style::default()
                .fg(die_color(die.color_idx))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        chips.push(Span::styled(format!("[{val}]"), style));
    }
    if app.modifier != 0 {
        let sign = if app.modifier > 0 { "+" } else { "−" };
        chips.push(Span::styled(
            format!("  {sign}{}", app.modifier.abs()),
            Style::default().fg(Color::Gray),
        ));
    }

    // Line 2: the total — held back as a dim "…" until every die has landed, so
    // no value is given away before its die stops. When the roll is staked, the
    // verdict slams down beside it.
    let (total_label, total_style) = if settled {
        (
            format!(" {} ", app.total()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            " … ".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )
    };
    let mut total_spans = vec![
        Span::styled("  Σ total ", Style::default().fg(Color::Gray)),
        Span::styled(total_label, total_style),
        Span::raw("   "),
    ];

    match (app.stake, app.verdict()) {
        // Settled with stakes: slam the verdict (the wording — and the stake's
        // `vs`/`vs ≤` label — is shared with the CLI's verbose breakdown so the
        // two can't drift).
        (Some(stake), Some((success, margin))) => {
            let bg = if success { Color::Green } else { Color::Red };
            total_spans.push(Span::styled(
                format!("{}  ", stake.label()),
                Style::default().fg(Color::Gray),
            ));
            total_spans.push(Span::styled(
                format!(" {} ", crate::app::verdict_text(success, margin)),
                Style::default()
                    .fg(Color::Black)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        // Stakes declared, dice still falling: show what's at stake.
        (Some(stake), None) => {
            total_spans.push(Span::styled(
                format!("{}  (rolling…)", stake.label()),
                Style::default().fg(Color::DarkGray),
            ));
        }
        _ => {
            total_spans.push(Span::styled(
                if settled { "" } else { "(rolling…)" },
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    // Maxed dice and 1s get named, whatever the stakes. A d20's own crit
    // keeps its beloved name; anything else is a crit on its merits.
    if settled {
        let crits = app.crit_dice().count();
        if crits > 0 {
            let all_d20 = app.crit_dice().all(|d| d.sides == 20);
            let mut label = if all_d20 {
                "  ✦ natural 20".to_string()
            } else {
                "  ✦ crit".to_string()
            };
            if crits > 1 {
                label.push_str(&format!(" ×{crits}"));
            }
            total_spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let fumbles = app.fumble_dice().count();
        if fumbles > 0 {
            let all_d20 = app.fumble_dice().all(|d| d.sides == 20);
            let mut label = if all_d20 {
                "  natural 1".to_string()
            } else {
                "  fumble".to_string()
            };
            if fumbles > 1 {
                label.push_str(&format!(" ×{fumbles}"));
            }
            total_spans.push(Span::styled(
                label,
                Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
            ));
        }
    }

    let p = Paragraph::new(vec![Line::from(chips), Line::from(total_spans)]);
    frame.render_widget(p, inner);
}

fn render_help(frame: &mut Frame, app: &App, area: Rect) {
    let key = |k| Span::styled(k, Style::default().fg(Color::Cyan).bold());
    let help = if app.shaking() {
        // Mid-shake the bar narrows to the only choices that exist.
        Line::from(vec![
            Span::styled(" › shaking…  ", Style::default().fg(Color::Yellow)),
            key("Enter"),
            Span::raw(" · "),
            key("Tab"),
            Span::raw(" throw  "),
            key("Esc"),
            Span::raw(" put them down"),
        ])
    } else {
        let mut spans = vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            key("Enter"),
            Span::raw(format!(" {}  ", app.mode.label())),
            key("Tab"),
            Span::raw(" mode  "),
            key("?"),
            Span::raw(" help  "),
            key("^H"),
            Span::raw(" history  "),
            key("^S"),
            Span::raw(" stats  "),
        ];
        spans.push(key("^Q"));
        spans.push(Span::raw(if app.muted {
            " unmute 🔇  "
        } else {
            " mute  "
        }));
        spans.push(key("Esc"));
        spans.push(Span::raw(" quit"));
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(help), area);
}

/// One row of the syntax table in the help overlay: an example on the left and
/// its meaning on the right.
fn syntax_row<'a>(example: &'a str, meaning: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{example:<11}"), Style::default().fg(Color::Cyan)),
        Span::styled(meaning, Style::default().fg(Color::Gray)),
    ])
}

/// A bold yellow section heading line for the overlays.
fn heading(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

/// The italic dim footer shared by every pane: how to scroll it and close it.
fn close_hint() -> Line<'static> {
    Line::from(Span::styled(
        "  ↑ ↓ scroll · Esc · q to close",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ))
}

/// Draw a centred, bordered panel of `lines` titled `title` over the UI. Sizes
/// itself to its content (capped to the frame), blanks what's behind it, and scrolls
/// by `scroll` lines when the content overflows. Returns the scroll offset actually
/// used — clamped so the last line can just reach the bottom — so the caller can
/// store the corrected value back. Shared by all three pop-out panes.
fn overlay_panel(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line>, scroll: u16) -> u16 {
    let content_h = lines.len() as u16;
    let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let panel_w = (inner_w + 4).min(area.width); // +4 for borders + side padding
    let panel_h = (content_h + 2).min(area.height); // +2 for top/bottom border

    let rect = centered(panel_w, panel_h, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::horizontal(1))
        .title(title.to_string().bold());

    // When the content overflows the inner height, scroll within it; the block
    // clips whatever falls outside. Clamp so scrolling can't run off the end.
    let inner_h = block.inner(rect).height;
    let scroll = scroll.min(content_h.saturating_sub(inner_h));

    let para = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Left)
        .scroll((scroll, 0));
    frame.render_widget(Clear, rect); // blank whatever's behind the panel
    frame.render_widget(para, rect);
    scroll
}

/// The dice-notation reference, drawn over the UI when `?` is pressed.
fn render_help_overlay(frame: &mut Frame, area: Rect, scroll: u16) -> u16 {
    let lines = vec![
        heading("Dice"),
        syntax_row("3d6", "three six-sided dice"),
        syntax_row("d20", "one die (count defaults to 1)"),
        syntax_row("d6+d8", "combine different dice"),
        syntax_row("2d20-1", "add or subtract a flat modifier"),
        Line::raw(""),
        heading("Keep / drop"),
        syntax_row("2d20kh1", "advantage — keep the highest 1"),
        syntax_row("2d20kl1", "disadvantage — keep the lowest 1"),
        syntax_row("4d6dl1", "drop the lowest 1 (ability scores)"),
        syntax_row("4d6dh1", "drop the highest 1"),
        Line::raw(""),
        heading("Explode & reroll"),
        syntax_row("3d6!", "a max face rolls another die"),
        syntax_row("d10!>8", "explode on any face over 8 (also !=N, !<N)"),
        syntax_row("4d6r1", "reroll 1s until they clear (ro1 = once)"),
        Line::raw(""),
        heading("Stakes & multiply"),
        syntax_row("d20 > 15", "beat a target (or 'vs'); < N rolls under"),
        syntax_row("4d6*2", "double this term's sum (modifiers stack)"),
        Line::raw(""),
        heading("The Throw"),
        syntax_row(
            "Enter",
            "shake the cup; Enter again throws — harder at the peak",
        ),
        syntax_row("Tab", "cycle Enter's mode: shake → roll → insta"),
        syntax_row("Esc", "put them down. Power never touches the values."),
        close_hint(),
    ];
    overlay_panel(frame, area, " 🎲  dice notation ", lines, scroll)
}

/// The roll-history pane: recent rolls, newest first.
fn render_history_overlay(frame: &mut Frame, app: &App, area: Rect, scroll: u16) -> u16 {
    let mut lines: Vec<Line> = Vec::new();

    if app.history.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no rolls yet — shake some dice loose with Enter",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Newest first; the whole list is laid out and the pane scrolls (↑/↓)
        // when it overflows the frame, so older rolls stay reachable.
        for (n, e) in app.history.iter().rev().enumerate() {
            let idx = app.history.len() - n; // 1-based, counting from the newest
            let faces = e
                .values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {idx:>3}. "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("{:<12}", e.expr), Style::default().fg(Color::Cyan)),
                Span::styled(format!("[{faces}]  "), Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("= {}", e.total),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }

    lines.push(Line::raw(""));
    lines.push(close_hint());
    overlay_panel(frame, area, " 🎲  history ", lines, scroll)
}

/// The statistics pane: theoretical odds for the current expression plus a
/// summary of the rolls actually made this session.
fn render_stats_overlay(frame: &mut Frame, app: &mut App, area: Rect, scroll: u16) -> u16 {
    let lines = match app.stats() {
        Ok(s) => stats_lines(&s),
        Err(e) => vec![
            Line::from(Span::styled(
                "  can't compute stats — the expression doesn't parse:",
                Style::default().fg(Color::Red),
            )),
            Line::from(Span::styled(
                format!("  {e}"),
                Style::default().fg(Color::Red),
            )),
            Line::raw(""),
            close_hint(),
        ],
    };
    overlay_panel(frame, area, " 🎲  statistics ", lines, scroll)
}

/// Lay out the statistics into display lines: a header, the theoretical
/// min/max/mean, a small distribution curve, and the session summary.
fn stats_lines(s: &Stats) -> Vec<Line<'static>> {
    let mut lines = vec![
        heading(format!("Odds for  {}", s.expr)),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("min {}   max {}   avg {:.1}", s.min, s.max, s.mean),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(Span::styled(
            format!("  (estimated from {} samples)", s.samples),
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
    ];

    // A staked expression leads with what matters: the odds of making it.
    if let (Some(stake), Some(odds)) = (s.stake, s.success_odds) {
        lines.insert(
            1,
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{}: ", stake.label()),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(
                    format!("{:.0}% to succeed", odds * 100.0),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        );
    }

    // A little horizontal distribution: one bar per bucket, scaled to the peak.
    if !s.dist.is_empty() {
        let peak = s
            .dist
            .iter()
            .map(|b| b.fraction)
            .fold(0.0_f64, f64::max)
            .max(1e-9);
        for b in &s.dist {
            let filled = (b.fraction / peak * 18.0).round() as usize;
            let bar: String = "█".repeat(filled);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:>4} ", b.total),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(bar, Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!(" {:>4.1}%", b.fraction * 100.0),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // Session summary for this exact expression.
    lines.push(heading("This session"));
    if s.session.count == 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "  no rolls of {} yet ({} rolls total)",
                s.expr, s.total_rolls
            ),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} rolls   low {}   high {}   mean {:.1}",
                s.session.count, s.session.min, s.session.max, s.session.mean
            ),
            Style::default().fg(Color::Gray),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(close_hint());
    lines
}

/// A `w`×`h` rectangle centred inside `area` (clamped to fit).
fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(h)])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(w)])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// The editable dice expression: a fixed prompt, then the expression with a block
/// caret (reverse-video over the character it covers, a solid block at line end).
/// The expression scrolls horizontally to keep the caret in view, so mid-line
/// editing never runs off-screen. Every span borrows `app.input`, so drawing
/// allocates nothing.
fn render_input(frame: &mut Frame, app: &App, area: Rect) {
    const PROMPT: &str = "dice ▸ ";
    let [prompt_area, text_area] = Layout::horizontal([
        Constraint::Length(PROMPT.chars().count() as u16),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            PROMPT,
            Style::default().fg(Color::Cyan).bold(),
        )),
        prompt_area,
    );

    let at = app.cursor_byte();
    let (before, rest) = app.input.split_at(at);
    // The cell under the caret: the character it covers, or a blank at line end.
    let (under, after) = match rest.chars().next() {
        Some(c) => (&rest[..c.len_utf8()], &rest[c.len_utf8()..]),
        None => (" ", ""),
    };

    // Scroll so the caret column stays inside the text area — pinned to the
    // right edge once the expression overflows it.
    let caret_col = Span::raw(before).width() as u16;
    let scroll_x = caret_col.saturating_sub(text_area.width.saturating_sub(1));
    let line = Line::from(vec![
        Span::raw(before),
        Span::styled(under, Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::raw(after),
    ]);
    frame.render_widget(Paragraph::new(line).scroll((0, scroll_x)), text_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::graphics::GraphicsMode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};

    /// Every glyph in the composed frame as one flat string, for content asserts.
    fn flatten(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// Draw one frame in `mode`, returning the arena report.
    fn draw_mode(
        terminal: &mut Terminal<TestBackend>,
        app: &mut App,
        mode: GraphicsMode,
    ) -> ArenaReport {
        let mut report = None;
        terminal
            .draw(|f| report = Some(render_bevy_mode(f, app, &[], 0, 0, mode)))
            .unwrap();
        report.unwrap()
    }

    #[test]
    fn quad_pattern_round_trips_the_glyph_table() {
        // Every quadrant glyph maps back to its 4-bit mask (the snapshot relies on
        // this to re-expand arena cells to pixels); non-block chars map to None.
        for (i, &g) in QUAD_GLYPHS.iter().enumerate() {
            assert_eq!(quad_pattern(g), Some(i as u8), "glyph {g:?}");
        }
        assert_eq!(quad_pattern('X'), None);
        assert_eq!(quad_pattern('│'), None);
    }

    #[test]
    fn quad_cell_fits_two_colours_and_the_matching_glyph() {
        let w = (240, 240, 240);
        let k = (10, 10, 10);
        // Top row light, bottom row dark → upper-half block, fg=light, bg=dark.
        let (ch, fg, bg) = quad_cell([w, w, k, k]);
        assert_eq!(
            (ch, fg, bg),
            ('▀', Color::Rgb(240, 240, 240), Color::Rgb(10, 10, 10))
        );
        // A uniform cell has no second colour to split on → a full block.
        let (ch, fg, _) = quad_cell([w, w, w, w]);
        assert_eq!((ch, fg), ('█', Color::Rgb(240, 240, 240)));
        // One bright sub-pixel (top-left) → the upper-left quadrant.
        assert_eq!(quad_cell([w, k, k, k]).0, '▘');
        // Bright bottom-right only → the lower-right quadrant.
        assert_eq!(quad_cell([k, k, k, w]).0, '▗');
    }

    #[test]
    fn glyph_raster_matches_the_font() {
        // At scale 1, GlyphRaster marks Ink exactly where DIGIT_FONT lights a bit,
        // and never Ink where it doesn't (a hole is outline or clear, not a stroke).
        for d in 0u8..=9 {
            let raster = GlyphRaster::new(&d.to_string(), 1);
            for (fy, row) in DIGIT_FONT[d as usize].iter().enumerate() {
                for fx in 0i32..3 {
                    let lit = (row >> (2 - fx)) & 1 == 1;
                    let px = raster.px(fx, fy as i32);
                    if lit {
                        assert_eq!(px, GlyphPx::Ink, "digit {d} sub-pixel ({fx},{fy})");
                    } else {
                        assert_ne!(px, GlyphPx::Ink, "digit {d} sub-pixel ({fx},{fy})");
                    }
                }
            }
        }
    }

    #[test]
    fn burn_numbers_fills_ink_and_leaves_die_showing() {
        // A tight sentinel-filled RGB image: burning a big "8" should paint ink
        // pixels and leave clear pixels (outside the glyph) as the sentinel.
        let (img_w, img_h) = (60u32, 40u32);
        let (inner_w, inner_h) = (30u16, 10u16); // sx = 2, sy = 2
        let mut rgb = vec![9u8; (img_w * img_h * 3) as usize];
        let burn = NumberBurn {
            label: "8".to_string(),
            center: (15.0, 5.0), // arena centre in cells
            scale: 2,
            ink: Color::White,
            mods: Modifier::BOLD,
            outline: Color::Rgb(10, 20, 30),
        };
        burn_numbers(
            &mut rgb,
            img_w,
            img_h,
            inner_w,
            inner_h,
            std::slice::from_ref(&burn),
        );
        let white = ink_rgb(Color::White);
        let has_ink = rgb
            .chunks_exact(3)
            .any(|p| p[0] == white.0 && p[1] == white.1 && p[2] == white.2);
        let has_sentinel = rgb.chunks_exact(3).any(|p| p == [9, 9, 9]);
        assert!(has_ink, "the '8' should paint ink pixels into the frame");
        assert!(
            has_sentinel,
            "clear sub-pixels must leave the die (sentinel) showing through"
        );
        // The outline halo dilates one sub-pixel *beyond* the glyph box (matching the
        // half-block path). The box top sits at y=10 (center.1·2·sy − glyph_h/2), so an
        // outline pixel must appear in the rows above it — this fails if the raster loop
        // stops at the box edge instead of `-1..=`, leaving kitty digits haloless.
        let outline = ink_rgb(Color::Rgb(10, 20, 30));
        let top_ring_has_outline = (0..10u32).any(|y| {
            (0..img_w).any(|x| {
                let i = ((y * img_w + x) * 3) as usize;
                rgb[i] == outline.0 && rgb[i + 1] == outline.1 && rgb[i + 2] == outline.2
            })
        });
        assert!(
            top_ring_has_outline,
            "the outline halo must burn above the glyph box, as the cell path draws it"
        );
        // A zero-size image (stale/mid-resize) is a no-op, not a panic.
        let mut empty: Vec<u8> = Vec::new();
        burn_numbers(
            &mut empty,
            0,
            0,
            inner_w,
            inner_h,
            std::slice::from_ref(&burn),
        );
    }

    #[test]
    fn blocks_mode_equals_render_bevy() {
        // `render_bevy` is exactly `render_bevy_mode(Blocks).view`, and Blocks mode
        // composes an identical buffer and reports no kitty panel.
        let mut app = App::new("2d6".to_string());
        let mut t1 = Terminal::new(TestBackend::new(60, 24)).unwrap();
        let mut t2 = Terminal::new(TestBackend::new(60, 24)).unwrap();
        t1.draw(|f| {
            render_bevy(f, &mut app, &[], 0, 0);
        })
        .unwrap(); // size the arena
        for _ in 0..6000 {
            app.update(1.0 / 60.0);
            if app.all_settled() {
                break;
            }
        }
        // Same app state feeds both draws (no update between), so the buffers match.
        t1.draw(|f| {
            render_bevy(f, &mut app, &[], 0, 0);
        })
        .unwrap();
        let mut kitty_none = false;
        t2.draw(|f| {
            kitty_none = render_bevy_mode(f, &mut app, &[], 0, 0, GraphicsMode::Blocks)
                .kitty
                .is_none();
        })
        .unwrap();
        assert!(kitty_none, "blocks mode reports no kitty panel");
        assert_eq!(
            t1.backend().buffer(),
            t2.backend().buffer(),
            "render_bevy must equal render_bevy_mode(Blocks)"
        );
    }

    #[test]
    fn kitty_mode_clears_arena_and_collects_burns() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let mode = GraphicsMode::Kitty { scale: 4 };

        // First draw sizes the arena and reports a panel.
        let r0 = draw_mode(&mut terminal, &mut app, mode);
        let panel = r0.kitty.expect("kitty mode reports a panel");
        // The requested view is the panel scaled by S, no supersample.
        assert_eq!(
            r0.view,
            (panel.inner.width * 4, panel.inner.height * 2 * 4),
            "kitty view = (w*S, h*2*S)"
        );

        // A settled roll → non-empty number burns, and NO half-blocks blitted into
        // the arena (the image carries the felt; ratatui only clears + chrome).
        app.input = "1d6".to_string();
        app.insta_roll();
        assert!(app.all_settled());
        let r1 = draw_mode(&mut terminal, &mut app, mode);
        let panel = r1.kitty.expect("kitty panel");
        assert!(
            !panel.burns.is_empty(),
            "a settled die contributes a number burn"
        );
        let buf = terminal.backend().buffer();
        let arena_has_block = (panel.inner.left()..panel.inner.right())
            .flat_map(|x| (panel.inner.top()..panel.inner.bottom()).map(move |y| (x, y)))
            .any(|(x, y)| {
                let ch = buf[(x, y)].symbol().chars().next().unwrap_or(' ');
                quad_pattern(ch).is_some_and(|mask| mask > 0) // any lit quadrant glyph
            });
        assert!(!arena_has_block, "kitty mode must not blit block glyphs");
        assert!(
            flatten(&terminal).contains("result"),
            "the chrome still renders around the image"
        );

        // Shaking gathers the dice in the cup: no burns, but the meter renders over
        // the image as cell chrome.
        app.start_shake();
        app.update(0.3);
        let r2 = draw_mode(&mut terminal, &mut app, mode);
        assert!(
            r2.kitty.expect("kitty panel").burns.is_empty(),
            "no number burns while shaking"
        );
        assert!(
            flatten(&terminal).contains("power"),
            "the power meter renders over the image"
        );
    }

    #[test]
    fn kitty_scale_is_capped_for_wide_arenas() {
        // A modest arena keeps its requested scale…
        assert_eq!(kitty_scale(8, 60), 8);
        // …but a very wide arena is knocked down so the image width stays under cap.
        let s = kitty_scale(8, 400);
        assert!(
            s * 400 <= crate::graphics::MAX_IMG_W,
            "width under MAX_IMG_W"
        );
        assert!(s >= 1, "never below 1");
    }
}
