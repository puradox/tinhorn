pub use glam::{Mat4, Quat, Vec2, Vec3, Vec4};

/// Construct a perspective projection matrix.
// tinhorn: glam 0.33 moved these constructors off `Mat4` into the `camera`
// module and deprecated the old methods; `proj::directx::perspective` /
// `view::look_at_mat4` are the drop-in replacements for `*_rh`.
pub fn perspective(fov_y_radians: f32, aspect: f32, z_near: f32, z_far: f32) -> Mat4 {
    glam::camera::rh::proj::directx::perspective(fov_y_radians, aspect, z_near, z_far)
}

/// Construct a look-at view matrix (right-handed).
pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    glam::camera::rh::view::look_at_mat4(eye, target, up)
}
