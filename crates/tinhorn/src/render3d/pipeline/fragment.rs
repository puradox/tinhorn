use crate::render3d::color::Rgb;
use crate::render3d::light::Light;
use crate::render3d::material::Material;
use crate::render3d::math::Vec3;

/// Phong shading: compute the final color for a fragment.
///
/// All light contributions are accumulated in normalized [0, 1+] space,
/// then multiplied by the material base color and clamped to [0, 255].
#[allow(clippy::too_many_arguments)]
pub fn shade_fragment(
    world_pos: Vec3,
    world_normal: Vec3,
    base_color: Rgb,
    material: &Material,
    lights: &[Light],
    camera_pos: Vec3,
    fog: Option<(f32, f32)>,
    background: Rgb,
) -> Rgb {
    let normal = world_normal;
    let view_dir = (camera_pos - world_pos).normalize_or_zero();

    // Accumulate light intensity per channel in [0, ∞) normalized range
    let mut total_r: f32 = 0.0;
    let mut total_g: f32 = 0.0;
    let mut total_b: f32 = 0.0;

    // tinhorn: an emissive material (no diffuse, no specular — e.g. the bokeh
    // and backdrop, which cover most of the frame) takes nothing from the
    // directional/point lights, so only the ambient terms are worth the loop.
    let lit = material.diffuse != 0.0 || material.specular != 0.0;

    for light in lights {
        match light {
            Light::Ambient { color, intensity } => {
                let factor = intensity * material.ambient;
                total_r += (color.0 as f32 / 255.0) * factor;
                total_g += (color.1 as f32 / 255.0) * factor;
                total_b += (color.2 as f32 / 255.0) * factor;
            }
            _ if !lit => {}
            Light::Directional {
                direction,
                color,
                intensity,
            } => {
                let light_dir = -*direction;
                let (dr, dg, db) =
                    diffuse_specular(normal, light_dir, view_dir, material, *color, *intensity);
                total_r += dr;
                total_g += dg;
                total_b += db;
            }
            Light::Point {
                position,
                color,
                intensity,
            } => {
                let to_light = *position - world_pos;
                let distance = to_light.length();
                let light_dir = to_light / distance;
                let attenuation = intensity / (1.0 + 0.09 * distance + 0.032 * distance * distance);
                let (dr, dg, db) =
                    diffuse_specular(normal, light_dir, view_dir, material, *color, attenuation);
                total_r += dr;
                total_g += dg;
                total_b += db;
            }
        }
    }

    // Multiply accumulated light by base color
    let r = (total_r * base_color.0 as f32).clamp(0.0, 255.0) as u8;
    let g = (total_g * base_color.1 as f32).clamp(0.0, 255.0) as u8;
    let b = (total_b * base_color.2 as f32).clamp(0.0, 255.0) as u8;
    let color = Rgb(r, g, b);

    // tinhorn: depth fog. A fragment recedes toward the scene background over the
    // camera-space distance range `[start, end]`, so far geometry (the floorboards,
    // the backdrop) seats into the room while the near tray and dice stay crisp.
    // `None` skips it entirely — the common case for the other render3d callers.
    if let Some((start, end)) = fog {
        let dist = (camera_pos - world_pos).length();
        let t = fog_smoothstep(start, end, dist);
        if t > 0.0 {
            return color.lerp(background, t);
        }
    }
    color
}

/// Smooth Hermite step in `[e0, e1]`, so fog eases in with distance rather than
/// banding. (render3d has no shared smoothstep; this is the fragment path's own.)
#[inline(always)]
fn fog_smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Compute diffuse + specular contribution from a single light direction.
/// Returns normalized [0, 1+] RGB contribution.
fn diffuse_specular(
    normal: Vec3,
    light_dir: Vec3,
    view_dir: Vec3,
    material: &Material,
    light_color: Rgb,
    intensity: f32,
) -> (f32, f32, f32) {
    // Diffuse (Lambert)
    let n_dot_l = normal.dot(light_dir).max(0.0);
    let diffuse = n_dot_l * material.diffuse * intensity;

    // Specular (Blinn-Phong). tinhorn: skipped for matte materials — powf is
    // the most expensive op in the whole per-fragment path, and most of the
    // frame (felt, floorboards, backdrop, outer walls) has specular == 0.
    let specular = if material.specular != 0.0 {
        let halfway = (light_dir + view_dir).normalize_or_zero();
        let n_dot_h = normal.dot(halfway).max(0.0);
        n_dot_h.powf(material.shininess) * material.specular * intensity
    } else {
        0.0
    };

    let total = diffuse + specular;
    (
        (light_color.0 as f32 / 255.0) * total,
        (light_color.1 as f32 / 255.0) * total,
        (light_color.2 as f32 / 255.0) * total,
    )
}
