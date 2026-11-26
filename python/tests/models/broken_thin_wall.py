"""Broken on purpose: an open box whose walls are 0.3 mm, thinner than can be printed."""

from build123d import *

PARAMS = {"wall": {"default": 0.3, "min": 0.1, "max": 4.0, "provenance": "guessed"}}


def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    outer = Box(40, 30, 20, align=bottom)
    cavity = Pos(0, 0, 2) * Box(40 - 2 * p["wall"], 30 - 2 * p["wall"], 20, align=bottom)
    return {"cup": outer - cavity}
