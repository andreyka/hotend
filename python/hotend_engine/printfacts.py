"""Print facts: what the print settings rules need to know about a piece as it lies on the bed.

The engine measures and decides nothing. The rules live in the Rust crate
`hotend-settings`, which reads the object this module writes (`src/facts.rs`, the names
and units below are the same on both sides) and refuses it when a number is missing, not
finite, negative or out of bounds. Every fact is a number, a flag or one of a few fixed
tokens; the only text is the name of a fit pair, held to `PAIR_PATTERN`.

Per piece, for the direction `up` it is printed in:

- `contact_mm2`, `height_mm`, `overhang_mm2`, `fit_overhang_mm2`: the measures print
  preparation already takes (`PartGeometry.measure`), unchanged.
- `contact_perimeter_mm`, `strip_width_mm`: the planar faces on the bed are grouped into
  contact regions (faces that share an edge are one region). A region's perimeter is the
  exact length of its boundary edges, the edges of holes included. `strip_width_mm` is the
  smallest `2 x area / perimeter` over the regions: the width of a wall standing on the
  bed (a ring with a 2 mm wall gives 2.0), half the radius-like size of a compact foot.
  That ratio is blind to a thin wall that shares its region with a wide face (the same
  ring with a pad grown onto it reads 10.9), so each region's narrowest wall is measured
  as well (below), and the smaller of the two is stated. The fact can therefore only be
  smaller than the ratio, never larger: it can ask for more adhesion, never for less.
- `contact_span_mm`: the longest side of the contact's bounding box along the bed axes.
- `slenderness`: the height over the width of the footing. The footing is as wide as
  `sqrt(contact)`, the quantity preparation scores for tipping, or as the narrowest
  width of the contact's convex hull when that is smaller: a wall of 60 x 2 mm that
  stands 40 mm tall is 20 times as tall as its footing is wide, not 3.65 times. The hull
  spans everything the piece stands on, so a ring, an L or four feet are as wide as
  their stance. The fact is never smaller than `height / sqrt(contact)`.
- `inner_loops`: every hole of a contact region with its narrowest width and whether a
  declared mating face borders it (a bore standing on the bed).
- `fit_at_bed`: declared mating faces that rise from the bed (one of their edges lies on
  it), with the side of the fit they are on.
- `overhang_regions`: the connected regions of `overhang_mm2`, each classified as a
  `bridge`, a `hole_ceiling` or a `free` overhang (below), and whether it hangs over the
  piece itself. `null` when the piece has no usable tessellation.
- `overhang_over_part_mm2`: the part of `overhang_mm2` that has the piece itself below it.
- `min_wall_mm`: the thinnest wall the `min_wall` check samples on the piece (the
  same rays, see `checks/min_wall.py`), wherever on the piece it is. `null` when the
  piece has no closed mesh to sample, no two opposing surfaces, or a mesh of more than
  `MAX_WALL_TRIANGLES`.

**The narrowest wall of a contact region.** From points all along the region's outline
(holes included) a line is drawn square to the outline into the material; where it
leaves the region again is the local width. A point counts only when it leaves through
an edge that runs the same way within 30 degrees, the other face of a wall, as the
`min_wall` check does: the corner of a wedge or the neck beside a round hole is no
wall. The narrowest wall is the smallest width that one unbroken `WALL_RUN_MM` of a
wall's face is at most as wide as: a stub shorter than that cannot lift a corner and is
not counted, and neither are short necks that lie apart (a row of holes beside an edge).
A region whose outline has more than `_MAX_OUTLINE_SEGMENTS` pieces is not measured this
way and keeps its ratio.

**Overhang regions.** A triangle of the tessellation hangs when its face does (a planar
face by its exact normal, a curved face triangle by triangle, as `overhang_mm2` is
measured). Hanging triangles that share an edge form a region; the areas of planar faces
are the exact B-rep areas, so the regions sum to `overhang_mm2`. An edge of a region is
*carried* when the surface beyond it goes down from the edge (a wall under a ceiling), or
lies on the bed: the layer that prints the edge then lies on material.

- `hole_ceiling`: the region lies on round faces whose axis lies flat and that curve
  around air (a cross hole, an arch), and both of its long sides are carried.
  `diameter_mm` is the hole's diameter.
- `bridge`: the region is flat and level, and every edge that is not carried runs in one
  direction, so that straight lines in that direction start and end on carried edges.
  `span_mm` is the region's extent in that direction (the narrowest width of the region
  when it is carried all round).
- `free`: anything else. `reach_mm` says how far the overhang sticks out: the farthest
  horizontal distance of one of its points from its nearest carried edge. Two things keep
  the number to what a person would measure. The distance is taken square to a carried
  edge and, past an end or a sharp corner of the carried line, along it (the larger of
  the two, not the diagonal), so that a rim that sticks out 2 mm all round a square foot
  reaches 2 mm at its corners too. And where the overhang ends on a curved face because
  the face gets steeper, the face is followed as far as it still faces down: a bottom
  fillet reaches as far as its radius, although only its flat half counts as area. An
  overhang that nothing carries reaches as far as it is large.

At most `MAX_ENTRIES` regions are listed, largest first; the smaller ones are merged into
one `free` region whose reach is the longest box diagonal among them, an upper bound.

Everything is computed from the B-rep and its tessellation. No model code runs.
"""

from __future__ import annotations

import math
import re
from typing import Any

import numpy as np

from .prepare import (
    BED_TOLERANCE_MM,
    PartFit,
    PartGeometry,
    location_of,
    overhang_threshold,
    rotation_to_up,
)
from .tessellate import PartMesh

# The material families the rules have a print card for (`Family` of hotend-settings).
FAMILIES = ("PLA", "PETG", "TPU", "ABS", "ASA")
# The most overhang regions, inner loops or fit entries of one piece.
MAX_ENTRIES = 64
MAX_SLENDERNESS = 1e4
# A fit pair's name as the rules accept it. Any other label is replaced by `pair_<n>`.
PAIR_PATTERN = r"[a-z][a-z0-9_.-]{0,63}"
INSIDE, OUTSIDE = "inside", "outside"
BRIDGE, HOLE_CEILING, FREE = "bridge", "hole_ceiling", "free"

_PAIR_RE = re.compile(PAIR_PATTERN)
# A curved face is followed for the reach as far as its triangles face down by this much.
_FACES_DOWN = -0.02
# Two carried edges that meet at less than 45 degrees are one bent line, not a corner.
_SMOOTH_COS = 0.7071
# A neighbour goes down from an edge when its far corner is lower by more than this.
_CARRY_TOLERANCE_MM = 1e-3
# How far from straight down a flat ceiling's normal may be (as `facing` of prepare).
_LEVEL = -0.9999
# Edges of a bridge's open sides are parallel within about one degree.
_PARALLEL_SIN = 0.02
# A hole's axis lies flat within about half a degree.
_FLAT_AXIS = 0.01
# A long side of a hole ceiling runs along the axis within about 25 degrees.
_ALONG_AXIS = 0.9
# Points sampled on a curved edge of a contact loop.
_EDGE_SAMPLES = 49
# The run of wall, in mm, that makes a narrow part of a contact region a wall (module doc).
WALL_RUN_MM = 10.0
# The two faces of a wall run the same way within 30 degrees (as `OPPOSING_COS` of the
# `min_wall` check).
_WALL_COS = 0.866
# The outline of a region is walked in steps of this length, in mm; longer steps when the
# outline is longer than `_OUTLINE_STEPS` of them, and a curved edge gets at least
# `_CURVE_STEPS`.
_OUTLINE_STEP_MM = 0.5
_OUTLINE_STEPS = 4000
_CURVE_STEPS = 16
_MAX_OUTLINE_SEGMENTS = 8000
# The narrowest wall replaces the ratio only when it is smaller by more than the error of
# walking a curve in straight steps.
_WALL_MARGIN = 0.99
# The largest mesh, in triangles, whose thinnest wall is sampled for `min_wall_mm`.
MAX_WALL_TRIANGLES = 60_000
# Ends of outline pieces closer than about this are one point.
_JOIN_MM = 1e-3
# The most (ray, triangle) pairs tested for "hangs over the piece itself".
_MAX_RAY_TESTS = 40_000_000
# The reach of a region is taken at its corners and at about this many points inside it.
_REACH_SAMPLES = 6_000
_AREA_DIGITS = 6
_LENGTH_DIGITS = 3


def family_token(material: Any) -> str | None:
    """The material's family as one of `FAMILIES`, or None. The request's own family text
    is never passed on: the rules get a known token or nothing.

    A stated family must be one of the tokens (capitals and spaces around it aside); it is
    never replaced by a guess. When the id begins with another family's name (`tpu-95a`
    stated as PLA), one of the two is wrong and the answer is None: the rules then refuse
    the plate instead of heating the bed for the wrong material. Only when no family is
    stated does the id say it, as in the fit library (`pla-basic`, `tpu_90a`)."""
    if material is None:
        return None

    def text(name: str) -> str | None:
        value = material.get(name) if isinstance(material, dict) else getattr(
            material, name, None)
        if value is None:
            return ""
        return value.strip().upper() if isinstance(value, str) else None

    stated, identifier = text("family"), text("id")
    if stated is None or identifier is None:
        return None
    by_id = next((family for family in FAMILIES
                  if identifier == family or identifier.startswith((family + "-", family + "_"))),
                 None)
    if not stated:
        return by_id
    if stated not in FAMILIES or by_id not in (None, stated):
        return None
    return stated


def plate_facts(family: str | None, pieces: list[dict]) -> dict:
    """The object the rules read for one plate, or for a whole preparation."""
    return {
        "family": family,
        "piece_count": len(pieces),
        "total_contact_mm2": round(sum(piece["contact_mm2"] for piece in pieces), _AREA_DIGITS),
        "pieces": pieces,
    }


def piece_facts(geometry: PartGeometry, mesh: PartMesh | None, up: Any, limit_deg: float,
                fit: PartFit | None = None, measures: dict | None = None) -> dict:
    """The facts of one piece printed with `up` pointing away from the bed (module doc).

    `measures` are the piece's `PartGeometry.measure` for the same direction, when the
    caller has them already."""
    up = _direction(up)
    threshold = overhang_threshold(limit_deg)
    if measures is None:
        measures = geometry.measure(up, threshold, fit)
    usable = mesh if mesh is not None and len(mesh.triangles) and mesh.face_map_ok else None
    faces = list(geometry.part.shape.faces())
    edges = _Edges(faces)
    low = float((geometry.points @ up).min())
    turn = rotation_to_up(up)
    facing = geometry.planar_normals @ up
    on_bed = (facing < _LEVEL) & (
        np.abs(geometry.planar_points @ up - low) <= BED_TOLERANCE_MM)
    bed = [int(i) for i in geometry.planar_index[on_bed]]
    areas = dict(zip((int(i) for i in geometry.planar_index),
                     (float(a) for a in geometry.planar_areas), strict=True))
    mating = dict(fit.faces) if fit is not None else {}

    contact = float(measures["contact"])
    height = float(measures["height"])
    regions = _contact_regions(faces, edges, bed, areas, turn)
    perimeter = sum(region["perimeter"] for region in regions)
    strips = []
    for region in regions:
        if region["perimeter"] <= 0.0:
            continue
        ratio = 2.0 * region["area"] / region["perimeter"]
        wall = _narrowest_wall(region["edges"], turn)
        strips.append(wall if wall is not None and wall < _WALL_MARGIN * ratio else ratio)
    loops = []
    for region in regions:
        for loop in region["inner"]:
            owned = any(edges.touches(face, loop["edges"]) for face in mating)
            loops.append({"min_width_mm": round(loop["min_width"], _LENGTH_DIGITS),
                          "fit": bool(owned)})
    loops.sort(key=lambda loop: (-loop["min_width_mm"], not loop["fit"]))
    if len(loops) > MAX_ENTRIES:
        # Loops a fit owns are kept first: they are the ones that forbid an inner brim.
        kept = sorted(loops, key=lambda loop: (not loop["fit"], -loop["min_width_mm"]))
        loops = sorted(kept[:MAX_ENTRIES], key=lambda loop: (-loop["min_width_mm"],
                                                             not loop["fit"]))

    overhang = float(measures["overhang"])
    hanging, over_part = None, overhang
    if overhang <= 1e-9 and geometry.unmeasured == 0:
        hanging, over_part = [], 0.0
    elif usable is not None and geometry.unmeasured == 0:
        found = _overhang_regions(geometry, usable, faces, up, turn, threshold, low, overhang)
        if found is not None:
            hanging, over_part = found
    return {
        "contact_mm2": round(contact, _AREA_DIGITS),
        "contact_perimeter_mm": round(perimeter, _AREA_DIGITS),
        "strip_width_mm": round(min(strips), _AREA_DIGITS) if strips else 0.0,
        "contact_span_mm": round(_contact_span(faces, bed, turn), _AREA_DIGITS),
        "height_mm": round(height, _AREA_DIGITS),
        "slenderness": round(_slenderness(height, contact, regions), _AREA_DIGITS),
        "overhang_mm2": round(overhang, _AREA_DIGITS),
        "fit_overhang_mm2": round(float(measures["fit_overhang"]), _AREA_DIGITS),
        # Unknown counts as "all of it": supports are then not kept away from the piece.
        "overhang_over_part_mm2": round(min(over_part, overhang), _AREA_DIGITS),
        "overhang_regions": hanging,
        "inner_loops": loops,
        "fit_at_bed": _fit_at_bed(faces, edges, mating, set(bed), up, low, usable),
        "min_wall_mm": _thinnest_wall(geometry.part, mesh),
    }


def _direction(up: Any) -> np.ndarray:
    """`up` as a unit vector. A direction that is none (not three finite numbers, or all
    zero) would turn every fact into "not a number"; it is refused."""
    try:
        vector = np.asarray(up, dtype=float)
    except (TypeError, ValueError):
        vector = np.zeros(0)
    largest = float(np.abs(vector).max()) if vector.shape == (3,) else 0.0
    if not math.isfinite(largest) or largest == 0.0:
        raise ValueError("the print direction `up` must be three finite numbers that are "
                         "not all zero; pass the `up` of the orientation that was chosen")
    # Scaled first, so that neither a very long nor a very short vector loses its length.
    vector = vector / largest
    return vector / float(np.linalg.norm(vector))


def _slenderness(height: float, contact: float, regions: list[dict]) -> float:
    """The height over the width of the footing (module doc), at most `MAX_SLENDERNESS`."""
    if contact <= 1e-9:
        # Nothing flat on the bed is the most slender a piece can be.
        return MAX_SLENDERNESS if height > 0.0 else 0.0
    footing = math.sqrt(contact)
    outline = [region["points"] for region in regions if len(region["points"])]
    if outline:
        footing = min(footing, _narrowest_width(np.concatenate(outline)))
    return min(height / footing, MAX_SLENDERNESS) if footing > 1e-9 else MAX_SLENDERNESS


def _thinnest_wall(part: Any, mesh: PartMesh | None) -> float | None:
    """The thinnest wall the `min_wall` check samples on the piece, or None when it cannot
    sample one. The check's own limit plays no part: only its measurement is used. A mesh
    of more than `MAX_WALL_TRIANGLES` is not sampled: the time grows with the square of
    the count, and a preparation that runs out of time gives no plate at all."""
    if mesh is None or not 0 < len(mesh.triangles) <= MAX_WALL_TRIANGLES:
        return None
    from .checks.base import CheckEnv
    from .checks.min_wall import min_wall
    from .request import CheckOptions
    from .tessellate import Tessellation

    env = CheckEnv([part], None, CheckOptions(),  # type: ignore[arg-type]
                   Tessellation({}, {part.name: mesh}, len(mesh.triangles), 0.0))
    thinnest = min_wall(env, part).data.get("thinnest")
    if not isinstance(thinnest, float) or not math.isfinite(thinnest):
        return None
    return round(min(max(thinnest, 0.0), 1000.0), _LENGTH_DIGITS)


def faces_on_inner_loops(shape: Any, up: Any = (0.0, 0.0, 1.0)) -> list[int]:
    """The faces of `shape` (by index in `shape.faces()`) that border a hole of its contact
    with the bed: the inside of a ring gauge, which is its mating surface."""
    from .parts import NamedPart

    geometry = PartGeometry(NamedPart("piece", shape, "#000000"), None)
    up = _direction(up)
    faces = list(shape.faces())
    edges = _Edges(faces)
    low = float((geometry.points @ up).min())
    on_bed = (geometry.planar_normals @ up < _LEVEL) & (
        np.abs(geometry.planar_points @ up - low) <= BED_TOLERANCE_MM)
    bed = [int(i) for i in geometry.planar_index[on_bed]]
    areas = dict(zip((int(i) for i in geometry.planar_index),
                     (float(a) for a in geometry.planar_areas), strict=True))
    found = set()
    for region in _contact_regions(faces, edges, bed, areas, rotation_to_up(up)):
        for loop in region["inner"]:
            found.update(index for index in range(len(faces))
                         if index not in bed and edges.touches(index, loop["edges"]))
    return sorted(found)


# --------------------------------------------------------------------------------------
# the contact with the bed (exact, from the B-rep)
# --------------------------------------------------------------------------------------


class _Edges:
    """The edges of a part's faces with one number per edge, however a face holds it."""

    def __init__(self, faces: list) -> None:
        self._faces = faces
        self._buckets: dict[int, list[tuple[Any, int]]] = {}
        self._of_face: dict[int, list[int]] = {}
        self.edges: list[Any] = []

    def _number(self, edge: Any) -> int:
        bucket = self._buckets.setdefault(hash(edge.wrapped), [])
        for known, number in bucket:
            if known.IsSame(edge.wrapped):
                return number
        bucket.append((edge.wrapped, len(self.edges)))
        self.edges.append(edge)
        return len(self.edges) - 1

    def of(self, face: int) -> list[int]:
        if face not in self._of_face:
            self._of_face[face] = [self._number(edge) for edge in self._faces[face].edges()]
        return self._of_face[face]

    def touches(self, face: int, edges: set[int]) -> bool:
        return 0 <= face < len(self._faces) and not edges.isdisjoint(self.of(face))


def _contact_regions(faces: list, edges: _Edges, bed: list[int], areas: dict[int, float],
                     turn: np.ndarray) -> list[dict]:
    """The bed faces grouped into regions: area, perimeter and the holes of each."""
    group = {face: face for face in bed}

    def root(face: int) -> int:
        while group[face] != face:
            group[face] = group[group[face]]
            face = group[face]
        return face

    users: dict[int, list[int]] = {}
    for face in bed:
        for edge in edges.of(face):
            users.setdefault(edge, []).append(face)
    for sharing in users.values():
        for other in sharing[1:]:
            a, b = root(sharing[0]), root(other)
            if a != b:
                group[max(a, b)] = min(a, b)
    regions: dict[int, dict] = {}
    for face in bed:
        region = regions.setdefault(root(face), {"area": 0.0, "boundary": []})
        region["area"] += areas[face]
    for edge, sharing in users.items():
        if len(sharing) == 1:
            regions[root(sharing[0])]["boundary"].append(edge)
    result = []
    for key in sorted(regions):
        region = regions[key]
        loops = _loops([(number, edges.edges[number]) for number in sorted(region["boundary"])],
                       turn)
        outer = max(range(len(loops)), key=lambda i: loops[i]["area"], default=None)
        result.append({
            "area": region["area"],
            "perimeter": sum(float(edges.edges[number].length) for number in region["boundary"]),
            "inner": [loop for index, loop in enumerate(loops) if index != outer],
            "edges": [edges.edges[number] for number in sorted(region["boundary"])],
            "points": (np.concatenate([loop["outline"] for loop in loops])
                       if loops else np.zeros((0, 2))),
        })
    return result


def _loops(boundary: list[tuple[int, Any]], turn: np.ndarray) -> list[dict]:
    """The boundary edges of a contact region chained into closed loops, each with the
    numbers of its edges, the area it encloses and its narrowest width."""
    from build123d import GeomType

    def key(point: np.ndarray) -> tuple:
        return tuple(np.round(point, 5) + 0.0)

    pieces = []
    for number, edge in boundary:
        if edge.geom_type == GeomType.LINE:
            points = np.array([tuple(edge.position_at(0.0)), tuple(edge.position_at(1.0))])
        else:
            points = np.array([tuple(edge.position_at(float(t)))
                               for t in np.linspace(0.0, 1.0, _EDGE_SAMPLES)])
        flat = (points @ turn.T)[:, :2]
        pieces.append({"number": number, "edge": edge, "points": flat,
                       "ends": (key(points[0]), key(points[-1]))})
    at: dict[tuple, list[int]] = {}
    for index, piece in enumerate(pieces):
        for end in piece["ends"]:
            at.setdefault(end, []).append(index)
    used = [False] * len(pieces)
    loops = []
    for first in range(len(pieces)):
        if used[first]:
            continue
        used[first] = True
        chain, numbers = [pieces[first]["points"]], [first]
        start, here = pieces[first]["ends"]
        while here != start:
            following = next((i for i in at[here] if not used[i]), None)
            if following is None:
                following = _nearest_end(pieces, used, chain[-1][-1])
            if following is None:
                break  # an open chain: closed by the straight line back to its start
            used[following] = True
            piece = pieces[following]
            if (np.linalg.norm(piece["points"][0] - chain[-1][-1])
                    <= np.linalg.norm(piece["points"][-1] - chain[-1][-1])):
                chain.append(piece["points"])
                here = piece["ends"][1]
            else:
                chain.append(piece["points"][::-1])
                here = piece["ends"][0]
            numbers.append(following)
        outline = np.concatenate(chain)
        x, y = outline[:, 0], outline[:, 1]
        area = 0.5 * abs(float(x @ np.roll(y, -1) - y @ np.roll(x, -1)))
        length = sum(float(pieces[i]["edge"].length) for i in numbers)
        lone = pieces[numbers[0]]["edge"] if len(numbers) == 1 else None
        if lone is not None and lone.geom_type == GeomType.CIRCLE:
            width = 2.0 * float(lone.radius)
        else:
            # The hull's width is right for a convex hole; `4 x area / perimeter` (the
            # diameter of a round hole) keeps a winding hole from looking wide.
            width = min(_narrowest_width(outline),
                        4.0 * area / length if length > 0.0 else 0.0)
        loops.append({"edges": {pieces[i]["number"] for i in numbers}, "area": area,
                      "min_width": width, "outline": outline})
    return loops


def _outline_segments(boundary: list, turn: np.ndarray) -> np.ndarray | None:
    """The outline of a contact region as short straight pieces, (n, 2, 2), seen from
    above. None when it would take more than `_MAX_OUTLINE_SEGMENTS`."""
    from build123d import GeomType

    lengths = [float(edge.length) for edge in boundary]
    step = max(_OUTLINE_STEP_MM, sum(lengths) / _OUTLINE_STEPS)
    counts = [max(math.ceil(length / step), 1 if edge.geom_type == GeomType.LINE
                  else _CURVE_STEPS)
              for edge, length in zip(boundary, lengths, strict=True)]
    if sum(counts) > _MAX_OUTLINE_SEGMENTS:
        return None
    chains = []
    for edge, length, count in zip(boundary, lengths, counts, strict=True):
        if length <= 1e-9:
            continue
        along = np.linspace(0.0, 1.0, count + 1)
        if edge.geom_type == GeomType.LINE:
            start, end = (np.array(tuple(edge.position_at(t))) for t in (0.0, 1.0))
            points = start + along[:, None] * (end - start)
        else:
            points = np.array([tuple(edge.position_at(float(t))) for t in along])
        flat = (points @ turn.T)[:, :2]
        chains.append(np.stack([flat[:-1], flat[1:]], axis=1))
    return np.concatenate(chains) if chains else None


def _narrowest_wall(boundary: list, turn: np.ndarray) -> float | None:
    """The narrowest wall of a contact region (module doc): the smallest width that one
    unbroken `WALL_RUN_MM` of a wall's face is at most as wide as. None when the region
    has no such run of wall, or its outline is too long to walk."""
    segments = _outline_segments(boundary, turn)
    if segments is None:
        return None
    start, end = segments[:, 0], segments[:, 1]
    along = end - start
    lengths = np.linalg.norm(along, axis=1)
    keep = lengths > 1e-9
    start, end, along, lengths = start[keep], end[keep], along[keep], lengths[keep]
    if len(start) < 3:
        return None
    unit = along / lengths[:, None]
    middle = 0.5 * (start + end)
    square = np.stack([-unit[:, 1], unit[:, 0]], axis=1)
    width = np.full(len(start), np.nan)
    chunk = max(1, 2_000_000 // len(start))
    for first in range(0, len(start), chunk):
        point, line = middle[first:first + chunk, None, :], square[first:first + chunk, None, :]
        # Every piece of the outline against the line through the point, square to the
        # outline there: which side of the line its two ends lie on, and how far along
        # the line it is crossed.
        a, b = start[None, :, :] - point, end[None, :, :] - point
        side_a = line[:, :, 0] * a[:, :, 1] - line[:, :, 1] * a[:, :, 0]
        side_b = line[:, :, 0] * b[:, :, 1] - line[:, :, 1] * b[:, :, 0]
        crossed = (side_a > 0.0) != (side_b > 0.0)
        far_a, far_b = (line * a).sum(axis=2), (line * b).sum(axis=2)
        share = side_a / np.where(crossed, side_a - side_b, 1.0)
        far = far_a + share * (far_b - far_a)
        ahead, behind = crossed & (far > 1e-6), crossed & (far < -1e-6)
        # The material lies on the side where the line crosses the outline an odd number
        # of times; the nearest crossing there is where it leaves the region.
        inward = np.where((ahead.sum(axis=1) % 2 == 1)[:, None], ahead, behind)
        known = (ahead.sum(axis=1) + behind.sum(axis=1)) % 2 == 1
        reach = np.where(inward, np.abs(far), np.inf)
        other = reach.argmin(axis=1)
        nearest = reach[np.arange(len(reach)), other]
        mine = unit[first:first + chunk]
        opposite = np.abs((mine * unit[other]).sum(axis=1)) >= _WALL_COS
        found = known & np.isfinite(nearest) & opposite
        width[first:first + chunk] = np.where(found, nearest, np.nan)
    measured = np.flatnonzero(~np.isnan(width))
    if lengths[measured].sum() < WALL_RUN_MM:
        return None
    # The pieces are taken narrowest first and joined to the pieces they touch end to
    # end; the first width at which one joined run is long enough is the answer. Necks
    # that are short each and lie apart (a row of holes beside an edge) never join.
    at: dict[tuple[int, int], list[int]] = {}
    cells = np.floor(np.stack([start, end], axis=1) / _JOIN_MM).astype(np.int64)
    leader = list(range(len(start)))
    run = [0.0] * len(start)

    def root(piece: int) -> int:
        while leader[piece] != piece:
            leader[piece] = leader[leader[piece]]
            piece = leader[piece]
        return piece

    for piece in (int(i) for i in measured[np.argsort(width[measured], kind="stable")]):
        run[piece] = float(lengths[piece])
        for x, y in ((int(c[0]), int(c[1])) for c in cells[piece]):
            for cell in ((x + i, y + j) for i in (-1, 0, 1) for j in (-1, 0, 1)):
                for neighbour in at.get(cell, ()):
                    a, b = root(piece), root(neighbour)
                    if a != b:
                        leader[b] = a
                        run[a] += run[b]
            at.setdefault((x, y), []).append(piece)
        if run[root(piece)] >= WALL_RUN_MM:
            return float(width[piece])
    return None


def _nearest_end(pieces: list[dict], used: list[bool], point: np.ndarray) -> int | None:
    """The unused edge that starts or ends at `point`, when rounding hid the match."""
    for index, piece in enumerate(pieces):
        if not used[index] and min(np.linalg.norm(piece["points"][0] - point),
                                   np.linalg.norm(piece["points"][-1] - point)) <= 1e-4:
            return index
    return None


def _contact_span(faces: list, bed: list[int], turn: np.ndarray) -> float:
    """The longest side of the bounding box of the contact, along the bed's axes."""
    if not bed:
        return 0.0
    placement = location_of(turn, np.zeros(3))
    low, high = np.full(2, math.inf), np.full(2, -math.inf)
    for face in bed:
        box = (placement * faces[face]).bounding_box()
        low = np.minimum(low, (box.min.X, box.min.Y))
        high = np.maximum(high, (box.max.X, box.max.Y))
    return float(max(high - low))


def _hull(points: np.ndarray) -> np.ndarray:
    """The convex hull of 2D points, counter-clockwise (Andrew's monotone chain)."""
    points = np.unique(np.round(points, 9), axis=0)
    if len(points) < 3:
        return points
    ordered = [tuple(p) for p in points]

    def half(sequence: list) -> list:
        chain: list = []
        for p in sequence:
            while len(chain) >= 2 and (
                (chain[-1][0] - chain[-2][0]) * (p[1] - chain[-2][1])
                - (chain[-1][1] - chain[-2][1]) * (p[0] - chain[-2][0])
            ) <= 0.0:
                chain.pop()
            chain.append(p)
        return chain

    lower, upper = half(ordered), half(ordered[::-1])
    return np.array(lower[:-1] + upper[:-1])


def _narrowest_width(points: np.ndarray) -> float:
    """The smallest distance between two parallel lines that enclose the points."""
    hull = _hull(points)
    if len(hull) < 3:
        return 0.0
    sides = np.roll(hull, -1, axis=0) - hull
    lengths = np.linalg.norm(sides, axis=1)
    keep = lengths > 1e-12
    normals = np.stack([-sides[keep, 1], sides[keep, 0]], axis=1) / lengths[keep, None]
    # For each side, how far the farthest hull point lies from its line.
    reach = (hull[None, :, :] - hull[keep][:, None, :]) @ normals[:, :, None]
    return float(np.abs(reach[:, :, 0]).max(axis=1).min()) if len(normals) else 0.0


# --------------------------------------------------------------------------------------
# mating faces on the bed
# --------------------------------------------------------------------------------------


def _fit_at_bed(faces: list, edges: _Edges, mating: dict[int, str], bed: set[int],
                up: np.ndarray, low: float, mesh: PartMesh | None) -> list[dict]:
    """`{pair, side}` for every declared mating face that rises from the bed."""
    standing = []
    for index in sorted(mating):
        if index in bed or not 0 <= index < len(faces):
            continue  # a face lying flat on the bed has no bottom edge to grow a foot
        on_bed = any(
            all(abs(float(np.array(tuple(edges.edges[number].position_at(t))) @ up) - low)
                <= BED_TOLERANCE_MM for t in (0.0, 0.25, 0.5, 0.75, 1.0))
            for number in edges.of(index)
        )
        if on_bed:
            standing.append(index)
    # A label outside the pattern, or one that is no text at all, is named by position,
    # never copied; the names given are ones no real pair of the piece has.
    def plain(label: Any) -> bool:
        return isinstance(label, str) and _PAIR_RE.fullmatch(label) is not None

    taken = {label for label in mating.values() if plain(label)}
    names: dict[Any, str] = {}
    number = 0
    for index in standing:
        label = mating[index]
        key = label if isinstance(label, str) else (type(label).__name__, repr(label))
        if plain(label) or key in names:
            continue
        number += 1
        while f"pair_{number}" in taken:
            number += 1
        names[key] = f"pair_{number}"

    def name_of(label: Any) -> str:
        if plain(label):
            return label
        return names[label if isinstance(label, str) else (type(label).__name__, repr(label))]

    entries = {(name_of(mating[index]), _side(faces[index], index, mesh)) for index in standing}
    return [{"pair": pair, "side": side} for pair, side in sorted(entries)][:MAX_ENTRIES]


def _side(face: Any, index: int, mesh: PartMesh | None) -> str:
    """`inside` when the face looks at more of its own piece (a bore, the wall of an
    opening), `outside` when it looks away from it (a peg)."""
    from build123d import GeomType

    if face.geom_type == GeomType.CYLINDER:
        from OCP.BRepAdaptor import BRepAdaptor_Surface

        cylinder = BRepAdaptor_Surface(face.wrapped).Cylinder()
        axis = cylinder.Axis()
        origin = np.array([axis.Location().X(), axis.Location().Y(), axis.Location().Z()])
        along = np.array([axis.Direction().X(), axis.Direction().Y(), axis.Direction().Z()])
        point = face.position_at(0.5, 0.5)
        normal = np.array(tuple(face.normal_at(point)))
        radial = np.array(tuple(point)) - origin
        radial = radial - (radial @ along) * along
        return INSIDE if float(normal @ radial) < 0.0 else OUTSIDE
    if mesh is None:
        return OUTSIDE
    mine = np.flatnonzero(mesh.triangle_face == index)
    if len(mine) == 0:
        return OUTSIDE
    normals, areas = mesh.triangle_normals_and_areas()
    seed = mine[int(np.argmax(areas[mine]))]
    # Turn the piece so that the face's normal points down, then look "below" the face.
    turn = rotation_to_up(-normals[seed])
    points = mesh.triangle_points @ turn.T
    hits = _covered_below(points[seed].mean(axis=0)[None, :], points)
    return INSIDE if hits is not None and bool(hits[0]) else OUTSIDE


# --------------------------------------------------------------------------------------
# overhang regions (from the tessellation)
# --------------------------------------------------------------------------------------


def _components(count: int, a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """The smallest member of each node's connected component, for edges (a[i], b[i])."""
    parent = np.arange(count)
    while len(a):
        pa, pb = parent[a], parent[b]
        differ = pa != pb
        if not differ.any():
            break
        np.minimum.at(parent, np.maximum(pa, pb)[differ], np.minimum(pa, pb)[differ])
        while True:
            above = parent[parent]
            if np.array_equal(above, parent):
                break
            parent = above
    return parent


def _covered_below(queries: np.ndarray, triangles: np.ndarray) -> np.ndarray | None:
    """For each query point, whether a triangle that faces up lies straight below it.
    None when the question is too large to answer within `_MAX_RAY_TESTS`."""
    hit = np.zeros(len(queries), dtype=bool)
    cross = np.cross(triangles[:, 1] - triangles[:, 0], triangles[:, 2] - triangles[:, 0])
    triangles = triangles[cross[:, 2] > 1e-9]
    if len(queries) == 0 or len(triangles) == 0:
        return hit
    low, high = queries[:, :2].min(axis=0), queries[:, :2].max(axis=0)
    box_low, box_high = triangles[:, :, :2].min(axis=1), triangles[:, :, :2].max(axis=1)
    near = (box_high >= low).all(axis=1) & (box_low <= high).all(axis=1) & (
        triangles[:, :, 2].min(axis=1) < queries[:, 2].max())
    triangles, box_low, box_high = triangles[near], box_low[near], box_high[near]
    if len(triangles) == 0:
        return hit
    # A grid over the queries; each triangle is listed in the cells its box covers.
    cells = int(np.clip(math.sqrt(len(queries)), 1, 512))
    while True:
        size = np.maximum((high - low) / cells, 1e-9)
        first = np.clip(np.floor((box_low - low) / size), 0, cells - 1).astype(np.int64)
        last = np.clip(np.floor((box_high - low) / size), 0, cells - 1).astype(np.int64)
        wide, deep = last[:, 0] - first[:, 0] + 1, last[:, 1] - first[:, 1] + 1
        covered = wide * deep
        if covered.sum() <= _MAX_RAY_TESTS // 4 or cells == 1:
            break
        cells = max(1, cells // 2)
    owner = np.repeat(np.arange(len(triangles)), covered)
    within = np.arange(len(owner)) - np.repeat(np.cumsum(covered) - covered, covered)
    cell = (first[owner, 0] + within // deep[owner]) * cells + first[owner, 1] + within % deep[owner]
    order = np.argsort(cell, kind="stable")
    cell, owner = cell[order], owner[order]
    spot = np.clip(np.floor((queries[:, :2] - low) / size), 0, cells - 1).astype(np.int64)
    spot = spot[:, 0] * cells + spot[:, 1]
    begin, end = np.searchsorted(cell, spot, "left"), np.searchsorted(cell, spot, "right")
    listed = end - begin
    if int(listed.sum()) > _MAX_RAY_TESTS:
        return None
    asker = np.repeat(np.arange(len(queries)), listed)
    target = owner[np.repeat(begin, listed) + np.arange(len(asker))
                   - np.repeat(np.cumsum(listed) - listed, listed)]
    step = 2_000_000
    for start in range(0, len(asker), step):
        q, t = queries[asker[start:start + step]], triangles[target[start:start + step]]
        a, b, c = t[:, 0], t[:, 1], t[:, 2]
        ab, ac, aq = b[:, :2] - a[:, :2], c[:, :2] - a[:, :2], q[:, :2] - a[:, :2]
        twice = ab[:, 0] * ac[:, 1] - ab[:, 1] * ac[:, 0]
        safe = np.where(np.abs(twice) > 1e-12, twice, 1.0)
        u = (aq[:, 0] * ac[:, 1] - aq[:, 1] * ac[:, 0]) / safe
        v = (ab[:, 0] * aq[:, 1] - ab[:, 1] * aq[:, 0]) / safe
        height = a[:, 2] + u * (b[:, 2] - a[:, 2]) + v * (c[:, 2] - a[:, 2])
        inside = (np.abs(twice) > 1e-12) & (u >= -1e-9) & (v >= -1e-9) & (u + v <= 1.0 + 1e-9)
        hit[asker[start:start + step][inside & (height < q[:, 2] - _CARRY_TOLERANCE_MM)]] = True
    return hit


def _cylinder(face: Any) -> tuple[np.ndarray, np.ndarray, float] | None:
    """(a point on the axis, the axis direction, the radius) of a cylindrical face."""
    from build123d import GeomType

    if face.geom_type != GeomType.CYLINDER:
        return None
    from OCP.BRepAdaptor import BRepAdaptor_Surface

    cylinder = BRepAdaptor_Surface(face.wrapped).Cylinder()
    axis = cylinder.Axis()
    return (np.array([axis.Location().X(), axis.Location().Y(), axis.Location().Z()]),
            np.array([axis.Direction().X(), axis.Direction().Y(), axis.Direction().Z()]),
            float(cylinder.Radius()))


def _farthest(points: np.ndarray, segments: np.ndarray) -> float:
    """How far the farthest of the 2D points lies from the nearest of the 2D segments
    (the carried edges). Beside a segment the distance is taken square to it. Past an end
    where the carried line bends on gently it is the distance to that end; past a free
    end or a sharp corner it is the larger of "square to it" and "along it"."""
    start, along = segments[:, 0], segments[:, 1] - segments[:, 0]
    lengths = np.maximum(np.linalg.norm(along, axis=1), 1e-9)
    unit = along / lengths[:, None]
    # Which ends continue smoothly into another carried edge.
    meeting: dict[tuple, list[int]] = {}
    for index, ends in enumerate(np.round(segments, 5).tolist()):
        for end in ends:
            meeting.setdefault(tuple(end), []).append(index)
    smooth = np.zeros((len(segments), 2), dtype=bool)
    for index, ends in enumerate(np.round(segments, 5).tolist()):
        for side, end in enumerate(ends):
            others = [other for other in meeting[tuple(end)] if other != index]
            smooth[index, side] = any(
                abs(float(unit[index] @ unit[other])) > _SMOOTH_COS for other in others)
    farthest = 0.0
    step = max(1, 2_000_000 // max(len(segments), 1))
    for index in range(0, len(points), step):
        offset = points[index:index + step, None, :] - start[None, :, :]
        beside = np.einsum("pcj,cj->pc", offset, unit)
        square = np.abs(offset[:, :, 0] * unit[None, :, 1] - offset[:, :, 1] * unit[None, :, 0])
        before, after = np.maximum(-beside, 0.0), np.maximum(beside - lengths[None, :], 0.0)
        past = np.maximum(before, after)
        rounded = np.where(before > 0.0, smooth[None, :, 0], smooth[None, :, 1])
        nearest = np.where(rounded, np.hypot(square, past), np.maximum(square, past)).min(axis=1)
        farthest = max(farthest, float(nearest.max()))
    return farthest


def _spread(triangles: np.ndarray) -> np.ndarray:
    """2D points of a region for measuring its reach: every corner of its triangles, and a
    grid of points inside it (a long strip of a round bar has corners at its ends only)."""
    flat = triangles[:, :, :2]
    corners = np.unique(np.round(flat.reshape(-1, 2), 6), axis=0)
    low, high = corners.min(axis=0), corners.max(axis=0)
    size = high - low
    step = max(math.sqrt(float(size[0] * size[1]) / _REACH_SAMPLES), 0.05)
    if min(size) <= step:
        step = max(float(max(size)) / _REACH_SAMPLES, 0.05)
    axes = [np.arange(low[k] + step / 2, high[k], step) if size[k] > step
            else np.array([(low[k] + high[k]) / 2]) for k in (0, 1)]
    grid = np.stack(np.meshgrid(*axes, indexing="ij"), axis=-1).reshape(-1, 2)
    # "Inside the region" is "below one of its triangles", seen from above with every
    # triangle turned to face up.
    lifted = np.concatenate([flat, np.zeros((len(flat), 3, 1))], axis=2)
    ab, ac = lifted[:, 1] - lifted[:, 0], lifted[:, 2] - lifted[:, 0]
    wrong = ab[:, 0] * ac[:, 1] - ab[:, 1] * ac[:, 0] < 0.0
    lifted[wrong] = lifted[wrong][:, [0, 2, 1]]
    inside = _covered_below(np.concatenate([grid, np.ones((len(grid), 1))], axis=1), lifted)
    return corners if inside is None else np.concatenate([corners, grid[inside]])


def _overhang_regions(geometry: PartGeometry, mesh: PartMesh, faces: list, up: np.ndarray,
                      turn: np.ndarray, threshold: float, low: float,
                      overhang: float) -> tuple[list[dict], float] | None:
    """The regions of the overhang and the area that hangs over the piece itself, or None
    when the tessellation does not account for the measured overhang."""
    vertices = mesh.vertices @ turn.T  # the piece as printed: +Z away from the bed
    corners = mesh.triangles
    face_of = mesh.triangle_face
    points = vertices[corners]
    cross = np.cross(points[:, 1] - points[:, 0], points[:, 2] - points[:, 0])
    length = np.linalg.norm(cross, axis=1)
    down = cross[:, 2] / np.where(length > 0.0, length, 1.0)
    area = 0.5 * length
    bed_level = float(vertices[:, 2].min())

    # Which faces hang, exactly as `PartGeometry.measure` judges them.
    count = max(len(faces), int(face_of.max()) + 1)
    planar, hangs, level = (np.zeros(count, dtype=bool) for _ in range(3))
    exact = np.zeros(count)
    facing = geometry.planar_normals @ up
    on_bed = (facing < _LEVEL) & (np.abs(geometry.planar_points @ up - low) <= BED_TOLERANCE_MM)
    planar[geometry.planar_index] = True
    hangs[geometry.planar_index] = (facing < threshold) & ~on_bed
    level[geometry.planar_index] = facing < _LEVEL
    exact[geometry.planar_index] = geometry.planar_areas
    hanging = np.where(planar[face_of], hangs[face_of], down < threshold)
    # A planar face counts with its exact area, spread over its triangles.
    meshed = np.bincount(face_of, weights=area, minlength=count)
    scale = np.where(planar & (meshed > 0.0), exact / np.where(meshed > 0.0, meshed, 1.0), 1.0)
    weight = area * scale[face_of]
    if abs(float(weight[hanging].sum()) - overhang) > 0.005 * overhang + 1e-6:
        return None
    if not hanging.any():
        return [], 0.0

    # Triangles that share an edge, with the vertices OCCT doubled along face borders welded.
    span = max(float(np.ptp(vertices, axis=0).max()), 1.0)
    _, welded = np.unique(np.round(vertices / (span * 1e-7)).astype(np.int64), axis=0,
                          return_inverse=True)
    welded = welded.ravel()[corners]
    one, other = welded.ravel(), welded[:, [1, 2, 0]].ravel()
    owner = np.repeat(np.arange(len(corners)), 3)
    which = np.tile(np.arange(3), len(corners))
    real = one != other
    code = (np.minimum(one, other) * (int(welded.max()) + 1) + np.maximum(one, other))[real]
    owner, which = owner[real], which[real]
    order = np.argsort(code, kind="stable")
    code, owner, which = code[order], owner[order], which[order]
    same = code[1:] == code[:-1]
    left, right = owner[:-1][same], owner[1:][same]
    left_edge, right_edge = which[:-1][same], which[1:][same]
    lone = np.ones(len(code), dtype=bool)
    lone[:-1] &= ~same
    lone[1:] &= ~same

    # Regions: hanging triangles joined across the edges they share.
    joined = hanging[left] & hanging[right]
    label = _components(len(corners), left[joined], right[joined])
    roots, region_of = np.unique(label[hanging], return_inverse=True)
    region = np.full(len(corners), -1)
    region[hanging] = region_of
    region_area = np.bincount(region_of, weights=weight[hanging], minlength=len(roots))

    # The edges of each region: an edge between a hanging triangle and one that does not
    # hang, or an edge that belongs to one triangle only.
    crossing = hanging[left] != hanging[right]
    inner = np.where(hanging[left], left, right)[crossing]
    inner_edge = np.where(hanging[left], left_edge, right_edge)[crossing]
    outer = np.where(hanging[left], right, left)[crossing]
    outer_edge = np.where(hanging[left], right_edge, left_edge)[crossing]
    start, end = points[inner, inner_edge], points[inner, (inner_edge + 1) % 3]
    far = points[outer, (outer_edge + 2) % 3]
    resting = (points[outer][:, :, 2] <= bed_level + BED_TOLERANCE_MM).all(axis=1)
    carried = resting | (far[:, 2] < np.minimum(start[:, 2], end[:, 2]) - _CARRY_TOLERANCE_MM)
    # For the reach: where a region ends on a curved face that merely gets steeper (the
    # edge is not carried), the face is followed while it still faces down.
    follows = ~hanging & ~planar[face_of] & (down < _FACES_DOWN)
    onward = np.zeros(len(left), dtype=bool)
    onward[np.flatnonzero(crossing)[~carried & follows[outer] & (face_of[inner] == face_of[outer])]] = True
    onward |= follows[left] & follows[right] & (face_of[left] == face_of[right])
    wider = _components(len(corners), left[joined | onward], right[joined | onward])
    open_owner, open_edge = owner[lone], which[lone]
    open_owner, open_edge = open_owner[hanging[open_owner]], open_edge[hanging[open_owner]]
    edge_region = np.concatenate([region[inner], region[open_owner]])
    edge_wider = np.concatenate([wider[inner], wider[open_owner]])
    edge_start = np.concatenate([start, points[open_owner, open_edge]])
    edge_end = np.concatenate([end, points[open_owner, (open_edge + 1) % 3]])
    edge_carried = np.concatenate([carried, np.zeros(len(open_owner), dtype=bool)])

    # What hangs over the piece itself: something of the piece lies straight below.
    seekers = np.flatnonzero(hanging)
    below = _covered_below(points[seekers].mean(axis=1), points)
    over = np.zeros(len(corners), dtype=bool)
    over[seekers] = True if below is None else below
    over_area = np.bincount(region_of, weights=(weight * over)[hanging], minlength=len(roots))

    ranked = sorted(range(len(roots)), key=lambda r: (-region_area[r], r))
    ranked = [r for r in ranked if region_area[r] > 1e-9]
    listed = ranked if len(ranked) <= MAX_ENTRIES else ranked[:MAX_ENTRIES - 1]
    cylinders: dict[int, Any] = {}
    result = []
    for r in listed:
        mine = np.flatnonzero(region == r)
        borders = edge_region == r
        # What the reach is measured on: the region, the curved faces followed from it,
        # and every carried edge of that whole patch.
        patch = wider[mine[0]]
        holds = (edge_wider == patch) & edge_carried
        result.append(_classify(
            float(region_area[r]), float(over_area[r]), points[mine], face_of[mine],
            edge_start[borders], edge_end[borders], edge_carried[borders],
            points[(hanging | follows) & (wider == patch)],
            np.stack([edge_start[holds][:, :2], edge_end[holds][:, :2]], axis=1),
            faces, level, turn, cylinders))
    rest = ranked[len(listed):]
    if rest:
        # The many small ones as one free overhang; its reach is the largest size among
        # them, which no reach can exceed.
        reach = 0.0
        patches = np.unique(wider[np.isin(region, rest)])
        for patch in patches[:4 * MAX_ENTRIES]:
            flat = points[(hanging | follows) & (wider == patch)][:, :, :2].reshape(-1, 2)
            reach = max(reach, float(np.linalg.norm(np.ptp(flat, axis=0))))
        if len(patches) > 4 * MAX_ENTRIES:
            # Too many to size one by one: no patch is larger than the piece.
            reach = float(np.linalg.norm(np.ptp(vertices[:, :2], axis=0)))
        total, hung = float(region_area[rest].sum()), float(over_area[rest].sum())
        result.append({"area_mm2": round(total, _AREA_DIGITS), "kind": FREE, "span_mm": None,
                       "diameter_mm": None, "reach_mm": round(reach, _LENGTH_DIGITS),
                       "over_part": _hangs_over(hung, total)})
        result.sort(key=lambda entry: -entry["area_mm2"])
    return result, float((weight * over)[hanging].sum())


def _hangs_over(over: float, area: float) -> bool:
    """A region hangs over the piece when 4 mm2 of it does (the area from which supports
    matter), or half of a smaller region."""
    return bool(over >= min(4.0, 0.5 * area) and over > 1e-9)


def _classify(area: float, over: float, points: np.ndarray, face_of: np.ndarray,
              start: np.ndarray, end: np.ndarray, carried: np.ndarray, patch: np.ndarray,
              holds: np.ndarray, faces: list, level: np.ndarray, turn: np.ndarray,
              cylinders: dict[int, Any]) -> dict:
    """One region as `{area_mm2, kind, span_mm, diameter_mm, reach_mm, over_part}`."""
    entry: dict[str, Any] = {"area_mm2": round(area, _AREA_DIGITS), "kind": FREE,
                             "span_mm": None, "diameter_mm": None, "reach_mm": None,
                             "over_part": _hangs_over(over, area)}
    used = [int(f) for f in np.unique(face_of)]
    along = (end - start)
    lengths = np.linalg.norm(along, axis=1)
    real = lengths > 1e-9

    diameter = _hole_ceiling(used, points, along, lengths, carried & real, ~carried & real,
                             faces, turn, cylinders)
    if diameter is not None:
        entry.update(kind=HOLE_CEILING, diameter_mm=round(diameter, _LENGTH_DIGITS))
        return entry

    flat = points[:, :, :2].reshape(-1, 2)
    heights = points[:, :, 2]
    if (all(level[f] for f in used) and float(np.ptp(heights)) <= BED_TOLERANCE_MM
            and (carried & real).any()):
        open_sides = ~carried & real
        if not open_sides.any():
            entry.update(kind=BRIDGE, span_mm=round(_narrowest_width(flat), _LENGTH_DIGITS))
            return entry
        sides = along[open_sides][:, :2]
        longest = sides[int(np.argmax(lengths[open_sides]))]
        direction = longest / np.linalg.norm(longest)
        skew = np.abs(sides[:, 0] * direction[1] - sides[:, 1] * direction[0])
        if (skew <= _PARALLEL_SIN * lengths[open_sides]).all():
            entry.update(kind=BRIDGE,
                         span_mm=round(float(np.ptp(flat @ direction)), _LENGTH_DIGITS))
            return entry

    holds = holds[np.linalg.norm(holds[:, 1] - holds[:, 0], axis=1) > 1e-9]
    if len(holds):
        reach = _farthest(_spread(patch), holds)
    else:
        # Nothing carries it: it reaches as far as it is large.
        reach = float(np.linalg.norm(np.ptp(patch[:, :, :2].reshape(-1, 2), axis=0)))
    entry["reach_mm"] = round(reach, _LENGTH_DIGITS)
    return entry


def _hole_ceiling(used: list[int], points: np.ndarray, along: np.ndarray,
                  lengths: np.ndarray, carried: np.ndarray, loose: np.ndarray, faces: list,
                  turn: np.ndarray, cylinders: dict[int, Any]) -> float | None:
    """The diameter of the round hole whose ceiling the region is, or None."""
    shared = None
    for index in used:
        if index >= len(faces):
            return None
        if index not in cylinders:
            cylinders[index] = _cylinder(faces[index])
        found = cylinders[index]
        if found is None:
            return None
        origin, axis, radius = turn @ found[0], turn @ found[1], found[2]
        if abs(float(axis[2])) > _FLAT_AXIS:
            return None
        if shared is None:
            shared = (origin, axis, radius)
        else:
            offset = origin - shared[0]
            apart = np.linalg.norm(offset - (offset @ shared[1]) * shared[1])
            if (abs(radius - shared[2]) > 1e-3 or apart > 1e-3
                    or abs(float(axis @ shared[1])) < 1.0 - 1e-6):
                return None
    if shared is None:
        return None
    origin, axis, radius = shared
    # A hole curves around air: over the ceiling, the axis lies below the surface.
    centres = points.mean(axis=1)
    if not (centres[:, 2] > origin[2]).all():
        return None
    # Both long sides rest on the hole's own wall.
    lengthwise = np.abs(along @ axis) > _ALONG_AXIS * np.where(lengths > 0.0, lengths, 1.0)
    if (loose & lengthwise).any() or not (carried & lengthwise).any():
        return None
    return 2.0 * radius
