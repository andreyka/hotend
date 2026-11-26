"""The snap_lid feature card. Its use is documented on the class `SnapLid`."""

from __future__ import annotations

import math
from typing import Any

from ..fit import Clearance
from ..verify import CARD, HOST, Side
from .base import ABOVE_THE_BED, Card, CardError, Port

# Height of the flat crest of the bead (mm): two layers at 0.3 mm, three at 0.2 mm.
CREST_MM = 0.6
# Each rail runs along this fraction of its wall, centred, so it clears rounded inner corners.
RAIL_FRACTION = 0.5
# The bead runs along this fraction of its rail, but is at least MIN_BEAD_MM long.
BEAD_FRACTION = 0.5
MIN_BEAD_MM = 6.0
# Wall left above the groove, between it and the rim (mm).
MIN_LIP_MM = 1.0
MIN_RAIL_MM = 1.2
# The rail follows the wall's thickness up to this; a thicker rail only gets stiffer and
# breaks sooner when the bead rides over the wall.
MAX_RAIL_MM = 2.0
# Largest bending strain the snap rail may see when the lid is pushed on, by material family.
# General practice for a rail printed lying on the lid (loaded along its layers); guessed.
# An unknown material gets the lowest.
STRAIN_LIMITS = {"PLA": 0.02, "PETG": 0.04}
# The groove may take at most this fraction of the wall's thickness.
MAX_GROOVE_RATIO = 0.6


class SnapLid(Card):
    """snap_lid: a lid that snaps onto a rectangular opening.

        card = cards.snap_lid(opening=(80, 60), wall=2.0, ctx=ctx)
        box = card.apply_a(box, at=Pos(0, 0, height))    # grooves in the box's inner walls
        lid = card.apply_b(lid, at=Pos(0, 0, height))    # rails under the lid plate

    Frame: the origin is the centre of the opening in the plane of the box's rim; +Z points out
    of the box; `opening` is the clear inner size along X and Y at the rim.

    Half A (the box) needs straight inner walls on the planes x = +/- opening[0] / 2 and
    y = +/- opening[1] / 2 from the rim down to `depth`, at least `wall` thick; it gets a short
    groove in each of the two long walls. Half B (the lid) needs a plate whose underside is the
    rim plane (z = 0); it gets four rails that hang into the opening: two snap rails along the
    long walls, each with a bead that rides over the wall and drops into the groove, and two
    plain rails along the short walls that locate the lid. The rails are free at their ends so
    the snap rails can flex. Pushing the lid on bends each snap rail by the snap undercut; a
    rail that is too short for that (too small a `depth`) would break, so the card refuses it
    and names the depth that is needed for the material.

    Fit pairs, each on both sides: `wall` (snap rail against its wall, slide), `snap` (how far
    the bead's crest stands past the wall it rides over; the snap class, negative), `seat` (crest
    against the bottom of the groove, snug), `end` (bead end against groove end, loose) and
    `locate` (plain rail against its wall, slide).
    """

    kind = "snap_lid"
    half_a = "box"
    half_b = "lid"

    def __init__(self, opening: tuple[float, float], wall: float, *, ctx: Any,
                 depth: float = 8.0, name: str | None = None,
                 _shared: dict[str, Clearance] | None = None, _declare: bool = True) -> None:
        try:
            width, length = (float(v) for v in opening)
        except (TypeError, ValueError):
            raise CardError("snap_lid opening must be (x size, y size) in mm") from None
        if not (width > 0 and length > 0 and wall > 0 and depth > 0):
            raise CardError("snap_lid opening, wall and depth must be positive")
        super().__init__(ctx, name, _declare=_declare)
        self.opening, self.wall, self.depth = (width, length), float(wall), float(depth)
        # Canonical layout: the snap walls are the long ones, at y = +/- span / 2, running
        # along x. When the opening is longer in Y the whole card is turned a quarter turn.
        self.turned = length > width
        self.run, self.span = (length, width) if self.turned else (width, length)
        self.rail = min(MAX_RAIL_MM, max(MIN_RAIL_MM, 0.8 * self.wall))

        self.c_wall = self._clearance(_shared, "wall", "slide", self.span)
        self.c_snap = self._clearance(_shared, "snap", "snap", self.span)
        self.c_seat = self._clearance(_shared, "seat", "snug", self.span)
        self.c_locate = self._clearance(_shared, "locate", "slide", self.run)
        self.rail_length = RAIL_FRACTION * self.run
        self.locate_length = RAIL_FRACTION * self.span
        self.bead_length = min(self.rail_length, max(MIN_BEAD_MM, BEAD_FRACTION * self.rail_length))
        self.c_end = self._clearance(_shared, "end", "loose", self.bead_length)

        g_wall, g_snap, g_seat = self.c_wall.gap, self.c_snap.gap, self.c_seat.gap
        self.bead_height = g_wall - g_snap
        self.groove_depth = g_seat - g_snap
        if self.bead_height <= 0 or self.groove_depth <= 0:
            raise CardError("this printer's calibration leaves no snap undercut to build")
        if self.groove_depth > MAX_GROOVE_RATIO * self.wall:
            raise CardError(
                f"the snap groove is {self.groove_depth:.2f} mm deep, too much for a "
                f"{self.wall:g} mm wall; use a wall of at least "
                f"{self.groove_depth / MAX_GROOVE_RATIO:.2f} mm"
            )
        for size, gap, what in ((self.span, g_wall, "short"), (self.run, self.c_locate.gap, "long")):
            needed = 4 * (gap + self.rail)
            if size <= needed:
                raise CardError(
                    f"the opening's {what} side is {size:g} mm, too small for the lid's rails; "
                    f"it must be more than {needed:.2f} mm"
                )
        # Heights (z, negative below the rim) of the bead and the groove.
        self.z_crest_low = -self.depth + self.bead_height
        self.z_crest_high = self.z_crest_low + CREST_MM
        self.z_bead_top = self.z_crest_high + self.bead_height
        self.z_crest = (self.z_crest_low + self.z_crest_high) / 2
        self.groove_half_bottom = CREST_MM / 2 + g_seat * (math.sqrt(2) - 1)
        self.groove_half_mouth = self.groove_half_bottom + self.groove_depth
        plain_from = max(self.z_crest + self.groove_half_mouth, self.z_bead_top)
        if -plain_from < MIN_LIP_MM:
            raise CardError(
                f"depth {self.depth:g} mm leaves no wall above the snap groove; use at least "
                f"{self.depth - plain_from + MIN_LIP_MM:.2f} mm"
            )
        self.z_wall = plain_from / 2
        # Undercut the printed bead has to ride over, and the bending strain that costs the
        # rail (a cantilever fixed at the lid plate, deflected at the bead's crest).
        undercut = max(0.0, -self.c_snap.target)
        arm = self.depth - self.bead_height - CREST_MM / 2
        self.insertion_strain = 1.5 * self.rail * undercut / arm**2 if arm > 0 else math.inf
        family = self.resolver.material_family
        self.strain_limit = STRAIN_LIMITS.get(family, min(STRAIN_LIMITS.values()))
        if self.insertion_strain > self.strain_limit:
            needed = (math.sqrt(1.5 * self.rail * undercut / self.strain_limit)
                      + self.bead_height + CREST_MM / 2)
            raise CardError(
                f"a snap rail {self.depth:g} mm deep would be bent by "
                f"{100 * self.insertion_strain:.1f} % when the lid is pushed on, more than the "
                f"{100 * self.strain_limit:g} % {family or 'an unknown material'} takes; use "
                f"a depth of at least {math.ceil(needed * 10) / 10:g} mm"
            )
        self._declare_pairs()

    # ---- frame helpers ----------------------------------------------------------------

    def _turn(self, vector: tuple[float, float, float]) -> tuple[float, float, float]:
        x, y, z = vector
        return (-y, x, z) if self.turned else (x, y, z)

    def _place(self, solid: Any) -> Any:
        from build123d import Rot

        return Rot(0, 0, 90) * solid if self.turned else solid

    def _side(self, point: tuple[float, float, float], normal: tuple[float, float, float],
              made: str = CARD) -> Side:
        return Side(self._turn(point), self._turn(normal), made=made)

    def _declare_pairs(self) -> None:
        half, g_wall, g_snap = self.span / 2, self.c_wall.gap, self.c_snap.gap
        y_rail, y_crest = half - g_wall, half - g_snap
        y_groove = y_crest + self.c_seat.gap
        x_bead = self.bead_length / 2
        x_wall, g_locate = self.run / 2, self.c_locate.gap
        for sign, tag in ((1.0, "plus"), (-1.0, "minus")):
            inward, outward = (0.0, -sign, 0.0), (0.0, sign, 0.0)
            # The box's own walls are the host's; everything else here the card makes.
            wall = self._side((0.0, sign * half, self.z_wall), inward, HOST)
            crest = self._side((0.0, sign * y_crest, self.z_crest), outward)
            self._pair(f"wall_{tag}", self.c_wall, wall,
                       self._side((0.0, sign * y_rail, self.z_wall), outward))
            self._pair(f"snap_{tag}", self.c_snap, wall, crest)
            self._pair(f"seat_{tag}", self.c_seat,
                       self._side((0.0, sign * y_groove, self.z_crest), inward), crest)
            self._pair(
                f"end_{tag}", self.c_end,
                self._side((x_bead + self.c_end.gap, sign * (half + self.groove_depth / 2),
                            self.z_crest), (-1.0, 0.0, 0.0)),
                self._side((x_bead, sign * (y_rail + self.bead_height / 2), self.z_crest),
                           (1.0, 0.0, 0.0)),
            )
            self._pair(
                f"locate_{tag}", self.c_locate,
                self._side((sign * x_wall, 0.0, -self.depth / 2), (-sign, 0.0, 0.0), HOST),
                self._side((sign * (x_wall - g_locate), 0.0, -self.depth / 2), (sign, 0.0, 0.0)),
            )

    def _resolved(self) -> dict[str, Clearance]:
        return {"wall": self.c_wall, "snap": self.c_snap, "seat": self.c_seat,
                "locate": self.c_locate, "end": self.c_end}

    def _args(self) -> dict:
        return {"opening": list(self.opening), "wall": self.wall, "depth": self.depth}

    def gauge_dimensions(self) -> list[dict]:
        return [
            {"id": "opening_width", "half": self.half_a, "feature": "hole",
             "drawn": self.span,
             "what": "the inside width of the opening between the two grooved walls, at the "
                     "rim" + ABOVE_THE_BED},
            {"id": "rail_width", "half": self.half_b, "feature": "peg",
             "drawn": 2 * (self.span / 2 - self.c_wall.gap),
             "what": "the outside width over the two snap rails, next to the lid plate "
                     "(above the beads), without squeezing them" + ABOVE_THE_BED},
        ]

    def flex_axes(self) -> dict[str, tuple[float, float, float]]:
        # The snap rails hang from the lid plate along the card's Z and bend sideways.
        return {"b": (0.0, 0.0, 1.0)}

    # ---- geometry ---------------------------------------------------------------------

    @staticmethod
    def _prism(points: list[tuple[float, float]], length: float) -> Any:
        """A (y, z) profile extruded along X, centred on x = 0."""
        from build123d import Plane, Polygon, extrude

        # Extruded both ways, so the result does not depend on the profile's winding.
        return extrude(Plane.YZ * Polygon(*points, align=None), amount=length / 2, both=True)

    def _solids_a(self) -> tuple[Any, Any]:
        half, overlap = self.span / 2, self.c_wall.gap
        y_bottom = half + self.groove_depth
        mouth, bottom, z = self.groove_half_mouth, self.groove_half_bottom, self.z_crest
        length = self.bead_length + 2 * self.c_end.gap
        grooves = None
        for sign in (1.0, -1.0):
            # The profile starts inside the opening (air) so the cut never ends on the wall.
            points = [
                (sign * (half - overlap), z - mouth), (sign * half, z - mouth),
                (sign * y_bottom, z - bottom), (sign * y_bottom, z + bottom),
                (sign * half, z + mouth), (sign * (half - overlap), z + mouth),
            ]
            groove = self._prism(points, length)
            grooves = groove if grooves is None else grooves + groove
        return None, self._place(grooves)

    def _solids_b(self) -> tuple[Any, Any]:
        from build123d import Align, Box, Pos

        hang = (Align.CENTER, Align.CENTER, Align.MAX)
        y_rail = self.span / 2 - self.c_wall.gap
        y_crest = self.span / 2 - self.c_snap.gap
        x_rail = self.run / 2 - self.c_locate.gap
        rails = None
        for sign in (1.0, -1.0):
            snap_rail = Pos(0, sign * (y_rail - self.rail / 2), 0) * Box(
                self.rail_length, self.rail, self.depth, align=hang
            )
            # The bead's profile reaches into the rail so the two fuse into one solid.
            back = sign * (y_rail - self.rail / 2)
            bead = self._prism([
                (back, -self.depth), (sign * y_rail, -self.depth),
                (sign * y_crest, self.z_crest_low), (sign * y_crest, self.z_crest_high),
                (sign * y_rail, self.z_bead_top), (back, self.z_bead_top),
            ], self.bead_length)
            plain_rail = Pos(sign * (x_rail - self.rail / 2), 0, 0) * Box(
                self.rail, self.locate_length, self.depth, align=hang
            )
            for solid in (snap_rail + bead, plain_rail):
                rails = solid if rails is None else rails + solid
        return self._place(rails), None

    def _make_ports(self) -> dict[str, Port]:
        from build123d import Align, Box, Compound, Location, Pos

        hang = (Align.CENTER, Align.CENTER, Align.MAX)
        reach = self.rail + 2 * self.c_wall.gap
        reach_locate = self.rail + 2 * self.c_locate.gap
        room_z = self.depth + self.c_end.gap
        rooms = []
        for sign in (1.0, -1.0):
            rooms.append(Pos(0, sign * (self.span / 2 - reach / 2), 0) * Box(
                self.rail_length + 2 * self.c_end.gap, reach, room_z, align=hang))
            rooms.append(Pos(sign * (self.run / 2 - reach_locate / 2), 0, 0) * Box(
                reach_locate, self.locate_length + 2 * self.c_end.gap, room_z, align=hang))
        keep_out = self._place(Compound(children=rooms))
        return {
            "box": Port("box", Location(), (0.0, 0.0, -1.0), keep_out,
                        "the lid's rails enter along -Z; keep the inside of the opening free "
                        f"next to the walls down to {room_z:.2f} mm below the rim"),
            "lid": Port("lid", Location(), (0.0, 0.0, -1.0), None,
                        "the rails hang from the lid plate's underside at z = 0; the plate "
                        "must cover the opening and rest on the rim"),
        }

    def _hosts(self) -> tuple[Any, Any]:
        from build123d import Align, Box

        width, length = self.opening
        height = self.depth + 2.0
        hang = (Align.CENTER, Align.CENTER, Align.MAX)
        ring = Box(width + 2 * self.wall, length + 2 * self.wall, height, align=hang) - Box(
            width, length, height, align=hang
        )
        plate = Box(width + 2 * self.wall, length + 2 * self.wall, 1.6,
                    align=(Align.CENTER, Align.CENTER, Align.MIN))
        return ring, plate

    def _gauge_card(self) -> Card:
        # The gauge is a short piece of the same joint: same walls, rails and clearances.
        run = max(24.0, 4 * (self.c_locate.gap + self.rail) + 4.0)
        span = max(16.0, 4 * (self.c_wall.gap + self.rail) + 4.0)
        return SnapLid((run, span), self.wall, ctx=self.resolver, depth=self.depth,
                       name=self.name, _shared=self._resolved(), _declare=False)
