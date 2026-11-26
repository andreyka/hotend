"""Broken on purpose: a valid solid that is longer than any nozzle of the X2D can reach."""

from build123d import *

PARAMS = {"length": {"default": 300.0, "min": 260.0, "max": 400.0, "provenance": "style"}}


def build(p, ctx):
    return {"beam": Box(p["length"], 20, 20, align=(Align.CENTER, Align.CENTER, Align.MIN))}
