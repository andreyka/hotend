"""What every feature card shares: ports, pair declaration, applying a half, the gauge.

A card is one joint. It is built from one definition, so its two halves cannot disagree:

    card = cards.peg_hole(diameter=5, length=6, ctx=ctx)
    base = card.apply_a(base, at=Pos(10, 0, 4))     # the hole
    cover = card.apply_b(cover, at=Pos(10, 0, 4))   # the peg

Rules a card keeps:
- It takes no clearance argument. Every gap comes from `ctx.fit` and is recorded there.
- `apply_a` and `apply_b` return a new part; `at` is where the card's frame sits in the
  model (a Location, a Plane or an (x, y, z) position; default the origin). Use the same `at`
  for both halves to show the joint assembled, or different ones to show the parts apart: the
  fit is measured with the halves brought together either way.
- A card is one joint: each half is applied once. Make one card per joint.
- Apply a card after the body of the part is finished and do not move or reshape the part
  afterwards; the `fit_pairs` check looks for the mating faces where the card put them.
- `gauge()` is the smallest print that tests the joint, built with the very same clearances.
- A card can be written down and built again: `declaration()` is its kind, its arguments and
  the frames its halves were applied at, as plain JSON data, and `restore_cards` rebuilds
  the cards from that data on another resolver. The engine uses this to verify the fits in
  a process that never ran the model script: the script's process only says which cards it
  applied where; every clearance, probe and expected face is recomputed from the card's
  definition.
"""

from __future__ import annotations

import math
import re
from dataclasses import dataclass, field
from typing import Any, ClassVar

from ..fit import Clearance, FitResolver, resolver_of
from ..verify import FitPair, Side, common_volume, reference_area

# Space between the two pieces of a gauge on the bed (mm).
GAUGE_SPACING_MM = 5.0
# The end of every gauge dimension's `what` text. The first layers are pressed into the bed
# and print wider than drawn (and a brim leaves a burr there), so a caliper at the bottom
# edge reads the bed, not the fit.
ABOVE_THE_BED = "; measure above the first millimetre from the bed"
NAME_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_ .-]{0,63}\Z")
# Limits on a declaration read back from another process.
MAX_DECLARED_CARDS = 256
MAX_FRAME_MM = 1.0e5
FRAME_TOLERANCE = 1e-9


class CardError(ValueError):
    """The card cannot be built as asked. The message says which dimension to change."""


@dataclass(frozen=True)
class Port:
    """Where a half of the card meets its host part, in the card's frame.

    `location` is the port's origin and orientation, `direction` the unit vector along which
    the mating half arrives, and `keep_out` a solid (or None), given in the port's own frame
    (`location`), that the host must leave empty so the mating half or the bought part has
    room. The joint check fails a host that has material there.
    """

    name: str
    location: Any
    direction: tuple[float, float, float]
    keep_out: Any
    note: str


@dataclass
class Gauge:
    """A card's test print: the two halves laid out on the bed and their fit pairs."""

    card: str
    parts: dict[str, Any]
    fits: list[FitPair]
    # What to measure on the printed gauge (`Card.gauge_dimensions` of the gauge's own card).
    dimensions: list[dict] = field(default_factory=list)

    @property
    def compound(self) -> Any:
        from build123d import Compound

        return Compound(children=list(self.parts.values()), label=f"{self.card}_gauge")

    @property
    def size(self) -> tuple[float, float, float]:
        return tuple(float(v) for v in tuple(self.compound.bounding_box().size))


def as_location(at: Any) -> Any:
    """`at` as a build123d Location: None, a Location, a Plane or an (x, y, z) position."""
    from build123d import Location, Plane, Pos

    if at is None:
        return Location()
    if isinstance(at, Location):
        return at
    if isinstance(at, Plane):
        return at.location
    if isinstance(at, (tuple, list)) and len(at) == 3:
        return Pos(*(float(c) for c in at))
    raise CardError(
        f"'at' must be a Location, a Plane or an (x, y, z) position, got {type(at).__name__}"
    )


def as_shape(part: Any) -> Any:
    """The build123d shape of a host part; a BuildPart builder is unwrapped."""
    from build123d import Shape

    if not isinstance(part, Shape) and isinstance(getattr(part, "part", None), Shape):
        part = part.part
    if not isinstance(part, Shape) or not part.solids():
        raise CardError(
            "a card is applied to a solid part (a build123d Part, Solid or Compound), got "
            f"{type(part).__name__}"
        )
    return part


class Card:
    """Base of the feature cards. Subclasses define the geometry of the two halves."""

    kind: ClassVar[str] = "card"
    half_a: ClassVar[str] = "a"
    half_b: ClassVar[str] = "b"

    def __init__(self, ctx: Any, name: str | None = None, *, _declare: bool = True) -> None:
        self.resolver: FitResolver = resolver_of(ctx)
        self._declare = _declare
        self._pairs: list[FitPair] = []
        self._applied: dict[str, Any] = {}
        self._ports: dict[str, Port] | None = None
        self._reference: dict[str, Any] = {}
        self._areas: dict[tuple, float | None] = {}
        if name is None:
            same = sum(1 for card in self.resolver.cards if card.kind == self.kind)
            name = f"{self.kind}_{same + 1}"
        if not isinstance(name, str) or not NAME_RE.match(name):
            # The refused name is not repeated: the message is shown and stored.
            raise CardError(
                "the card's name is not allowed; use 1 to 64 letters, digits, spaces, "
                "dots, dashes or underscores"
            )
        self.name = name

    # ---- for subclasses -------------------------------------------------------------

    def _clearance(self, shared: dict[str, Clearance] | None, key: str, fit_class: str,
                   nominal: float, **options: Any) -> Clearance:
        """A clearance from the resolver, or the one a parent card already resolved (a gauge
        is built with its product's clearances, not with new ones)."""
        if shared is not None:
            return shared[key]
        return self.resolver.clearance(fit_class, nominal, **options)

    def _pair(self, name: str, clearance: Clearance, a: Side, b: Side) -> None:
        pair = FitPair(self.kind, self.name, name, clearance, a, b, owner=self)
        self._pairs.append(pair)
        if self._declare:
            # A card registers with its first pair, that is once its dimensions were accepted.
            if not any(card is self for card in self.resolver.cards):
                self.resolver.cards.append(self)
            self.resolver.declared_fits.append(pair)

    def _solids_a(self) -> tuple[Any, Any]:
        """(solid to add, solid to cut) for half A in the card's frame; either may be None."""
        raise NotImplementedError

    def _solids_b(self) -> tuple[Any, Any]:
        raise NotImplementedError

    def _make_ports(self) -> dict[str, Port]:
        raise NotImplementedError

    def _hosts(self) -> tuple[Any, Any]:
        """(host for half A, host for half B): the smallest plain bodies this card's halves
        can be applied to, in the card's frame. They are the hosts of the gauge and of the
        reference halves the checks compare the built parts with."""
        raise NotImplementedError

    def _gauge_card(self) -> Card:
        """An undeclared card to build the gauge from, with this card's clearance records."""
        raise NotImplementedError

    def _args(self) -> dict:
        """The public constructor arguments (without `ctx` and `name`) as JSON data."""
        raise NotImplementedError

    def gauge_dimensions(self) -> list[dict]:
        """What a caliper can measure on this card's printed halves to calibrate the
        printer: one entry per dimension with `id`, `half` (the half's name), `feature`
        (`hole`: an inside size, which prints small by twice the hole offset; `peg`: an
        outside size, which prints large by twice the peg offset), `drawn` (mm, as this
        card draws it) and `what` (where to put the caliper, ending with `ABOVE_THE_BED`).
        Empty when a card has none."""
        return []

    def flex_axes(self) -> dict[str, tuple[float, float, float]]:
        """Per half (`a`, `b`) that carries a part meant to bend (a snap rail): the
        direction, in the card's frame, along which that part is long. Printed with this
        direction upright the layers lie across the bending part, its weaker direction;
        print preparation counts that against an orientation."""
        return {}

    def feature_boxes(self) -> list[tuple[tuple[float, ...], tuple[float, ...]]]:
        """The bounding boxes (min corner, max corner), in the model, of every solid an
        applied half of this card added to or cut from its part. Faces of the two parts
        that face each other inside these boxes belong to this card's joint."""
        boxes = []
        for key, solids in (("a", self._solids_a), ("b", self._solids_b)):
            frame = self._applied.get(key)
            if frame is None:
                continue
            for shape in solids():
                if shape is None:
                    continue
                for solid in shape.solids():
                    box = (frame * solid).bounding_box()
                    boxes.append((tuple(box.min), tuple(box.max)))
        return boxes

    # ---- public ---------------------------------------------------------------------

    @property
    def ports(self) -> dict[str, Port]:
        """The card's named ports, keyed by half name."""
        if self._ports is None:
            self._ports = self._make_ports()
        return self._ports

    def apply_a(self, part: Any, at: Any = None) -> Any:
        """Put half A on `part` with the card's frame at `at`; returns the new part."""
        return self._apply("a", part, at, *self._solids_a())

    def apply_b(self, part: Any, at: Any = None) -> Any:
        """Put the mating half B on `part` with the card's frame at `at`; returns the new part."""
        return self._apply("b", part, at, *self._solids_b())

    def checks(self) -> list[FitPair]:
        """The fit pairs the engine verifies for this joint."""
        return list(self._pairs)

    @property
    def frames(self) -> dict[str, Any]:
        """Where the card's frame sat when each half was applied: `{"a": Location, "b": ...}`
        with only the halves applied so far."""
        return dict(self._applied)

    def reference_half(self, key: str) -> Any:
        """Half `a` or `b` applied to the card's own plain host, in the card's frame: what
        the card makes when nothing else is in the way."""
        if key not in self._reference:
            host = self._hosts()[0 if key == "a" else 1]
            add, cut = self._solids_a() if key == "a" else self._solids_b()
            self._reference[key] = _combine(host, None, add, cut)
        return self._reference[key]

    def reference_area(self, key: str, side: Side) -> float | None:
        """The area of the mating face `side` probes on the reference half, or None."""
        cache_key = (key, side.point, side.normal)
        if cache_key not in self._areas:
            self._areas[cache_key] = reference_area(self.reference_half(key), side)
        return self._areas[cache_key]

    def intended_overlap(self) -> float:
        """The volume (mm3) the two halves share by design when the joint is closed. Zero for
        every joint but an interference fit, which overrides this."""
        return 0.0

    def _reference_overlap(self) -> float:
        return common_volume(self.reference_half("a"), self.reference_half("b"))

    def declaration(self) -> dict:
        """The card as plain JSON data: kind, name, arguments and the frames its halves were
        applied at (3 x 4 matrices, row by row; null for a half not applied)."""
        return {
            "card": self.kind,
            "name": self.name,
            "args": self._args(),
            "frames": {
                key: None if self._applied.get(key) is None else frame_to_json(self._applied[key])
                for key in ("a", "b")
            },
        }

    def _restore_frames(self, frames: dict[str, Any]) -> None:
        """Record where the halves were applied without touching any part (a card rebuilt
        from its declaration)."""
        for key in ("a", "b"):
            frame = frames.get(key)
            if frame is None:
                continue
            self._applied[key] = frame
            for pair in self._pairs:
                if key == "a":
                    pair.frame_a = frame
                else:
                    pair.frame_b = frame

    def gauge_set(self) -> Gauge:
        """The test print as named parts on the bed, with the fit pairs that verify it."""
        card = self._gauge_card()
        host_a, host_b = card._hosts()
        half_a = card.apply_a(host_a)
        half_b = card.apply_b(host_b)
        half_a, move_a = _onto_bed(half_a, flip=False, x=0.0)
        width = float(half_a.bounding_box().size.X)
        half_b, move_b = _onto_bed(half_b, flip=True, x=width + GAUGE_SPACING_MM)
        for pair in card._pairs:
            pair.frame_a = move_a * pair.frame_a if pair.frame_a is not None else None
            pair.frame_b = move_b * pair.frame_b if pair.frame_b is not None else None
        return Gauge(
            self.kind,
            {f"gauge_{self.half_a}": half_a, f"gauge_{self.half_b}": half_b},
            card.checks(),
            card.gauge_dimensions(),
        )

    def gauge(self) -> Any:
        """The smallest print that tests this fit: both halves side by side on the bed."""
        return self.gauge_set().compound

    # ---- internals ------------------------------------------------------------------

    def _apply(self, half: str, part: Any, at: Any, add: Any, cut: Any) -> Any:
        from build123d import Part

        if half in self._applied:
            raise CardError(
                f"half {half.upper()} of {self.name} was already applied; a card is one "
                "joint, so make one card for each joint"
            )
        shape = as_shape(part)
        frame = as_location(at)
        try:
            result = _combine(shape, frame, add, cut)
        except _Detached:
            raise CardError(
                f"half {half.upper()} of {self.name} does not touch the part at this "
                "location, so it would print as a loose piece; check 'at'"
            ) from None
        if not result.solids():
            raise CardError(f"applying half {half.upper()} of {self.name} left no solid")
        if not isinstance(result, Part):
            result = Part(result.wrapped)
        for attribute in ("label", "color"):
            value = getattr(shape, attribute, None)
            if value:
                setattr(result, attribute, value)
        self._applied[half] = frame
        for pair in self._pairs:
            if half == "a":
                pair.frame_a = frame
            else:
                pair.frame_b = frame
        return result


class _Detached(Exception):
    """The solid a half adds does not touch its host."""


def _combine(shape: Any, frame: Any, add: Any, cut: Any) -> Any:
    """`shape` with a half's solids added and cut at `frame` (None: the card's own frame)."""
    before = len(shape.solids())
    result = shape
    if add is not None:
        result = result + (add if frame is None else frame * add)
        if len(result.solids()) > before:
            raise _Detached
    if cut is not None:
        result = result - (cut if frame is None else frame * cut)
    return result


def frame_to_json(frame: Any) -> list[float]:
    """A Location as the 12 numbers of its 3 x 4 matrix, row by row."""
    transformation = frame.wrapped.Transformation()
    return [float(transformation.Value(row, column))
            for row in (1, 2, 3) for column in (1, 2, 3, 4)]


def frame_from_json(raw: Any) -> Any:
    """The Location of `frame_to_json`. Raises CardError unless `raw` is 12 finite numbers
    that form a rigid placement (a rotation and a translation, no scale, no mirror)."""
    from build123d import Location
    from OCP.gp import gp_Trsf

    if (not isinstance(raw, (list, tuple)) or len(raw) != 12
            or not all(_is_number(value) for value in raw)):
        raise CardError("a frame must be 12 finite numbers (a 3 x 4 matrix, row by row)")
    rows = [[float(v) for v in raw[i:i + 4]] for i in (0, 4, 8)]
    if any(abs(row[3]) > MAX_FRAME_MM for row in rows):
        raise CardError("a frame's position is implausibly far from the origin")
    rotation = [row[:3] for row in rows]
    for i in range(3):
        for j in range(3):
            dot = sum(rotation[i][k] * rotation[j][k] for k in range(3))
            if abs(dot - (1.0 if i == j else 0.0)) > FRAME_TOLERANCE:
                raise CardError("a frame must be a rigid placement (rotation and translation)")
    (a, b, c), (d, e, f), (g, h, i) = rotation
    determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
    if determinant < 0:
        raise CardError("a frame must not mirror the card")
    transformation = gp_Trsf()
    transformation.SetValues(*[value for row in rows for value in row])
    return Location(transformation)


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _onto_bed(part: Any, flip: bool, x: float) -> tuple[Any, Any]:
    """Move a gauge half so it sits on z = 0 with its lowest x at `x`; returns (part, move)."""
    from build123d import Pos, Rot

    turn = Rot(180, 0, 0) if flip else Rot(0, 0, 0)
    turned = turn * part
    box = turned.bounding_box()
    move = Pos(x - box.min.X, -(box.min.Y + box.max.Y) / 2, -box.min.Z) * turn
    return move * part, move
