"""Broken on purpose: PARAMS has an unknown key and a provenance that does not exist."""

from build123d import *

PARAMS = {
    "size": {"default": 20.0, "minimum": 5.0, "max": 50.0, "provenance": "invented"},
}


def build(p, ctx):
    return Box(p["size"], p["size"], p["size"])
