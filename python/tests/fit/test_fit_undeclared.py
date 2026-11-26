"""Joints made without a feature card: faces of two parts that face each other closely and
belong to no declared fit are warned about by `fit_pairs` (`data.kind` = `undeclared`), when
the request asks (`checks.undeclared_joints`)."""

from __future__ import annotations

import math

import pytest
from build123d import Align, Box, Cylinder, Pos
from fit_helpers import check_env, resolver

from hotend_engine.checks.fit_pairs import fit_pairs
from hotend_engine.checks.undeclared import MIN_AREA_MM2, REACH_MM
from hotend_fit import cards

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)


def undeclared(shapes: dict, r=None, **options) -> list:
    options.setdefault("undeclared_joints", True)
    results = fit_pairs(check_env(shapes, r or resolver(), **options))
    return [result for result in results if result.data.get("kind") == "undeclared"]


def lipped(gap: float, lip: float = 4.0):
    """An open box and a lid whose lip goes into the opening with `gap` on every side."""
    box = Box(60, 40, 30, align=BOTTOM) - Pos(0, 0, 2) * Box(56, 36, 30, align=BOTTOM)
    lid = Pos(0, 0, 30) * Box(60, 40, 2, align=BOTTOM) + Pos(0, 0, 30 - lip) * Box(
        56 - 2 * gap, 36 - 2 * gap, lip, align=BOTTOM)
    return {"box": box, "lid": lid}


def test_a_lip_drawn_by_hand_with_a_typed_gap_is_warned_about():
    (found,) = undeclared(lipped(0.3))
    assert found.id == "fit_pairs" and found.status == "warn" and found.part is None
    assert found.data["parts"] == ["box", "lid"] and found.data["reach"] == REACH_MM
    assert found.data["gap"] == pytest.approx(0.3, abs=0.005)
    assert found.data["gap_min"] == pytest.approx(0.3, abs=0.005)
    # The four sides of the lip, 4 mm deep, sampled at 0.5 mm.
    around = 2 * (55.4 + 35.4) * 4.0
    assert found.data["area"] == pytest.approx(around, rel=0.1)
    assert len(found.data["faces"]) == 4
    assert all(not face["contact"] for face in found.data["faces"])
    assert found.message.startswith(
        "'box' and 'lid' face each other about 0.30 mm apart over about ")
    assert "joint drawn by hand with a typed gap" in found.message
    assert "build the joint with a feature card" in found.message
    # A warning, not a failure: the build still stands.
    assert found.severity == "error" and found.status != "fail"


def test_it_is_only_looked_for_when_the_request_asks():
    results = fit_pairs(check_env(lipped(0.3), resolver()))
    assert [result.status for result in results] == ["skipped"]
    assert results[0].message == "Skipped: no declared fits."
    assert undeclared(lipped(0.3), undeclared_joints=False) == []


def test_a_gap_wider_than_the_reach_or_a_small_patch_is_not_a_joint():
    assert undeclared(lipped(REACH_MM + 0.2)) == []
    # A 2 x 2 mm stub next to a wall: 4 mm2 face each other, under the minimum.
    wall = Box(20, 2, 20, align=BOTTOM)
    stub = Pos(0, 2.3, 5) * Box(2, 2, 2, align=BOTTOM)
    assert 2 * 2 < MIN_AREA_MM2
    assert undeclared({"wall": wall, "stub": stub}) == []
    # The same wall with a plate beside it at the same distance is one.
    plate = Pos(0, 2.3, 0) * Box(20, 2, 20, align=BOTTOM)
    (found,) = undeclared({"wall": wall, "plate": plate})
    assert found.data["area"] == pytest.approx(400.0, rel=0.05)


def test_a_lid_that_only_rests_on_the_rim_is_not_a_joint():
    box = Box(60, 40, 30, align=BOTTOM) - Pos(0, 0, 2) * Box(56, 36, 30, align=BOTTOM)
    lid = Pos(0, 0, 30) * Box(60, 40, 2, align=BOTTOM)
    assert undeclared({"box": box, "lid": lid}) == []
    # Nor are parts drawn apart from each other.
    assert undeclared({"box": box, "lid": Pos(100, 0, 0) * lid}) == []


def test_a_tongue_drawn_exactly_as_wide_as_its_slot_is_reported_as_touching():
    base = Box(40, 20, 10, align=BOTTOM) - Pos(0, 0, 5) * Box(10, 20, 5, align=BOTTOM)
    tongue = Pos(0, 0, 5) * Box(10, 20, 12, align=BOTTOM)
    (found,) = undeclared({"base": base, "tongue": tongue})
    assert found.data["gap_max"] == 0.0 and "face each other touching over" in found.message
    assert any(face["contact"] for face in found.data["faces"])


def test_a_pin_in_a_hole_made_by_hand_is_found_on_curved_faces():
    base = Box(30, 30, 10, align=BOTTOM) - Pos(0, 0, 4) * Cylinder(3.2, 6, align=BOTTOM)
    cover = Pos(0, 0, 10) * Box(30, 30, 3, align=BOTTOM) + Pos(0, 0, 4.5) * Cylinder(
        3.0, 5.5, align=BOTTOM)
    (found,) = undeclared({"base": base, "cover": cover})
    # Curved faces are compared on facets: the gap is right within the mesh's deviation.
    assert found.data["gap"] == pytest.approx(0.2, abs=0.1)
    # The wall of the hole around the pin, and its floor 0.5 mm under the pin's end.
    assert found.data["area"] == pytest.approx(
        2 * math.pi * 3.2 * 5.5 + math.pi * 3.0**2, rel=0.05)


def test_a_joint_made_by_a_feature_card_is_not_reported_but_one_beside_it_is():
    r = resolver()
    card = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r)
    base = card.apply_a(Box(40, 30, 10, align=BOTTOM), at=Pos(0, 0, 10))
    cover = card.apply_b(Pos(0, 0, 10) * Box(40, 30, 3, align=BOTTOM), at=Pos(0, 0, 10))
    shapes = {"base": base, "cover": cover}
    results = fit_pairs(check_env(shapes, r, undeclared_joints=True))
    # The card's own pairs are measured as before; nothing else is said.
    assert {result.data["kind"] for result in results} == {"pair", "joint", "robustness"}
    assert not [result for result in results if result.status == "fail"]

    # A second pin, typed by hand 14 mm to the side of the card's peg.
    r2 = resolver()
    card2 = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r2)
    by_hand = {
        "base": card2.apply_a(Box(40, 30, 10, align=BOTTOM), at=Pos(0, 0, 10))
        - Pos(14, 0, 4) * Cylinder(2.25, 6, align=BOTTOM),
        "cover": card2.apply_b(Pos(0, 0, 10) * Box(40, 30, 3, align=BOTTOM), at=Pos(0, 0, 10))
        + Pos(14, 0, 4.5) * Cylinder(2.0, 5.5, align=BOTTOM),
    }
    (found,) = undeclared(by_hand, r2)
    assert found.data["gap"] == pytest.approx(0.25, abs=0.1)
    # Only the hand-made pin: the card's peg and hole (about 150 mm2 more) are left out.
    assert found.data["area"] == pytest.approx(
        2 * math.pi * 2.25 * 5.5 + math.pi * 2.0**2, rel=0.05)


def test_without_a_mesh_it_says_that_it_did_not_look():
    (found,) = undeclared(lipped(0.3), mesh=False)
    assert found.status == "skipped" and found.data["parts"] == ["box", "lid"]
    assert "could not be looked for" in found.message
