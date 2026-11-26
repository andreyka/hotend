"""A box whose CONTRACT is junk: a size that is not a number."""

from build123d import *

PARAMS = {
    "width": {"default": 60.0, "min": 40.0, "max": 120.0, "provenance": "guessed"},
}

CONTRACT = {
    "protocol": 0,
    "name": "Box",
    "parts": ["box"],
    "requirements": [
        {"id": "holds_the_thing", "kind": "cavity_min", "part": "box",
         "size": [50.0, "wide", 20.0], "provenance": "guessed"},
    ],
}


def build(p, ctx):
    return {"box": Box(p.width, 40, 30)}
