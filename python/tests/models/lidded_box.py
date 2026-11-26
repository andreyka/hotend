"""A two-part box: an open body and a plain lid with a locating lip, shown in place."""

from build123d import *

PARAMS = {
    "width": {"default": 90.0, "min": 40.0, "max": 220.0, "unit": "mm",
              "label": "Width", "provenance": "measured"},
    "depth": {"default": 60.0, "min": 30.0, "max": 220.0, "unit": "mm",
              "label": "Depth", "provenance": "measured"},
    "height": {"default": 35.0, "min": 15.0, "max": 150.0, "unit": "mm",
               "label": "Height", "provenance": "style"},
    "wall": {"default": 2.0, "min": 1.2, "max": 4.0, "unit": "mm",
             "label": "Wall thickness", "provenance": "guessed"},
    "gap": {"default": 0.3, "min": 0.1, "max": 0.8, "unit": "mm",
            "label": "Lid clearance", "provenance": "guessed"},
}


def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    outer = Box(p.width, p.depth, p.height, align=bottom)
    outer = fillet(outer.edges().filter_by(Axis.Z), radius=3 * p.wall)
    cavity = Pos(0, 0, p.wall) * Box(
        p.width - 2 * p.wall, p.depth - 2 * p.wall, p.height, align=bottom
    )
    cavity = fillet(cavity.edges().filter_by(Axis.Z), radius=2 * p.wall)
    box = outer - cavity

    plate = Box(p.width, p.depth, p.wall, align=bottom)
    plate = fillet(plate.edges().filter_by(Axis.Z), radius=3 * p.wall)
    lip_w = p.width - 2 * p.wall - 2 * p.gap
    lip_d = p.depth - 2 * p.wall - 2 * p.gap
    lip = Pos(0, 0, -p.wall) * Box(lip_w, lip_d, p.wall, align=bottom)
    lip = fillet(lip.edges().filter_by(Axis.Z), radius=2 * p.wall - p.gap)
    lip -= Pos(0, 0, -p.wall) * Box(lip_w - 2 * p.wall, lip_d - 2 * p.wall, p.wall, align=bottom)
    lid = Pos(0, 0, p.height) * (plate + lip)
    print("lid lip", round(lip_w, 2), "x", round(lip_d, 2))
    return {"box": box, "lid": lid}
