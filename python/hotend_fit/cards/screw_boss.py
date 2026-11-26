"""The screw_boss feature card. Its use is documented on the class `ScrewBoss`."""

from __future__ import annotations

from typing import Any

from ..fit import Clearance
from ..verify import CARD, ROUND, Side
from . import standards
from .base import ABOVE_THE_BED, Card, CardError, Port

KINDS = ("self_tap", "heat_set")
# Boss diameter for a thread-forming screw as a multiple of the screw diameter; 2 to 2.5 is
# the usual rule for bosses in thermoplastics.
SELF_TAP_BOSS_RATIO = 2.2
# A self-tapping screw needs at least this many diameters of thread in the boss.
MIN_ENGAGEMENT_RATIO = 1.5
# The through hole is cut this much past both faces of the plate so that no skin is left.
# It is cut into air, not a clearance between printed faces.
THROUGH_OVERSHOOT_MM = 0.5


def _cylinder_face(radius: float, z_low: float, z_high: float) -> Any:
    """The side surface of a cylinder about Z, used as a bought part's reference face."""
    from build123d import Align, Cylinder, GeomType, Pos

    solid = Pos(0, 0, z_low) * Cylinder(
        radius, z_high - z_low, align=(Align.CENTER, Align.CENTER, Align.MIN)
    )
    return solid.faces().filter_by(GeomType.CYLINDER)[0]


class ScrewBoss(Card):
    """screw_boss: a boss that takes a screw, and the hole in the part the screw holds down.

        card = cards.screw_boss(screw="M3", kind="self_tap", height=8.0, plate=2.4, ctx=ctx)
        base = card.apply_a(base, at=Pos(x, y, floor + 8.0))   # the boss, standing on the floor
        lid = card.apply_b(lid, at=Pos(x, y, floor + 8.0))     # the screw's through hole

    Frame: the origin is the centre of the boss's top face, where the held part rests; +Z points
    out of the boss. The boss stands on whatever the host has at z = -height.

    `kind="self_tap"`: the screw cuts its own thread in a pilot hole (fit class `thread_tap`).
    `screw` is a metric machine screw (`M2` to `M10`) or a tapping screw (`ST2.2` to `ST4.8`).
    `kind="heat_set"`: a brass heat-set insert is melted into the boss; the hole is the one the
    insert's maker specifies (bd_warehouse table), compensated for this printer.
    Half B is a through hole of fit class `thread_free` in a part that is `plate` mm thick there.

    The screw and the insert are bought parts: the fit pairs measure the printed holes against
    their standard surfaces.
    """

    kind = "screw_boss"
    half_a = "boss"
    half_b = "plate"

    def __init__(self, screw: str = "M3", kind: str = "self_tap", *, height: float = 8.0,
                 plate: float = 2.4, ctx: Any, insert: str | None = None,
                 maker: str = "CNCKitchen", name: str | None = None,
                 _shared: dict[str, Clearance] | None = None, _declare: bool = True) -> None:
        if kind not in KINDS:
            raise CardError(f"screw_boss kind must be one of {', '.join(KINDS)}, got {kind!r}")
        if not plate > 0 or not height > 0:
            raise CardError("screw_boss height and plate must be positive")
        self.screw = standards.screw(screw)
        diameter = self.screw.diameter
        super().__init__(ctx, name, _declare=_declare)
        self.boss_kind, self.height, self.plate = kind, float(height), float(plate)
        self.maker, self.insert = maker, None
        self.free = self._clearance(_shared, "free", "thread_free", diameter, printed_peg=False)
        self.free_radius = diameter / 2 + self.free.hole_delta
        self.depth_relief: Clearance | None = None
        if kind == "self_tap":
            if height < MIN_ENGAGEMENT_RATIO * diameter:
                raise CardError(
                    f"a boss {height:g} mm tall gives a {screw} screw too little thread; use "
                    f"at least {MIN_ENGAGEMENT_RATIO * diameter:g} mm"
                )
            self.grip = self._clearance(_shared, "grip", "thread_tap", diameter, printed_peg=False)
            self.mate_radius = diameter / 2
            self.hole_radius = self.mate_radius + self.grip.hole_delta
            self.hole_depth = self.height
            self.mate_length = self.height
            self.boss_diameter = SELF_TAP_BOSS_RATIO * diameter
        else:
            if self.screw.thread_forming:
                raise CardError(f"a heat-set insert takes a metric machine screw, not {screw}")
            # The relief below the insert is a loose gap along Z; it is resolved for the
            # shortest insert first so that the choice of insert does not depend on it.
            options = standards.heat_set_inserts(screw, maker)
            if not options:
                raise CardError(f"bd_warehouse lists no {maker} heat-set insert for {screw}")
            self.depth_relief = self._clearance(
                _shared, "depth_relief", "loose", options[0].length, axis="z", printed_peg=False
            )
            spec = standards.heat_set_insert(
                screw, self.height - self.depth_relief.gap, maker, insert
            )
            if spec.length + self.depth_relief.gap > self.height:
                raise CardError(
                    f"insert {spec.size} needs a boss at least "
                    f"{spec.length + self.depth_relief.gap:.2f} mm tall"
                )
            self.insert = spec
            self.grip = self._clearance(
                _shared, "grip", "press", spec.outer_diameter, printed_peg=False,
                datasheet_gap=(spec.hole_diameter - spec.outer_diameter) / 2,
            )
            self.mate_radius = spec.outer_diameter / 2
            self.hole_radius = self.mate_radius + self.grip.hole_delta
            self.hole_depth = spec.length + self.depth_relief.gap
            self.mate_length = spec.length
            self.boss_diameter = spec.outer_diameter + 2 * spec.min_wall
        if self.hole_radius <= 0 or self.hole_radius >= self.boss_diameter / 2:
            raise CardError(f"a {screw} boss cannot be built with this printer's calibration")
        inward = (-1.0, 0.0, 0.0)
        self._pair(
            "thread" if kind == "self_tap" else "insert", self.grip,
            Side((self.hole_radius, 0.0, -self.mate_length / 2), inward, made=CARD),
            Side((self.mate_radius, 0.0, -self.mate_length / 2), (1.0, 0.0, 0.0),
                 reference=_cylinder_face(self.mate_radius, -self.mate_length, 0.0)),
        )
        if self.depth_relief is not None:
            from build123d import Circle, Pos

            self._pair(
                "insert_depth", self.depth_relief,
                Side((0.0, 0.0, -self.hole_depth), (0.0, 0.0, 1.0), made=CARD),
                Side((0.0, 0.0, -self.mate_length), (0.0, 0.0, -1.0),
                     reference=Pos(0, 0, -self.mate_length) * Circle(self.mate_radius).face()),
            )
        self._pair(
            "screw", self.free,
            Side((diameter / 2, 0.0, self.plate / 2), (1.0, 0.0, 0.0),
                 reference=_cylinder_face(diameter / 2, 0.0, self.plate)),
            Side((self.free_radius, 0.0, self.plate / 2), inward, made=ROUND),
        )

    def _resolved(self) -> dict[str, Clearance | None]:
        return {"free": self.free, "grip": self.grip, "depth_relief": self.depth_relief}

    def _args(self) -> dict:
        return {"screw": self.screw.designation, "kind": self.boss_kind, "height": self.height,
                "plate": self.plate, "insert": self.insert.size if self.insert else None,
                "maker": self.maker}

    def gauge_dimensions(self) -> list[dict]:
        hole = "pilot hole" if self.boss_kind == "self_tap" else "hole for the insert"
        return [
            {"id": "boss_hole_diameter", "half": self.half_a, "feature": "hole",
             "drawn": 2 * self.hole_radius,
             "what": f"the inside diameter of the {hole} in the boss" + ABOVE_THE_BED},
            {"id": "screw_hole_diameter", "half": self.half_b, "feature": "hole",
             "drawn": 2 * self.free_radius,
             "what": "the inside diameter of the screw's through hole in the plate"
                     + ABOVE_THE_BED},
        ]

    def _solids_a(self) -> tuple[Any, Any]:
        from build123d import Align, Cylinder

        top = (Align.CENTER, Align.CENTER, Align.MAX)
        boss = Cylinder(self.boss_diameter / 2, self.height, align=top)
        hole = Cylinder(self.hole_radius, self.hole_depth, align=top)
        return boss, hole

    def _solids_b(self) -> tuple[Any, Any]:
        from build123d import Align, Cylinder, Pos

        hole = Pos(0, 0, -THROUGH_OVERSHOOT_MM) * Cylinder(
            self.free_radius, self.plate + 2 * THROUGH_OVERSHOOT_MM,
            align=(Align.CENTER, Align.CENTER, Align.MIN),
        )
        return None, hole

    def _make_ports(self) -> dict[str, Port]:
        from build123d import Align, Cylinder, Location

        head = Cylinder(self.screw.head_diameter / 2 + self.free.gap, self.screw.head_height,
                        align=(Align.CENTER, Align.CENTER, Align.MIN))
        return {
            "boss": Port("boss", Location(), (0.0, 0.0, -1.0), None,
                         f"a boss {self.boss_diameter:.2f} mm across stands on the host at "
                         f"z = -{self.height:g}; the screw arrives along -Z"),
            "plate": Port("plate", Location((0, 0, self.plate)), (0.0, 0.0, -1.0), head,
                          "the screw head rests on the plate's outer face; keep its "
                          "footprint flat and free"),
        }

    def _hosts(self) -> tuple[Any, Any]:
        from build123d import Align, Box, Pos

        side = self.boss_diameter + 6.0
        base = Pos(0, 0, -self.height) * Box(
            side, side, 2.0, align=(Align.CENTER, Align.CENTER, Align.MAX)
        )
        plate = Box(side, side, self.plate, align=(Align.CENTER, Align.CENTER, Align.MIN))
        return base, plate

    def _gauge_card(self) -> Card:
        return ScrewBoss(
            self.screw.designation, self.boss_kind, height=self.height, plate=self.plate,
            ctx=self.resolver, insert=self.insert.size if self.insert else None,
            maker=self.maker, name=self.name, _shared=self._resolved(), _declare=False,
        )
