use std::sync::Arc;

use crate::render3d::material::Material;
use crate::render3d::mesh::Mesh;
use crate::render3d::transform::Transform;

/// A renderable object in the scene.
///
/// tinhorn: the mesh sits behind an `Arc` so a cached mesh (a die's polyhedron,
/// the cup) joins the per-frame scene as a pointer bump instead of a deep clone
/// of its vertex and index buffers.
#[derive(Debug, Clone)]
pub struct SceneObject {
    pub mesh: Arc<Mesh>,
    pub material: Material,
    pub transform: Transform,
    pub visible: bool,
}

impl SceneObject {
    pub fn new(mesh: impl Into<Arc<Mesh>>) -> Self {
        Self {
            mesh: mesh.into(),
            material: Material::default(),
            transform: Transform::default(),
            visible: true,
        }
    }

    pub fn with_material(mut self, material: Material) -> Self {
        self.material = material;
        self
    }

    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = transform;
        self
    }
}
