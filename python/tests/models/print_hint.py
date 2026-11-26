"""Three parts that say how they want to be printed (PRINT), and a contract to measure.

- `post` is taller than wide and would be laid down; it asks to stand, which can print.
- `dome` asks to be printed on its round side, which cannot print: nothing flat on the bed.
- `tray` is an open tray that says nothing; its contract is measured on it.
"""

from build123d import *

PARAMS = {
    "height": {"default": 60.0, "min": 30.0, "max": 100.0, "provenance": "style"},
    "inside": {"default": 40.0, "min": 30.0, "max": 80.0, "provenance": "measured"},
    "wall": {"default": 2.0, "min": 1.2, "max": 4.0, "provenance": "guessed"},
}

PRINT = {
    "post": {"up": "+z"},
    "dome": {"up": [0, 0, -1]},
}

ASSEMBLY = {"dome": {"mate": "post.top"}}

CONTRACT = {
    "protocol": 0,
    "name": "Tray with a post",
    "parts": ["post", "dome", "tray"],
    "requirements": [
        {"id": "card_fits", "kind": "cavity_min", "part": "tray",
         "size": [40.0, 40.0, 8.0], "provenance": "measured", "params": ["inside"]},
        {"id": "brick_fits", "kind": "cavity_min", "part": "tray",
         "size": [45.0, 40.0, 8.0], "provenance": "measured"},
        {"id": "fits_the_shelf", "kind": "outer_max", "part": "tray",
         "size": [50.0, 50.0, 12.0], "provenance": "measured"},
        {"id": "fits_the_slot", "kind": "outer_max", "part": "post",
         "size": [25.0, 25.0, 40.0], "provenance": "standard"},
        {"id": "tray_walls", "kind": "wall_min", "part": "tray", "value": 1.2,
         "provenance": "style", "params": ["wall"]},
        {"id": "post_reaches", "kind": "dimension", "part": "post", "name": "post height",
         "value": 60.0, "provenance": "style"},
        {"id": "dome_clicks_on", "kind": "fit", "card": "snap_lid",
         "between": ["post", "dome"], "fit_class": "snap", "provenance": "standard"},
    ],
}


def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    post = Box(20, 20, p.height, align=bottom)
    dome = Pos(60, 0, 0) * (Sphere(15) & Box(40, 40, 15, align=bottom))
    outer = p.inside + 2 * p.wall
    tray = Pos(0, 70, 0) * (
        Box(outer, outer, 10, align=bottom)
        - Pos(0, 0, 2) * Box(p.inside, p.inside, 10, align=bottom)
    )
    return {"post": post, "dome": dome, "tray": tray}
