"""Broken on purpose: the fillet radius is larger than the box allows, so build() raises."""

from build123d import *

PARAMS = {"size": {"default": 20.0, "min": 5.0, "max": 50.0, "provenance": "style"}}


def rounded(block, radius):
    return fillet(block.edges(), radius=radius)


def build(p, ctx):
    block = Box(p["size"], p["size"], p["size"])
    print("about to fillet")
    return rounded(block, radius=p["size"])
