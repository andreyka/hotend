"""A plate pegged onto a base, where the model then widens the hole by hand: the peg and
hole come from a card, but the bore that follows types its own clearance. fit_pairs must
fail it and say what the clearance is now."""

from build123d import *
from hotend_fit import cards

PARAMS = {
    "extra": {"default": 0.1, "min": 0.0, "max": 0.5, "unit": "mm",
              "label": "Hand-typed extra clearance", "provenance": "guessed"},
}


def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    base = Box(30, 30, 10, align=bottom)
    cover = Pos(0, 0, 10) * Box(30, 30, 3, align=bottom)
    joint = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=ctx)
    base = joint.apply_a(base, at=Pos(0, 0, 10))
    cover = joint.apply_b(cover, at=Pos(0, 0, 10))
    if p.extra > 0:
        base -= Pos(0, 0, 4) * Cylinder(joint.hole_radius + p.extra, 6.0, align=bottom)
    return {"base": base, "cover": cover}
