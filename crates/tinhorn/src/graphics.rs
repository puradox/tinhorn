//! The kitty graphics protocol arena — the pixel-perfect output path.
//!
//! In a terminal that speaks the kitty graphics protocol (kitty, Ghostty), the
//! same GPU frame the half-block blit downsamples is instead handed to the
//! terminal as a *real image* over the arena panel, while ratatui paints the
//! chrome around it. Everywhere else the `▀` half-block blit in [`crate::ui`]
//! stays the fallback. This module owns the mode decision and the payload
//! pipeline; the compose lives in [`crate::ui`], the emission in [`crate::scene`].
//!
//! Everything here is pure and unit-tested except the two impure edges that touch
//! the environment ([`resolve`]) and stdout ([`emit`]/[`emit_raw`]). Named
//! `graphics`, not `kitty`, so it never collides with the vendored terminal's own
//! keyboard-protocol `term/crossterm_context/kitty.rs`.

use std::io::Write;

use base64::prelude::{BASE64_STANDARD, Engine};
use flate2::Compression;
use flate2::write::ZlibEncoder;

/// The chosen output path for the arena. `Blocks` is the universal text-glyph blit —
/// **quadrant** glyphs (2×2 sub-pixels/cell) normally, or seamless half-blocks
/// (`half_block: true`) on a terminal that doesn't tile the quadrants cleanly.
/// `Kitty` places the GPU frame as a real image, at `scale` render pixels per
/// half-block sub-pixel (so the image is `cols*scale` × `rows*2*scale`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsMode {
    Blocks { half_block: bool },
    Kitty { scale: u32 },
}

/// The `--graphics` flag: `auto` sniffs the terminal, the other two force a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum GraphicsArg {
    Auto,
    Kitty,
    Blocks,
}

/// Cap on the transmitted image width in pixels: a fullscreen hi-DPI arena could
/// otherwise ask for a several-thousand-pixel frame every frame, and readback →
/// zlib → base64 is the frame-budget bottleneck. Scale is knocked down until
/// `cols * scale <= MAX_IMG_W` — the tuning knob if the encode ever drags.
pub const MAX_IMG_W: u32 = 1600;

/// The kitty transmit chunk cap: each APC's base64 payload is at most this many
/// bytes (the protocol's documented 4096-byte limit).
const CHUNK: usize = 4096;

/// The machine's core count, cached: the per-frame readback passes ([`pack_rgb`]
/// and `ui::quadrant_blit`) split their row-band work across it, but it can't change
/// during a run, so `available_parallelism` runs once, not on the hot path.
pub(crate) fn core_count() -> usize {
    use std::sync::OnceLock;
    static CORES: OnceLock<usize> = OnceLock::new();
    *CORES.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    })
}

/// The byte stride of one row in a wgpu render readback: `img_w` RGBA pixels
/// padded up to wgpu's 256-byte row alignment. The single source of the readback
/// layout contract, shared by the kitty pack ([`pack_rgb`]) and the half-block
/// blit (`ui::blit_bevy_arena`), which read the same buffer.
pub(crate) fn readback_stride(img_w: u32) -> usize {
    (img_w as usize * 4).div_ceil(256) * 256
}

/// Does this terminal speak the kitty graphics protocol? A hermetic env sniff (no
/// escape probing — the flag is the override): kitty/Ghostty set a telltale
/// `TERM`/`TERM_PROGRAM`, kitty also exports `KITTY_WINDOW_ID`, WezTerm announces
/// via `TERM_PROGRAM`. **But never under tmux/screen**: a multiplexer inherits
/// `KITTY_WINDOW_ID` yet swallows graphics APCs, so the image would never land.
/// Kept pure — the caller passes the environment in — so the truth table is tested.
pub fn kitty_capable(term: &str, term_program: &str, kitty_window_id: bool, in_tmux: bool) -> bool {
    if in_tmux || term.starts_with("screen") || term.starts_with("tmux") {
        return false;
    }
    let term = term.to_ascii_lowercase();
    let tp = term_program.to_ascii_lowercase();
    term.contains("kitty")
        || term.contains("ghostty")
        || kitty_window_id
        || tp == "ghostty"
        || tp == "wezterm"
}

/// Does this terminal tile the 2×2 quadrant glyphs (`▖▗▘▙▚▛▜▝▞▟`) seamlessly?
/// Most terminals special-case the block-element glyphs to render pixel-perfect,
/// edge-to-edge — but macOS Terminal.app draws the quadrants through its font, which
/// leaves visible seams, so there the blit falls back to half-blocks (which it *does*
/// tile perfectly). Hermetic; the caller passes `TERM_PROGRAM` in.
pub fn quadrants_tile_cleanly(term_program: &str) -> bool {
    term_program != "Apple_Terminal"
}

/// The render scale (pixels per half-block sub-pixel) for a cell that is
/// `cell_px_h` pixels tall: half its height (a sub-pixel is a half-cell), clamped
/// to a sane `2..=12`, and `8` when the ioctl reports a zero pixel size (many
/// terminals leave `window_size`'s pixel fields unset). Native-resolution, no SSAA
/// — anti-aliasing comes from the render pipeline's own 4× MSAA.
pub fn scale_for(cell_px_h: u32) -> u32 {
    if cell_px_h == 0 {
        return 8;
    }
    ((cell_px_h as f32 / 2.0).round() as u32).clamp(2, 12)
}

/// Resolve the `--graphics` flag into a concrete [`GraphicsMode`], reading the
/// environment and the terminal pixel size. One of the two impure edges; the
/// hermetic pieces it leans on ([`kitty_capable`], [`scale_for`]) are unit-tested.
pub fn resolve(arg: GraphicsArg) -> GraphicsMode {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    // The text-glyph fallback: quadrants normally, half-blocks where they'd seam.
    let blocks = GraphicsMode::Blocks {
        half_block: !quadrants_tile_cleanly(&term_program),
    };
    match arg {
        GraphicsArg::Blocks => blocks,
        GraphicsArg::Kitty => GraphicsMode::Kitty {
            scale: detect_scale(),
        },
        GraphicsArg::Auto => {
            let term = std::env::var("TERM").unwrap_or_default();
            let kitty_window_id = std::env::var_os("KITTY_WINDOW_ID").is_some();
            let in_tmux = std::env::var_os("TMUX").is_some();
            if kitty_capable(&term, &term_program, kitty_window_id, in_tmux) {
                GraphicsMode::Kitty {
                    scale: detect_scale(),
                }
            } else {
                blocks
            }
        }
    }
}

/// The cell pixel height from crossterm's `window_size`, fed to [`scale_for`].
/// Zero (unreported by the terminal) falls back to `scale_for`'s default.
fn detect_scale() -> u32 {
    let cell_px_h = crossterm::terminal::window_size()
        .ok()
        .filter(|w| w.rows > 0 && w.height > 0)
        .map(|w| (w.height / w.rows) as u32)
        .unwrap_or(0);
    scale_for(cell_px_h)
}

/// Strip a wgpu render readback into tightly-packed, graded RGB ready to transmit:
/// drop the 256-byte row padding wgpu adds to each row, drop the alpha channel
/// (kitty's `f=24`), and apply the same warm radial [`vignette`](crate::ui::vignette)
/// the half-block blit uses, so the two paths grade the picture identically.
/// `None` when the buffer is empty or shorter than a full frame (a stale readback
/// mid-resize, or before the first frame lands).
///
/// This per-pixel pass over the whole readback is the biggest slice of the
/// per-frame main-thread cost, so it's **split across cores** by row band — each
/// thread fills a disjoint output chunk with identical per-pixel math, so the
/// result stays byte-identical. Rows are independent, so no synchronization.
pub fn pack_rgb(pixels: &[u8], img_w: u32, img_h: u32) -> Option<Vec<u8>> {
    if img_w == 0 || img_h == 0 {
        return None;
    }
    let stride = readback_stride(img_w); // wgpu 256-byte row pad
    if pixels.len() < stride * img_h as usize {
        return None; // short / stale readback
    }
    let (fw, fh) = (img_w as f32, img_h as f32);
    let row_out = img_w as usize * 3;
    let mut out = vec![0u8; img_h as usize * row_out];

    let threads = core_count().clamp(1, img_h as usize);
    let band = (img_h as usize).div_ceil(threads);
    std::thread::scope(|s| {
        for (t, chunk) in out.chunks_mut(band * row_out).enumerate() {
            let y0 = t * band;
            s.spawn(move || {
                for (r, orow) in chunk.chunks_exact_mut(row_out).enumerate() {
                    let y = y0 + r;
                    let ny = (y as f32 + 0.5) / fh - 0.5;
                    for x in 0..img_w as usize {
                        let i = y * stride + x * 4;
                        let nx = (x as f32 + 0.5) / fw - 0.5;
                        let (fr, fg, fb) = crate::ui::vignette(nx, ny);
                        let o = x * 3;
                        orow[o] = (pixels[i] as f32 * fr).min(255.0) as u8;
                        orow[o + 1] = (pixels[i + 1] as f32 * fg).min(255.0) as u8;
                        orow[o + 2] = (pixels[i + 2] as f32 * fb).min(255.0) as u8;
                    }
                }
            });
        }
    });
    Some(out)
}

/// zlib-fast-compress packed RGB (the kitty `o=z` payload). Split from
/// [`encode_apc`] so the emitter can time compression on its own — it's the
/// single heaviest step of the transmit for a large frame.
pub fn compress(rgb: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(rgb)
        .expect("zlib write into a Vec is infallible");
    enc.finish().expect("zlib finish into a Vec is infallible")
}

/// Wrap a (zlib-compressed) payload in the kitty APC escape stream that transmits
/// *and* places the image over the arena: base64 → 4096-byte chunks. The first
/// chunk carries the full control block, the rest only `m`:
///
/// - `a=T` transmit-and-display, `f=24` RGB, `o=z` zlib.
/// - `i=1,p=1` — a **fixed** image + placement id, so re-emitting each frame is
///   kitty's flicker-free in-place replace rather than a new image.
/// - `s`/`v` the source pixel dims; `c`/`r` the cell box to scale into (so any
///   cell-aspect mismatch is the same mild stretch the half-blocks already have).
/// - `z=-1073741825` a very deep negative z, so the image sits under even
///   non-default cell backgrounds and the chrome/overlays draw above it.
/// - `C=1` don't move the cursor; `q=2` suppress ALL responses, so nothing kitty
///   sends back ever lands in crossterm's input stream and reads as a keypress.
pub fn encode_apc(payload: &[u8], img_w: u32, img_h: u32, cols: u16, rows: u16) -> Vec<u8> {
    let b64 = BASE64_STANDARD.encode(payload);

    let bytes = b64.as_bytes();
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[][..]]
    } else {
        bytes.chunks(CHUNK).collect()
    };
    let last = chunks.len() - 1;

    let mut out = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = if i == last { 0 } else { 1 };
        out.extend_from_slice(b"\x1b_G");
        if i == 0 {
            let header = format!(
                "a=T,f=24,o=z,i=1,p=1,z=-1073741825,C=1,q=2,s={img_w},v={img_h},c={cols},r={rows},m={more}"
            );
            out.extend_from_slice(header.as_bytes());
        } else {
            out.extend_from_slice(format!("m={more}").as_bytes());
        }
        out.push(b';');
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// Wrap a *file path* (not the pixels) in the kitty APC — `t=f`, so the terminal
/// loads the raw RGB from disk instead of the pty. The pty then carries only ~50
/// bytes, which unblocks the per-frame stdout write (the measured bottleneck); the
/// file holds raw `f=24` RGB (no `o=z` — a local file has no bandwidth problem, so
/// skip the zlib CPU too). Header otherwise mirrors [`encode_apc`]; the short path
/// is always a single un-chunked APC.
pub fn encode_apc_path(path: &str, img_w: u32, img_h: u32, cols: u16, rows: u16) -> Vec<u8> {
    let b64 = BASE64_STANDARD.encode(path.as_bytes());
    let header =
        format!("a=T,f=24,t=f,i=1,p=1,z=-1073741825,C=1,q=2,s={img_w},v={img_h},c={cols},r={rows}");
    let mut out = Vec::with_capacity(header.len() + b64.len() + 6);
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(header.as_bytes());
    out.push(b';');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Delete just our placement (keeping the image data), for while a pane covers the
/// arena — the placement is re-emitted when the pane closes. Targets our fixed
/// `i=1,p=1`, so no other program's images are touched.
pub fn delete_placement_apc() -> Vec<u8> {
    b"\x1b_Ga=d,d=i,i=1,p=1,q=2;\x1b\\".to_vec()
}

/// Delete our image and its placements outright, for a clean exit — targeted to
/// `i=1` so it can't disturb anything else on screen.
pub fn delete_all_apc() -> Vec<u8> {
    b"\x1b_Ga=d,d=I,i=1,q=2;\x1b\\".to_vec()
}

/// Move the cursor to cell `(x, y)` and write `apc` to stdout, then flush. The
/// image places at the cursor, so this is how the frame lands on the arena origin.
/// One of the two impure edges; must run strictly *after* `context.draw()` returns
/// (ratatui owns stdout during a draw).
pub fn emit(x: u16, y: u16, apc: &[u8]) -> std::io::Result<()> {
    use crossterm::QueueableCommand;
    use crossterm::cursor::MoveTo;
    let mut out = std::io::stdout().lock();
    out.queue(MoveTo(x, y))?;
    out.write_all(apc)?;
    out.flush()
}

/// Write `apc` to stdout without moving the cursor — for the delete escapes, which
/// carry no placement position.
pub fn emit_raw(apc: &[u8]) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(apc)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn kitty_capable_truth_table() {
        // Positive detections.
        assert!(kitty_capable("xterm-kitty", "", false, false));
        assert!(kitty_capable("xterm-ghostty", "", false, false));
        assert!(kitty_capable("xterm-256color", "", true, false)); // KITTY_WINDOW_ID
        assert!(kitty_capable("xterm-256color", "ghostty", false, false));
        assert!(kitty_capable("xterm-256color", "WezTerm", false, false)); // case-insensitive

        // Plain terminals are not capable.
        assert!(!kitty_capable(
            "xterm-256color",
            "Apple_Terminal",
            false,
            false
        ));
        assert!(!kitty_capable("", "", false, false));

        // The tmux/screen veto wins even when the multiplexer inherited the host's
        // KITTY_WINDOW_ID or a kitty TERM — the APC would be swallowed.
        assert!(!kitty_capable("xterm-kitty", "", true, true)); // $TMUX set
        assert!(!kitty_capable("screen", "", true, false)); // TERM=screen*
        assert!(!kitty_capable("tmux-256color", "", true, false)); // TERM=tmux*
        assert!(!kitty_capable("xterm-kitty", "ghostty", false, true)); // $TMUX vetoes kitty TERM
    }

    #[test]
    fn quadrants_tile_cleanly_except_apple_terminal() {
        // Modern terminals tile the 2×2 glyphs pixel-perfect; macOS Terminal.app is
        // the one that seams them, so it (and only it) falls back to half-blocks.
        for tp in ["ghostty", "iTerm.app", "WezTerm", "vscode", ""] {
            assert!(quadrants_tile_cleanly(tp), "{tp} should tile quadrants");
        }
        assert!(!quadrants_tile_cleanly("Apple_Terminal"));
    }

    #[test]
    fn scale_for_clamps_and_falls_back() {
        assert_eq!(scale_for(0), 8, "unreported pixel size falls back to 8");
        assert_eq!(scale_for(1), 2, "clamped up to the floor");
        assert_eq!(scale_for(2), 2); // round(1.0) = 1 -> clamped to 2
        assert_eq!(scale_for(16), 8, "a 16px cell -> 8 px per sub-pixel");
        assert_eq!(scale_for(20), 10);
        assert_eq!(scale_for(100), 12, "clamped down to the ceiling");
    }

    /// Build a row-padded RGBA buffer (wgpu layout) with a recognisable pattern in
    /// the pixels and sentinel `0xAB` bytes filling the row padding, so a test can
    /// prove `pack_rgb` never reads the pad.
    fn padded_rgba(img_w: u32, img_h: u32) -> Vec<u8> {
        let stride = readback_stride(img_w);
        let mut buf = vec![0xABu8; stride * img_h as usize]; // pad sentinel everywhere
        for y in 0..img_h as usize {
            for x in 0..img_w as usize {
                let i = y * stride + x * 4;
                buf[i] = (x * 7 + y * 3) as u8;
                buf[i + 1] = (x * 5 + y * 11) as u8;
                buf[i + 2] = (x * 13 + y * 17) as u8;
                buf[i + 3] = 255;
            }
        }
        buf
    }

    #[test]
    fn pack_rgb_strips_padding_and_alpha() {
        let (w, h) = (70u32, 4u32); // 70*4 = 280 > 256, so the row is padded to 512
        let stride = readback_stride(w);
        assert!(stride > w as usize * 4, "test needs a genuinely padded row");
        let rgba = padded_rgba(w, h);
        let rgb = pack_rgb(&rgba, w, h).expect("a full-size buffer packs");

        // Tight RGB, three bytes per pixel, no padding rows.
        assert_eq!(rgb.len(), (w * h * 3) as usize);

        // Each packed pixel is the correctly-de-padded source pixel (proving the
        // 256-byte pad — filled with the 0xAB sentinel — is never read) with the
        // shared vignette applied. Check a spread of pixels, exactly.
        for (px, py) in [(0u32, 0u32), (w / 2, h / 2), (w - 1, h - 1), (w - 1, 0)] {
            let si = py as usize * stride + px as usize * 4;
            let di = (py * w + px) as usize * 3;
            let nx = (px as f32 + 0.5) / w as f32 - 0.5;
            let ny = (py as f32 + 0.5) / h as f32 - 0.5;
            let factor = crate::ui::vignette(nx, ny);
            for c in 0..3 {
                let expected =
                    (rgba[si + c] as f32 * [factor.0, factor.1, factor.2][c]).min(255.0) as u8;
                assert_eq!(
                    rgb[di + c],
                    expected,
                    "pixel ({px},{py}) channel {c}: pad-stripped source × vignette"
                );
            }
        }

        // A short (stale/mid-resize) buffer packs to nothing rather than reading OOB.
        assert!(pack_rgb(&rgba[..stride], w, h).is_none());
        assert!(pack_rgb(&[], w, h).is_none());
        assert!(pack_rgb(&rgba, 0, h).is_none());
    }

    /// Parse an APC escape stream back into `(control-string, payload-bytes)` per
    /// chunk. Each chunk is `ESC _ G <control> ; <payload> ESC \`.
    fn parse_apcs(stream: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut chunks = Vec::new();
        let mut i = 0;
        while i < stream.len() {
            assert_eq!(&stream[i..i + 3], b"\x1b_G", "chunk must open with ESC _ G");
            i += 3;
            let semi = i + stream[i..].iter().position(|&b| b == b';').unwrap();
            let control = String::from_utf8(stream[i..semi].to_vec()).unwrap();
            i = semi + 1;
            // Payload runs to the ST terminator ESC \.
            let mut j = i;
            while !(stream[j] == 0x1b && stream[j + 1] == b'\\') {
                j += 1;
            }
            let payload = stream[i..j].to_vec();
            chunks.push((control, payload));
            i = j + 2; // skip ESC \
        }
        chunks
    }

    /// Inflate zlib bytes back to raw.
    fn inflate(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(data)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn encode_frame_round_trips_multi_chunk() {
        // Incompressible-ish RGB big enough to force several base64 chunks: a hash
        // pattern zlib can't shrink below the 4096-byte chunk size.
        let n = 9000usize;
        let rgb: Vec<u8> = (0..n)
            .map(|i| {
                let mut h = (i as u32).wrapping_mul(2_654_435_761);
                h ^= h >> 15;
                h as u8
            })
            .collect();
        let stream = encode_apc(&compress(&rgb), 60, 50, 30, 25);
        let chunks = parse_apcs(&stream);
        assert!(chunks.len() >= 2, "the payload should span multiple chunks");

        // Header lives on the first chunk only, with all the load-bearing keys.
        let head = &chunks[0].0;
        for key in [
            "a=T",
            "f=24",
            "o=z",
            "i=1",
            "p=1",
            "z=-1073741825",
            "C=1",
            "q=2",
            "s=60",
            "v=50",
            "c=30",
            "r=25",
        ] {
            assert!(
                head.contains(key),
                "first chunk header missing {key}: {head}"
            );
        }

        // m flags: 1 on every chunk but the last, 0 on the last. Middle/last chunks
        // carry only the m key.
        for (idx, (control, payload)) in chunks.iter().enumerate() {
            assert!(payload.len() <= CHUNK, "chunk {idx} payload exceeds 4096");
            let is_last = idx == chunks.len() - 1;
            assert!(
                control.contains(if is_last { "m=0" } else { "m=1" }),
                "chunk {idx} wrong m flag: {control}"
            );
            if idx > 0 {
                assert_eq!(control, if is_last { "m=0" } else { "m=1" });
            }
        }

        // Strip wrappers → base64-decode → inflate → byte-identical RGB.
        let b64: Vec<u8> = chunks.iter().flat_map(|(_, p)| p.clone()).collect();
        let compressed = BASE64_STANDARD.decode(b64).unwrap();
        assert_eq!(
            inflate(&compressed),
            rgb,
            "round-trip must reproduce the RGB"
        );
    }

    #[test]
    fn encode_frame_single_chunk() {
        // A tiny, compressible frame fits one chunk: full header AND m=0.
        let rgb = vec![7u8; 30];
        let stream = encode_apc(&compress(&rgb), 5, 2, 5, 1);
        let chunks = parse_apcs(&stream);
        assert_eq!(chunks.len(), 1, "small frames fit a single chunk");
        assert!(chunks[0].0.contains("a=T"));
        assert!(
            chunks[0].0.contains("m=0"),
            "the sole chunk is the last chunk"
        );
        let compressed = BASE64_STANDARD.decode(chunks[0].1.clone()).unwrap();
        assert_eq!(inflate(&compressed), rgb);
    }

    #[test]
    fn delete_escapes_are_exact() {
        assert_eq!(delete_placement_apc(), b"\x1b_Ga=d,d=i,i=1,p=1,q=2;\x1b\\");
        assert_eq!(delete_all_apc(), b"\x1b_Ga=d,d=I,i=1,q=2;\x1b\\");
    }

    #[test]
    fn file_apc_carries_the_base64_path_and_t_f() {
        let apc = encode_apc_path("/tmp/tinhorn-42.rgb", 60, 50, 30, 25);
        let chunks = parse_apcs(&apc);
        assert_eq!(chunks.len(), 1, "a path fits one APC");
        let (control, payload) = &chunks[0];
        // File transfer, raw RGB (no o=z), our fixed ids, and the pixel/cell dims.
        for key in [
            "a=T", "f=24", "t=f", "i=1", "p=1", "s=60", "v=50", "c=30", "r=25",
        ] {
            assert!(control.contains(key), "header missing {key}: {control}");
        }
        assert!(!control.contains("o=z"), "the file holds raw RGB, no zlib");
        // The payload is the base64-encoded path.
        let decoded = BASE64_STANDARD.decode(payload).unwrap();
        assert_eq!(decoded, b"/tmp/tinhorn-42.rgb");
    }
}
