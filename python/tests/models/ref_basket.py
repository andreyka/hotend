"""A round basket that holds a reference object (a soft figure) with a cradle fit. With
`rim` above zero a ring narrows the opening, which traps the figure: the insertion check
must fail then."""

from build123d import *

PARAMS = {
    "wall": {"default": 2.0, "min": 1.2, "max": 4.0, "unit": "mm",
             "label": "Wall thickness", "provenance": "guessed"},
    "floor": {"default": 2.0, "min": 1.0, "max": 5.0, "unit": "mm",
              "label": "Floor thickness", "provenance": "guessed"},
    "rim": {"default": 0.0, "min": 0.0, "max": 12.0, "unit": "mm",
            "label": "Rim over the opening", "provenance": "style"},
}


def build(p, ctx):
    figure = ctx.references["figure"]
    pocket = figure.pocket("cradle", ctx=ctx, shape="cylinder")
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    cx, cy, _ = pocket.center
    # Open and low without a rim; with one, the wall rises past the figure's room and
    # the rim closes in above it.
    height = p.floor + (0.6 * pocket.depth if p.rim == 0 else pocket.size[2] + 0.5)
    basket = Pos(cx, cy, 0) * Cylinder(pocket.radius + p.wall, height, align=bottom)
    basket = pocket.cut(basket, at=Pos(0, 0, p.floor))
    if p.rim > 0:
        ring = Cylinder(pocket.radius + p.wall, 2.0, align=bottom) - Cylinder(
            pocket.radius - p.rim, 2.0, align=bottom)
        basket = basket + Pos(cx, cy, height) * ring
    print("figure", [round(v, 2) for v in figure.size], "gap", round(pocket.gap, 3))
    return {"basket": basket}
