//! The scene camera. Its definition (and the arena's camera choreography) moved
//! to [`tinhorn_core::view_math`] so the terminal renderer and the shared
//! world→screen math agree on one `Camera` type. Re-exported here so the
//! `render3d` pipeline keeps referring to `render3d::camera::Camera`.

pub use tinhorn_core::view_math::{Camera, Projection};
