//! Small CPU colour + texture types used by the arena's chrome and its baked
//! procedural textures (felt, floorboards, velvet, backdrop, wood grain).
//!
//! These outlived the vendored `render3d` software rasterizer they came from: the
//! Bevy renderer wraps the baked [`Texture`]s as `Image`s and tints its overlays
//! with [`Rgb`].

/// RGB colour with 8-bit channels — the tint/palette type for the arena's chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A 2D texture with RGBA pixel data, row-major, 4 bytes per pixel — the output
/// of the procedural generators, wrapped into a Bevy `Image` at spawn time.
#[derive(Debug, Clone)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Texture {
    pub fn from_rgba(width: u32, height: u32, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), (width * height * 4) as usize);
        Self {
            width,
            height,
            data,
        }
    }
}
