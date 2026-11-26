"""Bought devices as components: a phone, a board, anything a printed part is built around
whose shape comes from a maker's drawing and not from a file (docs/PROTOCOL.md section 4.7).

    phone = ctx.component("sample_phone")         # the engine loads it; a model never reads a file
    phone.size                                    # (width, height, depth) in mm
    pocket = phone.reference.pocket("stretch", ctx=ctx)      # the body, as a Reference
    case = pocket.cut(case, at=Pos(0, 0, back))              # the cavity, proven by reference_fit
    port = phone.feature("usb_c")                 # a button, a port, the camera, the magnets
    case -= at * port.keep_out(reach=6)           # the maker's keep-out volume, as a Solid
    case -= at * phone.feature("side_button").opening(ctx)   # the feature plus a resolver gap
    phone.rule("max_back_thickness")              # a design rule of the maker, as a number

Frame: every component says its own in `frame` (for a phone: origin at the centre of the
outline on the back, +Z to the front). `component.reference` and every feature solid are in
that frame; the Location a pocket is cut `at` puts all of them into the model.

Every number is a `Datum`: value, unit, tolerance, provenance (`measured`, `standard`,
`calibrated`, `guessed`, `style`), confidence (`verified`, `likely`, `unverified`), the source
it was read from (a URL) and a note. A number is proven only when its provenance is
`measured`, `standard` or `calibrated` and its confidence is `verified`. A feature that
has any other number carries a `question` for the user, and a model that uses such a
feature (`component.feature(id)`) gets that question in its result and a warning of the
`reference_fit` check: the model cannot be reported as fully proven until the user has
answered. Nothing here is rounded up: a dimension scaled off a drawing is `guessed`.

One component ships: `sample_phone`, an invented device for examples and tests (`SHIPPED`).
Any other `<id>.json` in this folder is a local file of this checkout: git ignores it, it is
loaded and validated the same way, and the catalogue says that it is not shipped.

Data format (JSON, one file per component in this folder, `<id>.json`; schema 1):

    {
      "schema": 1, "id": "<file name>", "name": "...", "kind": "phone", "units": "mm",
      "frame": "where the origin is and which way the axes point",
      "sources": {"<key>": {"title", "url" (https), "kind", "retrieved" (date), "note"}},
      "defaults": {"source", "provenance", "confidence", "tolerance"?},
      "envelope": {"width", "height", "depth": numbers; "corner_profile"?: points; ...},
      "features": [{
          "id", "kind", "label", "face" (+x -x +y -y +z -z: the face it is on),
          "defaults"?: {...}, "body"?: shape, "keep_out"?: [shape, ...],
          "data"?: {"<name>": number}, "question"?: "...", "note"?: "..."}],
      "rules": {"<name>": number}
    }

A number is either bare (`14.0`: it takes the feature's defaults, then the component's) or
an object `{"value", "tolerance", "unit", "source", "provenance", "confidence", "note"}`
whose missing keys are filled the same way. After filling, every number must have a
tolerance, a source that is listed in `sources`, a provenance and a confidence; a source of
kind `estimate` cannot back a `standard` or `measured` number.

Shapes (centre and extents in the component's frame; a shape sits on its feature's face and
runs outwards from it):
- `box`: `center` [x, y, z], `size` [x, y, z], optional `radius` (corners rounded in the
  plane of the face). The size along the face's normal is how far it stands out: 0 for a
  patch on the surface (an opening, no solid), `null` for "as far as it goes" (a keep-out).
  Optional `flare` (degrees from the face's normal, under 90): outside the surface the
  outline widens all round at that angle, as a maker asks of an opening over a control.
- `cylinder`: `center`, `diameter`, `length` along the normal (0 and `null` as for a box).
- `bar`: `center`, `size` and `end_profile`, points (distance from the bar's end, distance
  from its centre line) that give the shape of both ends.
- `cone`: `center` (the apex, or the centre of the starting circle when `diameter` is
  given), `angle` (full opening angle in degrees), `length` (`null`: as far as it goes).

`envelope.corner_profile` gives one corner of the outline as points (distance from the side
edge, distance from the end edge). Between the stated points the outline is drawn as the
smooth curve through them (their distance from the corner's centre as a polynomial of the
angle), and where the corner meets the straight edges it is closed with the edge itself, so
the outline passes through the stated points (to 0.002 mm) and is not inside the maker's.

`envelope.edge_profile` gives the section of the body's edge, the same on every side, as
points (distance inwards from the outline, height from the mid-plane), from the flat band
of the edge to where the flat face starts. The body is then not a plain prism: its edges
are rounded with the convex curve through those points (`Component.edge_section`), on the
back and on the front alike, and a pocket's `room(fitted=True)` follows them.

The validation test (tests/fit/test_fit_components.py) loads every file in this folder.
"""

from __future__ import annotations

import itertools
import json
import math
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCHEMA = 1
DATA_DIR = Path(__file__).resolve().parent
# The components that ship with Hotend; any other file in DATA_DIR is local to the checkout.
SHIPPED = ("sample_phone",)
NAME_RE = re.compile(r"[a-z][a-z0-9_]{0,31}\Z")
PROVENANCES = ("measured", "standard", "calibrated", "guessed", "style")
PROVEN_PROVENANCES = ("measured", "standard", "calibrated")
CONFIDENCES = ("verified", "likely", "unverified")
UNITS = ("mm", "deg", "g")
FACES = {"+x": (0, 1), "-x": (0, -1), "+y": (1, 1), "-y": (1, -1), "+z": (2, 1), "-z": (2, -1)}
DATUM_KEYS = {"value", "tolerance", "unit", "source", "provenance", "confidence", "note"}
DEFAULT_KEYS = {"source", "provenance", "confidence", "tolerance", "unit"}
SOURCE_KEYS = {"title", "url", "kind", "retrieved", "note"}
FEATURE_KEYS = {"id", "kind", "label", "face", "defaults", "body", "keep_out", "data",
                "question", "note"}
# shape -> (required keys, optional keys) besides "shape"
SHAPES = {
    "box": ({"center", "size"}, {"radius", "flare"}),
    "cylinder": ({"center", "diameter", "length"}, set()),
    "bar": ({"center", "size", "end_profile"}, set()),
    "cone": ({"center", "angle", "length"}, {"diameter"}),
}
MAX_FILE_BYTES = 1_000_000
MAX_FEATURES = 64
MAX_PROFILE_POINTS = 9
# A corner of an outline is drawn with this many points; a circle with this many sides.
CORNER_SAMPLES = 25
CIRCLE_SIDES = 64
# The rounded edge between two stated points of an edge profile is drawn with this many
# straight pieces (the curve bulges less than 0.01 mm beyond them on a phone).
EDGE_PIECES = 3
# How far the drawn body outline may lie outside the maker's: where a corner meets the
# straight edge it is closed with the edge itself (0.03 mm on the sample phone).
OUTLINE_TOLERANCE_MM = 0.05
# A feature solid is tested this much smaller than drawn, so that a wall which touches it
# exactly is contact, not a collision.
PROOF_TOLERANCE_MM = 0.005


class ComponentError(ValueError):
    """A component's data is wrong, or a model asked a component for something it does not
    have. The message says what."""


@dataclass(frozen=True)
class Datum:
    """One number of a component with where it comes from. `value` is a number or, for a
    profile, a list of points. `proven` is True only for a verified measured, standard or
    calibrated number."""

    path: str
    value: Any
    unit: str
    tolerance: float
    source: str
    url: str
    provenance: str
    confidence: str
    note: str = ""

    @property
    def proven(self) -> bool:
        return self.provenance in PROVEN_PROVENANCES and self.confidence == "verified"

    def to_json(self) -> dict:
        return {"path": self.path, "value": self.value, "unit": self.unit,
                "tolerance": self.tolerance, "provenance": self.provenance,
                "confidence": self.confidence, "source": self.url, "note": self.note}


def _is_number(value: Any) -> bool:
    return (isinstance(value, (int, float)) and not isinstance(value, bool)
            and math.isfinite(value))


def _datum(raw: Any, defaults: dict, sources: dict, path: str, *, points: bool = False,
           pair: bool = False) -> Datum:
    spec = dict(raw) if isinstance(raw, dict) else {"value": raw}
    if not set(spec) <= DATUM_KEYS or "value" not in spec:
        raise ComponentError(f"{path}: a number is a value or an object with the keys "
                             f"{', '.join(sorted(DATUM_KEYS))}")
    merged = {"unit": "mm", "note": "", **defaults, **spec}
    value = merged["value"]
    if points:
        good = (isinstance(value, list) and 2 <= len(value) <= MAX_PROFILE_POINTS
                and all(isinstance(p, list) and len(p) == 2 and all(_is_number(c) for c in p)
                        for p in value))
        value = [(float(p[0]), float(p[1])) for p in value] if good else None
    elif pair and isinstance(value, list):
        good = len(value) == 2 and all(_is_number(c) for c in value)
        value = [float(c) for c in value] if good else None
    else:
        good = _is_number(value)
        value = float(value) if good else None
    if not good:
        raise ComponentError(f"{path}: the value is not a number this field takes")
    for key in ("tolerance", "source", "provenance", "confidence"):
        if key not in merged:
            raise ComponentError(f"{path}: no {key} is given, here or in the defaults")
    if not _is_number(merged["tolerance"]) or merged["tolerance"] < 0:
        raise ComponentError(f"{path}: the tolerance must be a number, zero or more")
    if merged["source"] not in sources:
        raise ComponentError(f"{path}: the source {merged['source']!r} is not listed in sources")
    if merged["provenance"] not in PROVENANCES:
        raise ComponentError(f"{path}: provenance must be one of {', '.join(PROVENANCES)}")
    if merged["confidence"] not in CONFIDENCES:
        raise ComponentError(f"{path}: confidence must be one of {', '.join(CONFIDENCES)}")
    if merged["unit"] not in UNITS:
        raise ComponentError(f"{path}: unit must be one of {', '.join(UNITS)}")
    if not isinstance(merged["note"], str):
        raise ComponentError(f"{path}: the note must be text")
    source = sources[merged["source"]]
    if source["kind"] == "estimate" and merged["provenance"] in ("measured", "standard"):
        raise ComponentError(
            f"{path}: an estimate cannot back a {merged['provenance']} number; tag it guessed")
    return Datum(path, value, merged["unit"], float(merged["tolerance"]), merged["source"],
                 source["url"], merged["provenance"], merged["confidence"], merged["note"])


@dataclass
class Shape:
    """One shape of a feature: its kind and its numbers (None where the data says `null`)."""

    kind: str
    center: tuple[Datum, Datum, Datum]
    size: tuple[Datum | None, ...] | None = None
    diameter: Datum | None = None
    length: Datum | None = None
    radius: Datum | None = None
    angle: Datum | None = None
    end_profile: Datum | None = None
    flare: Datum | None = None

    def numbers(self) -> list[Datum]:
        found = list(self.center) + [d for d in (self.size or ()) if d is not None]
        found += [d for d in (self.diameter, self.length, self.radius, self.angle,
                              self.end_profile, self.flare) if d is not None]
        return found


def _shape(raw: Any, face: str, defaults: dict, sources: dict, path: str) -> Shape:
    if not isinstance(raw, dict) or raw.get("shape") not in SHAPES:
        raise ComponentError(f"{path}: shape must be one of {', '.join(SHAPES)}")
    kind = raw["shape"]
    required, optional = SHAPES[kind]
    keys = set(raw) - {"shape"}
    if not required <= keys <= required | optional:
        raise ComponentError(f"{path}: a {kind} has the keys {', '.join(sorted(required))}"
                             + (f" and optionally {', '.join(sorted(optional))}" if optional else ""))
    normal = FACES[face][0]

    def triple(name: str, nullable: bool) -> tuple:
        values = raw[name]
        if not isinstance(values, list) or len(values) != 3:
            raise ComponentError(f"{path}.{name}: three numbers are needed")
        made = []
        for index, value in enumerate(values):
            if value is None:
                if not nullable or index != normal:
                    raise ComponentError(
                        f"{path}.{name}: only the size along the face's normal may be null")
                made.append(None)
            else:
                made.append(_datum(value, defaults, sources, f"{path}.{name}.{'xyz'[index]}"))
        return tuple(made)

    def scalar(name: str, nullable: bool = False) -> Datum | None:
        if name not in raw or (raw[name] is None and nullable):
            return None
        return _datum(raw[name], defaults, sources, f"{path}.{name}")

    shape = Shape(kind, triple("center", False))
    if kind in ("box", "bar"):
        shape.size = triple("size", kind == "box")
        lateral = [d for index, d in enumerate(shape.size) if index != normal]
        if any(d is None or d.value <= 0 for d in lateral) or (
                shape.size[normal] is not None and shape.size[normal].value < 0):
            raise ComponentError(f"{path}.size: the sizes across the face must be positive")
        shape.radius = scalar("radius")
        shape.flare = scalar("flare")
        if shape.flare is not None and (shape.flare.unit != "deg"
                                        or not 0 < shape.flare.value < 90):
            raise ComponentError(f"{path}.flare: an angle from the normal in degrees, under 90")
    if kind == "bar":
        shape.end_profile = _datum(raw["end_profile"], defaults, sources,
                                   f"{path}.end_profile", points=True)
    if kind in ("cylinder", "cone"):
        shape.diameter = scalar("diameter")
        shape.length = scalar("length", nullable=True)
        if (shape.diameter is not None and shape.diameter.value <= 0) or (
                shape.length is not None and shape.length.value < 0):
            raise ComponentError(f"{path}: diameter and length must be positive")
    if kind == "cone":
        shape.angle = scalar("angle")
        if shape.angle.unit != "deg" or not 0 < shape.angle.value < 180:
            raise ComponentError(f"{path}.angle: an opening angle in degrees is needed")
    return shape


# --------------------------------------------------------------------------------------
# geometry
# --------------------------------------------------------------------------------------


def _polar_curve(points: list[tuple[float, float]], centre: tuple[float, float],
                 samples: int) -> list[tuple[float, float]]:
    """The smooth curve through `points`, drawn about `centre`: the distance from the centre
    as the polynomial of the angle that passes through every point."""
    import numpy as np

    relative = np.array(points, dtype=np.float64) - np.array(centre)
    angles = np.unwrap(np.arctan2(relative[:, 1], relative[:, 0]))
    radii = np.hypot(relative[:, 0], relative[:, 1])
    if np.any(np.abs(np.diff(angles)) < 1e-9):
        raise ComponentError("the points of a profile do not go round its centre")
    middle = angles.mean()
    polynomial = np.polyfit(angles - middle, radii, len(points) - 1)
    drawn = np.linspace(angles[0], angles[-1], samples)
    radius = np.polyval(polynomial, drawn - middle)
    return [(float(centre[0] + r * math.cos(a)), float(centre[1] + r * math.sin(a)))
            for r, a in zip(radius, drawn, strict=True)]


def _convex_curve(points: list[tuple[float, float]], pieces: int) -> list[tuple[float, float]]:
    """A smooth convex curve through `points` (a quarter of a rounded edge: it starts
    along the second axis and ends along the first), as a polyline. Between two points it
    is a parabola arc whose end directions halve the turn of the neighbouring chords, so
    it passes through every point, never turns back and never leaves the corner the
    chords' tangents span. Where the points do not turn one way, the chord is kept."""
    import numpy as np

    stated = np.array(points, dtype=np.float64)
    chords = np.diff(stated, axis=0)
    angles = np.arctan2(chords[:, 1], chords[:, 0])
    directions = [math.pi / 2]
    directions += [(a + b) / 2 for a, b in itertools.pairwise(angles)]
    directions.append(0.0)
    drawn = [tuple(stated[0])]
    for index in range(len(stated) - 1):
        start, end = stated[index], stated[index + 1]
        a, b = directions[index], directions[index + 1]
        ta, tb = np.array([math.cos(a), math.sin(a)]), np.array([math.cos(b), math.sin(b)])
        cross = ta[0] * tb[1] - ta[1] * tb[0]
        control = None
        if abs(cross) > 1e-9:
            step = end - start
            along_a = (step[0] * tb[1] - step[1] * tb[0]) / cross
            along_b = (ta[0] * step[1] - ta[1] * step[0]) / cross
            if along_a > 1e-9 and along_b > 1e-9:
                control = start + along_a * ta
        for piece in range(1, pieces + 1):
            t = piece / pieces
            if control is None:
                point = start + t * (end - start)
            else:
                point = (1 - t) ** 2 * start + 2 * t * (1 - t) * control + t * t * end
            drawn.append((float(point[0]), float(point[1])))
    return drawn


def _hull_polygon(points: list[tuple[float, float]]) -> list[tuple[float, float]]:
    import manifold3d
    import numpy as np

    hull = manifold3d.CrossSection.hull_points(np.array(points, dtype=np.float64, order="C"))
    polygons = hull.to_polygons()
    if len(polygons) != 1:
        raise ComponentError("an outline of the component is empty")
    return [(float(x), float(y)) for x, y in polygons[0]]


def _rounded_rectangle(width: float, height: float, radius: float) -> list[tuple[float, float]]:
    radius = min(radius, width / 2, height / 2)
    if radius <= 0:
        return [(-width / 2, -height / 2), (width / 2, -height / 2),
                (width / 2, height / 2), (-width / 2, height / 2)]
    # Corner arcs are drawn around the true arc, so the polygon contains the shape.
    steps = CIRCLE_SIDES // 4
    outer = radius / math.cos(math.pi / CIRCLE_SIDES)
    points = []
    for corner, (sx, sy) in enumerate(((1, 1), (-1, 1), (-1, -1), (1, -1))):
        cx, cy = sx * (width / 2 - radius), sy * (height / 2 - radius)
        for step in range(steps + 1):
            angle = (corner + step / steps) * math.pi / 2
            points.append((cx + outer * math.cos(angle), cy + outer * math.sin(angle)))
    return _hull_polygon([(max(-width / 2, min(width / 2, x)), max(-height / 2, min(height / 2, y)))
                          for x, y in points])


def _circle(radius: float) -> list[tuple[float, float]]:
    outer = radius / math.cos(math.pi / CIRCLE_SIDES)
    return [(outer * math.cos(2 * math.pi * i / CIRCLE_SIDES),
             outer * math.sin(2 * math.pi * i / CIRCLE_SIDES)) for i in range(CIRCLE_SIDES)]


def _bar(length: float, height: float, profile: list[tuple[float, float]]) -> list[tuple[float, float]]:
    """The outline of a bar whose two ends follow `profile`: points (distance from the end,
    distance from the centre line), from the tip to where the straight sides start."""
    cap = profile[-1][0]
    quarter = _polar_curve([(d - cap, h) for d, h in profile], (0.0, 0.0), CORNER_SAMPLES)
    half = length / 2 - cap
    if half < 0 or abs(profile[-1][1] - height / 2) > 0.05:
        raise ComponentError("a bar's end profile does not match its size")
    points = [(sx * (half - x), sy * min(max(y, 0.0), height / 2))
              for x, y in quarter for sx in (1, -1) for sy in (1, -1)]
    return _hull_polygon(points)


def _face_location(face: str, center: tuple[float, float, float]) -> Any:
    """Local coordinates of a face (x and y across it, z out of it) into the component."""
    import numpy as np

    from ..reference import _location

    axis, sign = FACES[face]
    rotation = np.zeros((3, 3))
    rotation[(axis + 1) % 3, 0] = 1.0
    rotation[(axis + 2) % 3, 1] = sign
    rotation[axis, 2] = sign
    return _location(rotation, center)


def _grow(polygon: list[tuple[float, float]], delta: float) -> list:
    import manifold3d
    import numpy as np

    from ..reference import _grown

    section = manifold3d.CrossSection([np.array(polygon, dtype=np.float64)],
                                      manifold3d.FillRule.Positive)
    grown = _grown(section, delta)
    if grown.is_empty():
        raise ComponentError("the clearance is larger than the feature")
    return [np.array(p) for p in grown.to_polygons() if len(p) >= 3]


def _flared(polygons: list, inward: float, length: float, slope: float) -> Any:
    """A keep-out that widens outside the surface: the outline as an upright prism from
    `inward` inside the face to the face, and from the face outwards over `length` the
    outline grown all round by `slope` times the distance from the face."""
    import numpy as np

    from ..reference import _prism, inset_polygon, solid_from_mesh, stacked_rings

    if len(polygons) != 1:
        raise ComponentError("a flared keep-out needs one outline")
    base = np.array(polygons[0], dtype=np.float64)
    wide = inset_polygon(base, -length * slope)
    if wide is None:
        raise ComponentError("a flared keep-out could not be drawn from its outline")
    solid = solid_from_mesh(*stacked_rings([base, wide], [0.0, length]))
    if inward > 1e-9:
        solid = _prism([base], -inward, 0.0) + solid
    return solid


class Feature:
    """A feature of a component: a button, a port, the camera, the magnets.

    `id`, `kind`, `label`, `face` (which face of the component it is on), `note`, `center`
    and `size` (mm, in the component's frame), `proven` (False when any of its numbers is
    not verified; `question` then says what the user should check), `data` (further numbers
    by name, read with `value(name)`).

    Solids, in the component's frame (multiply by the Location the pocket was cut `at`):
    - `solid`: the feature itself where it stands proud of the body (None for a patch such
      as a port opening). A part must not collide with it: `reference_fit` checks.
    - `keep_out(reach=10, inward=1)`: the room the maker asks to keep free around it (when
      the maker gives none: the feature's own outline), from `inward` mm inside the surface
      to `reach` mm outside it where the maker gives no length. Subtract it from the part.
    - `opening(ctx, fit="loose", reach=10, inward=1)`: the feature's outline grown by the
      clearance `ctx.fit` resolves for that fit class, as a cut through a wall, to `reach`
      mm beyond the feature. With a small `reach` it is a relief that stays covered (a
      button under a flexible skin).
    """

    def __init__(self, component: Component, raw: dict, defaults: dict, sources: dict,
                 path: str) -> None:
        if not isinstance(raw, dict) or not {"id", "kind", "label", "face"} <= set(raw) <= FEATURE_KEYS:
            raise ComponentError(f"{path}: a feature has the keys id, kind, label, face and "
                                 "optionally defaults, body, keep_out, data, question, note")
        if not isinstance(raw["id"], str) or not NAME_RE.match(raw["id"]):
            raise ComponentError(f"{path}: the id {raw['id']!r} is not a name")
        path = f"features.{raw['id']}"
        if raw["face"] not in FACES:
            raise ComponentError(f"{path}: face must be one of {', '.join(FACES)}")
        for key in ("kind", "label", "question", "note"):
            if not isinstance(raw.get(key, ""), str):
                raise ComponentError(f"{path}.{key} must be text")
        own = raw.get("defaults", {})
        if not isinstance(own, dict) or not set(own) <= DEFAULT_KEYS:
            raise ComponentError(f"{path}.defaults has keys other than {', '.join(sorted(DEFAULT_KEYS))}")
        defaults = {**defaults, **own}
        self.component = component
        self.id, self.kind, self.label, self.face = raw["id"], raw["kind"], raw["label"], raw["face"]
        self.note, self.question = raw.get("note", ""), raw.get("question", "")
        self.body = (_shape(raw["body"], self.face, defaults, sources, f"{path}.body")
                     if "body" in raw else None)
        keep_out = raw.get("keep_out", [])
        if not isinstance(keep_out, list) or len(keep_out) > 8:
            raise ComponentError(f"{path}.keep_out must be a short list of shapes")
        self.keep_outs = [_shape(item, self.face, defaults, sources, f"{path}.keep_out.{index}")
                          for index, item in enumerate(keep_out)]
        data = raw.get("data", {})
        if not isinstance(data, dict) or not all(isinstance(k, str) and NAME_RE.match(k) for k in data):
            raise ComponentError(f"{path}.data must map names to numbers")
        self.data = {name: _datum(value, defaults, sources, f"{path}.data.{name}", pair=True)
                     for name, value in data.items()}
        if self.body is None and not self.keep_outs:
            raise ComponentError(f"{path}: a feature needs a body or a keep_out")
        if self.unverified() and not self.question.strip():
            raise ComponentError(
                f"{path}: it has numbers that are not verified, so it needs a question that "
                "tells the user what to check")

    # ---- numbers ---------------------------------------------------------------------

    def numbers(self) -> list[Datum]:
        found = self.body.numbers() if self.body else []
        for shape in self.keep_outs:
            found += shape.numbers()
        return found + list(self.data.values())

    def unverified(self) -> list[Datum]:
        seen, found = set(), []
        for datum in self.numbers():
            if not datum.proven and datum.path not in seen:
                seen.add(datum.path)
                found.append(datum)
        return found

    @property
    def proven(self) -> bool:
        return not self.unverified()

    def value(self, name: str) -> Any:
        """A number of `data` by name (`magnet_ring.value("ring_outer_diameter")`)."""
        if name not in self.data:
            raise ComponentError(f"the feature '{self.id}' has no number {name!r}; it has "
                                 f"{', '.join(self.data) or 'none'}")
        return self.data[name].value

    @property
    def _main(self) -> Shape:
        return self.body or self.keep_outs[0]

    @property
    def center(self) -> tuple[float, float, float]:
        return tuple(d.value for d in self._main.center)

    @property
    def size(self) -> tuple[float, float, float]:
        """Extents along x, y and z of the feature itself (0 along the normal for a patch)."""
        shape, axis = self._main, FACES[self.face][0]
        if shape.size is not None:
            return tuple(0.0 if d is None else d.value for d in shape.size)
        across = 0.0 if shape.diameter is None else shape.diameter.value
        along = 0.0 if shape.length is None else shape.length.value
        return tuple(along if index == axis else across for index in range(3))

    # ---- solids ----------------------------------------------------------------------

    def _outline(self, shape: Shape) -> list[tuple[float, float]]:
        axis = FACES[self.face][0]
        if shape.kind == "cylinder":
            return _circle(shape.diameter.value / 2)
        width, height = shape.size[(axis + 1) % 3].value, shape.size[(axis + 2) % 3].value
        if shape.kind == "bar":
            # A bar lies along its longer side.
            if width >= height:
                return _bar(width, height, shape.end_profile.value)
            return [(y, x) for x, y in reversed(_bar(height, width, shape.end_profile.value))]
        return _rounded_rectangle(width, height, shape.radius.value if shape.radius else 0.0)

    def _stands_out(self, shape: Shape) -> float | None:
        """How far a shape runs out of its face: a length, or None for "as far as it goes"."""
        if shape.size is not None:
            datum = shape.size[FACES[self.face][0]]
        else:
            datum = shape.length
        return None if datum is None else datum.value

    def _build(self, shape: Shape, reach: float, inward: float, grow: float = 0.0) -> Any:
        from ..reference import _prism

        if not (_is_number(reach) and _is_number(inward) and 0 < reach <= 1000 and 0 <= inward <= 100):
            raise ComponentError("reach must be between 0 and 1000 mm and inward between 0 and 100 mm")
        stated = self._stands_out(shape)
        length = reach if stated is None or stated == 0 else stated
        place = _face_location(self.face, tuple(d.value for d in shape.center))
        if shape.kind == "cone":
            from build123d import Align, Cone, Pos

            start = (shape.diameter.value / 2 if shape.diameter else 0.0) + grow
            slope = math.tan(math.radians(shape.angle.value) / 2)
            low = -min(inward, start / slope) if slope > 0 else 0.0
            bottom = max(start + low * slope, 1e-6)
            return place * Pos(0, 0, low) * Cone(
                bottom, start + length * slope, length - low,
                align=(Align.CENTER, Align.CENTER, Align.MIN))
        outline = self._outline(shape)
        polygons = _grow(outline, grow) if abs(grow) > 1e-12 else [outline]
        if shape.flare is not None:
            return place * _flared(polygons, inward, length,
                                   math.tan(math.radians(shape.flare.value)))
        return place * _prism(polygons, -inward, length)

    @property
    def solid(self) -> Any:
        """The feature itself where it stands proud of the body, or None for a patch."""
        if self.body is None or not self._stands_out(self.body):
            return None
        return self._build(self.body, 1.0, 0.0)

    def keep_out(self, reach: float = 10.0, inward: float = 1.0) -> Any:
        """The room to keep free around this feature, as one build123d shape in the
        component's frame: the maker's keep-out volumes, or the feature's own outline when
        the maker states none. `reach` is used where the maker gives no length (for the
        feature's own outline: how far beyond the feature)."""
        self.component.used_features.add(self.id)
        return self._keep_out(reach, inward)

    def _keep_out(self, reach: float, inward: float) -> Any:
        if self.keep_outs:
            solids = [self._build(shape, reach, inward) for shape in self.keep_outs]
        else:
            proud = self._stands_out(self.body) or 0.0
            solids = [self._through(self.body, reach + proud, inward, 0.0)]
        fused = solids[0]
        for other in solids[1:]:
            fused = fused + other
        return fused

    def _through(self, shape: Shape, reach: float, inward: float, grow: float) -> Any:
        """The outline of a shape as a cut from `inward` inside the face to `reach` outside
        it, whatever length the shape itself has."""
        from ..reference import _prism

        if shape.kind == "cone":
            return self._build(shape, reach, inward, grow)
        if not (_is_number(reach) and _is_number(inward) and 0 < reach <= 1000 and 0 <= inward <= 100):
            raise ComponentError("reach must be between 0 and 1000 mm and inward between 0 and 100 mm")
        outline = self._outline(shape)
        polygons = _grow(outline, grow) if abs(grow) > 1e-12 else [outline]
        place = _face_location(self.face, tuple(d.value for d in shape.center))
        return place * _prism(polygons, -inward, reach)

    def opening(self, ctx: Any, fit: str = "loose", *, reach: float = 10.0,
                inward: float = 1.0) -> Any:
        """The feature's outline grown by the clearance `ctx.fit` resolves for `fit` (a
        clearance class: `loose`, `slide`, `snug` or `cradle`), from `inward` mm inside the
        surface to `reach` mm beyond the feature, as a build123d Solid in the component's
        frame."""
        from ..fit import FitError, resolver_of

        if fit not in ("loose", "slide", "snug", "cradle"):
            raise ComponentError("an opening's fit must be one of loose, slide, snug, cradle")
        shape = self.body or self.keep_outs[0]
        axis = FACES[self.face][0]
        across = [value for index, value in enumerate(self.size) if index != axis]
        try:
            clearance = resolver_of(ctx).clearance(fit, max(max(across), 0.1), printed_peg=False)
        except FitError as exc:
            raise ComponentError(str(exc)) from exc
        self.component.clearances.append(clearance)
        self.component.used_features.add(self.id)
        proud = (self._stands_out(shape) or 0.0) if shape is self.body else 0.0
        return self._through(shape, reach + proud, inward, clearance.gap)

    def report(self) -> dict:
        return {"id": self.id, "kind": self.kind, "label": self.label, "face": self.face,
                "center": list(self.center), "size": list(self.size), "proven": self.proven,
                "question": self.question if not self.proven else "", "note": self.note}


class Component:
    """A bought device: `id`, `name`, `kind`, `frame` (text: where the origin is), `size`
    ((width, height, depth) in mm), `envelope` and `rules` (numbers by name, as `Datum`),
    `features` (by id, in the data's order), `sources` (by key: title and URL).

    `reference` is the body as a `hotend_fit.reference.Reference` (make a pocket for it);
    `feature(id)` gives a feature and records that the model relies on it; `rule(name)` is a
    design rule of the maker as a number; `questions()` are what the user must be asked
    about the numbers the model relied on that are not verified.
    """

    def __init__(self, raw: Any, expected_id: str | None = None) -> None:
        keys = {"schema", "id", "name", "kind", "units", "frame", "sources", "defaults",
                "envelope", "features", "rules"}
        if not isinstance(raw, dict) or set(raw) != keys:
            raise ComponentError("a component has exactly the keys " + ", ".join(sorted(keys)))
        if raw["schema"] != SCHEMA:
            raise ComponentError(f"this reader knows schema {SCHEMA}, the file says {raw['schema']!r}")
        if not isinstance(raw["id"], str) or not NAME_RE.match(raw["id"]) or (
                expected_id is not None and raw["id"] != expected_id):
            raise ComponentError("the id must be the file's name")
        if raw["units"] != "mm":
            raise ComponentError("component data is in millimetres")
        for key in ("name", "kind", "frame"):
            if not isinstance(raw[key], str) or not raw[key].strip():
                raise ComponentError(f"{key} must be text")
        self.id, self.name, self.kind, self.frame = raw["id"], raw["name"], raw["kind"], raw["frame"]
        sources = raw["sources"]
        if not isinstance(sources, dict) or not sources:
            raise ComponentError("a component needs at least one source")
        for key, source in sources.items():
            if (not isinstance(source, dict) or set(source) != SOURCE_KEYS
                    or not all(isinstance(v, str) for v in source.values())
                    or not source["url"].startswith("https://") or not source["title"].strip()
                    or not re.fullmatch(r"\d{4}-\d{2}-\d{2}", source["retrieved"])):
                raise ComponentError(
                    f"sources.{key}: a source has a title, an https url, a kind, the date it "
                    "was retrieved and a note")
        self.sources = {key: dict(source) for key, source in sources.items()}
        defaults = raw["defaults"]
        if not isinstance(defaults, dict) or not set(defaults) <= DEFAULT_KEYS:
            raise ComponentError(f"defaults has keys other than {', '.join(sorted(DEFAULT_KEYS))}")
        envelope = raw["envelope"]
        if not isinstance(envelope, dict) or not {"width", "height", "depth"} <= set(envelope):
            raise ComponentError("the envelope needs width, height and depth")
        self.envelope = {
            name: _datum(value, defaults, sources, f"envelope.{name}",
                         points=name.endswith("_profile"), pair=True)
            for name, value in envelope.items()}
        for name in ("width", "height", "depth"):
            if not isinstance(self.envelope[name].value, float) or self.envelope[name].value <= 0:
                raise ComponentError(f"envelope.{name} must be a positive number")
        features = raw["features"]
        if not isinstance(features, list) or len(features) > MAX_FEATURES:
            raise ComponentError(f"a component has at most {MAX_FEATURES} features")
        self.used_features: set[str] = set()
        self.clearances: list = []
        self.features: dict[str, Feature] = {}
        for index, item in enumerate(features):
            feature = Feature(self, item, defaults, sources, f"features.{index}")
            if feature.id in self.features:
                raise ComponentError(f"the feature id {feature.id!r} is used twice")
            self.features[feature.id] = feature
        rules = raw["rules"]
        if not isinstance(rules, dict) or not all(isinstance(k, str) and NAME_RE.match(k) for k in rules):
            raise ComponentError("rules must map names to numbers")
        self.rules = {name: _datum(value, defaults, sources, f"rules.{name}", pair=True)
                      for name, value in rules.items()}
        self._reference: Any = None
        self._outline: list | None = None
        self._edge: list | None = None

    # ---- numbers ---------------------------------------------------------------------

    @property
    def size(self) -> tuple[float, float, float]:
        return tuple(self.envelope[name].value for name in ("width", "height", "depth"))

    def feature(self, feature_id: str) -> Feature:
        """A feature by id. Asking for it records that the model relies on it: its
        unverified numbers become questions for the user."""
        if feature_id not in self.features:
            raise ComponentError(f"'{self.id}' has no feature {str(feature_id)[:40]!r}; it has "
                                 f"{', '.join(self.features)}")
        self.used_features.add(feature_id)
        return self.features[feature_id]

    def rule(self, name: str) -> Any:
        """A design rule of the maker as a number (mm unless its unit says otherwise)."""
        if name not in self.rules:
            raise ComponentError(f"'{self.id}' has no rule {str(name)[:40]!r}; it has "
                                 f"{', '.join(self.rules)}")
        return self.rules[name].value

    def numbers(self) -> list[Datum]:
        found = list(self.envelope.values()) + list(self.rules.values())
        for feature in self.features.values():
            found += feature.numbers()
        return found

    def _body_numbers(self) -> list[Datum]:
        return [self.envelope[name]
                for name in ("width", "height", "depth", "corner_profile", "edge_profile")
                if name in self.envelope]

    def unverified(self, used: Any = None) -> list[Datum]:
        """The numbers the model relied on that are not verified: those of the body and of
        the features in `used` (default: the ones asked for with `feature`)."""
        used = self.used_features if used is None else set(used)
        found = [datum for datum in self._body_numbers() if not datum.proven]
        for feature_id, feature in self.features.items():
            if feature_id in used:
                found += feature.unverified()
        return found

    def questions(self, used: Any = None) -> list[dict]:
        """What to ask the user: one question per used feature with unverified numbers, in
        the shape of a result's `questions` (requirement, text, guessed_params)."""
        used = self.used_features if used is None else set(used)
        asked, seen = [], set()
        unproven_body = [datum for datum in self._body_numbers() if not datum.proven]
        if unproven_body:
            asked.append({
                "requirement": self.id[:32],
                "text": f"The size of the {self.name} is not verified "
                        f"({', '.join(d.path for d in unproven_body)}). Measure it with a "
                        "caliper before printing.",
                "guessed_params": []})
        for feature_id, feature in self.features.items():
            if feature_id not in used or feature.proven or feature.question in seen:
                continue
            seen.add(feature.question)
            asked.append({"requirement": f"{self.id}_{feature_id}"[:32],
                          "text": f"{self.name}: {feature.question}", "guessed_params": []})
        return asked

    # ---- the body --------------------------------------------------------------------

    def outline(self) -> list[tuple[float, float]]:
        """The body's outline seen from the front as (x, y) points, counter-clockwise."""
        if self._outline is None:
            width, height, _ = self.size
            profile = self.envelope.get("corner_profile")
            if profile is None:
                points = [(width / 2, height / 2), (-width / 2, height / 2),
                          (-width / 2, -height / 2), (width / 2, -height / 2)]
            else:
                reach = max(max(p) for p in profile.value)
                if reach > min(width, height) / 2:
                    raise ComponentError("envelope.corner_profile is larger than the body")
                corner = _polar_curve(profile.value, (reach, reach), CORNER_SAMPLES)
                corner = [(min(max(u, 0.0), reach), min(max(v, 0.0), reach)) for u, v in corner]
                # Where the corner meets the straight edges it is closed with the edge.
                corner += [(0.0, reach), (reach, 0.0)]
                points = [(sx * (width / 2 - u), sy * (height / 2 - v))
                          for u, v in corner for sx in (1, -1) for sy in (1, -1)]
            self._outline = _hull_polygon(points)
        return list(self._outline)

    def edge_section(self) -> list[tuple[float, float]]:
        """The section of the body's edge from the back to the front, the same on every
        side: points (distance inwards from the outline, height above the back). The body
        is the outline moved in by that distance at every height, so a point with distance
        0 is on the upright band of the edge and the first and last points are where the
        flat back and front start. A body without `envelope.edge_profile` has square
        edges: [(0, 0), (0, depth)]."""
        if self._edge is None:
            depth = self.size[2]
            profile = self.envelope.get("edge_profile")
            if profile is None:
                self._edge = [(0.0, 0.0), (0.0, depth)]
            else:
                stated = [(float(d), float(h)) for d, h in profile.value]
                rising = all(b[0] >= a[0] and b[1] >= a[1] and b != a
                             for a, b in itertools.pairwise(stated))
                if (not rising or stated[0][0] != 0.0 or stated[0][1] < 0
                        or abs(stated[-1][1] - depth / 2) > 0.05
                        or stated[-1][0] >= min(self.size[:2]) / 4):
                    raise ComponentError(
                        "envelope.edge_profile must run from the edge's flat band (distance "
                        "0) inwards and upwards to the flat face at half the depth")
                quarter = [(d, min(h, depth / 2)) for d, h in _convex_curve(stated, EDGE_PIECES)]
                quarter[-1] = (quarter[-1][0], depth / 2)
                upper = [(d, depth / 2 + h) for d, h in quarter]
                lower = [(d, depth / 2 - h) for d, h in reversed(quarter)]
                if quarter[0][1] <= 1e-9:
                    upper = upper[1:]
                self._edge = [(float(d), float(z)) for d, z in lower + upper]
        return list(self._edge)

    @property
    def provenance(self) -> str:
        """Where the body's size comes from: the weakest provenance of its numbers."""
        tags = [datum.provenance if datum.confidence == "verified" else "guessed"
                for datum in self._body_numbers()]
        for weakest in ("guessed", "style", "calibrated", "standard", "measured"):
            if weakest in tags:
                return weakest
        return "guessed"

    @property
    def reference(self) -> Any:
        """The body as a Reference, standing on z = 0: the outline over the full depth,
        with the edges rounded as `edge_section` says where the maker states their
        section. Seen from the front or the back it is the outline either way."""
        if self._reference is None:
            import numpy as np

            from ..reference import AXES, Reference, inset_polygon, stacked_rings

            outline = np.array(self.outline(), dtype=np.float64)
            depth = self.size[2]
            section = self.edge_section()
            rings = [inset_polygon(outline, d) for d, _ in section]
            if any(ring is None for ring in rings):
                raise ComponentError("envelope.edge_profile is larger than the body's corners")
            vertices, triangles = stacked_rings(rings, [z for _, z in section])
            rounded = len(section) > 2
            body = self._body_numbers()
            self._reference = Reference(
                self.id, vertices, triangles, kind="rigid",
                provenance=self.provenance, label=self.name,
                source={"component": self.id,
                        "urls": sorted({datum.url for datum in body}),
                        "tolerance": {datum.path: datum.tolerance for datum in body
                                      if isinstance(datum.value, float)}},
                outlines={AXES["+z"]: [outline],
                          AXES["-z"]: [outline[::-1] * np.array([1.0, -1.0])]},
                outline_tolerance=OUTLINE_TOLERANCE_MM,
                profiles=({"+z": section, "-z": [(d, depth - z) for d, z in reversed(section)]}
                          if rounded else None))
        return self._reference

    def report(self, used: Any = None) -> dict:
        """The component as data for a result: what it is, which features the model relied
        on, which numbers are not verified and the questions that follow."""
        used = sorted(self.used_features if used is None else set(used))
        unverified = self.unverified(used)
        return {
            "id": self.id, "name": self.name, "kind": self.kind, "frame": self.frame,
            "size": list(self.size), "provenance": self.provenance,
            "sources": {key: {"title": s["title"], "url": s["url"], "kind": s["kind"]}
                        for key, s in self.sources.items()},
            "numbers": len(self.numbers()),
            "features": [feature.report() for feature in self.features.values()],
            "used_features": used,
            "proven": not unverified,
            "unverified": [datum.to_json() for datum in unverified],
            "questions": self.questions(used),
        }


def list_components() -> list[str]:
    """The ids of the components of this installation: those that ship with Hotend and
    the local files put beside them."""
    return sorted(path.stem for path in DATA_DIR.glob("*.json") if NAME_RE.match(path.stem))


def load_component(component_id: str) -> Component:
    """Load and validate a component by id (`sample_phone`). Raises ComponentError."""
    if not isinstance(component_id, str) or not NAME_RE.match(component_id):
        raise ComponentError(f"{str(component_id)[:40]!r} is not a component id")
    path = DATA_DIR / f"{component_id}.json"
    if not path.is_file():
        raise ComponentError(f"there is no component {component_id!r}; Hotend has "
                             f"{', '.join(list_components()) or 'none'}")
    if path.stat().st_size > MAX_FILE_BYTES:
        raise ComponentError(f"the data of {component_id!r} is too large")
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        # The reason reaches the agent: it names no path on this computer.
        reason = exc.strerror or type(exc).__name__ if isinstance(exc, OSError) else str(exc)
        raise ComponentError(f"the data of {component_id!r} cannot be read: {reason}") from exc
    return Component(raw, component_id)


def use_component(ctx: Any, component_id: str) -> Component:
    """The component `component_id` of a build (`ctx.component(id)` calls this): loaded
    once per build and registered on the fit resolver, so the build's declarations name it
    and its body is a reference pockets can be made for."""
    from ..fit import resolver_of

    resolver = resolver_of(ctx)
    if component_id in resolver.components:
        return resolver.components[component_id]
    component = load_component(component_id)
    if component_id in resolver.references:
        raise ComponentError(f"the request already has a reference named {component_id!r}")
    resolver.components[component_id] = component
    resolver.references[component_id] = component.reference
    return component


def catalogue_entry(component: Component) -> dict:
    """What the agent is told of one component before it uses it: id, name, size, sources
    and which of its numbers are not verified, by feature."""
    unverified: dict[str, int] = {}
    for feature in component.features.values():
        if count := len(feature.unverified()):
            unverified[feature.id] = count
    body = [datum.path for datum in component._body_numbers() if not datum.proven]
    return {
        "id": component.id, "name": component.name, "kind": component.kind,
        "local": component.id not in SHIPPED,
        "size": list(component.size), "body_provenance": component.provenance,
        "body_unverified": body,
        "numbers": len(component.numbers()),
        "unverified": len(body) + sum(unverified.values()),
        "unverified_by_feature": unverified,
        "sources": {key: {"title": s["title"], "url": s["url"], "kind": s["kind"]}
                    for key, s in component.sources.items()},
    }


__all__ = [
    "MAX_FEATURES",
    "SHIPPED",
    "Component",
    "ComponentError",
    "Datum",
    "Feature",
    "Shape",
    "catalogue_entry",
    "list_components",
    "load_component",
    "use_component",
]
