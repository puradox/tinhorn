//! All ratatui rendering lives here.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};

use crate::app::{App, Die, Pane, Particle, Stats};
use crate::render3d::color::Rgb;

/// The arena's whole look in one place: the felt, the tray lip, and the lights.
/// Keeping it a struct means a re-theme is a one-line change — and tests can
/// swap it to render design mock-ups through the real pipeline.
#[derive(Clone, Copy)]
pub(crate) struct ArenaStyle {
    pub(crate) background: Rgb, // the void around/behind the tray
    pub(crate) floor: Rgb,      // the felt
    pub(crate) wall: Rgb,       // the raised lip (back + two flared side walls)
    pub(crate) lip_top: f32,    // wall height above the floor (shorter → more room shows)
    pub(crate) ambient: f32,    // white ambient intensity
    pub(crate) key: Rgb,        // overhead point light colour (the felt's hotspot)
    pub(crate) fill: Rgb,       // soft directional fill so far walls aren't black
}

impl ArenaStyle {
    pub(crate) const DEFAULT: ArenaStyle = ArenaStyle {
        background: Rgb(16, 11, 9), // warm near-black room
        floor: Rgb(22, 64, 42),     // deep green baize
        wall: Rgb(66, 40, 28),      // dark warm mahogany rail
        lip_top: 1.35,              // wall height above the floor — a low rail, room shows over it
        ambient: 0.28,              // low: the warm point light does the work
        key: Rgb(255, 222, 176),    // warm tungsten overhead — the felt's hotspot
        fill: Rgb(104, 80, 62),     // warm dim fill so far walls aren't black
    };
}

#[cfg(test)]
thread_local! {
    static STYLE_OVERRIDE: std::cell::Cell<Option<ArenaStyle>> =
        const { std::cell::Cell::new(None) };
}

/// Test hook: force the arena palette (for design mock-ups). `None` restores the default.
#[cfg(test)]
pub(crate) fn set_arena_style(style: Option<ArenaStyle>) {
    STYLE_OVERRIDE.with(|c| c.set(style));
}

/// The palette the arena renders with — the default, unless a test overrode it.
fn arena_style() -> ArenaStyle {
    #[cfg(test)]
    {
        if let Some(style) = STYLE_OVERRIDE.with(|c| c.get()) {
            return style;
        }
    }
    ArenaStyle::DEFAULT
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

/// The colour-keyed bake cache all the procedural textures share. Each texture
/// fn owns a static slot list and passes it here with its base colour and its
/// baker; the textures are asked for every frame but the palette rarely
/// changes, so each look is baked at most once. Bounded, and one
/// implementation, so the lookup/evict policy can't drift between textures.
type TexCache = std::sync::OnceLock<
    std::sync::Mutex<Vec<([u8; 3], std::sync::Arc<crate::render3d::texture::Texture>)>>,
>;
fn cached_texture(
    cache: &TexCache,
    base: Rgb,
    bake: impl FnOnce() -> crate::render3d::texture::Texture,
) -> std::sync::Arc<crate::render3d::texture::Texture> {
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

/// The tray floor's wooden thickness below the felt line. The tabletop (and so
/// the chip stacks) sits at `-HY - FLOOR_THICK`, so tuning the tray's base
/// moves the furniture with it instead of leaving the tray afloat.
const FLOOR_THICK: f32 = 0.2;

/// A double-sided quad mesh: four corners, a flat normal, per-vertex UVs, and
/// both windings — so no scene block ever fusses over which way a face points.
/// THE quad builder for the arena's flat geometry (backdrop, tray faces, room
/// floor, table); one 12-entry index list, defined once.
fn dquad(
    corners: [crate::render3d::math::Vec3; 4],
    n: crate::render3d::math::Vec3,
    uvs: [(f32, f32); 4],
) -> crate::render3d::mesh::Mesh {
    use crate::render3d::mesh::{Mesh, Vertex};
    let verts = corners
        .iter()
        .zip(uvs)
        .map(|(&p, (u, v))| Vertex::new(p, n).with_uv(u, v))
        .collect();
    Mesh::new(verts, vec![0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2])
}

/// A procedural grain texture: `base` colour with a soft fibrous grain baked in
/// (coarse blotches plus fine speckle), so a surface reads as fabric/painted
/// rather than a flat plastic plane.
fn grain_texture(base: Rgb) -> std::sync::Arc<crate::render3d::texture::Texture> {
    use crate::render3d::texture::Texture;

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

/// The felt of the tray floor, baked as a texture. Real trays recess a plush
/// felt into the wooden sides, so the fabric sits sunk below the lip: it reads as
/// velvet, not a painted plane, and it falls into shadow where it meets the walls.
/// So we bake two things a flat grain can't give: a soft, low-frequency **pile
/// mottle** (plush unevenness) and an **ambient-occlusion band** darkening the
/// felt toward the three walls (left, right, back — the open front is left lit).
/// Cached by colour; `wall_dist` is the UV distance at which the recess shadow
/// fades out. UVs run `u` across the width, `v` from the back wall (0) to the
/// open front (1).
pub(crate) fn felt_texture(base: Rgb) -> std::sync::Arc<crate::render3d::texture::Texture> {
    use crate::render3d::texture::Texture;

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

/// The stage curtains' velvet, baked as a texture: soft vertical streak
/// variation (pile catching light differently streak to streak — varies across
/// `u`, smeared down `v`) under a subtle darkening toward the top header. Kept
/// subtle on purpose: the drape's *geometry* (free-hanging scalloped edge +
/// corrugated folds) does the talking; this only keeps the cloth from reading
/// flat. Cached by colour.
pub(crate) fn velvet_texture(base: Rgb) -> std::sync::Arc<crate::render3d::texture::Texture> {
    use crate::render3d::texture::Texture;

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

/// The room floor, baked as **wooden floorboards**: long planks (seams running
/// one way, so they converge into the distance like real boards) with a dark
/// groove between them and a little shade variation plank to plank. At this tiny
/// frame the bold seam lines are what actually read as "a floor" rather than a
/// flat brown field. Planks run along the texture's `v`; seams sit at regular `u`.
/// Cached by colour.
pub(crate) fn floor_texture(base: Rgb) -> std::sync::Arc<crate::render3d::texture::Texture> {
    use crate::render3d::texture::Texture;

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
/// out-of-focus glowing **light blobs** (warm + a few neons) — a defocused casino
/// floor of signage and lights, impressionistic rather than a hard row of
/// machines. Emissive; the 3D bokeh add nearer, crisper glows in front. Cached
/// once; it never changes.
fn backdrop_texture() -> std::sync::Arc<crate::render3d::texture::Texture> {
    use crate::render3d::texture::Texture;
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
            // to the lit floorboards so the floor→wall horizon is seamless and there
            // is no dark band reading as "missing floor", fading up to a **dark
            // ceiling** where the bokeh hang as lights. `horizon` is the floor's own
            // rendered tone divided back out through the emissive factor.
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
                // Wainscoting: a thick dark chair-rail band across the wall, set well
                // above the floor seam (which sits near v≈0.8), with a subtly deeper,
                // warmer panelled paint on the wall just below the rail. Both fade to
                // nothing before the seam, so the wall's bottom tone still equals the
                // lit floorboards there — the horizon invariant below stays intact.
                // Band only — a thin line would shimmer at this frame.
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
    Rgb(60, 200, 210),
    Rgb(220, 200, 60),
    Rgb(70, 200, 90),
    Rgb(210, 90, 210),
    Rgb(220, 70, 70),
    Rgb(80, 130, 240),
    Rgb(130, 230, 130),
    Rgb(230, 130, 230),
];

/// The render3d colour for die palette slot `idx`.
fn die_rgb(idx: usize) -> Rgb {
    PALETTE[idx % PALETTE.len()]
}

/// The ratatui colour for die palette slot `idx` — [`die_rgb`] as a cell colour.
fn die_color(idx: usize) -> Color {
    let Rgb(r, g, b) = die_rgb(idx);
    Color::Rgb(r, g, b)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Min(5),    // bouncing arena
        Constraint::Length(4), // results
        Constraint::Length(1), // input field
        Constraint::Length(1), // help
    ])
    .split(area);

    render_arena(frame, app, chunks[0]);
    render_results(frame, app, chunks[1]);
    render_input(frame, app, chunks[2]);
    render_help(frame, app, chunks[3]);

    // A pop-out pane floats on top of everything when one is toggled open. Each
    // renderer clamps the scroll to its own overflow and hands it back, so an
    // over-scroll (Down past the end) settles on the next frame.
    let scroll = app.pane_scroll;
    match app.pane {
        Pane::None => {}
        Pane::Help => app.pane_scroll = render_help_overlay(frame, area, scroll),
        Pane::History => app.pane_scroll = render_history_overlay(frame, app, area, scroll),
        Pane::Stats => app.pane_scroll = render_stats_overlay(frame, app, area, scroll),
    }
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
/// While the die is still tumbling the number is dim and colourless — nothing
/// decided to the eye yet — and it **ducks out** whenever no face squarely fronts
/// the camera: `clarity` is how far the read-face's facing leads the runner-up's,
/// dropping toward zero as the die rolls edge- or corner-on (two faces tie), so
/// the digit blinks off there and reads as ink on the tumbling solid rather than
/// a fixed label. Once the die settles the burned value always shows in full —
/// hot on a crit, red on a fumble, grey if the die was dropped by keep/drop,
/// white otherwise.
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
/// face's `centroid` (in unit-mesh space, to anchor the digit on) and its
/// `clarity` — how far that face's facing leads the runner-up's. Clarity nears
/// zero when two faces tie for frontmost (the die rolled edge- or corner-on),
/// which is where the airborne number ducks out, and is large when a single face
/// squarely presents; that holds for a cube or a d20 alike, so one threshold
/// fits every die. `to_cam` is the unit eye direction in world space (from the
/// die toward the camera), `rot` the die's orientation.
pub(crate) fn read_face(
    faces: &[crate::render3d::dice::FaceGeom],
    rot: crate::render3d::math::Quat,
    to_cam: crate::render3d::math::Vec3,
) -> (crate::render3d::math::Vec3, f32) {
    let (mut best, mut second) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut centroid = crate::render3d::math::Vec3::ZERO;
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
    camera: &crate::render3d::camera::Camera,
    center: crate::render3d::math::Vec3,
    cols: f32,
    rows: f32,
) -> (f32, f32) {
    use crate::render3d_view::project_to_cell;
    let r = crate::physics::DIE_R;
    let forward = (camera.target - camera.position).normalize_or_zero();
    let right = forward.cross(camera.up).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    let span = |axis: crate::render3d::math::Vec3| -> f32 {
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

/// Draw one die's number into the arena at the frame's shared `scale` — computed
/// once in [`render_arena`] so every die reads the same, never a mix of a big
/// number on the nearest die and single cells on the rest. Scale 0 is the crisp
/// single-cell overlay (small dice, or the whole roll on a narrow terminal); ≥1
/// renders the digits as an outlined [`DIGIT_FONT`] glyph sitting on the
/// read-face. Both ride the read-face and share [`face_ink`], so colour, the
/// airborne duck-out, and the settle burn behave identically at every size.
fn draw_die_number(
    frame: &mut Frame,
    inner: Rect,
    camera: &crate::render3d::camera::Camera,
    die: &Die,
    cols: f32,
    rows: f32,
    scale: i32,
) {
    let to_cam = (camera.position - die.pos).normalize_or_zero();
    let (read_centroid, clarity) = read_face(
        crate::render3d::dice::face_geometry(die.sides),
        die.rot,
        to_cam,
    );
    let Some((ink, mods)) = face_ink(die, clarity) else {
        return;
    };
    let label = die.shown.to_string();

    if scale < 1 {
        // The crisp overlay: one centred cell per digit on a dark plate, sitting on
        // the read-face (a small label rides the top face nicely).
        let anchor = die.pos + die.rot * (read_centroid * crate::physics::DIE_R);
        let Some((cx, cy)) = crate::render3d_view::project_to_cell(camera, anchor, cols, rows)
        else {
            return;
        };
        let style = Style::default().bg(NUMBER_PLATE).fg(ink).add_modifier(mods);
        let x = (inner.x as f32 + cx - label.len() as f32 / 2.0).round() as i32;
        let y = (inner.y as f32 + cy).round() as i32;
        let max_x = (inner.right() as i32 - label.len() as i32).max(inner.x as i32);
        let x = x.clamp(inner.x as i32, max_x) as u16;
        let y = y.clamp(inner.y as i32, inner.bottom() as i32 - 1) as u16;
        frame.buffer_mut().set_string(x, y, &label, style);
    } else {
        // The block number centres on the die itself (not the read-face centroid):
        // from the near-overhead read the die's silhouette *is* its top face, and
        // centring keeps the digits contained on it rather than sliding off toward
        // a small top facet on a d20.
        let Some((cx, cy)) = crate::render3d_view::project_to_cell(camera, die.pos, cols, rows)
        else {
            return;
        };
        let center = (inner.x as f32 + cx, inner.y as f32 + cy);
        // Outline the number in a dark tint of *this die's* colour rather than a
        // generic black, so on a small die (where the digits cover most of it) the
        // number's surround still carries the die's hue — that's how you read which
        // number belongs to which die when the die itself is mostly hidden.
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
        draw_big_number(frame, inner, center, &label, scale, ink, outline);
    }
}

/// Blit `label` as [`DIGIT_FONT`] glyphs centred on cell `center`, scaled `scale`×
/// and drawn in **half-blocks** (two sub-rows per cell) so it stays compact on a
/// short die. Each lit stroke sub-pixel is `ink`, every sub-pixel *touching* a
/// stroke gets the `outline` colour (a dark tint of the die, so the number stays
/// tied to its die), and everything else is left transparent — so the die shows
/// through and the number reads as ink *on the face* rather than a plate covering
/// it. Compositing is per sub-pixel: a cell becomes `▀` with its upper and lower
/// halves coloured independently (ink, outline, or the die pixel already in the
/// buffer). Cells outside `area` clip cleanly.
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
    let digits: Vec<u8> = label.bytes().filter(u8::is_ascii_digit).collect();
    let n = digits.len() as i32;
    if n == 0 {
        return;
    }
    let gw = (4 * n - 1) * scale; // cells wide: n digits × 3 + (n−1) gaps, scaled
    let h_sub = 5 * scale; // half-block sub-rows tall
    let gh = (h_sub + 1) / 2; // cells tall (two sub-rows per cell, last may be half)
    // Is glyph sub-pixel `(x cell, sub row)` a lit font pixel? Off outside the grid
    // and in the one-column gap between digits.
    let lit = |x: i32, sub: i32| -> bool {
        if x < 0 || sub < 0 || x >= gw || sub >= h_sub {
            return false;
        }
        let span = 4 * scale; // 3 glyph columns + 1 gap, scaled
        let di = x / span;
        let local_x = x - di * span;
        if di >= n || local_x >= 3 * scale {
            return false;
        }
        let (fx, fy) = ((local_x / scale) as usize, (sub / scale) as usize);
        (DIGIT_FONT[(digits[di as usize] - b'0') as usize][fy] >> (2 - fx)) & 1 == 1
    };
    // Each sub-pixel: a lit stroke, its dark outline, or clear. The outline is a
    // *tight* one-sub-pixel dilation of the strokes on every side — enough to
    // separate the number from any die colour, but thin so the die still shows
    // around and between the digits (that colour is how you tell dice apart), not
    // a solid tile blotting the die out.
    #[derive(PartialEq)]
    enum Px {
        Ink,
        Outline,
        Clear,
    }
    let sub = |x: i32, s: i32| -> Px {
        if lit(x, s) {
            Px::Ink
        } else if (-1..=1).any(|dx| (-1..=1).any(|ds| lit(x + dx, s + ds))) {
            Px::Outline
        } else {
            Px::Clear
        }
    };

    let x0 = (cx - gw as f32 / 2.0).round() as i32;
    let y0 = (cy - gh as f32 / 2.0).round() as i32;
    let buf = frame.buffer_mut();
    // Iterate one cell *beyond* the glyph box on every side: the outline dilates
    // outward from the strokes, so its top / left / right border lives outside the
    // glyph's own bounds and would go undrawn if the loop stopped at the box edge
    // (only the bottom border, which has a spare half-cell, would show).
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
            let (up, lo) = (sub(col, 2 * row), sub(col, 2 * row + 1));
            if up == Px::Clear && lo == Px::Clear {
                continue; // leave the die pixel untouched — the number isn't here
            }
            let cell = &mut buf[(x as u16, y as u16)];
            // The die's own sub-pixels are already in the buffer: a HalfBlock `▀`
            // cell holds fg = upper pixel, bg = lower pixel. Clear keeps them.
            let (die_up, die_lo) = (cell.fg, cell.bg);
            let paint = |p: &Px, die: Color| match p {
                Px::Ink => ink,
                Px::Outline => outline,
                Px::Clear => die,
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

/// The arena: the actual roll as tumbling polyhedra, rendered through the
/// vendored render3d pipeline. Each die spins while airborne and freezes when it
/// settles; the instant it does, its RNG-decided value is "burned" onto the face
/// pointing at you. Position comes from the sim; the RNG-decided values and total
/// are untouched — the renderer only shows them off.
fn render_arena(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::render3d::dice;
    use crate::render3d::light::Light;
    use crate::render3d::material::Material;
    use crate::render3d::math::{Quat, Vec3};
    use crate::render3d::object::SceneObject;
    use crate::render3d::scene::Scene;
    use crate::render3d::transform::Transform;
    use crate::render3d_view::{self, RenderMode};

    let title = arena_title(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title.bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 || inner.height < 3 {
        return;
    }

    // Hand the arena size to the simulation so the physics has a box to bounce in.
    app.arena_w = inner.width as f32;
    app.arena_h = inner.height as f32;

    // Camera framing the physics tray (a fixed box in world units) from just
    // above the front, angled down at the felt — a dice tray on a table. The
    // distance adapts to the arena's aspect so the tray fills the frame (dice
    // read large) on any terminal width. `live_camera` folds in every per-frame
    // modifier — the idle drift, the crit punch-in, and the throw shudder — so
    // the particle projection in `app` sees the identical view.
    let aspect = render3d_view::arena_aspect(inner.width as f32, inner.height as f32);
    let flash = app.flash();
    let camera =
        render3d_view::live_camera(app.camera_shake(), aspect, app.focus(), app.clock(), flash);

    let style = arena_style();

    // One polyhedron per die, placed and oriented straight from its physics body.
    let mut scene = Scene::new();
    scene.background = style.background; // so the arena isn't a black void

    // Depth fog: recede far geometry into the room. The start sits well beyond the
    // tray AND the mid-ground boards (the camera rides ~5–8 units out; the tray and
    // dice sit within ~9 of it, the rug and the near/mid floorboards within ~15) so
    // the felt, the dice, and the room floor's bright lit-oak warmth all stay
    // crisp — only the floor's far reaches and the backdrop ease toward the
    // background, which seats the floor→wall horizon and gives the room depth
    // without dimming it into a void. Applied per-fragment in the pipeline.
    scene.fog = Some((16.0, 40.0));

    // Room backdrop: a big emissive gradient wall far behind everything (a warm
    // horizon glow low, fading to dark up top), so the void reads as a lit room
    // and the bokeh hang in a space rather than on black. The tray occludes its
    // lower half; you see the glow above the back rail.
    {
        let z = -22.0;
        let (x0, x1, y0, y1) = (-44.0, 44.0, -9.0, 22.0);
        let quad = dquad(
            [
                Vec3::new(x0, y1, z),
                Vec3::new(x1, y1, z),
                Vec3::new(x1, y0, z),
                Vec3::new(x0, y0, z),
            ],
            Vec3::Z,
            [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
        );
        let mat = Material::default()
            .with_ambient(3.1) // emissive glow — a warm dark room, lights still pop
            .with_diffuse(0.0)
            .with_specular(0.0)
            .with_texture(backdrop_texture());
        scene.add_object(SceneObject::new(quad).with_material(mat));
    }

    // Heavy oxblood stage curtains flank the background, built to be
    // *identified*, not just seen (flat straight-edged band drapes read as dark
    // walls to a cold viewer). Three resolution-safe identity cues:
    //  - FREE-HANGING SILHOUETTE: each drape hangs straight down, its width
    //    near-constant (just a whisker of outward relaxation toward the floor —
    //    gravity spreads fabric a little), and its inner edge is not a ruled
    //    line: it scallops gently at the fold amplitude, because on a free
    //    hang the innermost fold's crest/valley profile IS the fabric's edge.
    //    (A tied-back cinch was tried first: with no visible tie geometry the
    //    pinch read as unnatural — an untied curtain just hangs.)
    //  - REAL FOLDS: the surface is corrugated into wide, evenly spaced
    //    vertical waves (real geometry, flat-shaded facets), so the key and
    //    rim genuinely light the crests and shade the valleys. No painted bands.
    //  - VELVET: a subtle baked streak texture (`velvet_texture`) under the
    //    fold shading, so the cloth isn't a flat tone up close.
    // Tones tie to the rug; hung NEAR (z = −5, falling to the boards just past
    // the rug's far edge — deeper, perspective shrinks them to corner patches).
    // Lit materials (not emissive), so a natural crit's gold flare catches the
    // inner folds.
    {
        use crate::render3d::mesh::{Mesh, Vertex};
        const COLS: usize = 16; // across the drape — 4 per fold, folds stay wide
        const ROWS: usize = 9; // down the drop — enough to bend the edge scallop
        const FOLDS: f32 = 3.0; // full sine periods across the drape's width
        const AMP: f32 = 0.28; // fold depth — and the edge scallop's amplitude
        let (y_top, y_bot, cz) = (2.6f32, -3.15f32, -5.0f32);
        let x_out = 13.8f32; // outer edge, well past the frame
        // The free-hanging inner edge: essentially straight down the drop, with
        // a whisker of spread at the floor and a gentle scallop at the fold
        // amplitude (no more — a deeper wobble stops reading as the same cloth).
        let inner_x = |v: f32, edge_phase: f32| 6.15 - 0.2 * v + AMP * (v * 4.4 + edge_phase).sin();
        let mat = Material::default()
            .with_color(Rgb(104, 32, 33)) // the rug field's crimson, as cloth
            .with_ambient(1.5)
            .with_diffuse(0.95)
            .with_specular(0.0)
            .with_texture(velvet_texture(Rgb(104, 32, 33)));
        for side in [-1.0f32, 1.0] {
            // Vertex grid: u across the drape (0 = inner edge), v down the drop.
            let phase = if side < 0.0 { 0.0 } else { 0.9 }; // don't mirror the folds
            let grid: Vec<Vec<Vec3>> = (0..=ROWS)
                .map(|r| {
                    let v = r as f32 / ROWS as f32;
                    let y = y_top + (y_bot - y_top) * v;
                    let xi = inner_x(v, phase * 2.3 + 1.1);
                    (0..=COLS)
                        .map(|c| {
                            let u = c as f32 / COLS as f32;
                            let wave = (u * FOLDS * std::f32::consts::TAU + phase).sin();
                            Vec3::new(side * (xi + (x_out - xi) * u), y, cz + AMP * wave)
                        })
                        .collect()
                })
                .collect();
            // Emit each cell flat-shaded (its own geometric normal, faced toward
            // the camera), double-wound — crisp light/shadow facets that read as
            // fabric folds after the downsample.
            let mut verts: Vec<Vertex> = Vec::new();
            let mut idx: Vec<u32> = Vec::new();
            for r in 0..ROWS {
                for c in 0..COLS {
                    let (v0, v1) = (r as f32 / ROWS as f32, (r + 1) as f32 / ROWS as f32);
                    let (u0, u1) = (c as f32 / COLS as f32, (c + 1) as f32 / COLS as f32);
                    let p = [
                        grid[r][c],
                        grid[r][c + 1],
                        grid[r + 1][c + 1],
                        grid[r + 1][c],
                    ];
                    let mut n = (p[1] - p[0]).cross(p[3] - p[0]).normalize_or_zero();
                    if n.z < 0.0 {
                        n = -n;
                    }
                    let base = verts.len() as u32;
                    for (&pt, uv) in p.iter().zip([(u0, v0), (u1, v0), (u1, v1), (u0, v1)]) {
                        verts.push(Vertex::new(pt, n).with_uv(uv.0, uv.1));
                    }
                    idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                    idx.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
                }
            }
            scene.add_object(SceneObject::new(Mesh::new(verts, idx)).with_material(mat.clone()));
        }
    }

    // The tray the dice roll in, drawn from the physics box so what you see is
    // what they bounce off: a felt floor with a wooden lip on three sides (the
    // near side is left open so you look straight in). The dice render over it.
    {
        use crate::physics::{HX, HY, HZ};
        use crate::render3d::mesh::{Mesh, Vertex};
        let wall_h = style.lip_top; // the wall rises this high above the floor
        let flare = 0.12; // inner walls nearly vertical (a solid well, not a bowl)
        // The tray floor has real thickness down to the table: the walls' outer
        // faces and the open-front edge drop this far below the felt, so the felt
        // reads as a pad set into a solid tray rather than paint on the tabletop.
        let floor_thick = FLOOR_THICK;

        // Felt is matte — no plastic sheen — so the overhead light reads as a soft
        // wash, not a hotspot on shiny plastic — and carries a plush pile texture
        // with the recess shadow baked in, so it reads as a sunken felt bed.
        let felt_mat = Material::default()
            .with_ambient(0.9)
            .with_diffuse(1.05)
            .with_specular(0.0)
            .with_texture(felt_texture(style.floor));
        // The exposed cut edge of the felt at the open front. Kept deliberately
        // dark and dim: from the near-overhead settled angle a brighter edge here
        // caught the light and read as a floating bright-green stripe, so it's
        // toned right down to read as the felt's own shadowed thickness instead.
        let felt_edge_mat = Material::default()
            .with_color(style.floor.scale(0.42))
            .with_ambient(0.5)
            .with_diffuse(0.5)
            .with_specular(0.0);
        // The wall faces (inner + outer): mahogany grain. A lifted ambient keeps
        // the shadowed outer faces reading as dark wood, not black wedges.
        let wall_mat = Material::default()
            .with_ambient(0.95)
            .with_diffuse(0.9)
            .with_specular(0.10)
            .with_shininess(6.0)
            .with_texture(grain_texture(style.wall));
        // The flat top rail: lighter polished wood that catches the overhead light.
        let rail_mat = Material::default()
            .with_color(Rgb(150, 108, 72))
            .with_ambient(0.8)
            .with_diffuse(1.0)
            .with_specular(0.14)
            .with_shininess(10.0);
        // The outer faces and the capped wall ends sit far from the key light and
        // face away from it; a strong self-ambient keeps them reading as solid dark
        // wood instead of the black wedges that made the tray look hollow.
        let outer_mat = Material::default()
            .with_ambient(1.7)
            .with_diffuse(0.5)
            .with_specular(0.0)
            .with_texture(grain_texture(style.wall));

        // A three-walled tray: a back wall and two side walls flaring up from the
        // rectangular floor, the front left fully open so you look straight in.
        // Four floor-edge vertices, CCW from the back-left.
        let poly = [
            Vec3::new(-HX, -HY, -HZ), // 0 back-left
            Vec3::new(HX, -HY, -HZ),  // 1 back-right
            Vec3::new(HX, -HY, HZ),   // 2 front-right
            Vec3::new(-HX, -HY, HZ),  // 3 front-left
        ];
        const OPEN_EDGE: usize = 2; // edge 2→3 is the open front

        // The top rim: each floor vertex pushed radially outward and up. Doing it
        // per-vertex (not per-edge) mitres the corners, so adjacent walls share a
        // rim vertex and leave no gap where they meet.
        let rim: Vec<Vec3> = poly
            .iter()
            .map(|&p| {
                p + Vec3::new(p.x, 0.0, p.z).normalize_or_zero() * flare
                    + Vec3::new(0.0, wall_h, 0.0)
            })
            .collect();

        // Floor: a triangle fan over the polygon, double-sided (so we needn't fuss
        // over winding), UVs mapped across the bounding rect for the felt grain.
        let mut fv = vec![Vertex::new(Vec3::new(0.0, -HY, 0.0), Vec3::Y).with_uv(0.5, 0.5)];
        for &p in &poly {
            let uv = (p.x / (2.0 * HX) + 0.5, p.z / (2.0 * HZ) + 0.5);
            fv.push(Vertex::new(p, Vec3::Y).with_uv(uv.0, uv.1));
        }
        let n = poly.len() as u32;
        let mut fi = Vec::new();
        for i in 0..n {
            let (a, b) = (1 + i, 1 + (i + 1) % n);
            fi.extend_from_slice(&[0, a, b, 0, b, a]); // both windings
        }
        scene.add_object(SceneObject::new(Mesh::new(fv, fi)).with_material(felt_mat));

        // Each wall is a solid lip with real depth: an inner face flaring up from
        // the floor edge, a flat **top rail** you look straight down onto, and an
        // outer face — so the tray reads as a carved rim, not a paper-thin plane.
        // The rail's outer edge is the rim pushed radially out; corners stay mitred
        // (shared vertices). Double-sided quads dodge winding fuss.
        let rail_w = 0.6; // width of the flat top rail — a thick, solid wooden frame
        let radial = |p: Vec3| Vec3::new(p.x, 0.0, p.z).normalize_or_zero();
        let top_outer: Vec<Vec3> = poly
            .iter()
            .enumerate()
            .map(|(i, &p)| rim[i] + radial(p) * rail_w)
            .collect();
        // Drop to the table, not just the felt line, so the tray shows real wood
        // thickness under the rail rather than sitting paper-thin on the tabletop.
        let outer_bot: Vec<Vec3> = top_outer
            .iter()
            .map(|&p| Vec3::new(p.x, -HY - floor_thick, p.z))
            .collect();
        // A double-sided quad with a flat normal; UVs run top(v=0)→bottom(v=1).
        let quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3, nrm: Vec3| -> Mesh {
            dquad(
                [a, b, c, d],
                nrm,
                [(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)],
            )
        };
        for i in 0..poly.len() {
            if i == OPEN_EDGE {
                continue; // the front edge is left open — you look straight in
            }
            let j = (i + 1) % poly.len();
            // inner face — perpendicular normal oriented toward the tray interior
            let mut n_in = (poly[j] - poly[i])
                .cross(rim[i] - poly[i])
                .normalize_or_zero();
            let mid = (poly[i] + poly[j]) * 0.5;
            if n_in.dot(Vec3::new(-mid.x, 0.0, -mid.z)) < 0.0 {
                n_in = -n_in;
            }
            scene.add_object(
                SceneObject::new(quad(poly[i], poly[j], rim[j], rim[i], n_in))
                    .with_material(wall_mat.clone()),
            );
            // flat top rail (faces up, catches the light)
            scene.add_object(
                SceneObject::new(quad(rim[i], rim[j], top_outer[j], top_outer[i], Vec3::Y))
                    .with_material(rail_mat.clone()),
            );
            // outer face (faces radially away — solidity, mostly seen at the edges)
            let n_out = radial((top_outer[i] + top_outer[j]) * 0.5);
            scene.add_object(
                SceneObject::new(quad(
                    top_outer[i],
                    top_outer[j],
                    outer_bot[j],
                    outer_bot[i],
                    n_out,
                ))
                .with_material(outer_mat.clone()),
            );
        }
        // Cap the two open ends of the side walls at the front corners — the
        // cross-section from inner-bottom up to the rim and down the outer face —
        // so you don't see into the hollow lip (that's what read as dark wedges).
        // The cap faces the open front, away from every light, so it leans almost
        // entirely on a strong self-ambient to read as solid mid-brown wood.
        let cap_mat = Material::default()
            .with_ambient(3.4)
            .with_diffuse(0.4)
            .with_specular(0.0)
            .with_texture(grain_texture(style.wall));
        for &v in &[OPEN_EDGE, (OPEN_EDGE + 1) % poly.len()] {
            scene.add_object(
                SceneObject::new(quad(poly[v], rim[v], top_outer[v], outer_bot[v], Vec3::Z))
                    .with_material(cap_mat.clone()),
            );
        }
        // The open front's floor edge, so the felt reads as a pad with thickness:
        // a thin band of felt on top of the tray's wooden base, dropping to the
        // table. Faces the open front (+z, toward you). `poly[2]`/`poly[3]` are the
        // front-right/front-left floor corners at the felt line.
        let (fr, fl) = (poly[2], poly[3]);
        let drop = |p: Vec3, d: f32| Vec3::new(p.x, p.y - d, p.z);
        let felt_lip = 0.06; // the felt's own visible thickness
        scene.add_object(
            SceneObject::new(quad(
                fl,
                fr,
                drop(fr, felt_lip),
                drop(fl, felt_lip),
                Vec3::Z,
            ))
            .with_material(felt_edge_mat),
        );
        scene.add_object(
            SceneObject::new(quad(
                drop(fl, felt_lip),
                drop(fr, felt_lip),
                drop(fr, floor_thick),
                drop(fl, floor_thick),
                Vec3::Z,
            ))
            .with_material(wall_mat.clone()),
        );
    }

    // The room floor: a broad wooden-floorboard plane dropped well below the table
    // so the table reads as raised furniture, not the ground. It recedes toward the
    // backdrop; the plank seams run front-to-back and converge into the distance
    // like real boards, which is what actually reads as a floor at this tiny frame.
    {
        use crate::physics::HY;
        let y = -HY - 1.15; // sits below the table's apron, with daylight between
        let (x0, x1, z0, z1) = (-26.0, 26.0, -26.0, 9.0);
        let ts = 5.0; // board tile size in world units — ~1.25-unit-wide planks
        // Boards run front-to-back (seams at constant x, converging into the
        // distance). Unlike left-to-right boards — whose seams bunch and blur into
        // the shallow grazing band behind the tray — these stay separated across the
        // width, so the floor still reads as boards at a shallow angle, not "blank".
        let uv = |x: f32, z: f32| (x / ts, z / ts);
        // A single quad, double-sided. Its near edge sits behind the camera, so its
        // triangles straddle the near plane — which used to make the rasterizer drop
        // the whole floor at shallow angles. The pipeline now *clips* at the near
        // plane instead of rejecting (see `render3d::pipeline`), so one plain quad
        // draws correctly from every angle; no tessellation needed.
        let floor = dquad(
            [
                Vec3::new(x0, y, z1),
                Vec3::new(x1, y, z1),
                Vec3::new(x1, y, z0),
                Vec3::new(x0, y, z0),
            ],
            Vec3::Y,
            [uv(x0, z1), uv(x1, z1), uv(x1, z0), uv(x0, z0)],
        );
        // Pure ambient (no diffuse), so the boards are one even warm wood tone with
        // no key-light pool streaking a bright patch across them. The average board
        // tone is matched to the backdrop's base (below), so the floor→backdrop
        // horizon reads as one continuous surface — no seam, no void; the plank
        // seams break up that horizon so it never lands as a hard line.
        let mat = Material::default()
            .with_ambient(2.8)
            .with_diffuse(0.0)
            .with_specular(0.0)
            .with_texture(floor_texture(Rgb(156, 116, 80))); // warm oak floorboards
        scene.add_object(SceneObject::new(floor).with_material(mat));
    }

    // A casino rug laid on the boards under the table: a broad dark-red mass that
    // grounds the furniture so the table doesn't float on the bare floor. Two flat
    // quads — a deeper oxblood border band under a lighter crimson field — sit a
    // hair above the boards (a small y offset dodges z-fighting). Pure ambient like
    // the boards, so no key-light pool streaks it; plain tones, no thin borders
    // (they'd shimmer at this frame) — the colour mass is the whole effect.
    {
        use crate::physics::HY;
        let y0 = -HY - 1.15 + 0.03; // just above the floorboards
        let rug_mat = |c: Rgb| {
            Material::default()
                .with_color(c)
                .with_ambient(2.6)
                .with_diffuse(0.0)
                .with_specular(0.0)
        };
        let rug_quad = |x0: f32, x1: f32, z0: f32, z1: f32, y: f32| {
            dquad(
                [
                    Vec3::new(x0, y, z1),
                    Vec3::new(x1, y, z1),
                    Vec3::new(x1, y, z0),
                    Vec3::new(x0, y, z0),
                ],
                Vec3::Y,
                [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            )
        };
        // Outer border band. Generous toward the viewer and the sides — in the
        // wide establishing view (shaking, mid-roll) the boards that show are the
        // flanks beside the table and the strip in front of its apron, and the rug
        // must peek out there — but the FAR edge is capped at z = −4.5: the
        // near-overhead read sees only a narrow floor band behind the tray, and the
        // boards must stay clearly visible in it, so the rug reads as furniture
        // dressing, never the room's floor colour. Don't push the far edge back.
        scene.add_object(
            SceneObject::new(rug_quad(-8.0, 8.0, -4.5, 6.8, y0))
                .with_material(rug_mat(Rgb(50, 16, 18))),
        );
        // Inner field, a hair higher so it wins the depth test over the border.
        scene.add_object(
            SceneObject::new(rug_quad(-7.0, 7.0, -3.9, 5.9, y0 + 0.02))
                .with_material(rug_mat(Rgb(96, 30, 31))),
        );
    }

    // The small table the tray + chips rest on: a wood slab a little larger than
    // the tray's footprint, with a visible front and side **apron** (edge) so it
    // reads as a raised piece of furniture rather than another floor. The top
    // catches the overhead key; the aprons face away from it and lean on a strong
    // self-ambient to stay legible, like the tray's own outer faces.
    {
        use crate::physics::{HY, HZ};
        let top = -HY - FLOOR_THICK; // the tray's wooden base sits flush on it
        let bot = top - 0.6; // apron depth — the thickness of the tabletop
        // The slab's depth follows the tray: the outer rail reaches ≈ HZ + 0.75
        // in z, and the chips sit just past the open front (≈ HZ + 1), so the
        // z extents keep a fixed apron margin beyond both rather than assuming
        // the tray's old depth. Width clears the rail + chips with room spare.
        let (x0, x1, z0, z1) = (-5.4, 5.4, -(HZ + 1.7), HZ + 2.5); // clears tray, walls, chips
        let ts = 2.6;
        let uv = |a: f32, b: f32| (a / ts, b / ts);
        let top_mat = Material::default()
            .with_ambient(1.6)
            .with_diffuse(1.0)
            .with_specular(0.07)
            .with_shininess(7.0)
            .with_texture(grain_texture(Rgb(96, 70, 50))); // warm tabletop wood
        let apron_mat = Material::default()
            .with_ambient(2.6) // faces away from the key; self-ambient carries it
            .with_diffuse(0.6)
            .with_specular(0.0)
            .with_texture(grain_texture(Rgb(74, 54, 38))); // the table's edge, in shade
        // Top surface (front-facing from above).
        scene.add_object(
            SceneObject::new(dquad(
                [
                    Vec3::new(x0, top, z1),
                    Vec3::new(x1, top, z1),
                    Vec3::new(x1, top, z0),
                    Vec3::new(x0, top, z0),
                ],
                Vec3::Y,
                [uv(x0, z1), uv(x1, z1), uv(x1, z0), uv(x0, z0)],
            ))
            .with_material(top_mat),
        );
        // Front apron (faces +z, toward you) and the two side aprons.
        scene.add_object(
            SceneObject::new(dquad(
                [
                    Vec3::new(x0, top, z1),
                    Vec3::new(x1, top, z1),
                    Vec3::new(x1, bot, z1),
                    Vec3::new(x0, bot, z1),
                ],
                Vec3::Z,
                [uv(x0, top), uv(x1, top), uv(x1, bot), uv(x0, bot)],
            ))
            .with_material(apron_mat.clone()),
        );
        for (xf, nx) in [(x0, -1.0f32), (x1, 1.0f32)] {
            scene.add_object(
                SceneObject::new(dquad(
                    [
                        Vec3::new(xf, top, z0),
                        Vec3::new(xf, top, z1),
                        Vec3::new(xf, bot, z1),
                        Vec3::new(xf, bot, z0),
                    ],
                    Vec3::new(nx, 0.0, 0.0),
                    [uv(z0, top), uv(z1, top), uv(z1, bot), uv(z0, bot)],
                ))
                .with_material(apron_mat.clone()),
            );
        }
    }

    // Poker-chip stacks on the table just outside the open front corners, framing
    // the shot with casino colour and foreground depth. Pure décor — the physics
    // never sees them; placed low and to the sides so they frame, not block.
    {
        use crate::physics::{HX, HY, HZ};
        use crate::render3d::mesh::{Mesh, Vertex};
        let chip_cols = [
            Rgb(198, 44, 44),   // red
            Rgb(28, 140, 64),   // green
            Rgb(30, 30, 36),    // black
            Rgb(216, 216, 206), // white
            Rgb(44, 84, 184),   // blue
        ];
        let seg = 12u32;
        let (r, ch) = (0.42, 0.09); // chip radius, height
        let table_y = -HY - FLOOR_THICK; // the tabletop the stacks rest on
        // A cheap integer hash → u32, so stack heights and per-chip colour order
        // are varied deterministically (no RNG). Different `k` picks different
        // draws off the same stack index, so no two stacks share a tidy repeat.
        let chip_rnd = |si: u32, k: u32| {
            hash32(
                si.wrapping_mul(0x9E37_79B1)
                    .wrapping_add(k.wrapping_mul(2_246_822_519))
                    .wrapping_add(1),
            )
        };
        let mut chip_stack = |cx: f32, cz: f32, n: usize, si: u32| {
            for j in 0..n {
                // Hashed per (stack, chip): a hand-stacked jumble, not a fixed cycle.
                // The `+ 7` salt is picked so the four stacks' *top* chips come out
                // pairwise distinct with no red on top — an eye-catching red top
                // chip in the front corners pulls the gaze off the dice.
                let col = chip_cols[chip_rnd(si.wrapping_mul(101).wrapping_add(7), j as u32)
                    as usize
                    % chip_cols.len()];
                let (y0, y1) = (table_y + j as f32 * ch, table_y + j as f32 * ch + ch * 0.82);
                let mut verts = Vec::new();
                let mut idx = Vec::new();
                // Side wall (double-sided so winding never culls it).
                for s in 0..seg {
                    let (a0, a1) = (
                        std::f32::consts::TAU * s as f32 / seg as f32,
                        std::f32::consts::TAU * (s + 1) as f32 / seg as f32,
                    );
                    let (n0, n1) = (
                        Vec3::new(a0.cos(), 0.0, a0.sin()),
                        Vec3::new(a1.cos(), 0.0, a1.sin()),
                    );
                    let b = verts.len() as u32;
                    verts.push(Vertex::new(Vec3::new(cx + n0.x * r, y0, cz + n0.z * r), n0));
                    verts.push(Vertex::new(Vec3::new(cx + n1.x * r, y0, cz + n1.z * r), n1));
                    verts.push(Vertex::new(Vec3::new(cx + n1.x * r, y1, cz + n1.z * r), n1));
                    verts.push(Vertex::new(Vec3::new(cx + n0.x * r, y1, cz + n0.z * r), n0));
                    idx.extend_from_slice(&[
                        b,
                        b + 1,
                        b + 2,
                        b,
                        b + 2,
                        b + 3,
                        b,
                        b + 2,
                        b + 1,
                        b,
                        b + 3,
                        b + 2,
                    ]);
                }
                // Top cap (only the top chip's is really seen; cheap to give each).
                let cb = verts.len() as u32;
                verts.push(Vertex::new(Vec3::new(cx, y1, cz), Vec3::Y));
                for s in 0..seg {
                    let a = std::f32::consts::TAU * s as f32 / seg as f32;
                    verts.push(Vertex::new(
                        Vec3::new(cx + a.cos() * r, y1, cz + a.sin() * r),
                        Vec3::Y,
                    ));
                }
                for s in 0..seg {
                    let (a, b) = (cb + 1 + s, cb + 1 + (s + 1) % seg);
                    idx.extend_from_slice(&[cb, a, b, cb, b, a]); // double-sided so the top shows
                }
                let mat = Material::default()
                    .with_color(col)
                    .with_ambient(0.7) // a touch self-lit so they read outside the key pool
                    .with_diffuse(0.9)
                    .with_specular(0.12)
                    .with_shininess(10.0);
                scene.add_object(SceneObject::new(Mesh::new(verts, idx)).with_material(mat));
            }
        };
        // A little cluster at each open-front corner: paired stacks of hash-varied
        // height, so the pile reads as casually built rather than two tidy towers.
        // Positions are hand-placed to frame the shot; heights come off the hash.
        let spots = [
            (-(HX - 0.3), HZ + 0.2),
            (-(HX - 1.15), HZ + 0.5),
            (HX - 0.2, HZ + 0.5),
            (HX - 1.1, HZ + 0.2),
        ];
        for (si, &(cx, cz)) in spots.iter().enumerate() {
            let n = 3 + (chip_rnd(si as u32, 7) % 5) as usize; // 3..7 chips high
            chip_stack(cx, cz, n, si as u32);
        }
    }

    // A soft contact patch under each near-floor die grounds it on the felt: a
    // dark disc just above the floor, mostly hidden by the die so only its rim
    // shows, like a contact shadow. Drawn before the dice so they occlude it;
    // airborne dice mid-throw are too high to cast one. Skipped while shaking —
    // the dice are gathered in the cup, so they cast nothing on the felt.
    if !app.shaking() {
        use crate::physics::{DIE_R, HY};
        use crate::render3d::mesh::{Mesh, Vertex};
        let f = style.floor;
        // A shadow tone is the felt darkened by `k`, lit like the felt so its edge
        // blends into the surrounding surface rather than reading as a black hole.
        let tone = |k: f32| {
            Material::default()
                .with_color(f.scale(k))
                .with_ambient(0.9)
                .with_diffuse(0.65)
                .with_specular(0.0)
        };
        // Two layers: a wide, faint penumbra under a smaller, darker core. Both are
        // the die's *own* silhouette flattened onto the felt (so the shadow matches
        // the die's shape), and the two tones + the 2× downsample soften the edge.
        // (expand around the die centre, height above the floor, darkness).
        let layers = [
            (1.55_f32, 0.015_f32, 0.72_f32),
            (1.06_f32, 0.028_f32, 0.5_f32),
        ];
        for die in &app.dice {
            if die.pos.y + HY > 0.8 {
                continue; // too far above the felt to cast a contact shadow
            }
            let src = crate::render3d::dice::mesh_for(die.sides);
            // The die's triangles, double-sided so the flattened silhouette fills
            // solid whichever way each face ends up pointing once collapsed.
            let mut idx = src.indices.clone();
            for tri in src.indices.chunks_exact(3) {
                idx.extend_from_slice(&[tri[0], tri[2], tri[1]]);
            }
            for &(expand, dy, dark) in &layers {
                let verts: Vec<Vertex> = src
                    .vertices
                    .iter()
                    .map(|v| {
                        let wp = die.rot * (v.position * DIE_R) + die.pos; // die vertex in world
                        let sx = die.pos.x + (wp.x - die.pos.x) * expand;
                        let sz = die.pos.z + (wp.z - die.pos.z) * expand;
                        Vertex::new(Vec3::new(sx, -HY + dy, sz), Vec3::Y) // flattened onto the felt
                    })
                    .collect();
                scene.add_object(
                    SceneObject::new(Mesh::new(verts, idx.clone())).with_material(tone(dark)),
                );
            }
        }
    }

    // While the cup is shaking the dice are gathered inside it, so don't draw them
    // (or their numbers, below) on the felt then — the cup stands in for them.
    // Without this the *previous* roll's settled dice linger on the table and their
    // number overlays paint in front of the cup and its power meter.
    if !app.shaking() {
        for die in &app.dice {
            let color = if die.kept {
                die_rgb(die.color_idx)
            } else {
                Rgb(90, 90, 90)
            };
            scene.add_object(
                SceneObject::new(dice::mesh_for(die.sides))
                    // A soft, broad sheen rather than the default sharp plastic hotspot.
                    .with_material(
                        Material::default()
                            .with_color(color)
                            .with_specular(0.28)
                            .with_shininess(12.0),
                    )
                    .with_transform(Transform {
                        position: die.pos,
                        rotation: die.rot,
                        scale: Vec3::splat(crate::physics::DIE_R),
                    }),
            );
        }
    }
    // The dice cup, while shaking: an open tin tumbler that sits low in the
    // tray. Everything rides the one shake clock — the sway, a bob that bounces
    // at each direction flip, a lean *into* the swing, and a high-frequency
    // rattle jitter — all scaled by the building power, so the cup visibly works
    // harder as the meter climbs. Kept within the walls and near the floor so it
    // stays fully in frame.
    if app.shaking() {
        let grip = 0.35 + 0.65 * app.power();
        let t = app.shake_t() * crate::app::CUP_SWAY_RATE; // the sway/rattle phase
        // The cup's normalised sway comes from the app — the same value the
        // throw aims away from — so the dice always fly from the drawn cup.
        let sway = app.cup_offset() * crate::physics::HX * 0.6;
        let bob = (t * 2.0).sin().abs() * 0.09 * grip; // two hops per sway cycle
        let lean = t.cos() * 0.14 * grip; // sway velocity ∝ cos: lean into the swing
        let rattle = (app.shake_t() * 23.0).sin() * 0.05 * grip;
        scene.add_object(
            SceneObject::new(dice::cup())
                // Tin: a cool grey with a hard, tight highlight — the metal is
                // the specular, and the rolled lip is what catches it. Ambient
                // is lifted so the shadowed bowl stays grey metal, not a void.
                .with_material(
                    Material::default()
                        .with_color(Rgb(176, 182, 190))
                        .with_ambient(0.95)
                        .with_diffuse(0.55)
                        .with_specular(0.9)
                        .with_shininess(24.0),
                )
                .with_transform(Transform {
                    position: Vec3::new(sway, -1.15 + bob, crate::physics::HZ * 0.25),
                    // Tip the mouth toward the camera so the shaded hollow shows —
                    // that opening is what reads "cup" instead of "cylinder".
                    rotation: Quat::from_rotation_z(lean + rattle) * Quat::from_rotation_x(0.18),
                    scale: Vec3::new(0.9, 1.05, 0.9),
                }),
        );
    }

    // A warm tungsten point light hung low over the front of the tray throws a
    // hot pool on the felt that falls to shadow at the walls (a spotlit table);
    // a cool rim from high behind catches the die tops and back rail to pop the
    // silhouettes — warm key against cool rim shapes the dice; a dim directional
    // fill keeps the side walls in form, over a warm ambient floor. Per-fragment
    // shading makes the falloff smooth.
    scene.add_light(Light::ambient(Rgb(255, 236, 210), style.ambient));
    // A near-subliminal sway of the hung key light: slow sines of the clock (tens
    // of seconds per cycle) nudge its x/z so the felt's hotspot breathes like a
    // lamp on a chain rather than sitting nailed in place.
    let kt = app.clock();
    let key_sway = Vec3::new((kt * 0.19).sin() * 0.16, 0.0, (kt * 0.13).cos() * 0.12);
    scene.add_light(Light::Point {
        position: Vec3::new(0.0, 2.4, 1.1) + key_sway,
        color: style.key,
        intensity: 3.0 * (1.0 + 0.22 * app.impact_energy()), // flinch on hard bounces
    });
    scene.add_light(Light::Point {
        position: Vec3::new(0.0, 3.4, -4.8), // far + dim → 1/d² keeps it to the tops
        color: Rgb(198, 214, 255),           // pale blue-white: cool rim vs the warm key
        intensity: 1.0,
    });
    scene.add_light(Light::directional(Vec3::new(0.3, -0.5, -0.35), style.fill));

    // A natural crit floods the tray with a warm gold flare that fades fast.
    if flash > 0.0 {
        scene.add_light(Light::Point {
            position: Vec3::new(0.0, 2.8, 0.6),
            color: Rgb(255, 216, 140),
            intensity: flash * 4.0,
        });
    }

    render3d_view::draw(
        frame.buffer_mut(),
        inner,
        &scene,
        &camera,
        RenderMode::HalfBlock,
    );

    draw_arena_overlays(frame, app, inner, &camera);
}

/// The arena block title: names what the dice are doing (shaking, a named throw,
/// settled, idle). Shared by the software `render_arena` and the Bevy path.
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
/// every die (riding the face that points at us — anchored to that face, faded
/// edge-on, burned to the RNG value on settle; skipped while shaking, dice in the
/// cup), crit/fumble particles, the shake power meter, the release echo, and the
/// idle hint. Shared by the software `render_arena` and the Bevy path, so both
/// draw the identical ceremony; `camera` MUST be the one the arena was rendered
/// through, so the numbers and bursts land on their dice.
fn draw_arena_overlays(
    frame: &mut Frame,
    app: &App,
    inner: Rect,
    camera: &crate::render3d::camera::Camera,
) {
    if !app.shaking() {
        let (cols, rows) = (inner.width as f32, inner.height as f32);
        // One number size for the whole roll, so the dice read the same — never a
        // big number on the nearest die and single cells on the rest. Size it from a
        // reference die at the felt centre, fit it within the read-face (not the
        // whole die box), and reserve room for the widest value any die here can
        // show, so a two-digit d20 lands at the same scale a one-digit d6 uses.
        let ref_center =
            crate::render3d::math::Vec3::new(0.0, -crate::physics::HY + crate::physics::DIE_R, 0.0);
        let (ref_w, ref_h) = die_screen_extent(camera, ref_center, cols, rows);
        let max_digits = app
            .dice
            .iter()
            .map(|d| d.sides.to_string().len() as i32)
            .max()
            .unwrap_or(1);
        let num_scale = number_scale(ref_w * FACE_FRAC_W, ref_h * FACE_FRAC_H, max_digits);
        for die in &app.dice {
            draw_die_number(frame, inner, camera, die, cols, rows, num_scale);
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
}

/// Compose the full interactive frame for the **Bevy** arena: the same four-row
/// layout and chrome as [`render`], but the arena panel is the CPU-read Bevy
/// render blitted as half-blocks (fg = upper pixel, bg = lower) rather than the
/// software `render_arena`. `pixels` is the row-padded RGBA8 readback of a
/// `img_w`×`img_h` image sized to this arena's inner cell grid, so the blit is
/// 1:1 and the overlays (projected through the same `live_camera`) land on their
/// dice. Returns the arena inner size so the scene can size its render target.
pub fn render_bevy(
    frame: &mut Frame,
    app: &mut App,
    pixels: &[u8],
    img_w: u32,
    img_h: u32,
) -> (u16, u16) {
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
    if inner.width >= 4 && inner.height >= 3 {
        // Feed the arena size to the sim (launch/particle geometry reads it) and
        // report it back so the render target can track it.
        app.arena_w = inner.width as f32;
        app.arena_h = inner.height as f32;
        view = (inner.width, inner.height * 2);

        blit_bevy_arena(frame.buffer_mut(), inner, pixels, img_w, img_h);

        let aspect = crate::render3d_view::arena_aspect(inner.width as f32, inner.height as f32);
        let camera = crate::render3d_view::live_camera(
            app.camera_shake(),
            aspect,
            app.focus(),
            app.clock(),
            app.flash(),
        );
        draw_arena_overlays(frame, app, inner, &camera);
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

    view
}

/// Blit a row-padded RGBA8 Bevy render (sized to `inner`'s half-block grid, so
/// `img_w == inner.width` and `img_h == inner.height*2`) into `inner` as
/// half-block cells: each cell takes its two stacked pixels as fg (upper) and bg
/// (lower). A no-op until the first readback of the right size lands.
fn blit_bevy_arena(
    buf: &mut ratatui::buffer::Buffer,
    inner: Rect,
    pixels: &[u8],
    img_w: u32,
    img_h: u32,
) {
    let stride = (img_w as usize * 4).div_ceil(256) * 256; // wgpu 256-byte row pad
    if img_w == 0 || img_h == 0 || pixels.len() < stride * img_h as usize {
        return;
    }
    let sample = |x: u32, y: u32| -> (u8, u8, u8) {
        let x = x.min(img_w - 1) as usize;
        let y = y.min(img_h - 1) as usize;
        let i = y * stride + x * 4;
        (pixels[i], pixels[i + 1], pixels[i + 2])
    };
    // A warm-graded radial vignette (quartic, biting only near the edges) so the
    // void recedes and the eye is pulled into the lit tray — the same grade the
    // software `render3d_view::vignette` applies.
    let (fw, fh) = (inner.width as f32, inner.height as f32 * 2.0);
    let graded = |c: (u8, u8, u8), nx: f32, ny: f32| -> Color {
        let d2 = ((nx * nx + ny * ny) / 0.5).min(1.0);
        let f = 1.0 - 0.34 * d2 * d2;
        let (fr, fg, fb) = (f * 1.04, f, f * 0.95);
        Color::Rgb(
            (c.0 as f32 * fr).min(255.0) as u8,
            (c.1 as f32 * fg).min(255.0) as u8,
            (c.2 as f32 * fb) as u8,
        )
    };
    for row in 0..inner.height {
        for col in 0..inner.width {
            let ix = (col as u32).min(img_w - 1);
            let up = sample(ix, row as u32 * 2);
            let lo = sample(ix, row as u32 * 2 + 1);
            let nx = (col as f32 + 0.5) / fw - 0.5;
            let cell = &mut buf[(inner.x + col, inner.y + row)];
            cell.set_char('▀');
            cell.set_style(
                Style::default()
                    .fg(graded(up, nx, (row as f32 * 2.0 + 0.5) / fh - 0.5))
                    .bg(graded(lo, nx, (row as f32 * 2.0 + 1.5) / fh - 0.5)),
            );
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

/// The throw's power meter, drawn while shaking (the Throw). The cup itself is
/// now a real 3D tumbler in the arena scene ([`dice::cup`](crate::render3d::dice::cup));
/// this is just the instrument read-out above it — fixed and centred so your eye
/// can time the release against it, colour-coding the power you'd catch right now.
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
    // colourless and dim while the die is still tumbling (the face on show is a
    // flickering decoy, not the outcome), then bold in the die's colour once it
    // settles. A dropped die stays greyed whatever it lands on.
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
/// itself to its content (capped to the frame), blanks what's behind it, and
/// scrolls by `scroll` lines when the content is taller than the frame allows.
/// Returns the scroll offset actually used — clamped so the last line can just
/// reach the bottom and no further — so the caller can store the corrected
/// value back. Shared by all three pop-out panes.
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
        heading("Stakes & multiply"),
        syntax_row("d20 > 15", "beat a target (or 'vs'); < N rolls under"),
        syntax_row("4d6*2", "double this term's sum (modifiers stack)"),
        Line::raw(""),
        heading("Exploding"),
        syntax_row("3d6!", "a max face rolls another die"),
        syntax_row("d10!>8", "explode on any face over 8 (also !=N, !<N)"),
        Line::raw(""),
        heading("The Throw"),
        syntax_row(
            "Enter",
            "shake the cup; Enter again throws — harder at the peak",
        ),
        syntax_row("Tab", "cycle Enter's mode: shake → roll → insta"),
        syntax_row("Esc", "put them down. Power never touches the values."),
        Line::from(Span::styled(
            "  Separators: +  -  ,  space  or just write dice next to each other.",
            Style::default().fg(Color::DarkGray),
        )),
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

/// The editable dice expression: a fixed prompt, then the expression with a
/// block caret (reverse-video over the character it covers, a solid block at the
/// end of the line). The expression scrolls horizontally to keep the caret in
/// view when it's wider than the row, so mid-line editing never runs off-screen.
/// Every span borrows `app.input`, so drawing the line allocates nothing.
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
