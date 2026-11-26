"""References: real things a printed part is fitted to (docs/PROTOCOL.md section 4.6).

A reference is a thing that exists outside the model: a figure the user prints from somebody
else's file, a phone, a bought board. The model does not print it; it builds a part around
it. Two kinds reach a model the same way, as a `Reference`:

- a reference object: the request lists a file (`context.references`), the engine reads it
  as hostile input and hands the shape over as `ctx.references["name"]`, provenance
  `measured` (it is the user's real file);
- a bought device: `ctx.component("sample_phone")` loads a component from
  `hotend_fit.components`; its body is `component.reference`, and every number of it
  carries its own provenance and source.

What a model does with a reference:

    dumpling = ctx.references["dumpling"]
    dumpling.size                      # (x, y, z) in mm, exact from the file
    dumpling.volume, dumpling.bbox
    dumpling.hull()                    # the convex hull as a Solid
    dumpling.section(20.0)             # the outline 20 mm above its lowest point (Sketch)
    dumpling.silhouette()              # its largest horizontal outline (Sketch)

    pocket = dumpling.pocket("cradle", ctx=ctx, shape="cylinder")   # room for it, from ctx.fit
    basket = pocket.cut(basket, at=Pos(0, 0, floor))                # the cavity, open at the top
    pocket.radius, pocket.depth, pocket.gap                         # to size the walls with

Frame of a reference object: it stands on z = 0 with the centre of its footprint on the Z
axis, turned as the file has it. `at` (a Location, a Plane or an (x, y, z) position) is
where that frame sits in the model.

A pocket is one joint between a printed part and a real thing, the counterpart of a feature
card for a thing that is not printed:

- It takes no clearance argument. The room around the thing comes from `ctx.fit` for the
  named fit class (`cradle`, `stretch`, `loose`, `slide`, `snug`) and the thing's size; the
  thing is not printed here, so only the printed side is compensated. The printer's
  shrinkage is compensated as a scale of the whole pocket (a long pocket grows more than a
  short one) and the offsets as a uniform gap; `pocket.gap` is the resulting gap per side
  at the thing's widest point, which is the resolver's number.
- `shape` says what is kept free: `silhouette` (the thing's outline seen along the
  insertion direction, extruded over its height), `hull` (its convex hull), `cylinder` (the
  smallest upright cylinder around it) or `box`. All of them contain the thing.
- `direction` is the way the thing is taken out, in the reference's frame (`+z`, `-z`,
  `+x`, ... or a vector); it rests against the pocket's seat at the other end, where no gap
  is drawn.
- `pocket.cut(part, at)` cuts the room from a part and returns the new part (by default
  open along the direction for a clearance fit, closed for a `stretch` fit, whose opening
  the model makes itself). `pocket.place(at)` only says where the thing sits when the
  model shapes the cavity itself. Either is called once.
- The engine's `reference_fit` check then proves, on the solids and with its own copy of
  the reference, that no part reaches into that room (the clearance holds all round),
  that the thing can be put in and taken out along the direction (nothing is in the way
  except parts named `removable`, such as a lid), and for a `stretch` fit that the
  material is flexible, that the pocket grips at least half of the thing's rim and that
  the lip it is pushed past is not deeper than the material stretches.
- `pocket.gauge()` is the smallest print that tests the fit before the product is printed:
  a ring that the thing must drop through (`cradle`) or a thin band of the pocket's rim
  that must stretch over the thing and stay on (`stretch`).

Guarantees:
- Every envelope contains the thing. A silhouette taken from a mesh is simplified only
  outwards and the result is checked to contain the exact outline; how far it may lie
  outside is `Pocket.envelope_tolerance` (mm).
- A reference is never a part: it is not in the B-rep, the exports or the plates. The
  viewer mesh shows it as a ghost.
- Nothing here reads a file. The engine reads reference files (`hotend_engine.reference_io`)
  and component data is read by `hotend_fit.components`.
"""

from __future__ import annotations

import contextlib
import math
import os
import re
import sys
import traceback
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from .fit import ALL_FIT_CLASSES, Clearance, FitError, FitResolver, resolver_of

NAME_RE = re.compile(r"[a-z][a-z0-9_]{0,31}\Z")
KINDS = ("rigid", "soft")
SHAPES = ("silhouette", "hull", "cylinder", "box")
POCKET_FITS = ("cradle", "stretch", "loose", "slide", "snug")
AXES = {"+x": (1.0, 0.0, 0.0), "-x": (-1.0, 0.0, 0.0), "+y": (0.0, 1.0, 0.0),
        "-y": (0.0, -1.0, 0.0), "+z": (0.0, 0.0, 1.0), "-z": (0.0, 0.0, -1.0)}

# A silhouette taken from a mesh is simplified until it has at most this many corners.
MAX_OUTLINE_VERTICES = 160
OUTLINE_TOLERANCES_MM = (0.02, 0.05, 0.1, 0.2, 0.4, 0.8, 1.6)
# Planes a convex hull is described with: this many spread evenly, the six axis directions,
# and the flat faces of the exact hull that each make up 2 % of its surface or more.
HULL_PLANES = 384
HULL_FACES = 32
HULL_FACE_SHARE = 0.02
# Round corners of an offset outline are drawn with this many segments to the full turn.
ROUND_SEGMENTS = 64
# The room a pocket must keep free is tested this much smaller than it is drawn, so that
# faces which lie exactly on it (the pocket's own walls) are contact, not intrusion.
PROOF_TOLERANCE_MM = 0.005
OVERLAP_TOLERANCE_MM3 = 1e-3
# A wall counts as holding the thing where material starts within this of the drawn gap.
GRIP_PROBE_MM = 0.05
GRIP_STATIONS = 240
GRIP_LEVELS = (0.3, 0.5, 0.7)
# A stretch fit must touch at least this share of the thing's rim. A guess: a shell with
# every button and port cut open still holds about three quarters of a phone's rim.
MIN_GRIP_COVERAGE = 0.5
MAX_DECLARED_POCKETS = 64
# Gauges: the height of the test ring and band (Qidi's advice for a TPU case: "print a 5 mm
# test ring first", https://qidi3d.com/blogs/print-lab/tpu-phone-cases) and their wall.
GAUGE_HEIGHT_MM = 5.0
GAUGE_WALL_MM = 2.0


class ReferenceProblem(ValueError):
    """A reference or a pocket cannot be used as asked. The message says what to change."""


def parse_direction(direction: Any) -> tuple[float, float, float]:
    """A direction as a unit vector: `+x` ... `-z`, or three numbers (not all zero)."""
    if isinstance(direction, str):
        if direction not in AXES:
            raise ReferenceProblem(
                f"direction must be one of {', '.join(AXES)} or a vector, got {direction!r}")
        return AXES[direction]
    try:
        vector = tuple(float(c) for c in direction)
    except (TypeError, ValueError):
        vector = ()
    length = math.sqrt(sum(c * c for c in vector)) if len(vector) == 3 else 0.0
    if not math.isfinite(length) or length < 1e-9:
        raise ReferenceProblem(f"direction must be a vector of three numbers, got {direction!r}")
    unit = tuple(c / length for c in vector)
    for axis in AXES.values():  # an axis given as a vector is exactly that axis
        if all(abs(a - b) < 1e-12 for a, b in zip(unit, axis, strict=True)):
            return axis
    return unit


def _rotation_to_z(direction: tuple[float, float, float]) -> np.ndarray:
    """The rotation R with R @ direction = +Z. Exact for the six axis directions."""
    exact = {
        AXES["+z"]: ((1, 0, 0), (0, 1, 0), (0, 0, 1)),
        AXES["-z"]: ((1, 0, 0), (0, -1, 0), (0, 0, -1)),
        AXES["+x"]: ((0, 0, -1), (0, 1, 0), (1, 0, 0)),
        AXES["-x"]: ((0, 0, 1), (0, 1, 0), (-1, 0, 0)),
        AXES["+y"]: ((1, 0, 0), (0, 0, -1), (0, 1, 0)),
        AXES["-y"]: ((1, 0, 0), (0, 0, 1), (0, -1, 0)),
    }
    if direction in exact:
        return np.array(exact[direction], dtype=np.float64)
    d = np.array(direction)
    axis = np.cross(d, (0.0, 0.0, 1.0))
    sine, cosine = float(np.linalg.norm(axis)), float(d[2])
    k = axis / sine
    cross = np.array([[0, -k[2], k[1]], [k[2], 0, -k[0]], [-k[1], k[0], 0]])
    return np.eye(3) + sine * cross + (1 - cosine) * (cross @ cross)


def _turn(rotation: np.ndarray) -> np.ndarray:
    """A rotation as the 3 x 4 matrix manifold3d transforms with."""
    return np.ascontiguousarray(np.hstack([rotation, np.zeros((3, 1))]), dtype=np.float64)


def _location(rotation: np.ndarray, translation: Any = (0.0, 0.0, 0.0)) -> Any:
    from build123d import Location
    from OCP.gp import gp_Trsf

    transformation = gp_Trsf()
    values = [float(v) for row, t in zip(rotation, translation, strict=True) for v in (*row, t)]
    transformation.SetValues(*values)
    return Location(transformation)


def _matrix_of(location: Any) -> tuple[np.ndarray, np.ndarray]:
    transformation = location.wrapped.Transformation()
    rows = [[transformation.Value(r, c) for c in (1, 2, 3, 4)] for r in (1, 2, 3)]
    matrix = np.array(rows, dtype=np.float64)
    return matrix[:, :3], matrix[:, 3]


def _signed_area(polygon: np.ndarray) -> float:
    x, y = polygon[:, 0], polygon[:, 1]
    return 0.5 * float(np.dot(x, np.roll(y, -1)) - np.dot(np.roll(x, -1), y))


def _outer(section: Any) -> Any:
    """A cross-section without its holes: what a thing's outline keeps other things out of."""
    import manifold3d

    polygons = [np.array(p, dtype=np.float64) for p in section.to_polygons()]
    kept = [p for p in polygons if len(p) >= 3 and _signed_area(p) > 0]
    return manifold3d.CrossSection(kept, manifold3d.FillRule.Positive) if kept else section


def _grown(section: Any, delta: float) -> Any:
    """`section` offset by `delta` with round corners. Outwards the corner arcs are drawn
    around the true arc, so the result contains the exact offset."""
    import manifold3d

    if abs(delta) < 1e-12:
        return section
    if delta > 0:
        delta = delta / math.cos(math.pi / ROUND_SEGMENTS)
    return section.offset(delta, manifold3d.JoinType.Round, 2.0, ROUND_SEGMENTS)


def _circle_through(points: list[np.ndarray]) -> tuple[np.ndarray, float]:
    """The smallest circle with up to three given points on its edge: (centre, radius)."""
    if not points:
        return np.zeros(2), 0.0
    if len(points) == 1:
        return points[0], 0.0
    if len(points) == 2:
        centre = (points[0] + points[1]) / 2.0
        return centre, float(np.linalg.norm(points[0] - centre))
    a, b, c = points
    d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]))
    if abs(d) < 1e-12:
        # Three points in a line: the two that lie farthest apart span the circle.
        pairs = [(a, b), (a, c), (b, c)]
        p, q = max(pairs, key=lambda pair: float(np.linalg.norm(pair[0] - pair[1])))
        return _circle_through([p, q])
    aa, bb, cc = float(a @ a), float(b @ b), float(c @ c)
    centre = np.array([
        (aa * (b[1] - c[1]) + bb * (c[1] - a[1]) + cc * (a[1] - b[1])) / d,
        (aa * (c[0] - b[0]) + bb * (a[0] - c[0]) + cc * (b[0] - a[0])) / d,
    ])
    return centre, float(np.linalg.norm(a - centre))


def _enclosing_circle(points: np.ndarray) -> tuple[float, float, float]:
    """The smallest circle around 2D points: (centre x, centre y, radius). Exact (Welzl's
    construction, written as loops): a circle that is merely close to the smallest one
    makes every round room and every part sized from it larger than the fit asks for. The
    radius is then taken from the farthest point, so the circle always contains them all."""
    points = np.unique(np.asarray(points, dtype=np.float64).reshape(-1, 2), axis=0)
    if len(points) == 0:
        return 0.0, 0.0, 0.0
    # Working about the middle keeps the arithmetic small; the fixed order keeps the
    # result the same from run to run.
    middle = (points.min(axis=0) + points.max(axis=0)) / 2.0
    scale = max(float(np.abs(points - middle).max()), 1e-12)
    work = (points - middle) / scale
    work = work[np.random.default_rng(20261117).permutation(len(work))]
    eps = 1e-12

    def outside(centre: np.ndarray, radius: float, point: np.ndarray) -> bool:
        return float(np.linalg.norm(point - centre)) > radius + eps

    centre, radius = work[0], 0.0
    for i in range(1, len(work)):
        if not outside(centre, radius, work[i]):
            continue
        centre, radius = work[i], 0.0
        for j in range(i):
            if not outside(centre, radius, work[j]):
                continue
            centre, radius = _circle_through([work[i], work[j]])
            for k in range(j):
                if outside(centre, radius, work[k]):
                    centre, radius = _circle_through([work[i], work[j], work[k]])
    centre = centre * scale + middle
    radius = float(np.sqrt(((points - centre) ** 2).sum(axis=1).max()))
    return float(centre[0]), float(centre[1]), radius


def _prism(polygons: list[np.ndarray], z_low: float, z_high: float) -> Any:
    """Upright prisms over 2D polygons, as one build123d shape."""
    from build123d import Compound, Face, Solid, Wire

    if z_high - z_low <= 1e-9:
        raise ReferenceProblem("the room for the reference has no height")
    solids = []
    for polygon in polygons:
        points = [(float(x), float(y), float(z_low)) for x, y in polygon]
        face = Face(Wire.make_polygon(points, close=True))
        solids.append(Solid.extrude(face, (0.0, 0.0, float(z_high - z_low))))
    if not solids:
        raise ReferenceProblem("the room for the reference is empty")
    return solids[0] if len(solids) == 1 else Compound(solids)


def tidy_polygon(polygon: np.ndarray, tolerance: float = 1e-3) -> np.ndarray:
    """A convex polygon without the corners that do not shape it: a corner closer than
    `tolerance` to the line through its neighbours is dropped (the short arcs an offset
    draws at every corner, points on a straight edge). The result lies inside the
    polygon by less than the tolerance and can be inset corner for corner."""
    points = [np.asarray(point, dtype=np.float64) for point in polygon]
    changed = True
    while changed and len(points) > 3:
        changed = False
        index = 0
        while index < len(points) and len(points) > 3:
            before, here, after = points[index - 1], points[index], points[(index + 1) % len(points)]
            chord = after - before
            length = float(np.linalg.norm(chord))
            away = (abs(float(chord[0] * (here - before)[1] - chord[1] * (here - before)[0])) / length
                    if length > 1e-12 else float(np.linalg.norm(here - before)))
            if away < tolerance:
                del points[index]
                changed = True
            else:
                index += 1
    return np.array(points)


def inset_polygon(polygon: np.ndarray, inset: float) -> np.ndarray | None:
    """A convex polygon (counter-clockwise) with every edge moved inwards by `inset`, corner
    for corner, so the result has the same number of points in the same order. None when
    an edge would vanish (the inset is larger than a corner's radius allows)."""
    points = np.asarray(polygon, dtype=np.float64)
    if abs(inset) < 1e-12:
        return points.copy()
    following = np.roll(points, -1, axis=0)
    edge = following - points
    length = np.linalg.norm(edge, axis=1)
    if np.any(length < 1e-9):
        return None
    normal = np.column_stack([edge[:, 1], -edge[:, 0]]) / length[:, None]  # outwards
    level = (normal * points).sum(axis=1) - inset
    before_normal, before_level = np.roll(normal, 1, axis=0), np.roll(level, 1)
    determinant = before_normal[:, 0] * normal[:, 1] - before_normal[:, 1] * normal[:, 0]
    moved = np.empty_like(points)
    straight = np.abs(determinant) < 1e-9
    safe = np.where(straight, 1.0, determinant)
    moved[:, 0] = (before_level * normal[:, 1] - before_normal[:, 1] * level) / safe
    moved[:, 1] = (before_normal[:, 0] * level - before_level * normal[:, 0]) / safe
    moved[straight] = points[straight] - inset * normal[straight]
    new_edge = np.roll(moved, -1, axis=0) - moved
    if np.any((new_edge * edge).sum(axis=1) <= 1e-9):
        return None
    return moved


def stacked_rings(rings: list[np.ndarray], heights: list[float]) -> tuple[np.ndarray, np.ndarray]:
    """A closed triangle mesh over rings of equal point count stacked at `heights` (rising):
    flat ends, and one band of two triangles per edge between neighbouring rings."""
    count = len(rings[0])
    vertices = np.concatenate([
        np.column_stack([ring, np.full(count, float(height))])
        for ring, height in zip(rings, heights, strict=True)])
    index = np.arange(count)
    following = (index + 1) % count
    triangles = []
    for level in range(len(rings) - 1):
        low, high = level * count, (level + 1) * count
        triangles.append(np.column_stack([low + index, low + following, high + following]))
        triangles.append(np.column_stack([low + index, high + following, high + index]))
    fan = np.arange(1, count - 1)
    top = (len(rings) - 1) * count
    triangles.append(np.column_stack([np.zeros(count - 2, dtype=int), fan + 1, fan]))
    triangles.append(np.column_stack([np.full(count - 2, top), top + fan, top + fan + 1]))
    return vertices, np.concatenate(triangles)


def solid_from_mesh(vertices: np.ndarray, triangles: np.ndarray) -> Any:
    """A closed triangle mesh as a build123d Solid, with coplanar triangles merged into
    faces. Meant for small convex meshes (a hull described by about a hundred planes)."""
    from build123d import Solid
    from OCP.BRepBuilderAPI import (
        BRepBuilderAPI_MakeFace,
        BRepBuilderAPI_MakePolygon,
        BRepBuilderAPI_MakeSolid,
        BRepBuilderAPI_Sewing,
    )
    from OCP.gp import gp_Pnt
    from OCP.ShapeUpgrade import ShapeUpgrade_UnifySameDomain
    from OCP.TopAbs import TopAbs_SHELL
    from OCP.TopExp import TopExp_Explorer
    from OCP.TopoDS import TopoDS

    sewing = BRepBuilderAPI_Sewing(1e-6)
    points = [gp_Pnt(*(float(c) for c in vertex)) for vertex in vertices]
    for a, b, c in triangles:
        wire = BRepBuilderAPI_MakePolygon(points[a], points[b], points[c], True).Wire()
        sewing.Add(BRepBuilderAPI_MakeFace(wire, True).Face())
    sewing.Perform()
    explorer = TopExp_Explorer(sewing.SewedShape(), TopAbs_SHELL)
    if not explorer.More():
        raise ReferenceProblem("the hull of the reference could not be closed into a solid")
    solid = BRepBuilderAPI_MakeSolid(TopoDS.Shell(explorer.Current())).Solid()
    unify = ShapeUpgrade_UnifySameDomain(solid, True, True, False)
    unify.Build()
    result = Solid(TopoDS.Solid(unify.Shape()))
    if result.volume < 0:
        result = Solid(TopoDS.Solid(result.wrapped.Reversed()))
    return result


def _within_cylinder(solid: Any, cx: float, cy: float, radius: float, z_low: float,
                     z_high: float) -> Any:
    """The part of a convex `solid` inside the upright cylinder of `radius` about (cx, cy).
    A solid that is inside already is returned as it is."""
    from build123d import Align, Cylinder, Pos

    if radius <= 0:
        raise ReferenceProblem("the fit's interference is larger than the reference")
    corners = np.array([(v.X - cx, v.Y - cy) for v in solid.vertices()])
    if float(np.hypot(corners[:, 0], corners[:, 1]).max()) <= radius:
        return solid
    bar = Pos(cx, cy, z_low - 1.0) * Cylinder(
        radius, z_high - z_low + 2.0, align=(Align.CENTER, Align.CENTER, Align.MIN))
    common = solid.intersect(bar)
    solids = [s for item in (common or []) for s in item.solids()]
    if len(solids) != 1:
        raise ReferenceProblem("the room around the reference could not be built")
    return solids[0]


def _hull_directions() -> np.ndarray:
    """Unit vectors spread evenly over the sphere (a Fibonacci lattice) plus the six axes."""
    index = np.arange(HULL_PLANES) + 0.5
    polar = np.arccos(1 - 2 * index / HULL_PLANES)
    azimuth = np.pi * (1 + 5 ** 0.5) * index
    lattice = np.stack([np.cos(azimuth) * np.sin(polar), np.sin(azimuth) * np.sin(polar),
                        np.cos(polar)], axis=1)
    return np.concatenate([np.array(list(AXES.values())), lattice])


@dataclass
class _Frame:
    """A reference seen along one direction: local coordinates in which the direction is +Z."""

    direction: tuple[float, float, float]
    rotation: np.ndarray  # local = rotation @ reference
    z0: float
    z1: float
    exact: Any  # CrossSection: the outline seen along the direction, holes filled
    base: Any  # CrossSection the room is grown from; contains `exact`
    tolerance: float  # how far `base` may lie outside `exact` (mm)
    circle: tuple[float, float, float]
    bounds: tuple[float, float, float, float]
    hull_points: np.ndarray  # the hull's corners in local coordinates
    from_hull: bool  # the outline is the hull's because the mesh is not closed

    @property
    def centre(self) -> tuple[float, float]:
        return ((self.bounds[0] + self.bounds[2]) / 2, (self.bounds[1] + self.bounds[3]) / 2)

    @property
    def to_reference(self) -> Any:
        return _location(self.rotation.T)


class Reference:
    """A real thing a printed part is fitted to: its shape, exact measures and provenance.

    Made by the engine from a file the request lists (`ctx.references[name]`) or by a
    component from its data (`ctx.component(id).reference`); a model never constructs one.

    Measures (mm, in the reference's own frame): `bbox` ((min), (max)), `size`, `center`,
    `volume` and `area` of its surface, `closed` (False when the file's mesh has holes; the
    volume is then an estimate and outlines are taken from the convex hull), `hull_volume`.
    `kind` is `rigid` or `soft` (a squishy thing: fit it with the `cradle` class).
    `provenance` is `measured` for a reference object; `source` says where it came from.
    """

    def __init__(self, name: str, vertices: Any, triangles: Any, *, kind: str = "rigid",
                 provenance: str = "measured", source: dict | None = None,
                 notes: Any = (), outlines: dict | None = None,
                 outline_tolerance: float = 0.0, label: str | None = None,
                 profiles: dict | None = None) -> None:
        if not isinstance(name, str) or not NAME_RE.match(name):
            raise ReferenceProblem(
                f"reference name {name!r} is not allowed; use lower-case letters, digits "
                "and underscores, starting with a letter, at most 32 characters")
        if kind not in KINDS:
            raise ReferenceProblem(f"reference kind must be one of {', '.join(KINDS)}")
        self.name, self.kind, self.provenance = name, kind, provenance
        self.label = label or name
        self.source = dict(source or {})
        self.notes = list(notes)
        self.vertices = np.ascontiguousarray(vertices, dtype=np.float64).reshape(-1, 3)
        self.triangles = np.ascontiguousarray(triangles, dtype=np.int64).reshape(-1, 3)
        if len(self.triangles) == 0:
            raise ReferenceProblem(f"the reference '{name}' has no surface")
        # Exact outlines a component knows (direction -> polygons), with their tolerance.
        self._outlines = dict(outlines or {})
        self._outline_tolerance = float(outline_tolerance)
        # The section of the body's edge a component knows (direction -> points (inset
        # from the outline, height above the seat), rising): its rounded shoulders.
        self._profiles = {parse_direction(key): [(float(d), float(z)) for d, z in value]
                          for key, value in (profiles or {}).items()}
        self._frames: dict[tuple, _Frame] = {}
        self._manifold: Any = None
        self._manifold_tried = False
        self._hull: Any = None

    # ---- measures -------------------------------------------------------------------

    @property
    def bbox(self) -> tuple[tuple[float, ...], tuple[float, ...]]:
        return (tuple(float(v) for v in self.vertices.min(axis=0)),
                tuple(float(v) for v in self.vertices.max(axis=0)))

    @property
    def size(self) -> tuple[float, float, float]:
        low, high = self.bbox
        return tuple(h - lo for lo, h in zip(low, high, strict=True))

    @property
    def center(self) -> tuple[float, float, float]:
        low, high = self.bbox
        return tuple((lo + h) / 2 for lo, h in zip(low, high, strict=True))

    def _solid(self) -> Any:
        """The mesh as a manifold3d.Manifold, or None when it is not a closed surface."""
        if not self._manifold_tried:
            import manifold3d

            self._manifold_tried = True
            candidate = manifold3d.Manifold(manifold3d.Mesh64(
                vert_properties=np.array(self.vertices, dtype=np.float64, order="C"),
                tri_verts=np.array(self.triangles, dtype=np.uint64, order="C"),
            ))
            if candidate.status() == manifold3d.Error.NoError and not candidate.is_empty():
                self._manifold = candidate
        return self._manifold

    @property
    def closed(self) -> bool:
        return self._solid() is not None

    @property
    def volume(self) -> float:
        solid = self._solid()
        if solid is not None:
            return float(solid.volume())
        tri = self.vertices[self.triangles]
        return abs(float(np.einsum("ij,ij->i", tri[:, 0], np.cross(tri[:, 1], tri[:, 2])).sum())) / 6

    @property
    def area(self) -> float:
        tri = self.vertices[self.triangles]
        return 0.5 * float(np.linalg.norm(
            np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1).sum())

    def _hull_manifold(self) -> Any:
        if self._hull is None:
            import manifold3d

            self._hull = manifold3d.Manifold.hull_points(
                np.array(self.vertices, dtype=np.float64, order="C"))
        return self._hull

    def _hull_face_normals(self) -> np.ndarray:
        """Unit normals of the large flat faces of the exact hull (coplanar triangles
        counted as one face), in the reference's frame."""
        mesh = self._hull_manifold().to_mesh64()
        corners = np.array(mesh.vert_properties)[:, :3][np.array(mesh.tri_verts)]
        cross = np.cross(corners[:, 1] - corners[:, 0], corners[:, 2] - corners[:, 0])
        doubled = np.linalg.norm(cross, axis=1)
        keep = doubled > 0
        normals = np.round(cross[keep] / doubled[keep][:, None], 6)
        unique, inverse = np.unique(normals, axis=0, return_inverse=True)
        area = np.zeros(len(unique))
        np.add.at(area, inverse.ravel(), doubled[keep])
        # Only real faces: a facet of a round thing is no better than the even spread.
        order = np.argsort(-area, kind="stable")[:HULL_FACES]
        largest = unique[order][area[order] >= HULL_FACE_SHARE * area.sum()]
        return largest / np.linalg.norm(largest, axis=1)[:, None]

    @property
    def hull_volume(self) -> float:
        return float(self._hull_manifold().volume())

    # ---- outlines -------------------------------------------------------------------

    def _frame(self, direction: Any = "+z") -> _Frame:
        unit = parse_direction(direction)
        if unit in self._frames:
            return self._frames[unit]
        import manifold3d

        rotation = _rotation_to_z(unit)
        hull_points = np.array(self._hull_manifold().to_mesh64().vert_properties)[:, :3] @ rotation.T
        z0, z1 = float(hull_points[:, 2].min()), float(hull_points[:, 2].max())
        from_hull = False
        if unit in self._outlines:
            exact = manifold3d.CrossSection(
                [np.array(p, dtype=np.float64) for p in self._outlines[unit]],
                manifold3d.FillRule.Positive)
            base, tolerance = exact, self._outline_tolerance
        else:
            solid = self._solid()
            if solid is None:
                from_hull = True
                exact = manifold3d.CrossSection.hull_points(
                    np.array(hull_points[:, :2], dtype=np.float64, order="C"))
            else:
                exact = _outer(solid.transform(_turn(rotation)).project())
            base, tolerance = self._simplified(exact)
        if exact.is_empty():
            raise ReferenceProblem(f"the reference '{self.name}' has no outline along {direction!r}")
        corners = np.concatenate([np.array(p) for p in base.to_polygons()])
        frame = _Frame(unit, rotation, z0, z1, exact, base, tolerance,
                       _enclosing_circle(corners), tuple(float(v) for v in exact.bounds()),
                       hull_points, from_hull)
        self._frames[unit] = frame
        return frame

    @staticmethod
    def _simplified(exact: Any) -> tuple[Any, float]:
        """An outline with few corners that contains `exact`, and how far outside it may
        lie. Simplifying moves an outline by up to its tolerance either way, so the
        outline is first grown by that much and the result is checked to contain the
        original; the tolerance is raised until the corner count fits."""
        best = (exact, 0.0)
        if exact.num_vert() <= MAX_OUTLINE_VERTICES:
            return best
        area = exact.area()
        for tolerance in OUTLINE_TOLERANCES_MM:
            for grow in (1.0, 2.0, 4.0):
                candidate = _outer(_grown(exact, grow * tolerance).simplify(tolerance))
                if (exact - candidate).area() <= 1e-9 * max(area, 1.0):
                    best = (candidate, (grow + 1.0) * tolerance)
                    break
            if best[0].num_vert() <= MAX_OUTLINE_VERTICES:
                break
        return best

    def _sketch(self, section: Any, frame: _Frame, z: float) -> Any:
        from build123d import Face, Sketch, Wire

        faces, holes = [], []
        for polygon in section.to_polygons():
            polygon = np.array(polygon)
            if len(polygon) < 3:
                continue
            face = Face(Wire.make_polygon([(float(x), float(y), float(z)) for x, y in polygon],
                                          close=True))
            (faces if _signed_area(polygon) > 0 else holes).append(face)
        if not faces:
            raise ReferenceProblem(f"the reference '{self.name}' has no outline there")
        sketch = Sketch(faces)
        if holes:
            sketch = sketch - Sketch(holes)
        return frame.to_reference * sketch

    def silhouette(self, direction: Any = "+z") -> Any:
        """The thing's largest outline seen along `direction` (its shadow), as a build123d
        Sketch lying at its lowest point along that direction, holes filled. For a
        reference object the outline has at most 160 corners and contains the exact
        shadow. `silhouette_points` gives the same outline as numbers."""
        frame = self._frame(direction)
        return self._sketch(frame.base, frame, frame.z0)

    def silhouette_points(self, direction: Any = "+z") -> list[list[tuple[float, float]]]:
        """The outline of `silhouette` as lists of (x, y) points, counter-clockwise, in the
        plane across `direction` (for `+z`: the reference's own x and y)."""
        return [[(float(x), float(y)) for x, y in polygon]
                for polygon in self._frame(direction).base.to_polygons()]

    def silhouette_size(self, direction: Any = "+z") -> dict:
        """Numbers of the silhouette along `direction`: `area` (mm2), `size` (its two
        extents), `radius` and `center` of the smallest circle around it, and `height`
        (the thing's extent along the direction)."""
        frame = self._frame(direction)
        x0, y0, x1, y1 = frame.bounds
        return {"area": float(frame.exact.area()), "size": (x1 - x0, y1 - y0),
                "radius": frame.circle[2], "center": frame.circle[:2],
                "height": frame.z1 - frame.z0}

    def section_points(self, height: float, direction: Any = "+z") -> list[list[tuple[float, float]]]:
        """The cross-section `height` mm above the thing's lowest point along `direction`,
        as lists of (x, y) points: outer outlines counter-clockwise, holes clockwise.
        Needs a closed mesh."""
        frame = self._frame(direction)
        solid = self._solid()
        if solid is None:
            raise ReferenceProblem(
                f"the reference '{self.name}' is not a closed surface, so it has no "
                "cross-sections; use its silhouette or hull")
        if not 0.0 <= height <= frame.z1 - frame.z0:
            raise ReferenceProblem(
                f"height {height!r} is outside the reference '{self.name}', which is "
                f"{frame.z1 - frame.z0:.2f} mm tall along this direction")
        section = solid.transform(_turn(frame.rotation)).slice(frame.z0 + float(height))
        return [[(float(x), float(y)) for x, y in polygon] for polygon in section.to_polygons()]

    def section(self, height: float, direction: Any = "+z", tolerance: float = 0.05) -> Any:
        """The cross-section `height` mm above the thing's lowest point along `direction`,
        as a build123d Sketch at that height. Corners closer than `tolerance` to a straight
        line are left out (a section of a fine mesh has thousands). Needs a closed mesh."""
        import manifold3d

        frame = self._frame(direction)
        polygons = [np.array(p) for p in self.section_points(height, direction)]
        section = manifold3d.CrossSection(polygons, manifold3d.FillRule.EvenOdd)
        if tolerance > 0:
            section = section.simplify(float(tolerance))
        return self._sketch(section, frame, frame.z0 + float(height))

    def hull(self) -> Any:
        """The convex hull as a build123d Solid in the reference's frame. It is described
        by planes (an even spread of directions and the large flat faces of the exact
        hull), so it contains the exact hull and lies a fraction of a millimetre outside
        it where the thing is round; `hull_volume` is the exact hull's volume."""
        return self._room(self._frame("+z"), "hull", 0.0, 0.0, 0.0, 0.0, 1.0)

    # ---- the room around the thing ----------------------------------------------------

    def _lateral(self, frame: _Frame, shape: str, offset: float, scale: float) -> Any:
        """The outline of the room across the direction, scaled about its centre and grown
        by `offset`, as a CrossSection."""
        import manifold3d

        if shape == "silhouette":
            section = frame.base
        elif shape == "hull":
            section = frame.base.hull()
        elif shape == "box":
            x0, y0, x1, y1 = frame.bounds
            cx, cy = frame.centre
            hx, hy = (x1 - x0) / 2 * scale + offset, (y1 - y0) / 2 * scale + offset
            if hx <= 0 or hy <= 0:
                raise ReferenceProblem("the fit's interference is larger than the reference")
            return manifold3d.CrossSection.square((2 * hx, 2 * hy)).translate((cx - hx, cy - hy))
        else:
            cx, cy, radius = frame.circle
            if radius * scale + offset <= 0:
                raise ReferenceProblem("the fit's interference is larger than the reference")
            return manifold3d.CrossSection.circle(radius * scale + offset, 256).translate((cx, cy))
        cx, cy = frame.centre
        section = section.translate((-cx, -cy)).scale((scale, scale)).translate((cx, cy))
        section = _outer(_grown(section, offset))
        if section.is_empty():
            raise ReferenceProblem("the fit's interference is larger than the reference")
        return section

    def has_profile(self, direction: Any = "+z") -> bool:
        """Whether the section of the body's edge is known along `direction` (a component
        whose maker states it): `Pocket.room(fitted=True)` then follows the rounded edges."""
        return parse_direction(direction) in self._profiles

    def _fitted_room(self, frame: _Frame, offset: float, top: float, sweep: float,
                     inset: float, scale: float) -> Any | None:
        """The room of `_room` for the `silhouette` shape, following the section of the
        body's edge: the outline's rings, each moved in by the section's inset, stacked.
        Below the widest level the rings stay where the seat puts them; from the widest
        level up they are raised by `top` and `sweep`, so the room holds the thing wherever
        the gap lets it go. None when the outline cannot be inset corner for corner."""
        profile = self._profiles[frame.direction]
        polygons = [np.array(p) for p in self._lateral(frame, "silhouette", offset, scale).to_polygons()]
        if len(polygons) != 1 or _signed_area(polygons[0]) <= 0:
            return None
        base = tidy_polygon(polygons[0])
        insets = [d for d, _ in profile]
        widest = min(insets)
        first = insets.index(widest)
        last = len(insets) - 1 - insets[::-1].index(widest)
        rings, heights = [], []
        for index, (d, z) in enumerate(profile):
            ring = inset_polygon(base, (d - widest) * scale)
            if ring is None:
                return None
            if index <= first:
                levels = [frame.z0 + inset + z * scale]
                if index == first == last:
                    levels.append(frame.z0 + z * scale + top + sweep)
            else:
                levels = [frame.z0 + z * scale + top + sweep] if index >= last else []
            for level in levels:
                rings.append(ring)
                heights.append(level)
        kept_rings, kept_heights = [rings[0]], [heights[0]]
        for ring, height in zip(rings[1:], heights[1:], strict=True):
            if height > kept_heights[-1] + 1e-9:
                kept_rings.append(ring)
                kept_heights.append(height)
        if len(kept_rings) < 2:
            raise ReferenceProblem("the room for the reference has no height")
        return solid_from_mesh(*stacked_rings(kept_rings, kept_heights))

    def _room(self, frame: _Frame, shape: str, offset: float, top: float, sweep: float,
              inset: float, scale: float, fitted: bool = False) -> Any:
        """The room around the thing in the reference's frame: the shape scaled by `scale`
        about the seat's centre, grown sideways by `offset`, by `top` at the far end and
        raised by `inset` at the seat, and extended by `sweep` along the direction. With
        `fitted`, a silhouette room follows the section of the body's edge where it is
        known (`has_profile`); otherwise it is the upright prism over the outline."""
        z_low = frame.z0 + inset
        z_high = frame.z0 + (frame.z1 - frame.z0) * scale + top + sweep
        if fitted and shape == "silhouette" and frame.direction in self._profiles:
            solid = self._fitted_room(frame, offset, top, sweep, inset, scale)
            if solid is not None:
                return frame.to_reference * solid
        if shape == "hull":
            solid = self._hull_room(frame, offset, z_low, z_high, sweep, scale)
            # The hull is described by planes, and on a round thing they meet outside the
            # circle around it. The thing and its gap lie inside that circle, so the room
            # ends there too: a round cavity of `Pocket.radius` holds it, as documented.
            cx, cy, radius = frame.circle
            solid = _within_cylinder(solid, cx, cy, radius * scale + offset, z_low, z_high)
        elif shape == "cylinder":
            from build123d import Align, Cylinder, Pos

            cx, cy, radius = frame.circle
            if radius * scale + offset <= 0 or z_high <= z_low:
                raise ReferenceProblem("the fit's interference is larger than the reference")
            solid = Pos(cx, cy, z_low) * Cylinder(
                radius * scale + offset, z_high - z_low,
                align=(Align.CENTER, Align.CENTER, Align.MIN))
        else:
            section = self._lateral(frame, shape, offset, scale)
            polygons = [np.array(p) for p in section.to_polygons()]
            solid = _prism([p for p in polygons if len(p) >= 3], z_low, z_high)
        return frame.to_reference * solid

    def _hull_room(self, frame: _Frame, offset: float, z_low: float, z_high: float,
                   sweep: float, scale: float) -> Any:
        import manifold3d

        directions = np.concatenate([_hull_directions(), self._hull_face_normals() @ frame.rotation.T])
        cx, cy = frame.centre
        points = (frame.hull_points - (cx, cy, frame.z0)) * scale + (cx, cy, frame.z0)
        supports = (points @ directions.T).max(axis=0) + offset
        low = points.min(axis=0) - abs(offset) - 1.0
        high = points.max(axis=0) + abs(offset) + 1.0
        room = manifold3d.Manifold.cube(tuple(float(v) for v in high - low)).translate(
            tuple(float(v) for v in low))
        for normal, support in zip(directions, supports, strict=True):
            if tuple(normal) == AXES["+z"]:
                support = z_high - sweep
            elif tuple(normal) == AXES["-z"]:
                support = -z_low
            room = room.trim_by_plane(tuple(float(-v) for v in normal), float(-support))
        if room.is_empty():
            raise ReferenceProblem("the fit's interference is larger than the reference")
        if sweep > 0:
            # The room moved along the direction is the hull of the room where it starts
            # and where it ends: exactly as wide as the room. (Adding the sweep to each
            # plane instead lets the planes that lean upwards drift outwards, so the swept
            # room came out wider than the room above its widest level, and a round
            # cavity that held the room was reported as trapping the thing.)
            room = manifold3d.Manifold.batch_hull(
                [room, room.translate((0.0, 0.0, float(sweep)))])
        mesh = room.to_mesh64()
        return solid_from_mesh(np.array(mesh.vert_properties)[:, :3], np.array(mesh.tri_verts))

    def envelope(self, fit: str, ctx: Any, *, shape: str = "silhouette",
                 direction: Any = "+z") -> Any:
        """The room this thing needs with the named fit class, as a build123d Solid in the
        reference's frame: its `shape` (`silhouette`, `hull`, `cylinder` or `box`) grown by
        the clearance `ctx.fit` resolves, never by a typed number. Cutting it by hand is not
        verified (the `fit_pairs` check warns about a clearance taken without a card or a
        pocket); use `pocket(...)` to have the fit proven."""
        return Pocket(self, fit, ctx=ctx, shape=shape, direction=direction,
                      _declare=False).envelope

    def pocket(self, fit: str = "cradle", *, ctx: Any, shape: str = "silhouette",
               direction: Any = "+z", removable: Any = (), name: str | None = None) -> Pocket:
        """A pocket for this thing: `Pocket(self, fit, ctx=ctx, ...)`."""
        return Pocket(self, fit, ctx=ctx, shape=shape, direction=direction,
                      removable=removable, name=name)

    # ---- reporting ------------------------------------------------------------------

    def report(self) -> dict:
        """The reference as data for a result: where it came from and its measures."""
        low, high = self.bbox
        outline = self.silhouette_size("+z")
        return {
            "name": self.name, "label": self.label, "kind": self.kind,
            "provenance": self.provenance, "source": self.source,
            "bbox": {"min": [round(v, 4) for v in low], "max": [round(v, 4) for v in high]},
            "size": [round(v, 4) for v in self.size],
            "volume": round(self.volume, 3), "area": round(self.area, 3),
            "closed": self.closed, "hull_volume": round(self.hull_volume, 3),
            "vertices": len(self.vertices), "triangles": len(self.triangles),
            "silhouette": {
                "direction": "+z", "area": round(outline["area"], 3),
                "size": [round(v, 4) for v in outline["size"]],
                "radius": round(outline["radius"], 4),
                "center": [round(v, 4) for v in outline["center"]],
            },
            "notes": list(self.notes),
        }


class Pocket:
    """A pocket: the room a printed part keeps for a real thing, with a proven fit.

        pocket = ctx.references["dumpling"].pocket("cradle", ctx=ctx, shape="cylinder")
        basket = pocket.cut(basket, at=Pos(0, 0, floor))

    `fit` is `cradle` (a soft thing dropped into a rigid holder, generous room), `stretch`
    (a flexible shell that grips a rigid thing; the material must be flexible), or one of
    the clearance classes `loose`, `slide`, `snug` for a rigid thing in a rigid part.
    `shape` is `silhouette`, `hull`, `cylinder` or `box`; `direction` the way the thing is
    taken out, in the reference's frame; `removable` names parts that are taken off first
    (a lid): they may close the way out but must still stay out of the room.

    Numbers for sizing the part around it (mm): `gap` (per side at the widest point, from
    the fit resolver), `radius` and `center` (the circle around the room's outline, about
    the direction), `size` (the room's extents: two across, one along the direction),
    `depth` (the thing's own height along the direction), `top_gap` (room kept beyond the
    thing's far end). `envelope` is the room as a Solid in the reference's frame.

    `cut(part, at)` or `place(at)` is called once; `at` is where the reference's frame sits
    in the model. The `reference_fit` check verifies the result.
    """

    kind = "pocket"

    def __init__(self, reference: Reference, fit: str = "cradle", *, ctx: Any,
                 shape: str = "silhouette", direction: Any = "+z", removable: Any = (),
                 name: str | None = None, _declare: bool = True,
                 _clearance: Clearance | None = None) -> None:
        if not isinstance(reference, Reference):
            raise ReferenceProblem(
                "a pocket is made for a reference: ctx.references[name] or "
                f"ctx.component(id).reference, got {type(reference).__name__}")
        if fit not in POCKET_FITS:
            raise ReferenceProblem(
                f"a pocket's fit must be one of {', '.join(POCKET_FITS)}, got {fit!r}")
        if shape not in SHAPES:
            raise ReferenceProblem(f"a pocket's shape must be one of {', '.join(SHAPES)}, got {shape!r}")
        if isinstance(removable, str):
            removable = (removable,)
        removable = tuple(removable)
        if not all(isinstance(part, str) and NAME_RE.match(part) for part in removable):
            raise ReferenceProblem("removable must name parts of the model")
        self.resolver: FitResolver = resolver_of(ctx)
        self.reference, self.fit, self.shape = reference, fit, shape
        self.direction = parse_direction(direction)
        self.removable = removable
        self._frame = reference._frame(self.direction)
        x0, y0, x1, y1 = self._frame.bounds
        height = self._frame.z1 - self._frame.z0
        self.nominal = max(x1 - x0, y1 - y0, height)
        try:
            self.clearance = _clearance or self.resolver.clearance(
                fit, self.nominal, printed_peg=False)
        except FitError as exc:
            raise ReferenceProblem(str(exc)) from exc
        # The resolver compensates shrinkage as half the nominal size times the ratio; a
        # pocket applies the same ratio as a scale, so every dimension of it gets its own.
        self.shrink = float(self.resolver.calibration.xy_shrink.value)
        self.scale = 1.0 + self.shrink
        self.offset = self.clearance.gap - self.shrink * self.nominal / 2.0
        self.top_gap = max(self.offset, 0.0)
        self.at: Any = None
        self.through: bool | None = None
        self._declare = _declare
        if name is None:
            name = f"pocket_{len(self.resolver.pockets) + 1}"
        if not isinstance(name, str) or not NAME_RE.match(name):
            raise ReferenceProblem(
                f"pocket name {name!r} is not allowed; use lower-case letters, digits and "
                "underscores, starting with a letter, at most 32 characters")
        self.name = name
        if _declare:
            if len(self.resolver.pockets) >= MAX_DECLARED_POCKETS:
                raise ReferenceProblem(f"a model has at most {MAX_DECLARED_POCKETS} pockets")
            self.resolver.pockets.append(self)
            self.resolver.references.setdefault(reference.name, reference)

    # ---- numbers ---------------------------------------------------------------------

    @property
    def gap(self) -> float:
        """The gap per side between the thing and the pocket's wall at the thing's widest
        point, as drawn: the printed gap the fit aims for (`clearance.target`, negative
        for a stretch fit) plus what this printer takes away."""
        return self.clearance.gap

    @property
    def depth(self) -> float:
        return self._frame.z1 - self._frame.z0

    @property
    def radius(self) -> float:
        """The radius of the smallest circle around the room's outline (about `direction`):
        a round cavity of this radius holds the thing with the fit's gap."""
        return self._frame.circle[2] * self.scale + self.offset

    @property
    def center(self) -> tuple[float, float, float]:
        """The centre of that circle at the seat, in the reference's frame."""
        cx, cy, _ = self._frame.circle
        point = self._frame.rotation.T @ np.array([cx, cy, self._frame.z0])
        return tuple(float(v) for v in point)

    @property
    def size(self) -> tuple[float, float, float]:
        """The room's extents: two across the direction and one along it."""
        x0, y0, x1, y1 = self._frame.bounds
        return ((x1 - x0) * self.scale + 2 * self.offset, (y1 - y0) * self.scale + 2 * self.offset,
                self.depth * self.scale + self.top_gap)

    @property
    def envelope_tolerance(self) -> float:
        """How far the room's outline may lie outside the exact one (mm): 0 for `cylinder`
        and `box` and for a bought device, a few hundredths for a mesh silhouette."""
        return 0.0 if self.shape in ("cylinder", "box") else self._frame.tolerance

    # ---- solids ----------------------------------------------------------------------

    def room(self, *, inset: float = 0.0, sweep: float = 0.0, offset: float | None = None,
             top: float | None = None, fitted: bool = False) -> Any:
        """The room in the reference's frame, optionally `inset` on every side (a test
        solid), extended by `sweep` along the direction, or with another sideways offset.
        It is the upright prism over the outline: what `cut` removes, and with another
        `offset` the shape of walls and lips. `fitted=True` gives the room that follows
        the rounded edges of a component whose maker states their section (`fitted` is
        True on the pocket then): the room the `reference_fit` check proves free, so a
        lip may lean into the corner a rounded edge leaves, down to that room."""
        lateral = self.offset if offset is None else offset
        return self.reference._room(
            self._frame, self.shape, lateral - inset,
            (self.top_gap if top is None else top) - inset, sweep, inset, self.scale,
            fitted=fitted)

    def taper(self, low: float, high: float, inset: float, *, offset: float | None = None) -> Any:
        """The room's outline as a frustum, in the reference's frame: at `low` mm above
        the seat the outline (with the pocket's offset, or another `offset`), at `high`
        the same outline moved in by `inset` mm, corner for corner (negative: moved out).
        For a lip that leans over a rounded edge at an angle that prints without support,
        or a bevel around an opening: every face of it is flat. Not for the `hull`,
        `cylinder` and `box` shapes."""
        if self.shape != "silhouette":
            raise ReferenceProblem("a taper is made from a silhouette pocket's outline")
        if not all(isinstance(v, (int, float)) and math.isfinite(v) for v in (low, high, inset)) \
                or high - low <= 1e-6:
            raise ReferenceProblem("a taper needs two heights, the lower first, and an inset")
        lateral = self.offset if offset is None else float(offset)
        polygons = [np.array(q) for q in self.reference._lateral(
            self._frame, self.shape, lateral, self.scale).to_polygons()]
        base = tidy_polygon(polygons[0]) if len(polygons) == 1 else None
        ring = inset_polygon(base, float(inset)) if base is not None else None
        if ring is None:
            raise ReferenceProblem(
                "the outline cannot be moved in that far corner for corner; use a smaller inset")
        z0 = self._frame.z0
        return self._frame.to_reference * solid_from_mesh(
            *stacked_rings([base, ring], [z0 + float(low), z0 + float(high)]))

    @property
    def fitted(self) -> bool:
        """Whether `room(fitted=True)` differs from the prism: the reference states the
        section of its edge along the pocket's direction and the shape is `silhouette`."""
        return self.shape == "silhouette" and self.reference.has_profile(self.direction)

    @property
    def envelope(self) -> Any:
        """The room as a build123d Solid in the reference's frame."""
        return self.room()

    def _placed(self, at: Any) -> Any:
        from .cards.base import as_location

        if self.at is not None:
            raise ReferenceProblem(
                f"{self.name} was already placed; a pocket is one joint, so make one pocket "
                "for each thing a part holds")
        try:
            return as_location(at)
        except ValueError as exc:
            raise ReferenceProblem(str(exc)) from exc

    def place(self, at: Any = None) -> Pocket:
        """Say where the thing sits (the reference's frame in the model) without cutting
        anything: for a model that shapes the cavity itself. The check still proves the
        room is free and the way out is clear."""
        self.at = self._placed(at)
        self.through = False
        return self

    def cut(self, part: Any, at: Any = None, *, through: bool | None = None) -> Any:
        """Cut the room from `part` with the reference's frame at `at`; returns the new
        part. `through` extends the cut along the direction until it leaves the part, so
        the thing can be put in (default: yes for a clearance fit, no for `stretch`)."""
        from build123d import Part

        from .cards.base import as_shape

        try:
            shape = as_shape(part)
        except ValueError as exc:
            raise ReferenceProblem(str(exc)) from exc
        frame = self._placed(at)
        if through is None:
            through = self.fit != "stretch"
        sweep = 0.0
        if through:
            rotation, translation = _matrix_of(frame)
            along = rotation @ np.array(self.direction)
            box = shape.bounding_box()
            corners = np.array([[x, y, z] for x in (box.min.X, box.max.X)
                                for y in (box.min.Y, box.max.Y) for z in (box.min.Z, box.max.Z)])
            seat = float(along @ translation) + self._frame.z0
            reach = float((corners @ along).max()) - seat
            # Until the seat of the room has left the part: a room that narrows towards
            # its far end (a hull) is only out once its widest level is.
            sweep = max(reach, 0.0) + 1.0
        result = shape - frame * self.room(sweep=sweep)
        if not result.solids():
            raise ReferenceProblem(f"cutting {self.name} left nothing of the part")
        if not isinstance(result, Part):
            result = Part(result.wrapped)
        for attribute in ("label", "color"):
            value = getattr(shape, attribute, None)
            if value:
                setattr(result, attribute, value)
        self.at, self.through = frame, bool(through)
        return result

    # ---- data ------------------------------------------------------------------------

    def _args(self) -> dict:
        return {"reference": self.reference.name, "fit": self.fit, "shape": self.shape,
                "direction": list(self.direction)}

    def declaration(self) -> dict:
        """The pocket as plain JSON data, for a process that verifies it without having
        run the model: which reference, fit, shape and direction, the removable parts and
        where it was placed (12 numbers, or null when it never was)."""
        from .cards.base import frame_to_json

        return {
            "pocket": self.name, **self._args(), "removable": list(self.removable),
            "frame": None if self.at is None else frame_to_json(self.at),
            "through": bool(self.through),
        }

    # ---- the gauge -------------------------------------------------------------------

    def gauge_set(self) -> Any:
        """The test print of this pocket as one named part lying on the bed: a ring whose
        inside is the pocket's outline, 5 mm tall with a 2 mm wall. For a `cradle` the
        thing must drop through it; for a `stretch` fit it is a band of the pocket's rim
        that must stretch over the thing and stay on. `dimensions` says what to measure
        with a caliper (the two inside widths), for the `calibrate` op."""
        from build123d import Align, Cylinder, Pos

        from .cards.base import Gauge

        frame = self._frame
        if self.shape == "cylinder":
            cx, cy, radius = frame.circle
            inner_radius = radius * self.scale + self.offset
            ring = Pos(cx, cy, 0) * (
                Cylinder(inner_radius + GAUGE_WALL_MM, GAUGE_HEIGHT_MM,
                         align=(Align.CENTER, Align.CENTER, Align.MIN))
                - Cylinder(inner_radius, GAUGE_HEIGHT_MM,
                           align=(Align.CENTER, Align.CENTER, Align.MIN)))
            widths = (2 * inner_radius, 2 * inner_radius)
        else:
            inner = self.reference._lateral(frame, self.shape, self.offset, self.scale)
            outer = self.reference._lateral(frame, self.shape, self.offset + GAUGE_WALL_MM,
                                            self.scale)
            ring = (_prism([np.array(p) for p in outer.to_polygons()], 0.0, GAUGE_HEIGHT_MM)
                    - _prism([np.array(p) for p in inner.to_polygons()], 0.0, GAUGE_HEIGHT_MM))
            x0, y0, x1, y1 = inner.bounds()
            widths = (x1 - x0, y1 - y0)
        box = ring.bounding_box()
        ring = Pos(-box.min.X, -(box.min.Y + box.max.Y) / 2, -box.min.Z) * ring
        what = ("the thing must drop through the ring without being squeezed"
                if self.fit != "stretch" else
                "the band must stretch over the thing and stay on when it is shaken")
        dimensions = [
            {"id": f"inside_{axis}", "half": "ring", "feature": "hole", "drawn": float(width),
             "what": f"the inside of the ring across its {words} ({what})"}
            for axis, width, words in (("a", widths[0], "first width"),
                                       ("b", widths[1], "second width"))
        ]
        return Gauge(self.kind, {"gauge_ring": ring}, [], dimensions)

    def gauge(self) -> Any:
        """The smallest print that tests this fit: the ring of `gauge_set`."""
        return self.gauge_set().parts["gauge_ring"]


# --------------------------------------------------------------------------------------
# declarations of another process
# --------------------------------------------------------------------------------------


def restore_components(resolver: FitResolver, declarations: Any) -> list[str]:
    """Load the components a model's process says it used onto `resolver`, from this
    process's own component data. Returns one sentence per entry that could not be."""
    from .components import MAX_FEATURES, ComponentError, use_component

    if not isinstance(declarations, list) or len(declarations) > 32:
        return ["the list of components the model used is malformed"]
    problems = []
    for raw in declarations:
        try:
            if (not isinstance(raw, dict) or set(raw) != {"id", "features"}
                    or not isinstance(raw["id"], str) or not isinstance(raw["features"], list)
                    or len(raw["features"]) > MAX_FEATURES
                    or not all(isinstance(f, str) for f in raw["features"])):
                raise ComponentError("it is not a component declaration")
            component = use_component(resolver, raw["id"])
            for feature in raw["features"]:
                component.feature(feature)
        except Exception as exc:  # noqa: BLE001 - untrusted data may break anywhere
            problems.append(f"a component the model used could not be loaded: {str(exc)[:200]}")
    return problems


def restore_pockets(resolver: FitResolver, declarations: Any) -> list[str]:
    """Rebuild pockets from `Pocket.declaration()` data on `resolver`, as if a build had
    made them: each is made for this resolver's own reference of that name, with this
    resolver's clearance. The data is untrusted; only the names, the fit, the shape, the
    direction and the frame are read. Returns one sentence per pocket that could not be
    rebuilt."""
    from .cards.base import frame_from_json

    if not isinstance(declarations, list):
        return ["the declared pockets are not a list"]
    problems = []
    if len(declarations) > MAX_DECLARED_POCKETS:
        problems.append(f"{len(declarations)} pockets were declared; only the first "
                        f"{MAX_DECLARED_POCKETS} are verified")
    keys = {"pocket", "reference", "fit", "shape", "direction", "removable", "frame", "through"}
    for index, raw in enumerate(declarations[:MAX_DECLARED_POCKETS]):
        label = f"declared pocket {index + 1}"
        try:
            if not isinstance(raw, dict) or set(raw) != keys:
                raise ReferenceProblem("it is not a pocket declaration")
            reference = resolver.references.get(raw["reference"]) if isinstance(
                raw["reference"], str) else None
            if reference is None:
                raise ReferenceProblem(
                    f"the request has no reference named {str(raw['reference'])[:40]!r}")
            direction = raw["direction"]
            if (not isinstance(direction, list) or len(direction) != 3
                    or not all(isinstance(c, (int, float)) and not isinstance(c, bool)
                               for c in direction)):
                raise ReferenceProblem("its direction is not three numbers")
            removable = raw["removable"]
            if not isinstance(removable, list) or len(removable) > 64:
                raise ReferenceProblem("its removable parts are malformed")
            if not isinstance(raw["through"], bool):
                raise ReferenceProblem("its through flag is not a flag")
            pocket = Pocket(reference, raw["fit"], ctx=resolver, shape=raw["shape"],
                            direction=direction, removable=removable, name=raw["pocket"])
            if raw["frame"] is not None:
                pocket.at = frame_from_json(raw["frame"])
            pocket.through = raw["through"]
        except Exception as exc:  # noqa: BLE001 - untrusted data may break a pocket anywhere
            problems.append(f"{label} could not be rebuilt: {str(exc)[:200]}")
    return problems


# --------------------------------------------------------------------------------------
# verification
# --------------------------------------------------------------------------------------


@dataclass
class PocketReport:
    """What the verification of one pocket found. `problems` make it fail, `cautions` make
    it a warning (nothing is proven wrong, but something is not proven)."""

    pocket: Pocket
    problems: list[str] = field(default_factory=list)
    cautions: list[str] = field(default_factory=list)
    data: dict = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return not self.problems

    @property
    def status(self) -> str:
        return "fail" if self.problems else ("warn" if self.cautions else "pass")

    @property
    def message(self) -> str:
        pocket = self.pocket
        what = f"The {pocket.fit} pocket '{pocket.name}' for '{pocket.reference.label}'"
        if self.problems:
            return f"{what} is not proven: " + "; ".join(self.problems) + "."
        target, drawn = pocket.clearance.target, pocket.offset
        if pocket.fit != "stretch":
            said = (f"{what} keeps {target:.2f} mm free all round once printed (drawn "
                    f"{drawn:.2f} mm larger per side plus {100 * pocket.shrink:.1f} % for "
                    "shrinkage, from the fit resolver)")
        else:
            said = (f"{what} prints {-target:.2f} mm smaller per side than the thing (from "
                    "the fit resolver) and is nowhere tighter")
        if self.data.get("path_clear"):
            said += f", and the thing goes in and out along {_direction_words(pocket.direction)}"
            if pocket.removable:
                said += f" once {', '.join(repr(p) for p in pocket.removable)} is off"
        if self.data.get("grip_coverage") is not None:
            said += f"; it holds {100 * self.data['grip_coverage']:.0f} % of the rim"
        if self.cautions:
            said += ". But: " + "; ".join(self.cautions)
        return said + "."

    def to_json(self) -> dict:
        pocket = self.pocket
        return {
            "kind": "pocket", "pocket": pocket.name, "reference": pocket.reference.name,
            "reference_kind": pocket.reference.kind,
            "reference_provenance": pocket.reference.provenance,
            "fit_class": pocket.fit, "shape": pocket.shape, "direction": list(pocket.direction),
            "removable": list(pocket.removable), "nominal": pocket.nominal,
            "expected": pocket.clearance.gap, "target": pocket.clearance.target,
            "offset": pocket.offset, "scale": pocket.scale,
            "envelope_tolerance": pocket.envelope_tolerance,
            "provenance": pocket.clearance.provenance,
            "offsets_provenance": pocket.clearance.offsets_provenance,
            "target_provenance": pocket.clearance.target_provenance,
            "problems": list(self.problems), "cautions": list(self.cautions), **self.data,
        }


def _direction_words(direction: tuple[float, float, float]) -> str:
    for name, axis in AXES.items():
        if axis == direction:
            return name.upper()
    return "(" + ", ".join(f"{c:.2f}" for c in direction) + ")"


def _shared(shape: Any, solid: Any) -> tuple[float, Any]:
    """(volume two shapes share, the shared solids or None)."""
    box_a, box_b = shape.bounding_box(), solid.bounding_box()
    if any(lo_a > hi_b + 1e-6 or lo_b > hi_a + 1e-6
           for lo_a, hi_a, lo_b, hi_b in zip(tuple(box_a.min), tuple(box_a.max),
                                              tuple(box_b.min), tuple(box_b.max), strict=True)):
        return 0.0, None
    common = shape.intersect(solid)
    solids = [s for item in (common or []) for s in item.solids()]
    return sum(float(s.volume) for s in solids), solids


def _depth_inside(points: np.ndarray, polygons: list[np.ndarray]) -> float:
    """How far the deepest of `points` lies inside the outline `polygons` (0 when none is
    inside): its distance to the outline's edge."""
    if not len(points):
        return 0.0
    deepest = 0.0
    for polygon in polygons:
        a, b = polygon, np.roll(polygon, -1, axis=0)
        edge = b - a
        length = (edge ** 2).sum(axis=1)
        relative = points[:, None, :] - a[None, :, :]
        t = np.clip((relative * edge).sum(axis=2) / np.where(length > 0, length, 1.0), 0.0, 1.0)
        distance = np.linalg.norm(relative - t[..., None] * edge[None], axis=2).min(axis=1)
        # Inside by the crossing rule.
        x, y = points[:, 0][:, None], points[:, 1][:, None]
        crosses = ((a[:, 1] > y) != (b[:, 1] > y)) & (
            x < (b[:, 0] - a[:, 0]) * (y - a[:, 1]) / np.where(
                b[:, 1] != a[:, 1], b[:, 1] - a[:, 1], 1.0) + a[:, 0])
        inside = crosses.sum(axis=1) % 2 == 1
        if inside.any():
            deepest = max(deepest, float(distance[inside].max()))
    return deepest


def _stations(polygons: list[np.ndarray], count: int) -> np.ndarray:
    """`count` points spread evenly along the outlines."""
    segments = np.concatenate([
        np.stack([polygon, np.roll(polygon, -1, axis=0)], axis=1) for polygon in polygons])
    lengths = np.linalg.norm(segments[:, 1] - segments[:, 0], axis=1)
    edges = np.concatenate([[0.0], np.cumsum(lengths)])
    marks = (np.arange(count) + 0.5) * edges[-1] / count
    index = np.clip(np.searchsorted(edges, marks, side="right") - 1, 0, len(segments) - 1)
    t = (marks - edges[index]) / np.where(lengths[index] > 0, lengths[index], 1.0)
    return segments[index, 0] + t[:, None] * (segments[index, 1] - segments[index, 0])


def _inside_any(points: np.ndarray, shapes: list[Any]) -> np.ndarray:
    """For every point: does it lie in the material of one of the shapes."""
    from OCP.BRepClass3d import BRepClass3d_SolidClassifier
    from OCP.gp import gp_Pnt
    from OCP.TopAbs import TopAbs_IN, TopAbs_ON

    inside = np.zeros(len(points), dtype=bool)
    for shape in shapes:
        for solid in shape.solids():
            box = solid.bounding_box()
            low, high = np.array(tuple(box.min)) - 1e-6, np.array(tuple(box.max)) + 1e-6
            near = ((points >= low) & (points <= high)).all(axis=1) & ~inside
            if not near.any():
                continue
            classifier = BRepClass3d_SolidClassifier(solid.wrapped)
            for index in np.flatnonzero(near):
                classifier.Perform(gp_Pnt(*(float(c) for c in points[index])), 1e-7)
                inside[index] = classifier.State() in (TopAbs_IN, TopAbs_ON)
    return inside


def _thing_tolerance(pocket: Pocket) -> float:
    """How much the thing itself may differ per side across the pocket's direction, from
    the tolerances its source states for its sizes (a bought component's envelope): half
    the largest. 0 for a reference object, whose file states none."""
    stated = pocket.reference.source.get("tolerance") if isinstance(
        pocket.reference.source, dict) else None
    if not isinstance(stated, dict):
        return 0.0
    names = ("width", "height", "depth")
    along = int(np.argmax(np.abs(np.array(pocket.direction))))
    across = [float(value) for path, value in stated.items()
              if path.rsplit(".", 1)[-1] in names
              and names.index(path.rsplit(".", 1)[-1]) != along
              and isinstance(value, (int, float))]
    return max(across) / 2.0 if across else 0.0


def verify_pocket(pocket: Pocket, parts: list[tuple[str, Any]]) -> PocketReport:
    """Prove one pocket on the built solids (`parts`: name and build123d shape of every
    part). See the module documentation for what is proven."""
    report = PocketReport(pocket)
    resolver, data = pocket.resolver, report.data
    spec = ALL_FIT_CLASSES[pocket.fit]
    facts = resolver.material
    robustness = resolver.robustness(pocket.clearance)
    data.update({"robustness": robustness, "material_flexible": facts.flexible,
                 "placed": pocket.at is not None})
    if spec.flexible_only and not facts.flexible:
        named = resolver.material_family or resolver.material_id or "this material"
        report.problems.append(
            f"a stretch fit needs a flexible material such as TPU, and {named} is rigid: the "
            "part would crack or never go over the thing. Print it in TPU, or give a rigid "
            "part room with the slide or loose class and hold the thing another way")
    if not resolver.was_issued(pocket.clearance):
        report.problems.append(
            "its clearance did not come from this build's fit resolver")
    if pocket.at is None:
        report.problems.append(
            "it was never placed: call cut(part, at) or place(at) so the fit can be checked")
        return report
    names = [name for name, _ in parts]
    unknown = [name for name in pocket.removable if name not in names]
    if unknown:
        report.problems.append(
            f"it names removable part(s) {', '.join(map(repr, unknown))} that the model "
            "does not have")
    frame, local = pocket.at, pocket._frame
    rotation, translation = _matrix_of(frame)
    to_local = local.rotation @ rotation.T  # model -> the pocket's local coordinates

    # 1. Nothing reaches into the room: the clearance holds all round.
    room = frame * pocket.room(inset=PROOF_TOLERANCE_MM, fitted=True)
    data["room"] = "fitted to the stated edge section" if pocket.fitted else "outline prism"
    intrusions = []
    for name, shape in parts:
        volume, _ = _shared(shape, room)
        if volume > OVERLAP_TOLERANCE_MM3:
            intrusions.append({"part": name, "volume": volume})
    data["intrusions"] = intrusions
    data["clearance_proven"] = not intrusions
    if intrusions:
        worst = max(intrusions, key=lambda entry: entry["volume"])
        if not spec.flexible_only:
            report.problems.append(
                f"'{worst['part']}' reaches {worst['volume']:.1f} mm3 into the room the thing "
                f"needs, so it has less than the {pocket.clearance.target:.2f} mm the "
                f"{pocket.fit} fit asks for")
        else:
            report.problems.append(
                f"'{worst['part']}' is {worst['volume']:.1f} mm3 tighter around the thing "
                f"than the {pocket.fit} fit allows")

    # How far the parts reach beyond the thing along the way out.
    reach = 0.0
    for _, shape in parts:
        box = shape.bounding_box()
        corners = np.array([[x, y, z] for x in (box.min.X, box.max.X)
                            for y in (box.min.Y, box.max.Y) for z in (box.min.Z, box.max.Z)])
        heights = (corners - translation) @ to_local.T
        reach = max(reach, float(heights[:, 2].max()))
    sweep = max(reach - local.z1, 0.0) + 1.0
    staying = [(name, shape) for name, shape in parts if name not in pocket.removable]

    if not spec.flexible_only:
        # 2. The way out is free: the room moved along the direction meets no part.
        # The whole room has to leave, not only its far end: a hull-shaped room is
        # widest low down, and a rim above it that is narrower than that traps the thing.
        way_out = max(reach - local.z0, 0.0) + 1.0
        path = frame * pocket.room(inset=PROOF_TOLERANCE_MM, sweep=way_out, fitted=True)
        blocked = []
        for name, shape in staying:
            volume, _ = _shared(shape, path)
            if volume > OVERLAP_TOLERANCE_MM3:
                blocked.append({"part": name, "volume": volume})
        data["blocked"] = blocked
        data["path_clear"] = not blocked
        trapped = [entry for entry in blocked
                   if entry["part"] not in {i["part"] for i in intrusions}]
        if trapped:
            worst = max(trapped, key=lambda entry: entry["volume"])
            report.problems.append(
                f"the thing is trapped: '{worst['part']}' has {worst['volume']:.1f} mm3 of "
                f"material in its way out along {_direction_words(pocket.direction)} (an "
                "undercut or a rim narrower than the thing). Open the pocket along that "
                "direction, or name a part that is taken off first in `removable`")
    else:
        _verify_stretch(pocket, report, staying, frame, sweep, facts)

    if not robustness["ok"]:
        (report.problems if robustness["offsets_provenance"] == "calibrated"
         and pocket.clearance.target_provenance != "guessed" else report.cautions).append(
            f"across this printer's spread the fit is not sure ({robustness['reason']}); "
            "print the pocket's gauge before the product")
    elif pocket.clearance.provenance == "guessed" and pocket.fit == "stretch":
        report.cautions.append(
            "the stretch fit's interference is a guess until its gauge (a 5 mm band of the "
            "rim) has been printed and tried on the thing")
    allowed = _thing_tolerance(pocket)
    if allowed > 0:
        # The thing itself varies: its maker's tolerance, per side, widens the spread.
        low, high = robustness["window"]
        printed = robustness["printed"].values()
        tightest, loosest = min(printed) - allowed, max(printed) + allowed
        outside = ((low is not None and tightest < low - 1e-9)
                   or (high is not None and loosest > high + 1e-9))
        data["thing_tolerance"] = {"per_side": allowed, "printed": [tightest, loosest],
                                   "window": [low, high], "ok": not outside}
        if outside:
            report.cautions.append(
                f"the maker allows '{pocket.reference.label}' itself {allowed:.2f} mm either "
                f"way per side; with that the printed gap runs from {tightest:+.2f} to "
                f"{loosest:+.2f} mm per side, which leaves the {pocket.fit} fit's window on "
                "a thing at the end of its tolerance: try the gauge on the real thing")
    if pocket.reference.provenance not in ("measured", "standard", "verified"):
        report.cautions.append(
            f"the size of '{pocket.reference.label}' is {pocket.reference.provenance}, not "
            "measured")
    return report


def _verify_stretch(pocket: Pocket, report: PocketReport, staying: list, frame: Any,
                    sweep: float, facts: Any) -> None:
    """The two things a stretch fit adds: it holds the rim, and its lip can be passed."""
    local, data = pocket._frame, report.data
    reference = pocket.reference
    to_model_rotation, to_model_translation = _matrix_of(frame * local.to_reference)

    def to_model(points: np.ndarray) -> np.ndarray:
        return points @ to_model_rotation.T + to_model_translation

    # Holding: material starts within GRIP_PROBE_MM of the drawn wall, around the rim.
    probe = reference._lateral(local, pocket.shape, pocket.offset + GRIP_PROBE_MM, pocket.scale)
    stations = _stations([np.array(p) for p in probe.to_polygons()], GRIP_STATIONS)
    height = (local.z1 - local.z0) * pocket.scale
    held = np.zeros(len(stations), dtype=bool)
    shapes = [shape for _, shape in staying]
    for level in GRIP_LEVELS:
        points = np.column_stack([stations, np.full(len(stations), local.z0 + level * height)])
        held |= _inside_any(to_model(points), shapes)
    coverage = float(held.mean())
    data["grip_coverage"] = coverage
    if coverage < MIN_GRIP_COVERAGE:
        report.problems.append(
            f"it holds only {100 * coverage:.0f} % of the thing's rim (walls where the fit "
            f"put them); under {100 * MIN_GRIP_COVERAGE:.0f} % a shell does not grip. Keep "
            "the walls the pocket cut, or close some openings")

    # Passing the lip: what stands in the thing's way beyond its far end must stretch.
    outline = [np.array(p) for p in local.base.to_polygons()]
    beyond = frame * local.to_reference * _prism(
        [np.array(p) for p in _grown(local.base, -PROOF_TOLERANCE_MM).to_polygons()],
        local.z0 + height + PROOF_TOLERANCE_MM, local.z0 + height + sweep)
    back_rotation = to_model_rotation.T
    depth, volume = 0.0, 0.0
    for _, shape in staying:
        shared, solids = _shared(shape, beyond)
        if shared <= OVERLAP_TOLERANCE_MM3:
            continue
        volume += shared
        samples = []
        for solid in solids:
            samples += [tuple(v) for v in solid.vertices()]
            samples += [tuple(edge.position_at(u)) for edge in solid.edges()
                        for u in (0.25, 0.5, 0.75)]
        points = (np.array(samples) - to_model_translation) @ back_rotation.T
        depth = max(depth, _depth_inside(points[:, :2], outline))
    x0, y0, x1, y1 = local.bounds
    strain = 2.0 * depth / min(x1 - x0, y1 - y0)
    limit = facts.stretch_strain
    data.update({"lip_depth": depth, "lip_volume": volume, "stretch_strain": strain,
                 "stretch_limit": limit, "path_clear": None})
    if depth <= 0.0:
        report.cautions.append(
            f"nothing reaches over the thing's far end, so only friction keeps it from "
            f"sliding out along {_direction_words(pocket.direction)}")
    elif limit is not None and strain > limit:
        report.problems.append(
            f"the lip reaches {depth:.2f} mm over the thing per side, so the rim must "
            f"stretch {100 * strain:.1f} % to let it in; {facts.family} is taken to allow "
            f"{100 * limit:.0f} % (a guess). Make the lip shallower")


#: Set in a test run only: an exception in a pocket's proof is written to stderr. Off in the
#: application, whose worker starts from a cleared environment, so nothing of it can reach a
#: conversation, the window or a stored crash report.
TRACE_VARIABLE = "HOTEND_GEOMETRY_TRACE"
MAX_TRACE_CHARS = 3000


def _trace_exception() -> None:
    """The end of the traceback being handled, to stderr, when the test variable is set.
    Never raises: it must not turn a failed check into something worse."""
    with contextlib.suppress(Exception):
        if os.environ.get(TRACE_VARIABLE) == "1":
            print(traceback.format_exc()[-MAX_TRACE_CHARS:], file=sys.stderr, flush=True)


def verify_pockets(parts: list[tuple[str, Any]], resolver: FitResolver) -> list[PocketReport]:
    """`verify_pocket` for every pocket the build declared on `resolver`."""
    reports = []
    for pocket in resolver.pockets:
        try:
            reports.append(verify_pocket(pocket, parts))
        except Exception as exc:  # noqa: BLE001 - a kernel failure must not read as a pass
            _trace_exception()
            report = PocketReport(pocket)
            report.problems.append(
                f"it could not be examined ({type(exc).__name__}: {str(exc)[:160]})")
            reports.append(report)
    return reports
