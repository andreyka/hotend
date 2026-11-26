"""Desk cable box: a two-part box that hides a power strip, with a lid that snaps on and one
screw that locks it. The end-to-end Proven Fit example: both joints come from feature cards,
so every clearance is resolved from the printer's calibration.

The declared ranges passed the range sampler at both material extremes; what fails outside
them is noted next to each range (tests/fit/test_fit_cable_box.py checks both).
"""

from build123d import *
from hotend_fit import cards

PARAMS = {
    # Inside sizes. From about 253 mm the box no longer fits the X2D's main nozzle; 200 mm
    # also keeps it inside the auxiliary nozzle's reach.
    "width": {"default": 120.0, "min": 60.0, "max": 200.0, "unit": "mm",
              "label": "Inside width", "provenance": "guessed"},
    # Under about 7.5 mm the lid's rails no longer fit the opening. The screw boss stands in
    # a post outside one end wall, so the whole width x depth inside is free.
    "depth": {"default": 60.0, "min": 30.0, "max": 80.0, "unit": "mm",
              "label": "Inside depth", "provenance": "guessed"},
    # Under 20 mm the cable slot cuts away the wall the lid's locating rails bear on.
    "height": {"default": 40.0, "min": 26.0, "max": 80.0, "unit": "mm",
               "label": "Height", "provenance": "style"},
    # At 1.2 mm and below the wall behind the snap groove is thinner than the 0.8 mm minimum
    # (at 1.2 mm only for the tightest printer the calibration allows).
    "wall": {"default": 2.0, "min": 1.6, "max": 3.0, "unit": "mm",
             "label": "Wall thickness", "provenance": "style"},
    "cable": {"default": 12.0, "unit": "mm", "label": "Cable slot", "provenance": "guessed"},
}

CONTRACT = {
    "protocol": 0,
    "name": "Desk cable box",
    "parts": ["box", "lid"],
    "requirements": [
        {"id": "holds_power_strip", "kind": "cavity_min", "part": "box",
         "size": [120.0, 60.0, 36.0], "provenance": "guessed",
         "params": ["width", "depth", "height"]},
        {"id": "cables_pass", "kind": "dimension", "part": "box", "name": "cable slot width",
         "value": 12.0, "provenance": "guessed", "params": ["cable"]},
        {"id": "lid_snaps", "kind": "fit", "card": "snap_lid", "between": ["box", "lid"],
         "fit_class": "snap", "provenance": "guessed"},
        {"id": "lid_screwed", "kind": "fit", "card": "screw_boss", "between": ["box", "lid"],
         "fit_class": "thread_tap", "provenance": "standard"},
        {"id": "walls_print", "kind": "wall_min", "part": "box", "value": 0.8,
         "provenance": "style", "params": ["wall"]},
    ],
}

FLOOR = 2.0
LID = 2.4
CORNER = 3.0
# The screw post stands on the outside of one end wall, near a corner: its centre is this far
# beyond the inside along X, so the boss (6.6 mm across for an M3) stays clear of the inside,
# and this far in from the inside corner along Y, so the post stays within the box's depth.
# The post is POST_SKIN wider than the boss all round.
POST_OUT = 3.5
POST_IN = 3.0
POST_SKIN = 1.2


def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    outer_x, outer_y = p.width + 2 * p.wall, p.depth + 2 * p.wall
    snap = cards.snap_lid(opening=(p.width, p.depth), wall=p.wall, ctx=ctx)
    screw = cards.screw_boss("M3", "self_tap", height=p.height - FLOOR, plate=LID, ctx=ctx)
    post_x, post_y = p.width / 2 + POST_OUT, p.depth / 2 - POST_IN
    post_radius = screw.boss_diameter / 2 + POST_SKIN

    shell = Box(outer_x, outer_y, p.height, align=bottom)
    shell = fillet(shell.edges().filter_by(Axis.Z), radius=CORNER)
    shell += Pos(post_x, post_y, 0) * Cylinder(post_radius, p.height, align=bottom)
    # The inside is cut last, so it is the full width x depth whatever stands round it.
    box = shell - Pos(0, 0, FLOOR) * Box(p.width, p.depth, p.height, align=bottom)
    # One slot through both end walls, low enough to stay clear of the lid's rails.
    box -= Pos(0, 0, FLOOR + 2.0) * Box(outer_x + 2.0, p.cable, p.cable, align=bottom)

    lid = Pos(0, 0, p.height) * Box(outer_x, outer_y, LID, align=bottom)
    lid = fillet(lid.edges().filter_by(Axis.Z), radius=CORNER)
    lid += Pos(post_x, post_y, p.height) * Cylinder(post_radius, LID, align=bottom)

    rim = Pos(0, 0, p.height)
    box = snap.apply_a(box, at=rim)
    lid = snap.apply_b(lid, at=rim)

    # The screw boss in the post, standing on the floor, outside the inside.
    corner = Pos(post_x, post_y, p.height)
    box = screw.apply_a(box, at=corner)
    lid = screw.apply_b(lid, at=corner)
    return {"box": box, "lid": lid}
