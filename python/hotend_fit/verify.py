"""Verifying declared fits on the built solids.

A feature card declares each mating pair as two sides in the card's own frame. A side is a
probe (a point on the mating face and the outward normal there) or, for a bought part such as
a screw, the surface itself as a reference face. After the build the faces are looked up on
the finished parts, wherever later modelling left them, and three things are checked:

1. Pairs (`measure_pairs`). The clearance between the two faces of a pair, measured with the
   kernel's exact B-rep distance: the two closest points of the faces and the component of
   the vector between them along the outward normal of the printed face. For faces that lie
   opposite each other that is their distance; it is negative when the faces interfere and
   it stays the plane offset when two parallel faces do not overlap (a snap bead and the wall
   it has to ride over). A face the card made must also be there in full: a hole on the edge
   of its host is half a hole, whatever the distance at one point says.
2. Joints (`check_joints`). A pair is two faces; a joint is two parts. For every card the
   two solids are brought together the way the card's frames say and must not collide beyond
   the interference the card intends; the host must leave the port's keep-out volume empty;
   and all cards between the same two parts must agree on where the parts sit relative to
   each other (a screw hole 8 mm from its boss is two joints that cannot both close).
3. Robustness (`check_robustness`). What the drawing becomes on the tightest and the
   loosest printer the calibration allows (`FitResolver.robustness`).

Guarantees:
- A pair passes only when both faces were found where the card put them, in full, and the
  measured clearance is within the pair's tolerance of the clearance the resolver handed out.
- A pair whose clearance did not come from this build's resolver fails, whatever it measures.
- A half that was never applied, or a face that later modelling removed, moved or covered,
  fails with a reason; it is never skipped or assumed. A face that moved by less than
  SEARCH_MM is still measured, so the failure carries the clearance as it is now.
- A joint passes only when its two parts were identified and none of the three joint rules
  is broken; a joint that could not be examined fails.
"""

from __future__ import annotations

import math
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from typing import Any

from .fit import Clearance, FitResolver

# A measured clearance may differ from the resolver's by this much (mm).
FIT_TOLERANCE_MM = 1e-3
# A probe point counts as on a face within this distance (mm).
SURFACE_TOLERANCE_MM = 1e-4
# The face's normal at the probe must agree with the declared one to this cosine.
NORMAL_COS = 0.999
# A mating face that is not where the card put it is still looked for this far away (mm), so
# the failure can say what the clearance is now.
SEARCH_MM = 1.0
# A mating face the card made may be smaller than designed by this fraction at most.
AREA_TOLERANCE = 1e-6
# Shared volume below this is contact, not a collision (mm3).
OVERLAP_TOLERANCE_MM3 = 1e-3
# Two cards agree on where their parts sit when their relative placements differ by less.
PLACEMENT_TOLERANCE_MM = 1e-6
PLACEMENT_TOLERANCE_RAD = 1e-9
# Parts closer than this are drawn touching, that is assembled (mm).
TOUCH_MM = 1e-6

# How a side's face is checked for completeness.
HOST, CARD, ROUND = "host", "card", "round"


@dataclass(frozen=True)
class Side:
    """One side of a mating pair, in the card's frame.

    `point` lies on the mating face and `normal` points out of the solid there. With
    `reference` set (a build123d Face) the side is a bought part: the face is the surface
    itself and nothing is looked up on the built solids.

    `made` says how much of the face must exist: `host` (the face belongs to the host part;
    its size is the model's business), `card` (the card made the face; it must be at least
    as large as on the card's own reference half) or `round` (the card made a round hole
    whose depth the host decides; it must be a complete cylinder).
    """

    point: tuple[float, float, float]
    normal: tuple[float, float, float]
    reference: Any = None
    made: str = HOST


@dataclass
class FitPair:
    """A declared mating pair: what a card built and the clearance it was built with."""

    card: str
    instance: str
    name: str
    clearance: Clearance
    a: Side
    b: Side
    frame_a: Any = None  # Location the card's frame had when half A was applied
    frame_b: Any = None
    tolerance: float = FIT_TOLERANCE_MM
    owner: Any = None  # the Card that declared the pair

    @property
    def expected(self) -> float:
        return self.clearance.gap

    @property
    def label(self) -> str:
        return f"{self.instance}.{self.name}"


# The quantities of a calibration entry in words (the window's Settings use the same).
_QUANTITY_WORDS = {
    "hole_offset": "hole sizes",
    "peg_offset": "peg sizes",
    "xy_shrink": "shrinkage across the plate",
    "z_shrink": "shrinkage in height",
    "min_clearance": "the smallest gap that still comes apart",
}


def _listed(names: Iterable[str]) -> str:
    words = [_QUANTITY_WORDS.get(name, name.replace("_", " ")) for name in names]
    return words[0] if len(words) == 1 else f"{', '.join(words[:-1])} and {words[-1]}"


def calibration_state(measured: Iterable[str]) -> str:
    """How far this printer is measured, in words for a sentence that goes on "This printer
    ..." or "The printer ...": never more than was measured, and never less."""
    measured = tuple(measured)
    if not measured:
        return "is not calibrated"
    general = [name for name in _QUANTITY_WORDS if name not in measured]
    if not general:
        return "is calibrated"
    return (f"is measured only in part ({_listed(measured)}; general values for "
            f"{_listed(general)})")


def _basis(clearance: Clearance) -> str:
    """Where a clearance's numbers come from, in words. Never says more than was measured,
    and never less: a printer measured in part is not called unmeasured."""
    if clearance.offsets_provenance != "calibrated":
        if not clearance.measured:
            return "the guessed default calibration asks for (this printer is not measured yet)"
        general = [name for name in _QUANTITY_WORDS if name not in clearance.measured]
        return (
            f"this printer's measured {_listed(clearance.measured)} and the general values "
            f"for {_listed(general)} ask for (measured in part)"
        )
    if clearance.target_provenance == "guessed":
        return (
            "this printer's measured offsets give for the class's guessed target (offsets "
            "calibrated, target guessed until the gauge is printed)"
        )
    return "this printer's calibration and the maker's datasheet ask for"


@dataclass
class FitMeasurement:
    """The outcome of measuring one pair. `measured` is None when it could not be measured."""

    pair: FitPair
    ok: bool
    reason: str
    measured: float | None = None
    distance: float | None = None
    parts: tuple[str | None, str | None] = (None, None)
    points: tuple = field(default_factory=tuple)

    @property
    def difference(self) -> float | None:
        return None if self.measured is None else self.measured - self.pair.expected

    @property
    def message(self) -> str:
        pair = self.pair
        what = f"{pair.card.replace('_', ' ')} '{pair.label}'"
        if self.ok:
            return (
                f"The {what} is drawn with the {pair.clearance.fit_class} clearance "
                f"{_basis(pair.clearance)}: {self.measured:.3f} mm."
            )
        if self.measured is None:
            return f"The {what} could not be verified: {self.reason}"
        if abs(self.difference) <= pair.tolerance:
            return f"The {what} measures {self.measured:.3f} mm as asked, but: {self.reason}"
        return (
            f"The {what} measures {self.measured:.3f} mm but the fit resolver asked for "
            f"{pair.expected:.3f} mm ({pair.clearance.fit_class}); a difference of "
            f"{abs(self.difference):.3f} mm. {self.reason}"
        )

    def to_json(self) -> dict:
        pair = self.pair
        return {
            "kind": "pair",
            "card": pair.card,
            "instance": pair.instance,
            "pair": pair.name,
            "fit_class": pair.clearance.fit_class,
            "nominal": pair.clearance.nominal,
            "expected": pair.expected,
            "measured": self.measured,
            "difference": self.difference,
            "distance": self.distance,
            "tolerance": pair.tolerance,
            "parts": list(self.parts),
            "points": [list(p) for p in self.points],
            "provenance": pair.clearance.provenance,
            "offsets_provenance": pair.clearance.offsets_provenance,
            "target_provenance": pair.clearance.target_provenance,
            "extreme": pair.clearance.extreme,
            "reason": self.reason,
        }


class _Faces:
    """The faces of every part with their bounding boxes, computed once per measurement."""

    def __init__(self, parts: Iterable[tuple[str, Any]]) -> None:
        self._parts = list(parts)
        self._cache: dict[str, list] = {}

    def of(self, name: str, shape: Any) -> list:
        if name not in self._cache:
            self._cache[name] = [(face, face.bounding_box()) for face in shape.faces()]
        return self._cache[name]

    def find(self, point: Any, normal: Any,
             only: str | None = None) -> tuple[str | None, Any, float]:
        """(part name, face, how far the face is from `point`).

        The face through `point` whose outward normal is `normal` gives a distance of 0.
        Failing that, the nearest face within SEARCH_MM that faces the same way is returned
        with its distance, so a moved face can still be measured (and reported as moved).
        (None, None, inf) when there is nothing nearby. With `only`, just that part is
        searched.
        """
        from build123d import Vertex

        vertex = Vertex(*tuple(point))
        nearest: tuple[float, str | None, Any] = (float("inf"), None, None)
        for name, shape in self._parts:
            if only is not None and name != only:
                continue
            for face, box in self.of(name, shape):
                if not _box_contains(box, tuple(point), SEARCH_MM):
                    continue
                distance, on_face, _ = face.distance_to_with_closest_points(vertex)
                distance = float(distance)
                if distance > SEARCH_MM or distance >= nearest[0]:
                    continue
                try:
                    agrees = face.normal_at(on_face).dot(normal) >= NORMAL_COS
                except Exception:  # noqa: BLE001 - OCCT raises unrelated types
                    agrees = False
                if not agrees:
                    continue
                if distance <= SURFACE_TOLERANCE_MM:
                    return name, face, 0.0
                nearest = (distance, name, face)
        return nearest[1], nearest[2], nearest[0]


def _box_contains(box: Any, point: Any, margin: float) -> bool:
    low, high = tuple(box.min), tuple(box.max)
    return all(lo - margin <= c <= hi + margin for lo, c, hi in zip(low, point, high, strict=True))


def _to_world(frame: Any, side: Side) -> tuple[Any, Any]:
    from build123d import Plane, Vector

    plane = Plane(frame)
    point = plane.from_local_coords(Vector(*side.point))
    tip = plane.from_local_coords(Vector(*side.point) + Vector(*side.normal))
    return point, (tip - point).normalized()


def reference_area(shape: Any, side: Side) -> float | None:
    """The area of the face of `shape` (in the card's frame) that `side` probes, or None."""
    from build123d import Vector

    _, face, miss = _Faces([("reference", shape)]).find(Vector(*side.point), Vector(*side.normal))
    return float(face.area) if face is not None and miss == 0.0 else None


def _is_complete_cylinder(face: Any) -> bool:
    """True for a cylindrical face that runs the whole way round with nothing cut out."""
    from build123d import GeomType
    from OCP.BRepAdaptor import BRepAdaptor_Surface
    from OCP.BRepTools import BRepTools

    if face.geom_type != GeomType.CYLINDER:
        return False
    u_low, u_high, v_low, v_high = BRepTools.UVBounds_s(face.wrapped)
    radius = BRepAdaptor_Surface(face.wrapped).Cylinder().Radius()
    whole = 2.0 * math.pi * radius * (v_high - v_low)
    return (u_high - u_low >= 2.0 * math.pi * (1.0 - AREA_TOLERANCE)
            and float(face.area) >= whole * (1.0 - AREA_TOLERANCE))


def _incomplete(pair: FitPair, key: str, side: Side, face: Any, name: str | None) -> str | None:
    """A sentence when the face found for a side is less than the card made, else None."""
    half = key.upper()
    if side.made == ROUND:
        if _is_complete_cylinder(face):
            return None
        return (
            f"the hole of half {half} on '{name}' is not a complete round hole: it runs off "
            "an edge of its host or was partly cut away"
        )
    if side.made != CARD or pair.owner is None:
        return None
    expected = pair.owner.reference_area(key, side)
    if expected is None:
        return None
    area = float(face.area)
    if area >= expected * (1.0 - AREA_TOLERANCE) - 1e-9:
        return None
    return (
        f"only {100.0 * area / expected:.0f} % of the mating face of half {half} exists on "
        f"'{name}' ({area:.2f} of {expected:.2f} mm2): the feature runs off an edge of its "
        "host, is blocked, or was partly cut away"
    )


def _half_hosts(pairs: list[FitPair], faces: _Faces) -> dict[tuple[int, str], str]:
    """The part that carries each half of each card: the part most of the half's probes land
    on. Where two parts have a face through the same point (they collide there), a single
    probe cannot tell them apart; the half's other probes can."""
    votes: dict[tuple[int, str], dict[str, int]] = {}
    for pair in pairs:
        if pair.owner is None:
            continue
        for key, side, frame in (("a", pair.a, pair.frame_a), ("b", pair.b, pair.frame_b)):
            if side.reference is not None or frame is None:
                continue
            try:
                name, _, _ = faces.find(*_to_world(frame, side))
            except Exception:  # noqa: BLE001, S112 - OCCT raises unrelated types
                continue
            if name is not None:
                tally = votes.setdefault((id(pair.owner), key), {})
                tally[name] = tally.get(name, 0) + 1
    hosts = {}
    for half, tally in votes.items():
        best = max(tally.values())
        leaders = [name for name, count in tally.items() if count == best]
        if len(leaders) == 1:
            hosts[half] = leaders[0]
    return hosts


def _measure(pair: FitPair, faces: _Faces, resolver: FitResolver | None,
             hosts: dict[tuple[int, str], str] | None = None) -> FitMeasurement:
    from build123d import Vector
    from OCP.BRepExtrema import BRepExtrema_DistShapeShape

    def failed(reason: str, **extra: Any) -> FitMeasurement:
        return FitMeasurement(pair, False, reason, **extra)

    if resolver is not None and not resolver.was_issued(pair.clearance):
        return failed(
            "its clearance did not come from this build's fit resolver; a clearance must "
            "never be typed in or carried over from another build."
        )
    built = [(key, side) for key, side in (("a", pair.a), ("b", pair.b)) if side.reference is None]
    if not built:
        return failed("neither side is a printed surface.")
    found: dict[str, Any] = {}
    names: dict[str, str | None] = {"a": None, "b": None}
    frames = {"a": pair.frame_a, "b": pair.frame_b}
    moved, partial = [], []
    for key, side in built:
        if frames[key] is None:
            return failed(
                f"half {key.upper()} of the card was never applied to a part, so only one "
                "side of the joint exists."
            )
        point, normal = _to_world(frames[key], side)
        host = (hosts or {}).get((id(pair.owner), key))
        name, face, miss = faces.find(point, normal, host)
        if face is None:
            where = "({:.2f}, {:.2f}, {:.2f})".format(*tuple(point))
            return failed(
                f"no part has the mating face of half {key.upper()} near {where}. The face "
                "the card made was removed or covered by later modelling; apply the card "
                "last, at the place where the part stays."
            )
        if miss > 0.0:
            moved.append(f"the mating face of half {key.upper()} on '{name}' is {miss:.3f} mm "
                         "from where the card put it")
        else:
            problem = _incomplete(pair, key, side, face, name)
            if problem:
                partial.append(problem)
        found[key], names[key] = face, name
    # Everything is measured in half A's frame (or half B's when A is a bought part).
    anchor = "a" if "a" in found else "b"
    for key, side in (("a", pair.a), ("b", pair.b)):
        if side.reference is not None:
            found[key] = frames[anchor] * side.reference
        elif key != anchor and frames["a"] is not None and frames["b"] is not None:
            found[key] = (frames["a"] * frames["b"].inverse()) * found[key]
    extrema = BRepExtrema_DistShapeShape(found["a"].wrapped, found["b"].wrapped)
    if not extrema.IsDone() or extrema.NbSolution() < 1:
        return failed("the kernel could not measure the distance between the two faces.")
    point_a = Vector(extrema.PointOnShape1(1))
    point_b = Vector(extrema.PointOnShape2(1))
    if anchor == "a":
        measured = (point_b - point_a).dot(found["a"].normal_at(point_a))
    else:
        measured = (point_a - point_b).dot(found["b"].normal_at(point_b))
    measured = float(measured)
    extra = {
        "measured": measured,
        "distance": float(extrema.Value()),
        "parts": (names["a"], names["b"]),
        "points": (tuple(point_a), tuple(point_b)),
    }
    if moved:
        return failed(
            f"{'; '.join(moved).capitalize()}. A part was reshaped or moved after the card "
            "was applied, or a clearance was typed by hand; build both halves from the card "
            "and leave them as the card made them.",
            **extra,
        )
    if abs(measured - pair.expected) > pair.tolerance:
        return failed(
            "The mating faces were changed after the card made them, or a clearance was typed "
            "by hand; rebuild both halves from the card.",
            **extra,
        )
    if partial:
        return failed(
            f"{'; '.join(partial).capitalize()}. Place the card where its host has material "
            "all around the feature.",
            **extra,
        )
    return FitMeasurement(pair, True, "", **extra)


def measure_pairs(
    parts: Mapping[str, Any] | Iterable[tuple[str, Any]],
    pairs: Iterable[FitPair],
    resolver: FitResolver | None = None,
) -> list[FitMeasurement]:
    """Measure every declared pair on the built parts (name -> build123d shape, in world
    coordinates). With `resolver`, a pair whose clearance it did not issue fails."""
    items = parts.items() if isinstance(parts, Mapping) else parts
    faces = _Faces(items)
    pairs = list(pairs)
    hosts = _half_hosts(pairs, faces)
    results = []
    for pair in pairs:
        try:
            results.append(_measure(pair, faces, resolver, hosts))
        except Exception as exc:  # noqa: BLE001 - OCCT raises unrelated types
            results.append(FitMeasurement(
                pair, False, f"measuring failed ({type(exc).__name__}: {str(exc)[:200]})."
            ))
    return results


@dataclass
class LocatedSide:
    """A printed side of a declared pair, found on a built part."""

    pair: FitPair
    half: str  # "a" or "b"
    part: str
    face: Any  # the build123d Face of that part


def locate_sides(
    parts: Mapping[str, Any] | Iterable[tuple[str, Any]], pairs: Iterable[FitPair]
) -> list[LocatedSide]:
    """The mating faces of the declared pairs on the built parts: every printed side whose
    face is where the card put it, or within SEARCH_MM of it. Sides that are bought parts,
    were never applied or cannot be found are left out. Print preparation uses this to
    know which faces of a part are fit faces."""
    items = parts.items() if isinstance(parts, Mapping) else parts
    faces = _Faces(items)
    pairs = [pair for pair in pairs if isinstance(pair, FitPair)]
    hosts = _half_hosts(pairs, faces)
    found = []
    for pair in pairs:
        for key, side, frame in (("a", pair.a, pair.frame_a), ("b", pair.b, pair.frame_b)):
            if side.reference is not None or frame is None:
                continue
            try:
                name, face, _ = faces.find(*_to_world(frame, side),
                                           hosts.get((id(pair.owner), key)))
            except Exception:  # noqa: BLE001, S112 - OCCT raises unrelated types
                continue
            if face is not None and name is not None:
                found.append(LocatedSide(pair, key, name, face))
    return found


def frame_direction(frame: Any, direction: tuple[float, float, float]) -> tuple[float, ...]:
    """A direction given in a card's frame, as a unit vector in the model."""
    from build123d import Plane, Vector

    plane = Plane(frame)
    tip = plane.from_local_coords(Vector(*direction))
    return tuple((tip - plane.from_local_coords(Vector(0, 0, 0))).normalized())


def unverified_clearances(resolver: FitResolver, pairs: Iterable[FitPair]) -> list[Clearance]:
    """Clearances the resolver handed out that no declared pair accounts for."""
    used = {id(pair.clearance) for pair in pairs}
    return [record for record in resolver.issued if id(record) not in used]


# --------------------------------------------------------------------------------------
# joints
# --------------------------------------------------------------------------------------


@dataclass
class JointReport:
    """The outcome of checking one card as a joint between two parts."""

    card: Any
    hosts: dict[str, str | None]
    problems: list[str] = field(default_factory=list)
    data: dict = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return not self.problems

    @property
    def label(self) -> str:
        return f"{self.card.kind.replace('_', ' ')} '{self.card.name}'"

    @property
    def message(self) -> str:
        if self.ok:
            a, b = self.hosts["a"], self.hosts["b"]
            return (
                f"The {self.label} goes together: '{b}' reaches its place on '{a}' without "
                "colliding, and the room the joint needs is free."
            )
        return f"The {self.label} cannot be assembled as drawn: {' '.join(self.problems)}"

    def to_json(self) -> dict:
        return {
            "kind": "joint",
            "card": self.card.kind,
            "instance": self.card.name,
            "parts": [self.hosts["a"], self.hosts["b"]],
            "problems": list(self.problems),
            **self.data,
        }


def common_volume(a: Any, b: Any) -> float:
    """The volume two shapes share (0 when their bounding boxes are apart)."""
    box_a, box_b = a.bounding_box(), b.bounding_box()
    apart = any(
        lo_a > hi_b + 1e-6 or lo_b > hi_a + 1e-6
        for lo_a, hi_a, lo_b, hi_b in zip(tuple(box_a.min), tuple(box_a.max),
                                           tuple(box_b.min), tuple(box_b.max), strict=True)
    )
    if apart:
        return 0.0
    common = a.intersect(b)
    return sum(float(solid.volume) for item in common for solid in item.solids()) if common else 0.0


def _placement_gap(location: Any) -> tuple[float, float]:
    """(translation in mm, rotation angle in rad) by which a Location differs from identity."""
    transformation = location.wrapped.Transformation()
    shift = transformation.TranslationPart()
    angle = abs(transformation.GetRotation().GetRotationAngle())
    return math.sqrt(shift.X() ** 2 + shift.Y() ** 2 + shift.Z() ** 2), min(
        angle, abs(2.0 * math.pi - angle)
    )


def _same_placement(location: Any) -> bool:
    shift, angle = _placement_gap(location)
    return shift <= PLACEMENT_TOLERANCE_MM and angle <= PLACEMENT_TOLERANCE_RAD


def _touching(a: Any, b: Any) -> bool:
    from OCP.BRepExtrema import BRepExtrema_DistShapeShape

    extrema = BRepExtrema_DistShapeShape(a.wrapped, b.wrapped)
    return bool(extrema.IsDone()) and extrema.NbSolution() >= 1 and extrema.Value() <= TOUCH_MM


def _hosts_of(card: Any, measurements: Iterable[FitMeasurement]) -> dict[str, set]:
    hosts: dict[str, set] = {"a": set(), "b": set()}
    for measurement in measurements:
        if measurement.pair.owner is card:
            for key, name in zip(("a", "b"), measurement.parts, strict=True):
                if name is not None:
                    hosts[key].add(name)
    return hosts


def check_joints(
    parts: Mapping[str, Any] | Iterable[tuple[str, Any]],
    cards: Iterable[Any],
    measurements: Iterable[FitMeasurement],
) -> list[JointReport]:
    """Check every card as a joint between two solids; one report per card, in card order.

    `measurements` are the pair measurements of the same build: they say on which part each
    half's mating faces were found.
    """
    shapes = dict(parts.items() if isinstance(parts, Mapping) else parts)
    order = {name: index for index, name in enumerate(shapes)}
    measurements = list(measurements)
    reports: list[JointReport] = []
    groups: dict[tuple[str, str], list[tuple[JointReport, Any]]] = {}
    for card in cards:
        found = _hosts_of(card, measurements)
        hosts = {key: next(iter(names)) if len(names) == 1 else None
                 for key, names in found.items()}
        report = JointReport(card, hosts)
        reports.append(report)
        frames = card.frames
        for key, half in (("a", card.half_a), ("b", card.half_b)):
            if frames.get(key) is None:
                report.problems.append(
                    f"Half {key.upper()} ({half}) was never applied to a part."
                )
            elif len(found[key]) > 1:
                report.problems.append(
                    f"The mating faces of half {key.upper()} ({half}) are spread over the "
                    f"parts {', '.join(sorted(found[key]))}; a half belongs to one part."
                )
            elif hosts[key] is None:
                report.problems.append(
                    f"The part that carries half {key.upper()} ({half}) could not be "
                    "identified because its mating faces were not found."
                )
        if report.problems:
            continue
        if hosts["a"] == hosts["b"]:
            report.problems.append(
                f"Both halves are on the part '{hosts['a']}'; a joint is between two parts."
            )
            continue
        try:
            _check_keep_out(report, shapes, frames)
        except Exception as exc:  # noqa: BLE001 - OCCT booleans raise unrelated types
            report.problems.append(
                f"The room the joint needs could not be examined ({type(exc).__name__})."
            )
        first, second = sorted((hosts["a"], hosts["b"]), key=order.__getitem__)
        # The placement that moves `second` to where the card says it sits on `first`.
        placement = frames["a"] * frames["b"].inverse()
        if hosts["a"] != first:
            placement = placement.inverse()
        groups.setdefault((first, second), []).append((report, placement))
    for (first, second), members in groups.items():
        try:
            _check_group(shapes[first], shapes[second], first, second, members)
        except Exception as exc:  # noqa: BLE001 - OCCT booleans raise unrelated types
            for report, _ in members:
                report.problems.append(
                    f"Bringing '{second}' to its place on '{first}' could not be computed "
                    f"({type(exc).__name__})."
                )
    return reports


def _check_keep_out(report: JointReport, shapes: dict, frames: dict) -> None:
    card = report.card
    for key, half in (("a", card.half_a), ("b", card.half_b)):
        port = card.ports[half]
        if port.keep_out is None:
            continue
        host = report.hosts[key]
        room = (frames[key] * port.location) * port.keep_out
        blocked = common_volume(shapes[host], room)
        report.data.setdefault("keep_out", {})[half] = blocked
        if blocked > OVERLAP_TOLERANCE_MM3:
            report.problems.append(
                f"'{host}' has {blocked:.1f} mm3 of material in the room the {half} half "
                f"must leave free ({port.note})."
            )


def _check_group(first_shape: Any, second_shape: Any, first: str, second: str,
                 members: list[tuple[JointReport, Any]]) -> None:
    reference_report, reference = members[0]
    agreed = True
    for report, placement in members[1:]:
        shift, angle = _placement_gap(reference.inverse() * placement)
        if shift > PLACEMENT_TOLERANCE_MM or angle > PLACEMENT_TOLERANCE_RAD:
            agreed = False
            sentence = (
                "{own} and {other} are joints between the same two parts ('{a}' and '{b}') "
                f"but put them in different places: they disagree by {shift:.3f} mm"
                + (f" and {math.degrees(angle):.3f} degrees" if angle > PLACEMENT_TOLERANCE_RAD
                   else "")
                + ". Both joints cannot close at once; place the two halves of each card "
                "with the same `at`, or move every half of a part together."
            )
            for own, other in ((report, reference_report), (reference_report, report)):
                own.problems.append(
                    sentence.format(own=own.label.capitalize(), other=other.label,
                                    a=first, b=second)
                )
                own.data.setdefault("disagrees_with", []).append(
                    {"instance": other.card.name, "shift": shift, "angle": angle}
                )
    if not agreed:
        return
    in_place = _same_placement(reference)
    shift, angle = _placement_gap(reference)
    for report, _ in members:
        report.data["in_place"] = in_place
        report.data["placement_shift"] = shift
    if not in_place and _touching(first_shape, second_shape):
        for report, _ in members:
            report.problems.append(
                f"'{first}' and '{second}' are drawn touching, as if assembled, but the two "
                f"halves of this card were placed {shift:.3f} mm"
                + (f" and {math.degrees(angle):.3f} degrees" if angle > PLACEMENT_TOLERANCE_RAD
                   else "")
                + " apart, so the joint does not line up where the parts sit. Use the same "
                "`at` for both halves, or show the parts apart."
            )
        return
    moved = second_shape if in_place else reference * second_shape
    shared = common_volume(first_shape, moved)
    intended = sum(report.card.intended_overlap() for report, _ in members)
    for report, _ in members:
        report.data["assembled_overlap"] = shared
        report.data["intended_overlap"] = intended
    if shared > intended + OVERLAP_TOLERANCE_MM3:
        for report, _ in members:
            report.problems.append(
                f"With '{second}' brought to its place on '{first}' the two parts share "
                f"{shared:.1f} mm3"
                + (f" (the joint itself intends {intended:.1f} mm3 of interference)"
                   if intended > OVERLAP_TOLERANCE_MM3 else "")
                + ": something on one part is in the way of the other."
            )


def allowed_overlap(joints: Iterable[JointReport], a: str, b: str) -> float:
    """The interference the joints between parts `a` and `b` intend where the parts are drawn
    in place (an assembled press fit), in mm3. Zero when no such joint was verified."""
    total = 0.0
    for joint in joints:
        if ({joint.hosts["a"], joint.hosts["b"]} == {a, b} and joint.ok
                and joint.data.get("in_place")):
            total += joint.card.intended_overlap()
    return total


# --------------------------------------------------------------------------------------
# robustness
# --------------------------------------------------------------------------------------


def check_robustness(resolver: FitResolver) -> list[dict]:
    """`FitResolver.robustness` for every clearance the resolver handed out, each with the
    labels of the declared pairs that use it (`pairs`; empty for one taken without a card)."""
    users: dict[int, list[str]] = {}
    for pair in resolver.declared_fits:
        if isinstance(pair, FitPair):
            users.setdefault(id(pair.clearance), []).append(pair.label)
    return [
        {**resolver.robustness(record), "index": record.index,
         "pairs": users.get(id(record), [])}
        for record in resolver.issued
    ]
