"""The peg_hole feature card. Its use is documented on the class `PegHole`."""

from __future__ import annotations

from typing import Any

from ..fit import Clearance
from ..verify import CARD, ROUND, Side
from .base import ABOVE_THE_BED, Card, CardError, Port

FITS = ("press", "snug", "slide", "loose")
MIN_DIAMETER_MM = 1.0
# The peg's tip is chamfered by this fraction of its diameter (at most 0.6 mm) as a lead-in.
TIP_CHAMFER_RATIO = 0.12
MAX_TIP_CHAMFER_MM = 0.6
# The hole's mouth is bevelled at 45 degrees by this fraction of the diameter (at most
# 0.4 mm) as a lead-in for the peg. It also keeps the mouth open when that face is the first
# layer on the bed, where the squashed first layers (elephant's foot) narrow a plain hole
# exactly where the peg has to enter. It is not a clearance: the fit is the straight part.
MOUTH_BEVEL_RATIO = 0.08
MAX_MOUTH_BEVEL_MM = 0.4
# A through hole is cut this much past the peg's length so that no skin is left in the host.
# It is cut into air, not a clearance between printed faces.
THROUGH_OVERSHOOT_MM = 1.0


class PegHole(Card):
    """peg_hole: a round peg on one part and the hole it enters on the other.

        card = cards.peg_hole(diameter=5.0, length=6.0, fit="press", ctx=ctx)
        base = card.apply_a(base, at=Pos(x, y, top_of_base))      # the hole, drilled along -Z
        cover = card.apply_b(cover, at=Pos(x, y, top_of_base))    # the peg, hanging along -Z

    Frame: the origin is the centre of the hole's mouth on the face where the two parts meet;
    +Z points out of the holed part (A) into the pegged part (B). The hole runs along -Z.

    `fit` is `press` (the peg is held by interference), `snug`, `slide` or `loose`. The peg keeps
    its nominal diameter as printed and the hole takes the gap. The peg's tip is chamfered and
    the hole's mouth is bevelled (0.4 mm at most) so the peg finds its way in, also when the
    mouth is the first layer on the print bed. A blind hole is deeper than the
    peg by a loose gap along Z so the peg never bottoms out; `through=True` is for a hole that
    goes right through part A, which must then be no thicker than `length`.

    A press fit drawn with interference overlaps in CAD when the two parts are shown
    assembled. That overlap is the joint's own and is allowed for; anything more is reported.
    Use the same orientation of `at` for both halves.
    """

    kind = "peg_hole"
    half_a = "hole"
    half_b = "peg"

    def __init__(self, diameter: float, length: float, fit: str = "slide", *, ctx: Any,
                 through: bool = False, name: str | None = None,
                 _shared: dict[str, Clearance] | None = None, _declare: bool = True) -> None:
        if fit not in FITS:
            raise CardError(f"peg_hole fit must be one of {', '.join(FITS)}, got {fit!r}")
        if not diameter >= MIN_DIAMETER_MM:
            raise CardError(f"peg diameter must be at least {MIN_DIAMETER_MM} mm, got {diameter}")
        if not length >= 0.5 * diameter:
            raise CardError(
                f"peg length {length} mm is too short to locate a {diameter} mm peg; use at "
                f"least {0.5 * diameter:g} mm"
            )
        super().__init__(ctx, name, _declare=_declare)
        self.diameter, self.length = float(diameter), float(length)
        self.fit, self.through = fit, through
        self.radial = self._clearance(_shared, "radial", fit, self.diameter)
        self.hole_radius = self.diameter / 2 + self.radial.hole_delta
        self.peg_radius = self.diameter / 2 - self.radial.peg_delta
        if self.peg_radius <= 0 or self.hole_radius <= 0:
            raise CardError(f"a {diameter} mm peg is too thin for this printer's calibration")
        self.tip_chamfer = min(MAX_TIP_CHAMFER_MM, TIP_CHAMFER_RATIO * self.diameter)
        self.mouth_bevel = min(MAX_MOUTH_BEVEL_MM, MOUTH_BEVEL_RATIO * self.diameter)
        middle = -self.length / 2
        self._pair(
            "radial", self.radial,
            # A through hole is as deep as its host is thick, so only its roundness is
            # known; a blind hole is the card's own, wall and bottom.
            Side((self.hole_radius, 0.0, middle), (-1.0, 0.0, 0.0),
                 made=ROUND if through else CARD),
            Side((self.peg_radius, 0.0, middle), (1.0, 0.0, 0.0), made=CARD),
        )
        if through:
            self.relief = None
            self.hole_depth = self.length + THROUGH_OVERSHOOT_MM
        else:
            self.relief = self._clearance(_shared, "relief", "loose", self.length, axis="z")
            self.hole_depth = self.length + self.relief.gap
            self._pair(
                "relief", self.relief,
                Side((0.0, 0.0, -self.hole_depth), (0.0, 0.0, 1.0), made=CARD),
                Side((0.0, 0.0, -self.length), (0.0, 0.0, -1.0), made=CARD),
            )

    def _resolved(self) -> dict[str, Clearance]:
        return {"radial": self.radial, "relief": self.relief}

    def _args(self) -> dict:
        return {"diameter": self.diameter, "length": self.length, "fit": self.fit,
                "through": self.through}

    def gauge_dimensions(self) -> list[dict]:
        return [
            {"id": "hole_diameter", "half": self.half_a, "feature": "hole",
             "drawn": 2 * self.hole_radius,
             "what": "the inside diameter of the hole" + ABOVE_THE_BED},
            {"id": "peg_diameter", "half": self.half_b, "feature": "peg",
             "drawn": 2 * self.peg_radius,
             "what": "the diameter of the peg, away from its chamfered tip" + ABOVE_THE_BED},
        ]

    def intended_overlap(self) -> float:
        """The ring of material a peg drawn larger than its hole shares with the hole's wall."""
        return self._reference_overlap() if self.radial.gap < 0 else 0.0

    def _solids_a(self) -> tuple[Any, Any]:
        from build123d import Align, Cone, Cylinder

        top = (Align.CENTER, Align.CENTER, Align.MAX)
        hole = Cylinder(self.hole_radius, self.hole_depth, align=top)
        mouth = Cone(self.hole_radius, self.hole_radius + self.mouth_bevel, self.mouth_bevel,
                     align=top)
        return None, hole + mouth

    def _solids_b(self) -> tuple[Any, Any]:
        from build123d import Align, Axis, Cylinder, chamfer

        peg = Cylinder(self.peg_radius, self.length, align=(Align.CENTER, Align.CENTER, Align.MAX))
        tip_edge = peg.edges().sort_by(Axis.Z)[0]
        return chamfer(tip_edge, self.tip_chamfer), None

    def _make_ports(self) -> dict[str, Port]:
        from build123d import Align, Cylinder, Location

        room = Cylinder(self.hole_radius, self.hole_depth,
                        align=(Align.CENTER, Align.CENTER, Align.MAX))
        return {
            "hole": Port("hole", Location(), (0.0, 0.0, -1.0), room,
                         "the peg enters along -Z; the host needs material around the hole "
                         f"down to {self.hole_depth:.2f} mm"),
            "peg": Port("peg", Location(), (0.0, 0.0, -1.0), None,
                        "the peg hangs along -Z from the host's face at z = 0"),
        }

    def _hosts(self) -> tuple[Any, Any]:
        from build123d import Align, Box

        side = self.diameter + 8.0
        thickness = self.length if self.through else self.hole_depth + 2.0
        block = Box(side, side, thickness, align=(Align.CENTER, Align.CENTER, Align.MAX))
        plate = Box(side, side, 2.0, align=(Align.CENTER, Align.CENTER, Align.MIN))
        return block, plate

    def _gauge_card(self) -> Card:
        return PegHole(self.diameter, self.length, self.fit, ctx=self.resolver,
                       through=self.through, name=self.name, _shared=self._resolved(),
                       _declare=False)
