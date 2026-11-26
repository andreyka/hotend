"""Broken on purpose: build() returns a flat sketch instead of a solid."""

from build123d import *

PARAMS = {"size": {"default": 20.0, "min": 5.0, "max": 50.0, "provenance": "style"}}


def build(p, ctx):
    return {"plate": Rectangle(p["size"], p["size"])}
