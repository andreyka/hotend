"""A simple parametric open box (a tray): one part, three sliders."""

from build123d import *

PARAMS = {
    "width": {"default": 80.0, "min": 40.0, "max": 200.0, "unit": "mm",
              "label": "Width", "provenance": "style"},
    "depth": {"default": 60.0, "min": 30.0, "max": 200.0, "unit": "mm",
              "label": "Depth", "provenance": "style"},
    "height": {"default": 40.0, "min": 10.0, "max": 150.0, "unit": "mm",
               "label": "Height", "provenance": "style"},
    "wall": {"default": 2.0, "min": 1.2, "max": 4.0, "unit": "mm",
             "label": "Wall thickness", "provenance": "guessed"},
}


def build(p, ctx):
    print(f"box {p['width']} x {p['depth']} x {p['height']}")
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    outer = Box(p["width"], p["depth"], p["height"], align=bottom)
    cavity = Pos(0, 0, p["wall"]) * Box(
        p["width"] - 2 * p["wall"], p["depth"] - 2 * p["wall"], p["height"], align=bottom
    )
    return outer - cavity
