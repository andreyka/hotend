#!/usr/bin/env python3
"""Draws the app icons and the favicon from the mark (docs/ux/DESIGN_SPEC.md, section 7.7).

    python/.venv/bin/python docs/ux/make_icons.py            # write them
    python/.venv/bin/python docs/ux/make_icons.py --check    # fail if they differ

The mark is drawn in the dark theme's colours on a dark rounded tile. The same Pillow (the one
python/uv.lock pins) writes the same bytes every time.
"""
from __future__ import annotations

import io
import struct
import sys
from itertools import pairwise
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
ICONS = ROOT / "apps/desktop/src-tauri/icons"
FAVICON = ROOT / "apps/desktop/public/favicon.svg"

TILE, BODY = "#12161A", "#E9ECEE"
STOPS = [(0.0, BODY), (0.42, "#E8304A"), (0.78, "#FF7A2E"), (1.0, "#FFC46B")]
OVER = 8  # drawn this much larger, then scaled down


def rgb(colour: str) -> tuple[int, int, int]:
    return int(colour[1:3], 16), int(colour[3:5], 16), int(colour[5:7], 16)


def heat(at: float) -> tuple[int, int, int]:
    """The bead's colour at `at` (0 to 1) of its length."""
    at = min(1.0, max(0.0, at))
    for (start, low), (end, high) in pairwise(STOPS):
        if at <= end:
            t = (at - start) / (end - start)
            a, b = rgb(low), rgb(high)
            return tuple(round(x + (y - x) * t) for x, y in zip(a, b, strict=True))  # type: ignore[return-value]
    return rgb(STOPS[-1][1])


def tile(size: int) -> Image.Image:
    """The tile at `size` px. The mark's 48 units lie in a tile of 56; at 16 px in one of 48,
    because the mark is never drawn smaller than 16 px."""
    grid, radius = (48, 10) if size < 24 else (56, 12)
    side = size * OVER
    unit, shift = side / grid, (grid - 48) / 2

    def box(x: float, y: float, w: float, h: float) -> tuple[float, float, float, float]:
        return ((x + shift) * unit, (y + shift) * unit, (x + w + shift) * unit - 1, (y + h + shift) * unit - 1)

    image = Image.new("RGBA", (side, side), (*rgb(TILE), 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((0, 0, side - 1, side - 1), radius * unit, fill=(*rgb(TILE), 255))
    body = (*rgb(BODY), 255)
    draw.rounded_rectangle(box(9, 4, 30, 13), 3.5 * unit, fill=body)
    draw.rounded_rectangle(box(14, 19.5, 20, 5.5), unit, fill=body)
    nozzle = [(15.5, 27.5), (32.5, 27.5), (26.4, 37), (21.6, 37)]
    draw.polygon([((x + shift) * unit, (y + shift) * unit) for x, y in nozzle], fill=body)
    bead, mask = Image.new("RGBA", image.size), Image.new("L", image.size)
    paint = ImageDraw.Draw(bead)
    for x in range(side):
        paint.line([(x, 0), (x, side)], fill=(*heat(((x + 0.5) / unit - shift - 4) / 23), 255))
    ImageDraw.Draw(mask).rounded_rectangle(box(4, 40, 23, 4), 2 * unit, fill=255)
    image.paste(bead, (0, 0), mask)
    return image.resize((size, size), Image.Resampling.BOX)


def png(size: int) -> bytes:
    out = io.BytesIO()
    tile(size).save(out, "PNG", optimize=True)
    return out.getvalue()


def ico(sizes: list[int]) -> bytes:
    frames = [png(size) for size in sizes]
    offset = 6 + 16 * len(frames)
    head = struct.pack("<HHH", 0, 1, len(frames))
    for size, frame in zip(sizes, frames, strict=True):
        head += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(frame), offset)
        offset += len(frame)
    return head + b"".join(frames)


def icns() -> bytes:
    # PNG entries: 16, 32, 64 (32@2x), 128, 256 and 512 px, and the two-to-one ones.
    kinds = {"icp4": 16, "icp5": 32, "icp6": 64, "ic07": 128, "ic08": 256, "ic09": 512, "ic10": 1024,
             "ic11": 32, "ic12": 64, "ic13": 256, "ic14": 512}
    frames = {size: png(size) for size in set(kinds.values())}
    body = b"".join(kind.encode() + struct.pack(">I", len(frames[size]) + 8) + frames[size]
                    for kind, size in kinds.items())
    return b"icns" + struct.pack(">I", len(body) + 8) + body


def favicon() -> bytes:
    stops = "".join(f'<stop offset="{at:g}" stop-color="{colour}"/>' for at, colour in STOPS)
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" role="img" aria-label="Hotend">'
        '<defs><linearGradient id="heat" x1="4" y1="0" x2="27" y2="0" gradientUnits="userSpaceOnUse">'
        f'{stops}</linearGradient></defs><rect width="48" height="48" rx="10" fill="{TILE}"/><g fill="{BODY}">'
        '<rect x="9" y="4" width="30" height="13" rx="3.5"/><rect x="14" y="19.5" width="20" height="5.5" rx="1"/>'
        '<path d="M15.5 27.5h17L26.4 37h-4.8z"/></g>'
        '<rect x="4" y="40" width="23" height="4" rx="2" fill="url(#heat)"/></svg>\n'
    ).encode()


def main() -> int:
    files = {
        ICONS / "32x32.png": png(32),
        ICONS / "128x128.png": png(128),
        ICONS / "icon.png": png(512),
        ICONS / "icon.ico": ico([16, 24, 32, 48, 64, 256]),
        ICONS / "icon.icns": icns(),
        FAVICON: favicon(),
    }
    stale = [path for path, data in files.items() if not path.exists() or path.read_bytes() != data]
    if "--check" in sys.argv[1:]:
        for path in stale:
            print("STALE", path.relative_to(ROOT))
        return 1 if stale else 0
    for path in stale:
        path.write_bytes(files[path])
        print("wrote", path.relative_to(ROOT))
    return 0


if __name__ == "__main__":
    sys.exit(main())
