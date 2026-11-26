"""A flat plank whose length and width can pass the edge of the bed: for the tests of what
fits the nozzle's reach as drawn, standing another way up, or not at all."""

from build123d import *

PARAMS = {
    "length": {"default": 254.0, "min": 100.0, "max": 300.0, "unit": "mm",
               "label": "Length", "provenance": "style"},
    "width": {"default": 20.0, "min": 10.0, "max": 300.0, "unit": "mm",
              "label": "Width", "provenance": "style"},
}


def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    return {"plank": Box(p["length"], p["width"], 5.0, align=bottom)}
