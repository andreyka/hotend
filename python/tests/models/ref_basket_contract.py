"""The basket of ref_basket.py with a contract that states the fit to the reference, and a
lid pegged on with a fit the script tags `calibrated`. On a printer nobody calibrated both
rest on guesses, and the result must say so by itself."""

from build123d import *
from hotend_fit import cards

PARAMS = {
    "rim": {"default": 0.0, "min": 0.0, "max": 12.0, "unit": "mm",
            "label": "Rim over the opening", "provenance": "style"},
}

CONTRACT = {
    "protocol": 0,
    "name": "Basket for a figure",
    "parts": ["basket", "lid"],
    "requirements": [
        {"id": "figure_drops_in", "kind": "reference_fit", "reference": "figure",
         "fit_class": "cradle", "provenance": "standard"},
        {"id": "lid_is_pegged", "kind": "fit", "card": "peg_hole", "between": ["basket", "lid"],
         "fit_class": "slide", "provenance": "calibrated"},
        {"id": "no_such_thing", "kind": "reference_fit", "reference": "other",
         "provenance": "measured"},
    ],
}


def build(p, ctx):
    figure = ctx.references["figure"]
    pocket = figure.pocket("cradle", ctx=ctx, shape="cylinder", removable=("lid",))
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    cx, cy, _ = pocket.center
    height = 2.0 + pocket.size[2] + 0.5
    radius = pocket.radius + 8.0
    basket = Pos(cx, cy, 0) * Cylinder(radius, height, align=bottom)
    basket = pocket.cut(basket, at=Pos(0, 0, 2.0))
    if p.rim > 0:
        ring = Cylinder(radius, 2.0, align=bottom) - Cylinder(pocket.radius - p.rim, 2.0,
                                                              align=bottom)
        basket = basket + Pos(cx, cy, height - 2.0) * ring
    lid = Pos(cx, cy, height) * Cylinder(radius, 2.0, align=bottom)
    peg = cards.peg_hole(diameter=4.0, length=4.0, fit="slide", ctx=ctx, name="lid_peg")
    at = Pos(cx + pocket.radius + 4.0, cy, height)
    return {"basket": peg.apply_a(basket, at=at), "lid": peg.apply_b(lid, at=at)}
