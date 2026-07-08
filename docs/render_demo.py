"""Render a record_demo frame dump into the README's demo GIF.

Usage:
    DEMO_OUT=/tmp/demo.json cargo test record_demo -- --ignored
    uv run --with pillow python docs/render_demo.py [take] [src] [out]

Defaults: take=rocket, src=/tmp/demo.json, out=docs/demo.gif.
"""

import json
import math
import sys

from PIL import Image, ImageDraw, ImageFont

TAKE = sys.argv[1] if len(sys.argv) > 1 else "rocket"
SRC = sys.argv[2] if len(sys.argv) > 2 else "/tmp/demo.json"
OUT = sys.argv[3] if len(sys.argv) > 3 else "docs/demo.gif"

FONT_SIZE = 13
FONT = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", FONT_SIZE)

BG_DEFAULT = (13, 17, 23)  # terminal backdrop, GitHub-dark friendly
FG_DEFAULT = (197, 204, 214)

# Colour codes emitted by record_demo's color_code(), mapped to a dark theme.
COLORS = {
    ".": FG_DEFAULT,
    "k": (28, 33, 38),
    "r": (224, 85, 97),
    "g": (140, 194, 101),
    "y": (217, 180, 91),
    "b": (74, 165, 240),
    "m": (193, 98, 222),
    "c": (66, 179, 194),
    "a": (154, 164, 175),
    "d": (92, 103, 115),
    "R": (255, 122, 133),
    "G": (165, 224, 117),
    "Y": (240, 217, 103),
    "B": (77, 196, 255),
    "M": (222, 115, 255),
    "C": (76, 209, 224),
    "w": (230, 230, 230),
}

CW = math.ceil(FONT.getlength("M"))
CH = math.ceil(FONT_SIZE * 1.38)
PAD = 8  # breathing room around the terminal


def render_frame(frame, w, h):
    img = Image.new("RGB", (w * CW + 2 * PAD, h * CH + 2 * PAD), BG_DEFAULT)
    draw = ImageDraw.Draw(img)
    for y, (text, fg, bg) in enumerate(zip(frame["x"], frame["f"], frame["b"])):
        text = text.replace("\U0001f3b2", " ")  # Menlo has no die glyph
        # Background runs first (rare: verdict chips).
        x = 0
        while x < len(bg):
            code = bg[x]
            if code in (".", "k"):
                x += 1
                continue
            run = x
            while run < len(bg) and bg[run] == code:
                run += 1
            draw.rectangle(
                [PAD + x * CW, PAD + y * CH, PAD + run * CW - 1, PAD + (y + 1) * CH - 1],
                fill=COLORS.get(code, BG_DEFAULT),
            )
            x = run
        # Foreground runs, grouped by colour so each row is a few draw calls.
        x = 0
        while x < len(text):
            code = fg[x] if x < len(fg) else "."
            run = x
            while run < len(text) and (fg[run] if run < len(fg) else ".") == code:
                run += 1
            chunk = text[x:run]
            if chunk.strip():
                draw.text(
                    (PAD + x * CW, PAD + y * CH),
                    chunk,
                    font=FONT,
                    fill=COLORS.get(code, FG_DEFAULT),
                )
            x = run
    return img


def main():
    with open(SRC) as f:
        data = json.load(f)
    take = data[TAKE]
    w, h, fps = take["w"], take["h"], take["fps"]
    frames = take["frames"]
    print(f"{TAKE}: {len(frames)} frames @ {fps} fps, {w}x{h} cells")

    # One shared palette keeps the GIF small and dither-free.
    swatch = [c for rgb in COLORS.values() for c in rgb] + list(BG_DEFAULT)
    pal_img = Image.new("P", (1, 1))
    pal_img.putpalette(swatch + [0] * (768 - len(swatch)))

    images = [
        render_frame(fr, w, h).quantize(palette=pal_img, dither=Image.Dither.NONE)
        for fr in frames
    ]

    dur = [1000 // fps] * len(images)
    dur[-1] = 2500  # hold the verdict
    images[0].save(
        OUT,
        save_all=True,
        append_images=images[1:],
        duration=dur,
        loop=0,
        optimize=True,
    )
    print(f"wrote {OUT} ({len(images)} frames)")


if __name__ == "__main__":
    main()
