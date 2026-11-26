"""Broken on purpose: a syntax error on line 9."""

from build123d import *

PARAMS = {"size": {"default": 20.0, "min": 5.0, "max": 50.0, "provenance": "style"}}


def build(p, ctx):
    return Box(p["size"], p["size"], p["size"]
