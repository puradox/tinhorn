use crate::render3d::math::{Mat4, Vec3, Vec4};

/// A vertex transformed all the way to screen space, ready to rasterize.
#[derive(Debug, Clone, Copy)]
pub struct TransformedVertex {
    /// Screen-space position (x, y in pixels, z = depth 0..1).
    pub screen_pos: Vec3,
    /// World-space position (for lighting calculations).
    pub world_pos: Vec3,
    /// World-space normal (for lighting calculations).
    pub world_normal: Vec3,
    /// Texture coordinates.
    pub uv: [f32; 2],
    /// tinhorn: reciprocal of clip-space w. The rasterizer weights the
    /// screen-space barycentrics by this to interpolate attributes
    /// perspective-correctly, so textures don't warp on angled faces.
    pub inv_w: f32,
}

/// A vertex in **clip space** — after the model/view/projection transform but
/// *before* the perspective divide. This is the space the near-plane clip runs
/// in: the plane test (`clip.w >= near`) and attribute interpolation are both
/// linear here, and no divide-by-`w` has happened yet, so a vertex behind the
/// camera is just data rather than a NaN. The attributes ride along so a clipped
/// edge interpolates them too.
#[derive(Debug, Clone, Copy)]
pub struct ClipVertex {
    pub clip: Vec4,
    pub world_pos: Vec3,
    pub world_normal: Vec3,
    pub uv: [f32; 2],
}

/// Model space → clip space. No divide, no rejection — near-plane clipping (which
/// needs the pre-divide `w`) happens next, in [`clip_near`].
pub fn transform_to_clip(
    position: Vec3,
    normal: Vec3,
    uv: [f32; 2],
    model: &Mat4,
    view_proj: &Mat4,
    normal_matrix: &Mat4,
) -> ClipVertex {
    let world_pos = model.transform_point3(position);
    let world_normal = normal_matrix.transform_vector3(normal).normalize_or_zero();
    let clip = *view_proj * Vec4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);
    ClipVertex {
        clip,
        world_pos,
        world_normal,
        uv,
    }
}

/// Interpolate two clip-space vertices at parameter `t` — the new vertex where an
/// edge crosses the near plane. Because `clip.w` is affine in world position, the
/// `t` that solves `clip.w == near` is also the world-space edge parameter, so a
/// plain lerp of the attributes lands them on the true crossing point.
fn lerp_clip(a: &ClipVertex, b: &ClipVertex, t: f32) -> ClipVertex {
    let lerp3 = |x: Vec3, y: Vec3| x + (y - x) * t;
    ClipVertex {
        clip: a.clip + (b.clip - a.clip) * t,
        world_pos: lerp3(a.world_pos, b.world_pos),
        world_normal: lerp3(a.world_normal, b.world_normal).normalize_or_zero(),
        uv: [
            a.uv[0] + (b.uv[0] - a.uv[0]) * t,
            a.uv[1] + (b.uv[1] - a.uv[1]) * t,
        ],
    }
}

/// Clip a triangle against the near plane `clip.w >= near` (Sutherland–Hodgman
/// against one plane). Returns the resulting convex polygon (its vertex count in
/// `.1`), at most 4 for a single-plane clip: 3 in → 3, 2 in → 4, 1 in → 3, 0 in
/// → 0. Winding is preserved, so downstream back-face culling still works. This is
/// what a real GPU does instead of dropping any triangle that pokes behind the
/// camera; every kept vertex ends up with `clip.w >= near > 0`, so the later
/// perspective divide is always safe and bounded.
pub fn clip_near(tri: [ClipVertex; 3], near: f32) -> ([ClipVertex; 4], usize) {
    let mut out = [tri[0]; 4];
    let mut n = 0;
    for i in 0..3 {
        let a = tri[i];
        let b = tri[(i + 1) % 3];
        let a_in = a.clip.w >= near;
        let b_in = b.clip.w >= near;
        if a_in {
            out[n] = a;
            n += 1;
        }
        if a_in != b_in {
            let t = (near - a.clip.w) / (b.clip.w - a.clip.w);
            out[n] = lerp_clip(&a, &b, t);
            n += 1;
        }
    }
    (out, n)
}

/// Clip space → screen space: the perspective divide and viewport map. Safe
/// because [`clip_near`] guarantees `clip.w >= near > 0` for every vertex by here.
pub fn clip_to_screen(
    v: &ClipVertex,
    viewport_width: f32,
    viewport_height: f32,
) -> TransformedVertex {
    let inv_w = 1.0 / v.clip.w;
    let ndc = Vec3::new(v.clip.x * inv_w, v.clip.y * inv_w, v.clip.z * inv_w);
    let screen_x = (ndc.x + 1.0) * 0.5 * viewport_width;
    let screen_y = (1.0 - ndc.y) * 0.5 * viewport_height; // flip Y for screen
    let screen_z = (ndc.z + 1.0) * 0.5; // map to [0, 1]
    TransformedVertex {
        screen_pos: Vec3::new(screen_x, screen_y, screen_z),
        world_pos: v.world_pos,
        world_normal: v.world_normal,
        uv: v.uv,
        inv_w,
    }
}
