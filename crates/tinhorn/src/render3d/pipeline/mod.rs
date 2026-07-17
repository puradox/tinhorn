pub mod fragment;
pub mod framebuffer;
pub mod rasterize;
pub mod vertex;

use crate::render3d::camera::{Camera, Projection};
use crate::render3d::scene::Scene;
use rasterize::rasterize_triangle;
use vertex::{clip_near, clip_to_screen, transform_to_clip};

pub use framebuffer::Framebuffer;

/// Which rendering pipeline to use. tinhorn: only the rasterizer is vendored;
/// the CPU/GPU ray-tracing variants were dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pipeline {
    /// Scanline rasterization (fast).
    #[default]
    Rasterize,
}

/// Execute the full rendering pipeline for a scene.
pub fn render(scene: &Scene, camera: &Camera, fb: &mut Framebuffer) {
    fb.clear(scene.background);

    let vw = fb.width as f32;
    let vh = fb.height as f32;

    if vw < 1.0 || vh < 1.0 {
        return;
    }

    let aspect = vw / vh;
    let view = camera.view_matrix();
    let proj = camera.projection_matrix(aspect);
    let view_proj = proj * view;
    // The near-plane distance to clip against. In any RH perspective `clip.w` is
    // the distance in front of the camera, so `clip.w >= near` is the near plane
    // regardless of the depth (GL vs D3D) convention.
    let Projection::Perspective { near, .. } = camera.projection;

    for obj in &scene.objects {
        if !obj.visible {
            continue;
        }

        let model = obj.transform.matrix();
        // Normal matrix: transpose of inverse of upper-left 3x3 of model matrix.
        // For uniform scaling, model itself works. For non-uniform, we need the inverse transpose.
        let normal_matrix = model.inverse().transpose();

        let mesh = &obj.mesh;

        for tri in 0..mesh.triangle_count() {
            let i0 = mesh.indices[tri * 3] as usize;
            let i1 = mesh.indices[tri * 3 + 1] as usize;
            let i2 = mesh.indices[tri * 3 + 2] as usize;

            let to_clip = |v: &crate::render3d::mesh::Vertex| {
                transform_to_clip(
                    v.position,
                    v.normal,
                    v.uv,
                    &model,
                    &view_proj,
                    &normal_matrix,
                )
            };
            let tri = [
                to_clip(&mesh.vertices[i0]),
                to_clip(&mesh.vertices[i1]),
                to_clip(&mesh.vertices[i2]),
            ];

            // Clip against the near plane, then fan-triangulate the (≤4-vertex)
            // result. A triangle straddling the camera is cut, not dropped, so
            // huge ground planes whose far reaches sit behind the camera still draw.
            let (poly, n) = clip_near(tri, near);
            for k in 1..n.saturating_sub(1) {
                let tv0 = clip_to_screen(&poly[0], vw, vh);
                let tv1 = clip_to_screen(&poly[k], vw, vh);
                let tv2 = clip_to_screen(&poly[k + 1], vw, vh);

                // Cheap reject: whole triangle off one edge of the frame.
                let all_outside = (tv0.screen_pos.x < 0.0
                    && tv1.screen_pos.x < 0.0
                    && tv2.screen_pos.x < 0.0)
                    || (tv0.screen_pos.x > vw && tv1.screen_pos.x > vw && tv2.screen_pos.x > vw)
                    || (tv0.screen_pos.y < 0.0 && tv1.screen_pos.y < 0.0 && tv2.screen_pos.y < 0.0)
                    || (tv0.screen_pos.y > vh && tv1.screen_pos.y > vh && tv2.screen_pos.y > vh);

                if !all_outside {
                    rasterize_triangle(
                        &tv0,
                        &tv1,
                        &tv2,
                        &obj.material,
                        &scene.lights,
                        camera.position,
                        scene.fog,
                        scene.background,
                        fb,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::render3d::camera::Camera;
    use crate::render3d::color::Rgb;
    use crate::render3d::light::Light;
    use crate::render3d::material::Material;
    use crate::render3d::math::Vec3;
    use crate::render3d::object::SceneObject;
    use crate::render3d::pipeline::framebuffer::Framebuffer;
    use crate::render3d::primitives;
    use crate::render3d::scene::Scene;

    // tinhorn: the whole vendored rasterize path runs end-to-end and actually
    // draws geometry. The default camera looks straight at the origin, where the
    // unit cube sits, so the frame's center must be covered and its depth written.
    #[test]
    fn renders_a_lit_cube_over_the_frame_center() {
        let mut scene = Scene::new();
        scene.add_object(
            SceneObject::new(primitives::cube())
                .with_material(Material::default().with_color(Rgb(200, 120, 60))),
        );
        scene.add_light(Light::ambient(Rgb::WHITE, 0.4));
        scene.add_light(Light::directional(Vec3::new(-1.0, -1.0, -1.0), Rgb::WHITE));

        let mut fb = Framebuffer::new(128, 128);
        render(&scene, &Camera::default(), &mut fb);

        let covered = fb.alpha.iter().filter(|&&a| a == 255).count();
        assert!(
            covered > 200,
            "cube should cover many pixels, covered {covered}"
        );

        let center = fb.index(64, 64);
        assert_eq!(fb.alpha[center], 255, "cube should cover the frame center");
        assert!(fb.depth[center].is_finite(), "center depth must be written");
        assert_ne!(
            fb.color[center], scene.background,
            "center should be shaded cube, not background",
        );
    }

    // tinhorn: near-plane clipping. A ground plane whose near edge sits *behind*
    // the camera has triangles that straddle the near plane. The old pipeline
    // rejected any straddling triangle outright, so such a floor vanished at
    // shallow angles; clipping must cut it at the plane and keep the visible part.
    #[test]
    fn near_plane_clip_keeps_a_floor_whose_near_edge_is_behind_the_camera() {
        use crate::render3d::camera::Projection;
        use crate::render3d::mesh::{Mesh, Vertex};

        let mut scene = Scene::new();
        // A big floor at y = -1, spanning from z = +4 (behind the camera at z = 0)
        // back to z = -40. Double-sided; emissive so it shows without a key light.
        let (y, x0, x1, z_near, z_far) = (-1.0f32, -20.0, 20.0, 4.0, -40.0);
        let floor = Mesh::new(
            vec![
                Vertex::new(Vec3::new(x0, y, z_near), Vec3::Y),
                Vertex::new(Vec3::new(x1, y, z_near), Vec3::Y),
                Vertex::new(Vec3::new(x1, y, z_far), Vec3::Y),
                Vertex::new(Vec3::new(x0, y, z_far), Vec3::Y),
            ],
            vec![0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2],
        );
        scene.add_object(
            SceneObject::new(floor).with_material(
                Material::default()
                    .with_color(Rgb(200, 160, 120))
                    .with_ambient(3.0),
            ),
        );
        scene.add_light(Light::ambient(Rgb::WHITE, 0.5));

        // Above the origin, looking forward and a little down over the floor.
        let camera = Camera {
            position: Vec3::new(0.0, 1.5, 0.0),
            target: Vec3::new(0.0, 0.0, -8.0),
            up: Vec3::Y,
            projection: Projection::Perspective {
                fov_y: std::f32::consts::FRAC_PI_4,
                near: 0.1,
                far: 100.0,
            },
        };

        let mut fb = Framebuffer::new(128, 128);
        render(&scene, &camera, &mut fb);

        // With the old reject-on-straddle pipeline this floor drew nothing.
        let covered = fb.alpha.iter().filter(|&&a| a == 255).count();
        assert!(
            covered > 2000,
            "the clipped floor should fill much of the frame, covered {covered}"
        );
        let low = fb.index(64, 112); // low-centre, where the receding floor lands
        assert_eq!(
            fb.alpha[low], 255,
            "a floor whose near edge is behind the camera must still render"
        );
    }
}
