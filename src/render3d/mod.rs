//! Vendored software 3D renderer (rasterize path only).
//!
//! Adapted from `render3d`, the core of limlabs/ratatui-3d
//! (<https://github.com/limlabs/ratatui-3d>), MIT-licensed — see
//! `LICENSE-render3d` in this directory. Trimmed to the rasterizer: the OBJ/glTF
//! loaders, the CPU/GPU ray tracers, and the ratatui widget layer are dropped.
//! Local changes carry a `// tinhorn:` marker (notably perspective-correct
//! attribute interpolation in `pipeline/{vertex,rasterize}.rs`).
//!
//! We wire this in incrementally, so unused items are expected for now.
#![allow(dead_code, unused_imports)]

pub mod camera;
pub mod color;
pub mod dice;
pub mod light;
pub mod material;
pub mod math;
pub mod mesh;
pub mod object;
pub mod pipeline;
pub mod primitives;
pub mod scene;
pub mod texture;
pub mod transform;

// Re-exports for convenience
pub use camera::{Camera, Projection};
pub use color::Rgb;
pub use light::Light;
pub use material::Material;
pub use mesh::{Mesh, Vertex};
pub use object::SceneObject;
pub use pipeline::Pipeline;
pub use scene::{Scene, Sky};
pub use texture::Texture;
pub use transform::Transform;

/// Prelude for convenient glob imports.
pub mod prelude {
    pub use crate::render3d::camera::{Camera, Projection};
    pub use crate::render3d::color::Rgb;
    pub use crate::render3d::light::Light;
    pub use crate::render3d::material::Material;
    pub use crate::render3d::math::{Mat4, Quat, Vec3};
    pub use crate::render3d::mesh::{Mesh, Vertex};
    pub use crate::render3d::object::SceneObject;
    pub use crate::render3d::pipeline::Pipeline;
    pub use crate::render3d::primitives;
    pub use crate::render3d::scene::{Scene, Sky};
    pub use crate::render3d::texture::Texture;
    pub use crate::render3d::transform::Transform;
}
