"""Deterministic PNG views of the model for the AI and for thumbnails.

Choice: a numpy z-buffer rasteriser over the tessellation that already exists for the viewer,
with Pillow only for PNG encoding and the caption text.

Why not the alternatives: build123d's hidden-line SVG projection gives line drawings without
shading (a vision model cannot tell a boss from a pocket), needs an SVG rasteriser on top and
its hidden-line pass takes seconds on filleted parts; OpenGL, VTK and pyrender need a GPU, a
display or a system Mesa; matplotlib's 3D plots sort whole polygons instead of pixels, so
concave parts render wrongly, and it writes a config directory. The rasteriser here has exact
per-pixel depth, no dependency beyond numpy and Pillow, reads and writes no files, and renders
a typical part (tens of thousands of triangles, 768 px) well under a second per view.

What a view shows: orthographic projection, smooth-shaded faces in the part colours, the
B-rep edges that are visible (depth-tested), an outline wherever depth or part changes, the
bounding box with its three dimensions, an axis triad, and a caption with the view name, the
box size in millimetres and a colour legend of the parts.

The same input gives the same bytes: no randomness, no timestamps in the PNG.
"""

from __future__ import annotations

import io
from dataclasses import dataclass

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from .tessellate import PartMesh

SUPERSAMPLE = 2
BACKGROUND = (255, 255, 255)
EDGE_COLOR = np.array([24, 24, 28], dtype=np.uint8)
BOX_COLOR = (150, 150, 158)
TEXT_COLOR = (20, 20, 24)
AXIS_COLORS = {"X": (200, 40, 40), "Y": (30, 140, 50), "Z": (40, 70, 200)}
CHUNK_PIXELS = 1_000_000

# name -> (direction from the model towards the camera, up vector)
VIEWS: dict[str, tuple[tuple[float, float, float], tuple[float, float, float]]] = {
    "iso": ((1.0, -1.0, 1.0), (0.0, 0.0, 1.0)),
    "front": ((0.0, -1.0, 0.0), (0.0, 0.0, 1.0)),
    "back": ((0.0, 1.0, 0.0), (0.0, 0.0, 1.0)),
    "right": ((1.0, 0.0, 0.0), (0.0, 0.0, 1.0)),
    "left": ((-1.0, 0.0, 0.0), (0.0, 0.0, 1.0)),
    "top": ((0.0, 0.0, 1.0), (0.0, 1.0, 0.0)),
    "bottom": ((0.0, 0.0, -1.0), (0.0, -1.0, 0.0)),
}


@dataclass
class _Camera:
    right: np.ndarray
    up: np.ndarray
    toward: np.ndarray  # unit vector from the model to the camera
    scale: float
    origin: np.ndarray  # pixel position of the projected (0, 0)

    def project(self, points: np.ndarray) -> np.ndarray:
        """World points (n, 3) -> pixel x, pixel y (down), depth (larger is farther)."""
        x = points @ self.right * self.scale + self.origin[0]
        y = -(points @ self.up) * self.scale + self.origin[1]
        return np.stack([x, y, -(points @ self.toward)], axis=1)


def _basis(view: str) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    toward, up_hint = (np.array(v, dtype=float) for v in VIEWS[view])
    toward /= np.linalg.norm(toward)
    right = np.cross(up_hint, toward)
    right /= np.linalg.norm(right)
    return right, np.cross(toward, right), toward


def _camera(view: str, low: np.ndarray, high: np.ndarray, area: tuple[int, int, int, int]) -> _Camera:
    """Fit the bounding box [low, high] into the pixel rectangle `area` = (x0, y0, x1, y1)."""
    right, up, toward = _basis(view)
    corners = np.array([[x, y, z] for x in (low[0], high[0]) for y in (low[1], high[1])
                        for z in (low[2], high[2])])
    u, v = corners @ right, corners @ up
    span_u, span_v = max(np.ptp(u), 1e-9), max(np.ptp(v), 1e-9)
    x0, y0, x1, y1 = area
    scale = min((x1 - x0) / span_u, (y1 - y0) / span_v)
    origin = np.array([
        (x0 + x1) / 2 - (u.min() + u.max()) / 2 * scale,
        (y0 + y1) / 2 + (v.min() + v.max()) / 2 * scale,
    ])
    return _Camera(right, up, toward, scale, origin)


def _rasterize(points: np.ndarray, triangles: np.ndarray, width: int, height: int):
    """Z-buffer the triangles.

    Returns (depth, triangle index, weight of corner 0, weight of corner 1) images; the index
    is -1 where nothing was drawn. Every triangle is expanded to the pixels of its bounding
    box in one vectorised step; the nearest candidate per pixel wins through np.minimum.at.
    """
    depth = np.full(width * height, np.inf, dtype=np.float32)
    owner = np.full(width * height, -1, dtype=np.int32)
    bary0 = np.zeros(width * height, dtype=np.float32)
    bary1 = np.zeros(width * height, dtype=np.float32)
    shape = (height, width)
    if len(triangles) == 0:
        return depth.reshape(shape), owner.reshape(shape), bary0.reshape(shape), bary1.reshape(shape)
    tri = points[triangles].astype(np.float32)
    xs, ys, zs = tri[:, :, 0], tri[:, :, 1], tri[:, :, 2]
    area2 = (xs[:, 1] - xs[:, 0]) * (ys[:, 2] - ys[:, 0]) - (xs[:, 2] - xs[:, 0]) * (ys[:, 1] - ys[:, 0])
    bx0 = np.clip(np.floor(xs.min(axis=1)), 0, width - 1).astype(np.int32)
    bx1 = np.clip(np.ceil(xs.max(axis=1)), 0, width - 1).astype(np.int32)
    by0 = np.clip(np.floor(ys.min(axis=1)), 0, height - 1).astype(np.int32)
    by1 = np.clip(np.ceil(ys.max(axis=1)), 0, height - 1).astype(np.int32)
    on_screen = (xs.max(axis=1) >= 0) & (xs.min(axis=1) <= width) & (ys.max(axis=1) >= 0) & (
        ys.min(axis=1) <= height
    )
    index = np.flatnonzero((np.abs(area2) > 1e-9) & on_screen).astype(np.int32)
    box_w = bx1[index] - bx0[index] + 1
    counts = box_w.astype(np.int64) * (by1[index] - by0[index] + 1)
    ends = np.cumsum(counts)
    best = np.empty(width * height, dtype=np.float32)
    start = 0
    while start < len(index):
        base = ends[start - 1] if start else 0
        stop = max(int(np.searchsorted(ends, base + CHUNK_PIXELS, side="right")), start + 1)
        sel = index[start:stop]
        cnt = counts[start:stop]
        local = np.repeat(np.arange(len(sel), dtype=np.int32), cnt)
        within = np.arange(int(cnt.sum()), dtype=np.int32) - np.repeat(
            (np.cumsum(cnt) - cnt).astype(np.int32), cnt
        )
        w = box_w[start:stop][local]
        t = sel[local]
        px = bx0[t] + within % w
        py = by0[t] + within // w
        cx, cy = px.astype(np.float32) + np.float32(0.5), py.astype(np.float32) + np.float32(0.5)
        x0, y0, x1, y1, x2, y2 = xs[t, 0], ys[t, 0], xs[t, 1], ys[t, 1], xs[t, 2], ys[t, 2]
        inv = np.float32(1.0) / area2[t]
        w0 = ((x1 - cx) * (y2 - cy) - (x2 - cx) * (y1 - cy)) * inv
        w1 = ((x2 - cx) * (y0 - cy) - (x0 - cx) * (y2 - cy)) * inv
        w2 = np.float32(1.0) - w0 - w1
        inside = np.flatnonzero((w0 >= -1e-6) & (w1 >= -1e-6) & (w2 >= -1e-6))
        t = t[inside]
        pix = py[inside] * np.int32(width) + px[inside]
        z = w0[inside] * zs[t, 0] + w1[inside] * zs[t, 1] + w2[inside] * zs[t, 2]
        best.fill(np.inf)
        np.minimum.at(best, pix, z)
        winners = np.flatnonzero((z <= best[pix]) & (z < depth[pix]))
        won = pix[winners]
        depth[won] = z[winners]
        owner[won] = t[winners]
        bary0[won] = w0[inside][winners]
        bary1[won] = w1[inside][winners]
        start = stop
    return depth.reshape(shape), owner.reshape(shape), bary0.reshape(shape), bary1.reshape(shape)


def _shade(normals, triangles, tri_color, owner, bary0, bary1, camera):
    """Per-pixel colour: one key light plus ambient, lit per vertex and interpolated."""
    height, width = owner.shape
    image = np.empty((height, width, 3), dtype=np.uint8)
    image[:] = BACKGROUND
    ys, xs = np.nonzero(owner >= 0)
    if len(ys) == 0:
        return image
    facing = normals @ camera.toward
    # Two-sided: a surface seen from behind (an open shell) is lit as if it faced the camera.
    lit = np.where(facing[:, None] < 0, -normals, normals)
    light = -0.45 * camera.right + 0.6 * camera.up + 0.66 * camera.toward
    light /= np.linalg.norm(light)
    vertex_light = (0.42 + 0.58 * np.clip(lit @ light, 0.0, 1.0) + 0.08 * np.abs(facing)).astype(
        np.float32
    )
    t = owner[ys, xs]
    corners = triangles[t]
    w0, w1 = bary0[ys, xs], bary1[ys, xs]
    intensity = (
        vertex_light[corners[:, 0]] * w0
        + vertex_light[corners[:, 1]] * w1
        + vertex_light[corners[:, 2]] * (1.0 - w0 - w1)
    )
    image[ys, xs] = np.clip(tri_color[t] * intensity[:, None], 0, 255).astype(np.uint8)
    return image


def _dilate(mask: np.ndarray, radius: int) -> np.ndarray:
    out = mask.copy()
    for _ in range(radius):
        grown = out.copy()
        grown[1:, :] |= out[:-1, :]
        grown[:-1, :] |= out[1:, :]
        grown[:, 1:] |= out[:, :-1]
        grown[:, :-1] |= out[:, 1:]
        out = grown
    return out


def _edge_mask(segments: np.ndarray, camera: _Camera, depth: np.ndarray, bias: float) -> np.ndarray:
    """Pixels covered by B-rep edges that are not hidden behind a nearer surface."""
    height, width = depth.shape
    mask = np.zeros((height, width), dtype=bool)
    if len(segments) == 0:
        return mask
    a, b = camera.project(segments[:, 0]), camera.project(segments[:, 1])
    steps = np.maximum(np.ceil(np.abs(b[:, :2] - a[:, :2]).max(axis=1) / 0.7).astype(np.int64), 1) + 1
    seg = np.repeat(np.arange(len(a)), steps)
    frac = (np.arange(int(steps.sum())) - np.repeat(np.cumsum(steps) - steps, steps)) / np.repeat(
        steps - 1, steps
    )
    sample = a[seg] + (b[seg] - a[seg]) * frac[:, None]
    px, py = np.floor(sample[:, 0]).astype(np.int64), np.floor(sample[:, 1]).astype(np.int64)
    ok = (px >= 0) & (px < width) & (py >= 0) & (py < height)
    px, py, z = px[ok], py[ok], sample[ok, 2]
    # Compare with the farthest depth in the 3x3 neighbourhood: an edge lies exactly on its
    # surface, and at a silhouette the pixel itself may already belong to what is behind.
    far = np.where(np.isfinite(depth), depth, np.float32(np.finfo(np.float32).max))
    padded = np.pad(far, 1, mode="edge")
    local_far = far.copy()
    for dy in range(3):
        for dx in range(3):
            np.maximum(local_far, padded[dy:dy + height, dx:dx + width], out=local_far)
    visible = z <= local_far[py, px] + bias
    mask[py[visible], px[visible]] = True
    return mask


def _outline_mask(depth: np.ndarray, part_id: np.ndarray, jump: float) -> np.ndarray:
    """Pixels where the part changes or the depth jumps: silhouettes of curved surfaces."""
    finite = np.where(np.isfinite(depth), depth, np.float32(1e30))
    mask = np.zeros(depth.shape, dtype=bool)
    for axis in (0, 1):
        a = [slice(None)] * 2
        b = [slice(None)] * 2
        a[axis], b[axis] = slice(1, None), slice(None, -1)
        a, b = tuple(a), tuple(b)
        diff = (part_id[a] != part_id[b]) | (np.abs(finite[a] - finite[b]) > jump)
        # Draw on the nearer of the two pixels so the line hugs the object in front; where
        # two parts are flush the choice is fixed, so the line does not flicker between them.
        nearer_a = finite[a] <= finite[b] + jump * 0.05
        mask[a] |= diff & nearer_a
        mask[b] |= diff & ~nearer_a
    return mask


def _font(size: int) -> ImageFont.ImageFont:
    return ImageFont.load_default(size=size)


def _hex_rgb(color: str) -> tuple[int, int, int]:
    value = color.lstrip("#")
    try:
        return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))  # type: ignore[return-value]
    except ValueError:
        return (160, 160, 160)


def _fmt(value: float) -> str:
    return f"{value:.1f}" if value < 1000 else f"{value:.0f}"


def render_view(
    meshes: list[PartMesh],
    view: str,
    size: tuple[int, int] = (768, 768),
    bbox: tuple[tuple[float, float, float], tuple[float, float, float]] | None = None,
    note: str = "",
) -> bytes:
    """Render one named view of the parts and return PNG bytes.

    `bbox` is the exact model bounding box for the dimensions; when omitted the box of the
    tessellation is used. `note` is appended to the caption (for example a section plane).
    """
    if view not in VIEWS:
        raise ValueError(f"unknown view {view!r}; choose from {', '.join(VIEWS)}")
    width, height = size
    ss = SUPERSAMPLE
    big_w, big_h = width * ss, height * ss
    caption_h = max(44, height // 11)
    margin = max(34, width // 14)

    verts = [m.vertices for m in meshes if len(m.vertices)]
    if bbox is not None:
        low, high = np.array(bbox[0], dtype=float), np.array(bbox[1], dtype=float)
    elif verts:
        allv = np.concatenate(verts)
        low, high = allv.min(axis=0), allv.max(axis=0)
    else:
        low, high = np.zeros(3), np.ones(3)
    dims = high - low

    area = (margin * ss, margin * ss, (width - margin) * ss, (height - caption_h - margin) * ss)
    camera = _camera(view, low, high, area)

    points, normals, triangles, colors, part_of_tri, segments = [], [], [], [], [], []
    offset = 0
    for index, mesh in enumerate(meshes):
        if len(mesh.triangles) == 0:
            continue
        kept = mesh.triangles
        if mesh.manifold()[0] is not None:
            # A closed mesh never shows its back faces; skipping them halves the work.
            kept = kept[mesh.triangle_normals_and_areas()[0] @ camera.toward > -1e-9]
        points.append(camera.project(mesh.vertices))
        normals.append(mesh.normals)
        triangles.append((kept + offset).astype(np.int32))
        colors.append(np.tile(np.array(_hex_rgb(mesh.color), dtype=np.float32), (len(kept), 1)))
        part_of_tri.append(np.full(len(kept), index, dtype=np.int64))
        segments.append(mesh.segments)
        offset += len(mesh.vertices)
    if points:
        pts, nrm, tri = np.concatenate(points), np.concatenate(normals), np.concatenate(triangles)
        col, part_tri = np.concatenate(colors), np.concatenate(part_of_tri)
        seg = np.concatenate(segments)
    else:
        pts = nrm = np.zeros((0, 3))
        tri = np.zeros((0, 3), dtype=np.int32)
        col, part_tri, seg = np.zeros((0, 3), np.float32), np.zeros(0, np.int64), np.zeros((0, 2, 3))

    depth, owner, bary0, bary1 = _rasterize(pts, tri, big_w, big_h)
    image = _shade(nrm, tri, col, owner, bary0, bary1, camera)

    diagonal = float(np.linalg.norm(dims)) or 1.0
    part_id = np.where(owner >= 0, part_tri[np.clip(owner, 0, None)] if len(part_tri) else -1, -1)
    lines = _edge_mask(seg, camera, depth, bias=diagonal * 2e-3)
    lines |= _outline_mask(depth, part_id, jump=diagonal * 0.02)
    lines = _dilate(lines, ss - 1)
    image[lines] = EDGE_COLOR

    # Bounding box wireframe with one labelled edge per axis. It is drawn only on the
    # background, so it never hides the model.
    def corner(ix: int, iy: int, iz: int) -> np.ndarray:
        return np.array([(low, high)[ix][0], (low, high)[iy][1], (low, high)[iz][2]])

    centre = camera.project(((low + high) / 2)[None, :])[0, :2]
    box_edges: dict[int, list] = {0: [], 1: [], 2: []}
    for axis in range(3):
        for i in (0, 1):
            for j in (0, 1):
                idx_a, idx_b = [i, j], [i, j]
                idx_a.insert(axis, 0)
                idx_b.insert(axis, 1)
                pa, pb = camera.project(np.array([corner(*idx_a), corner(*idx_b)]))[:, :2]
                mid = (pa + pb) / 2
                # Prefer the edge farthest from the centre on screen, lower ones first.
                score = np.linalg.norm(mid - centre) + 0.25 * (mid[1] - centre[1])
                box_edges[axis].append((score, pa, pb))
    box_layer = Image.new("L", (big_w, big_h), 0)
    box_draw = ImageDraw.Draw(box_layer)
    labelled = {}
    for axis, edges in box_edges.items():
        for _, pa, pb in edges:
            box_draw.line([tuple(pa), tuple(pb)], fill=120, width=ss)
        score, pa, pb = max(edges, key=lambda e: e[0])
        if np.linalg.norm(pb - pa) >= 12 * ss:
            labelled[axis] = (pa, pb)
    for pa, pb in labelled.values():
        box_draw.line([tuple(pa), tuple(pb)], fill=255, width=ss + 1)
    layer = np.asarray(box_layer)
    background = ~_dilate(owner >= 0, 1)
    pixels = image
    pixels[(layer > 0) & background] = BOX_COLOR
    pixels[(layer > 200) & background] = TEXT_COLOR
    canvas = Image.fromarray(pixels, "RGB").resize((width, height), Image.Resampling.BOX)
    draw = ImageDraw.Draw(canvas)

    font = _font(max(13, height // 48))
    centre = centre / ss
    for axis, (pa, pb) in labelled.items():
        label = "XYZ"[axis]
        mid = (pa + pb) / 2 / ss
        out = mid - centre
        out = out / (np.linalg.norm(out) or 1.0)
        text = f"{label} {_fmt(dims[axis])}"
        box = draw.textbbox((0, 0), text, font=font)
        tw, th = box[2] - box[0], box[3] - box[1]
        pos = mid + out * (max(tw, th) / 2 + 6) - np.array([tw / 2, th / 2])
        pos = np.clip(pos, [2, 2], [width - tw - 2, height - caption_h - th - 4])
        draw.text(tuple(pos), text, font=font, fill=AXIS_COLORS[label])

    # Axis triad, bottom-left above the caption.
    triad = np.array([margin * 0.75, height - caption_h - margin * 0.75])
    arm = margin * 0.5
    for label, axis in zip("XYZ", np.eye(3), strict=True):
        d = np.array([axis @ camera.right, -(axis @ camera.up)])
        tip = triad + d * arm
        if np.linalg.norm(d) < 0.15:
            draw.ellipse([triad[0] - 3, triad[1] - 3, triad[0] + 3, triad[1] + 3], outline=AXIS_COLORS[label])
            continue
        draw.line([tuple(triad), tuple(tip)], fill=AXIS_COLORS[label], width=2)
        draw.text(tuple(tip + d * 7 - np.array([4, 7])), label, font=font, fill=AXIS_COLORS[label])

    # Caption: view name and box size, then the part legend.
    top = height - caption_h
    draw.rectangle([0, top, width, height], fill=(244, 244, 246))
    draw.line([0, top, width, top], fill=(190, 190, 196), width=1)
    size_text = f"{_fmt(dims[0])} x {_fmt(dims[1])} x {_fmt(dims[2])} mm"
    extra = f"  |  {note}" if note else ""
    titles = (
        f"{view} view  |  bounding box {size_text} (X x Y x Z){extra}",
        f"{view} view  |  box {size_text}{extra}",
        f"{view}  |  {size_text}",
    )
    # The longest wording that fits the picture's width.
    title = next((t for t in titles if draw.textlength(t, font=font) <= width - 16), titles[-1])
    draw.text((8, top + 4), title, font=font, fill=TEXT_COLOR)
    x, y = 8, top + 4 + font.size + 6
    swatch = font.size - 2
    for mesh in meshes:
        label_w = draw.textlength(mesh.name, font=font)
        if x + swatch + 6 + label_w > width - 8:
            draw.text((x, y), "...", font=font, fill=TEXT_COLOR)
            break
        draw.rectangle([x, y + 2, x + swatch, y + 2 + swatch], fill=_hex_rgb(mesh.color), outline=TEXT_COLOR)
        draw.text((x + swatch + 5, y), mesh.name, font=font, fill=TEXT_COLOR)
        x += swatch + 5 + label_w + 16

    buffer = io.BytesIO()
    canvas.save(buffer, format="PNG", optimize=False, compress_level=6)
    return buffer.getvalue()
