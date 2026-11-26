"""A tray with a planted fault for the range sampler's tests: a wide and low tray gets a
floor far thinner than any printable wall, so min_wall fails for width > 80 and height < 20."""

from build123d import *

PARAMS = {
    "width": {"default": 60.0, "min": 20.0, "max": 100.0, "unit": "mm",
              "label": "Width", "provenance": "style"},
    "height": {"default": 30.0, "min": 10.0, "max": 60.0, "unit": "mm",
               "label": "Height", "provenance": "style"},
    "wall": {"default": 2.0, "unit": "mm", "label": "Wall", "provenance": "style"},
}


def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    floor = 0.3 if p.width > 80 and p.height < 20 else p.wall
    outer = Box(p.width, 30.0, p.height, align=bottom)
    cavity = Pos(0, 0, floor) * Box(p.width - 2 * p.wall, 30.0 - 2 * p.wall, p.height, align=bottom)
    return outer - cavity
