"""Broken on purpose: the size depends on a random number, so two builds differ."""

import random

from build123d import *

PARAMS = {"size": {"default": 20.0, "min": 5.0, "max": 50.0, "provenance": "style"}}


def build(p, ctx):
    return Box(p["size"] + random.random(), p["size"], p["size"])
