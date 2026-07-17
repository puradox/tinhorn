//! Dice geometry for the six standard polyhedra (d4, d6, d8, d10, d12, d20),
//! as plain glam data.
//!
//! This is the single source of the dice shapes. The physics collider for a die
//! is the convex hull of [`mesh_for`]'s vertices (see `app::mesh_points`), and
//! the terminal renderer builds its own `Mesh` from the same data — so the die
//! you see and the die the sim bounces are one shape.
//!
//! All six are built by one robust helper, [`polyhedron`], which takes each face
//! as an *unordered* set of its corner points and does the fiddly parts itself:
//! finds the outward normal, orders the corners counter-clockwise as seen from
//! outside, fan-triangulates, and splits vertices per face so each face is
//! flat-shaded. Every solid is scaled to circumradius 1, so a single world-space
//! size applies to all of them.

use std::sync::Arc;

use glam::Vec3;

/// A single vertex with position, normal, and texture coordinates. A plain data
/// carrier — the renderer copies these into its own vertex type.
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub uv: [f32; 2],
}

impl Vertex {
    pub fn new(position: Vec3, normal: Vec3) -> Self {
        Self {
            position,
            normal,
            uv: [0.0, 0.0],
        }
    }

    pub fn with_uv(mut self, u: f32, v: f32) -> Self {
        self.uv = [u, v];
        self
    }
}

/// An indexed triangle mesh in the die's own unit (circumradius-1) space.
#[derive(Debug, Clone)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    /// Triangle indices (length must be a multiple of 3).
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn new(vertices: Vec<Vertex>, indices: Vec<u32>) -> Self {
        debug_assert!(
            indices.len().is_multiple_of(3),
            "indices length must be a multiple of 3"
        );
        Self { vertices, indices }
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// The golden ratio — the icosahedron and dodecahedron are built from it.
const PHI: f32 = 1.618_034;

/// Build a convex polyhedron from its faces, each a set of coplanar corner
/// points in any order. The solid must be centred on the origin (all ours are).
fn polyhedron(faces: &[Vec<Vec3>]) -> Mesh {
    let mut verts: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for face in faces {
        let center = face.iter().fold(Vec3::ZERO, |a, &b| a + b) / face.len() as f32;

        // Outward normal: the plane normal from three corners, flipped to point
        // away from the origin (the centroid is on the outward side of a convex
        // origin-centred solid).
        let mut normal = (face[1] - face[0])
            .cross(face[2] - face[0])
            .normalize_or_zero();
        if normal.dot(center) < 0.0 {
            normal = -normal;
        }

        // Order the corners CCW around the outward normal, so the fan triangles
        // wind correctly for backface culling.
        let t = if normal.x.abs() < 0.9 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let t = (t - normal * normal.dot(t)).normalize_or_zero();
        let bt = normal.cross(t);
        let mut ordered = face.clone();
        ordered.sort_by(|a, b| {
            let angle = |p: &Vec3| (*p - center).dot(bt).atan2((*p - center).dot(t));
            angle(a)
                .partial_cmp(&angle(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let base = verts.len() as u32;
        let n = ordered.len();
        for (k, &p) in ordered.iter().enumerate() {
            // Simple radial UVs so a face texture (runes) could be applied later.
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            verts.push(Vertex::new(p, normal).with_uv(0.5 + 0.5 * a.cos(), 0.5 + 0.5 * a.sin()));
        }
        for k in 1..n as u32 - 1 {
            indices.extend_from_slice(&[base, base + k, base + k + 1]);
        }
    }

    Mesh::new(verts, indices)
}

/// Scale a mesh so its farthest vertex sits at `radius` from the centre.
fn fit(mut mesh: Mesh, radius: f32) -> Mesh {
    let max = mesh
        .vertices
        .iter()
        .map(|v| v.position.length())
        .fold(0.0_f32, f32::max);
    if max > 0.0 {
        let s = radius / max;
        for v in &mut mesh.vertices {
            v.position *= s;
        }
    }
    mesh
}

/// Tetrahedron — 4 triangular faces.
fn d4() -> Mesh {
    let v = [
        Vec3::new(1.0, 1.0, 1.0),
        Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::new(-1.0, -1.0, 1.0),
    ];
    let faces = vec![
        vec![v[0], v[1], v[2]],
        vec![v[0], v[1], v[3]],
        vec![v[0], v[2], v[3]],
        vec![v[1], v[2], v[3]],
    ];
    fit(polyhedron(&faces), 1.0)
}

/// Cube — 6 square faces.
fn d6() -> Mesh {
    let mut faces = Vec::new();
    for axis in 0..3 {
        for sign in [-1.0f32, 1.0] {
            let mut face = Vec::new();
            for a in [-1.0f32, 1.0] {
                for b in [-1.0f32, 1.0] {
                    let mut p = [0.0f32; 3];
                    p[axis] = sign;
                    p[(axis + 1) % 3] = a;
                    p[(axis + 2) % 3] = b;
                    face.push(Vec3::new(p[0], p[1], p[2]));
                }
            }
            faces.push(face);
        }
    }
    fit(polyhedron(&faces), 1.0)
}

/// Octahedron — 8 triangular faces.
fn d8() -> Mesh {
    let ax = [Vec3::X, Vec3::Y, Vec3::Z];
    let mut faces = Vec::new();
    for sx in [-1.0f32, 1.0] {
        for sy in [-1.0f32, 1.0] {
            for sz in [-1.0f32, 1.0] {
                faces.push(vec![ax[0] * sx, ax[1] * sy, ax[2] * sz]);
            }
        }
    }
    fit(polyhedron(&faces), 1.0)
}

/// Pentagonal bipyramid — 10 triangular faces. A true gaming d10 is a
/// trapezohedron (kite faces); the bipyramid is a convex 10-faced stand-in that
/// reads the same at terminal resolution. TODO: swap for the trapezohedron.
fn d10() -> Mesh {
    let apex_t = Vec3::new(0.0, 0.0, 1.3);
    let apex_b = Vec3::new(0.0, 0.0, -1.3);
    let ring: Vec<Vec3> = (0..5)
        .map(|i| {
            let a = i as f32 / 5.0 * std::f32::consts::TAU;
            Vec3::new(a.cos(), a.sin(), 0.0)
        })
        .collect();
    let mut faces = Vec::new();
    for i in 0..5 {
        let j = (i + 1) % 5;
        faces.push(vec![apex_t, ring[i], ring[j]]);
        faces.push(vec![apex_b, ring[i], ring[j]]);
    }
    fit(polyhedron(&faces), 1.0)
}

/// The 12 icosahedron vertices, shared by the d20 and (as its dual) the d12.
fn ico_verts() -> [Vec3; 12] {
    let p = PHI;
    [
        Vec3::new(-1.0, p, 0.0),
        Vec3::new(1.0, p, 0.0),
        Vec3::new(-1.0, -p, 0.0),
        Vec3::new(1.0, -p, 0.0),
        Vec3::new(0.0, -1.0, p),
        Vec3::new(0.0, 1.0, p),
        Vec3::new(0.0, -1.0, -p),
        Vec3::new(0.0, 1.0, -p),
        Vec3::new(p, 0.0, -1.0),
        Vec3::new(p, 0.0, 1.0),
        Vec3::new(-p, 0.0, -1.0),
        Vec3::new(-p, 0.0, 1.0),
    ]
}

/// The 20 icosahedron faces, as vertex-index triples.
const ICO_FACES: [[usize; 3]; 20] = [
    [0, 11, 5],
    [0, 5, 1],
    [0, 1, 7],
    [0, 7, 10],
    [0, 10, 11],
    [1, 5, 9],
    [5, 11, 4],
    [11, 10, 2],
    [10, 7, 6],
    [7, 1, 8],
    [3, 9, 4],
    [3, 4, 2],
    [3, 2, 6],
    [3, 6, 8],
    [3, 8, 9],
    [4, 9, 5],
    [2, 4, 11],
    [6, 2, 10],
    [8, 6, 7],
    [9, 8, 1],
];

/// Icosahedron — 20 triangular faces.
fn d20() -> Mesh {
    let v = ico_verts();
    let faces: Vec<Vec<Vec3>> = ICO_FACES
        .iter()
        .map(|f| vec![v[f[0]], v[f[1]], v[f[2]]])
        .collect();
    fit(polyhedron(&faces), 1.0)
}

/// Dodecahedron — 12 pentagonal faces, built as the dual of the icosahedron:
/// one pentagon per icosahedron vertex, from the centroids of the faces around it.
fn d12() -> Mesh {
    let v = ico_verts();
    let centroids: Vec<Vec3> = ICO_FACES
        .iter()
        .map(|f| (v[f[0]] + v[f[1]] + v[f[2]]) / 3.0)
        .collect();
    let mut faces = Vec::new();
    for vi in 0..12 {
        let pent: Vec<Vec3> = ICO_FACES
            .iter()
            .enumerate()
            .filter(|(_, f)| f.contains(&vi))
            .map(|(fi, _)| centroids[fi])
            .collect();
        faces.push(pent);
    }
    fit(polyhedron(&faces), 1.0)
}

/// The dice cup: a hollow, open-mouthed tin tumbler — a flared outer wall with
/// a **rolled lip**, a visible rim, an inner wall, and a raised inner floor, so
/// from the arena's raised camera you look into a dark mouth ringed by a bright
/// metal lip instead of at a solid closed cylinder. Centred on the origin,
/// ~1.2 tall; the arena scales, sways, and wobbles this single mesh while
/// shaking. It is the 3D heir to the old ASCII cup — the arena keeps no 2D
/// furniture. Cached like [`mesh_for`].
pub fn cup() -> Arc<Mesh> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Arc<Mesh>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(build_cup())).clone()
}

fn build_cup() -> Mesh {
    const SEG: usize = 16;
    let ring = |r: f32, y: f32| -> Vec<Vec3> {
        (0..SEG)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32 / SEG as f32;
                Vec3::new(a.cos() * r, y, a.sin() * r)
            })
            .collect()
    };

    let mut verts: Vec<Vertex> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    // Every face is emitted with BOTH windings: the camera must see the outer
    // wall from outside and the far side of the inner wall through the mouth,
    // and double-winding sidesteps backface culling entirely. The `normal` is
    // still the lighting normal, so inside and outside shade differently.
    let quad = |verts: &mut Vec<Vertex>, idx: &mut Vec<u32>, p: [Vec3; 4], n: Vec3| {
        let base = verts.len() as u32;
        for &pt in &p {
            verts.push(Vertex::new(pt, n));
        }
        idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        idx.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    };

    // The outer profile, base → mouth: a narrow foot flaring out, swelling into
    // a rolled lip just under the rim — the bead that catches the key light and
    // says "tin cup", not "cylinder".
    let profile = [
        (0.44_f32, -0.60_f32), // foot
        (0.50, -0.10),
        (0.57, 0.44), // under the lip
        (0.64, 0.52), // rolled lip, bulging out
        (0.62, 0.60), // lip top, tucking back in
    ];
    let rings: Vec<Vec<Vec3>> = profile.iter().map(|&(r, y)| ring(r, y)).collect();
    for w in rings.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        for i in 0..SEG {
            let j = (i + 1) % SEG;
            let mid = (a[i] + a[j] + b[j] + b[i]) / 4.0;
            let out = Vec3::new(mid.x, 0.0, mid.z).normalize_or_zero();
            quad(&mut verts, &mut idx, [a[i], a[j], b[j], b[i]], out);
        }
    }

    // The rim: a flat ring from the lip's outer edge in to the mouth, facing up.
    let (mouth_r, rim_y) = (0.52_f32, 0.60_f32);
    let mouth = ring(mouth_r, rim_y);
    let lip_top = rings.last().unwrap().clone();
    for i in 0..SEG {
        let j = (i + 1) % SEG;
        quad(
            &mut verts,
            &mut idx,
            [lip_top[i], lip_top[j], mouth[j], mouth[i]],
            Vec3::Y,
        );
    }

    // Inner wall, mouth down to a raised inner floor. Lighting normals point
    // inward with a slight downward lean — enough to shade the bowl below the
    // lit lip (the cue that sells "open cup") without dropping it to pure black.
    let inner_floor_y = -0.20_f32;
    let inner_bot = ring(0.44, inner_floor_y);
    for i in 0..SEG {
        let j = (i + 1) % SEG;
        let mid = (mouth[i] + mouth[j]) / 2.0;
        let inward = -(Vec3::new(mid.x, 0.0, mid.z).normalize_or_zero() + Vec3::Y * 0.35)
            .normalize_or_zero();
        quad(
            &mut verts,
            &mut idx,
            [mouth[i], mouth[j], inner_bot[j], inner_bot[i]],
            inward,
        );
    }

    // Inner floor (tilted toward the camera so it holds a bit of light) and the foot.
    let fan = |verts: &mut Vec<Vertex>, idx: &mut Vec<u32>, rim: &[Vec3], y: f32, n: Vec3| {
        let base = verts.len() as u32;
        verts.push(Vertex::new(Vec3::new(0.0, y, 0.0), n));
        for &p in rim {
            verts.push(Vertex::new(p, n));
        }
        for i in 0..SEG as u32 {
            let j = (i + 1) % SEG as u32;
            idx.extend_from_slice(&[base, base + 1 + i, base + 1 + j]);
            idx.extend_from_slice(&[base, base + 1 + j, base + 1 + i]);
        }
    };
    fan(
        &mut verts,
        &mut idx,
        &inner_bot,
        inner_floor_y,
        Vec3::new(0.0, 0.5, 0.85).normalize(),
    );
    let foot = rings.first().unwrap().clone();
    fan(&mut verts, &mut idx, &foot, -0.60, -Vec3::Y);

    Mesh::new(verts, idx)
}

/// The mesh for a die of `sides`. Non-standard sizes fall back to the cube.
///
/// The six polyhedra are built once and cached; each call returns a shared
/// `Arc` (a pointer bump, no vertex copy), so the render loop can ask for a
/// die's mesh every frame without rebuilding — or even copying — the geometry.
pub fn mesh_for(sides: u32) -> Arc<Mesh> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<[(u32, Arc<Mesh>); 6]> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        [
            (4, Arc::new(d4())),
            (6, Arc::new(d6())),
            (8, Arc::new(d8())),
            (10, Arc::new(d10())),
            (12, Arc::new(d12())),
            (20, Arc::new(d20())),
        ]
    });
    cache
        .iter()
        .find(|(s, _)| *s == sides)
        .map(|(_, m)| m.clone())
        .unwrap_or_else(|| cache[1].1.clone())
}

/// A die face located for the number overlay: its corner `centroid` and outward
/// `normal`, both in the mesh's own unit (circumradius-1) space.
pub type FaceGeom = (Vec3, Vec3);

/// Per-face `(centroid, outward_normal)` for a die of `sides`, in the mesh's own
/// unit (circumradius-1) space — the same space [`mesh_for`] renders in, so
/// scaling a centroid by the die's world radius and applying its pose lands it
/// exactly on the rendered face. The arena uses this to sit the number on the
/// face pointing at the camera and to fade it as that face turns edge-on.
///
/// Derived from the built mesh, not a second copy of the face data:
/// [`polyhedron`] lays each face down as a contiguous run of vertices that all
/// share that face's flat-shading normal, so consecutive runs of equal normal
/// recover the faces (a convex origin-centred solid gives every face a distinct
/// outward normal, and `fit` scales positions but never normals). Cached per
/// `sides` and falling back to the cube, mirroring [`mesh_for`].
pub fn face_geometry(sides: u32) -> &'static [FaceGeom] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<[(u32, Vec<FaceGeom>); 6]> = OnceLock::new();
    let cache = CACHE.get_or_init(|| [4u32, 6, 8, 10, 12, 20].map(|s| (s, faces_of(&mesh_for(s)))));
    cache
        .iter()
        .find(|(s, _)| *s == sides)
        .map(|(_, f)| f.as_slice())
        .unwrap_or_else(|| cache[1].1.as_slice())
}

/// Group a die mesh's vertices into faces by their shared flat-shading normal
/// (contiguous runs, as [`polyhedron`] emits them) and return each face's corner
/// centroid and outward normal.
fn faces_of(mesh: &Mesh) -> Vec<FaceGeom> {
    let mut faces = Vec::new();
    let mut i = 0;
    while i < mesh.vertices.len() {
        let normal = mesh.vertices[i].normal;
        let start = i;
        while i < mesh.vertices.len() && mesh.vertices[i].normal == normal {
            i += 1;
        }
        let block = &mesh.vertices[start..i];
        let centroid = block.iter().fold(Vec3::ZERO, |a, v| a + v.position) / block.len() as f32;
        faces.push((centroid, normal));
    }
    faces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_die_has_the_right_face_count() {
        // triangle_count = faces × (corners − 2): triangles as-is, squares ×2,
        // pentagons ×3.
        assert_eq!(mesh_for(4).triangle_count(), 4);
        assert_eq!(mesh_for(6).triangle_count(), 12);
        assert_eq!(mesh_for(8).triangle_count(), 8);
        assert_eq!(mesh_for(10).triangle_count(), 10);
        assert_eq!(mesh_for(12).triangle_count(), 36);
        assert_eq!(mesh_for(20).triangle_count(), 20);
        assert_eq!(mesh_for(100).triangle_count(), 12, "fallback is the cube");
    }

    #[test]
    fn the_cup_is_an_open_hollow_tumbler() {
        let cup = cup();

        // The mouth is open: nothing sits near the cup's axis in its upper
        // half. (The only near-axis vertices are the inner-floor and foot fan
        // centres, both below the midline — a top cap here would read as the
        // old solid cylinder.)
        for v in &cup.vertices {
            let radial = (v.position.x * v.position.x + v.position.z * v.position.z).sqrt();
            if radial < 0.1 {
                assert!(
                    v.position.y < 0.0,
                    "geometry near the axis at y={} — the mouth must stay open",
                    v.position.y
                );
            }
        }

        // It is hollow: some lighting normals face inward (the bowl) and some
        // outward (the wall) — a solid of revolution has only the latter.
        let radial_dot = |v: &Vertex| {
            v.normal
                .dot(Vec3::new(v.position.x, 0.0, v.position.z).normalize_or_zero())
        };
        assert!(
            cup.vertices.iter().any(|v| radial_dot(v) > 0.5),
            "no outward wall normals"
        );
        assert!(
            cup.vertices.iter().any(|v| radial_dot(v) < -0.5),
            "no inward bowl normals — the cup is not hollow"
        );

        // The rolled lip: the widest ring sits high on the cup, just under the
        // rim — the bead that reads \"tin cup\" instead of \"cylinder\".
        let widest = cup
            .vertices
            .iter()
            .max_by(|a, b| {
                let r = |v: &&Vertex| v.position.x.hypot(v.position.z);
                r(a).partial_cmp(&r(b)).unwrap()
            })
            .unwrap();
        assert!(
            widest.position.y > 0.3,
            "the rolled lip must bulge near the mouth, not at y={}",
            widest.position.y
        );
    }

    #[test]
    fn dice_are_unit_sized_and_outward_facing() {
        for sides in [4, 6, 8, 10, 12, 20] {
            let mesh = mesh_for(sides);
            let max = mesh
                .vertices
                .iter()
                .map(|v| v.position.length())
                .fold(0.0_f32, f32::max);
            assert!((max - 1.0).abs() < 1e-3, "d{sides} circumradius {max}");
            // Every face normal points away from the centre (outward winding).
            for v in &mesh.vertices {
                assert!(
                    v.normal.dot(v.position) > 0.0,
                    "d{sides} has an inward-facing normal"
                );
            }
        }
    }

    #[test]
    fn face_geometry_recovers_every_face() {
        // One (centroid, normal) per face, in the same unit space as the mesh:
        // the count matches the die, normals are unit and outward, and each
        // centroid sits inside the solid on its face's outward side.
        for (sides, faces) in [(4, 4), (6, 6), (8, 8), (10, 10), (12, 12), (20, 20)] {
            let geo = face_geometry(sides);
            assert_eq!(geo.len(), faces, "d{sides} face count");
            for &(centroid, normal) in geo {
                assert!(
                    (normal.length() - 1.0).abs() < 1e-3,
                    "d{sides} normal not unit"
                );
                assert!(
                    centroid.length() <= 1.0 + 1e-3,
                    "d{sides} centroid outside solid"
                );
                assert!(
                    centroid.dot(normal) > 0.0,
                    "d{sides} centroid on the inward side of its face"
                );
            }
        }
        // Non-standard sizes fall back to the cube, like `mesh_for`.
        assert_eq!(face_geometry(100).len(), 6, "fallback is the cube");
    }
}
