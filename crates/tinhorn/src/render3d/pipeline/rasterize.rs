use crate::render3d::color::Rgb;
use crate::render3d::math::Vec3;
use crate::render3d::pipeline::fragment::shade_fragment;
use crate::render3d::pipeline::framebuffer::Framebuffer;
use crate::render3d::pipeline::vertex::TransformedVertex;
use crate::render3d::{Light, Material};

/// Rasterize a single triangle into the framebuffer.
#[allow(clippy::too_many_arguments)]
pub fn rasterize_triangle(
    v0: &TransformedVertex,
    v1: &TransformedVertex,
    v2: &TransformedVertex,
    material: &Material,
    lights: &[Light],
    camera_pos: Vec3,
    fog: Option<(f32, f32)>,
    background: Rgb,
    fb: &mut Framebuffer,
) {
    let p0 = v0.screen_pos;
    let p1 = v1.screen_pos;
    let p2 = v2.screen_pos;

    // Signed area via 2D cross product
    let edge1 = p1 - p0;
    let edge2 = p2 - p0;
    let cross_z = edge1.x * edge2.y - edge1.y * edge2.x;

    // After viewport Y-flip, front-facing (originally CCW) triangles have negative cross_z.
    // Positive cross_z = back-facing → cull.
    if cross_z >= 0.0 {
        return;
    }

    // Bounding box (clamped to framebuffer)
    let min_x = p0.x.min(p1.x).min(p2.x).max(0.0) as u32;
    let max_x = p0.x.max(p1.x).max(p2.x).min(fb.width as f32 - 1.0) as u32;
    let min_y = p0.y.min(p1.y).min(p2.y).max(0.0) as u32;
    let max_y = p0.y.max(p1.y).max(p2.y).min(fb.height as f32 - 1.0) as u32;

    if min_x > max_x || min_y > max_y {
        return;
    }

    // cross_z is negative for front-facing triangles (after Y-flip).
    // Edge functions for interior points are POSITIVE (opposite sign of cross_z).
    // Negate cross_z for a positive area divisor so barycentrics come out positive.
    let inv_area = -1.0 / cross_z;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // Edge functions — positive for interior points (since cross_z < 0)
            let w0 = edge_function(p1, p2, px, py);
            let w1 = edge_function(p2, p0, px, py);
            let w2 = edge_function(p0, p1, px, py);

            if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                let b0 = w0 * inv_area;
                let b1 = w1 * inv_area;
                let b2 = w2 * inv_area;

                // Interpolate depth. Window-space z is affine in screen space, so
                // plain barycentric interpolation is already correct — do NOT
                // perspective-divide it (that would bow the depth buffer).
                let depth = b0 * p0.z + b1 * p1.z + b2 * p2.z;

                // Early depth test
                let idx = fb.index(x, y);
                if depth >= fb.depth[idx] {
                    continue;
                }

                // tinhorn: perspective-correct the barycentrics before
                // interpolating any world/UV attribute, so textures and shading
                // don't warp on faces viewed at an angle.
                let (c0, c1, c2) = perspective_correct(b0, b1, b2, v0.inv_w, v1.inv_w, v2.inv_w);

                // Interpolate world-space attributes
                let world_pos = v0.world_pos * c0 + v1.world_pos * c1 + v2.world_pos * c2;
                let world_normal =
                    (v0.world_normal * c0 + v1.world_normal * c1 + v2.world_normal * c2)
                        .normalize_or_zero();

                // Interpolate UV and determine base color
                let base_color = if let Some(tex) = &material.texture {
                    let u = v0.uv[0] * c0 + v1.uv[0] * c1 + v2.uv[0] * c2;
                    let v = v0.uv[1] * c0 + v1.uv[1] * c1 + v2.uv[1] * c2;
                    tex.sample(u, v)
                } else {
                    material.color
                };

                // Fragment shading
                let color = shade_fragment(
                    world_pos,
                    world_normal,
                    base_color,
                    material,
                    lights,
                    camera_pos,
                    fog,
                    background,
                );

                fb.depth[idx] = depth;
                fb.color[idx] = color;
                fb.alpha[idx] = 255;
            }
        }
    }
}

/// Signed area of the parallelogram formed by edge (a→b) and point p.
#[inline(always)]
fn edge_function(a: Vec3, b: Vec3, px: f32, py: f32) -> f32 {
    (px - a.x) * (b.y - a.y) - (py - a.y) * (b.x - a.x)
}

/// tinhorn: turn screen-space barycentric weights into perspective-correct ones
/// by weighting each with the vertex's `1/w` and renormalizing. Screen-space
/// barycentrics interpolate an attribute affinely across the projected triangle,
/// which warps textures on faces seen at an angle; scaling by `1/w` undoes the
/// projection so the interpolation is linear in *world* space instead. When all
/// `w` are equal (no perspective foreshortening) it returns the input unchanged.
#[inline(always)]
fn perspective_correct(
    b0: f32,
    b1: f32,
    b2: f32,
    inv_w0: f32,
    inv_w1: f32,
    inv_w2: f32,
) -> (f32, f32, f32) {
    let p0 = b0 * inv_w0;
    let p1 = b1 * inv_w1;
    let p2 = b2 * inv_w2;
    let sum = p0 + p1 + p2;
    if sum > 0.0 {
        (p0 / sum, p1 / sum, p2 / sum)
    } else {
        (b0, b1, b2)
    }
}

/// Rasterize a wireframe triangle (for debugging).
pub fn rasterize_wireframe(
    v0: &TransformedVertex,
    v1: &TransformedVertex,
    v2: &TransformedVertex,
    color: Rgb,
    fb: &mut Framebuffer,
) {
    draw_line(v0.screen_pos, v1.screen_pos, color, fb);
    draw_line(v1.screen_pos, v2.screen_pos, color, fb);
    draw_line(v2.screen_pos, v0.screen_pos, color, fb);
}

fn draw_line(a: Vec3, b: Vec3, color: Rgb, fb: &mut Framebuffer) {
    let dx = (b.x - a.x).abs();
    let dy = (b.y - a.y).abs();
    let steps = dx.max(dy) as u32;
    if steps == 0 {
        return;
    }
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = (a.x + (b.x - a.x) * t) as u32;
        let y = (a.y + (b.y - a.y) * t) as u32;
        let depth = a.z + (b.z - a.z) * t;
        fb.set_pixel(x, y, depth, color);
    }
}

#[cfg(test)]
mod tests {
    use super::perspective_correct;

    #[test]
    fn foreshortens_toward_the_nearer_vertex() {
        // Equal screen-space weight between a near vertex (w=1 → inv_w=1.0) and a
        // far one (w=4 → inv_w=0.25). Perspective-correct weighting must pull the
        // sample toward the near vertex rather than sitting at the affine 0.5.
        let (near, mid, far) = perspective_correct(0.5, 0.0, 0.5, 1.0, 1.0, 0.25);
        assert!((near - 0.8).abs() < 1e-5, "near weight was {near}");
        assert!(mid.abs() < 1e-5, "middle weight was {mid}");
        assert!((far - 0.2).abs() < 1e-5, "far weight was {far}");
    }

    #[test]
    fn is_identity_when_w_is_uniform() {
        // No foreshortening (all vertices share a w) → weights pass through.
        let (a, b, c) = perspective_correct(0.2, 0.3, 0.5, 2.0, 2.0, 2.0);
        assert!((a - 0.2).abs() < 1e-5);
        assert!((b - 0.3).abs() < 1e-5);
        assert!((c - 0.5).abs() < 1e-5);
    }
}
