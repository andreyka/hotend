"""A hull-shaped pocket is as wide as `pocket.radius` says, and so is its way out.

A round holder whose inside is `pocket.radius` holds the room of a hull-shaped pocket.
The room moved along the way out must fit the same holder, however far it is moved (a
lid lifted far above the holder makes the move longer); it used to come out wider above
the thing's widest level, and such a holder was reported as trapping the thing."""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import pytest
from build123d import Align, Cylinder, Pos
from fit_helpers import context

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reference_stand_in import stand_in_mesh

from hotend_fit import fit
from hotend_fit.reference import Reference, verify_pocket

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)


def pocket_for(shape: str = "hull", pleats: int = 7):
    vertices, triangles = stand_in_mesh(radius=40.0, height=58.0, pleats=pleats)
    figure = Reference("figure", vertices, triangles, kind="soft")
    return figure.pocket("cradle", ctx=fit.resolver_for(context()), shape=shape,
                         removable=("lid",))


def radius_of(solid) -> float:
    points = np.array([(v.X, v.Y) for v in solid.vertices()])
    return float(np.hypot(points[:, 0], points[:, 1]).max())


@pytest.mark.parametrize("sweep", [1.0, 20.0, 65.0, 150.0])
def test_the_swept_hull_room_is_no_wider_than_the_room(sweep):
    pocket = pocket_for()
    still, moved = pocket.room(), pocket.room(sweep=sweep)
    assert moved.is_valid
    assert radius_of(moved) == pytest.approx(radius_of(still), abs=1e-6)
    assert radius_of(moved) <= pocket.radius + 1e-6
    # It is the room moved straight along the way out: as tall as the room plus the move.
    assert moved.bounding_box().max.Z == pytest.approx(still.bounding_box().max.Z + sweep, abs=1e-6)
    assert moved.bounding_box().min.Z == pytest.approx(still.bounding_box().min.Z, abs=1e-6)
    assert moved.volume > still.volume


@pytest.mark.parametrize("lift", [0.0, 45.0, 120.0])
def test_a_round_holder_of_the_pockets_radius_does_not_trap_a_hull_shaped_room(lift):
    pocket = pocket_for()
    floor, wall = 2.0, 2.4
    height = floor + 0.7 * pocket.depth
    holder = (Cylinder(pocket.radius + wall, height, align=BOTTOM)
              - Pos(0, 0, floor) * Cylinder(pocket.radius, height, align=BOTTOM))
    # A lid far above the holder: it lengthens the way out the check sweeps.
    lid = Pos(0, 0, floor + pocket.size[2] + lift) * Cylinder(pocket.radius + wall, 2.0, align=BOTTOM)
    pocket.place(Pos(0, 0, floor))
    report = verify_pocket(pocket, [("holder", holder), ("lid", lid)])
    assert report.status == "pass", report.message
    assert report.data["clearance_proven"] and report.data["path_clear"]


def test_a_rim_over_a_hull_shaped_room_still_traps_the_thing():
    pocket = pocket_for()
    floor, wall = 2.0, 2.4
    height = floor + pocket.size[2] + 0.5
    holder = (Cylinder(pocket.radius + wall, height, align=BOTTOM)
              - Pos(0, 0, floor) * Cylinder(pocket.radius, height, align=BOTTOM))
    rim = Pos(0, 0, height) * (Cylinder(pocket.radius + wall, 2.0, align=BOTTOM)
                               - Cylinder(pocket.radius - 8.0, 2.0, align=BOTTOM))
    lid = Pos(0, 0, height + 10.0) * Cylinder(pocket.radius + wall, 2.0, align=BOTTOM)
    pocket.place(Pos(0, 0, floor))
    report = verify_pocket(pocket, [("holder", holder + rim), ("lid", lid)])
    assert report.status == "fail" and "trapped" in report.message
    assert report.data["clearance_proven"] and not report.data["path_clear"]


def test_cutting_a_hull_shaped_room_through_a_tall_block_leaves_the_way_out_open():
    pocket = pocket_for()
    floor = 2.0
    block = Cylinder(pocket.radius + 3.0, floor + 2.5 * pocket.depth, align=BOTTOM)
    lid = Pos(0, 0, floor + 3 * pocket.depth) * Cylinder(pocket.radius + 3.0, 2.0, align=BOTTOM)
    holder = pocket.cut(block, at=Pos(0, 0, floor))
    report = verify_pocket(pocket, [("holder", holder), ("lid", lid)])
    assert report.status == "pass", report.message


@pytest.mark.parametrize("pleats", [0, 7])
def test_a_round_cavity_of_the_pockets_radius_holds_a_hull_shaped_room(pleats):
    # pleats = 0 is a perfectly round dome: the planes that describe its hull used to
    # meet outside the circle around it, and the holder was reported as too tight.
    pocket = pocket_for(pleats=pleats)
    cx, cy, _ = pocket.center
    corners = np.array([(v.X - cx, v.Y - cy) for v in pocket.room().vertices()])
    assert float(np.hypot(corners[:, 0], corners[:, 1]).max()) <= pocket.radius + 0.005
    floor, wall = 2.0, 2.4
    height = floor + 0.7 * pocket.depth
    holder = Pos(cx, cy, 0) * (Cylinder(pocket.radius + wall, height, align=BOTTOM)
                               - Pos(0, 0, floor) * Cylinder(pocket.radius, height, align=BOTTOM))
    lid = Pos(cx, cy, floor + pocket.size[2] + 30.0) * Cylinder(pocket.radius, 2.0, align=BOTTOM)
    pocket.place(Pos(0, 0, floor))
    report = verify_pocket(pocket, [("holder", holder), ("lid", lid)])
    assert report.status == "pass", report.message
    assert report.data["intrusions"] == [] and report.data["blocked"] == []
