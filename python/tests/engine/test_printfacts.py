"""Print facts: what the engine measures on each piece for the print settings rules
(hotend_engine.printfacts).

The first half measures shapes in this process: the contact with the bed (a thin ring, a
slender peg, a bore), mating faces that stand on the bed, and the overhang regions with
their kind (a slot ceiling, a cross hole, a bottom fillet, a ledge). The second half reads
worker jobs: the facts in the `gauge` and `prepare` results, the `orient` stage that writes
no file, and requests from before the feature, which are answered as before.

The names and tokens are the ones of `crates/hotend-settings/src/facts.rs`; a test reads
that file so that the two sides cannot drift apart. `data/ring-gauge-facts.json` and
`data/overhang-kinds-facts.json` are what the engine answers for two known pieces, in the
shape the rules read (`PlateFacts::from_json`).
"""

from __future__ import annotations

import copy
import json
import math
import re
import sys
from pathlib import Path

import numpy as np
import pytest
from build123d import (
    Align,
    Axis,
    Box,
    Cylinder,
    Face,
    GeomType,
    Pos,
    Rot,
    Shell,
    Solid,
    Sphere,
    Vector,
    Wire,
    chamfer,
    fillet,
)
from conftest import SMALL_VIEW, X2D_CONTEXT, ok, prepare_job, run_all

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reference_stand_in import write_stl

from hotend_engine import prepare, printfacts
from hotend_engine.parts import NamedPart
from hotend_engine.request import Material
from hotend_engine.tessellate import tessellate

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)
DATA = Path(__file__).resolve().with_name("data")
FACTS_RS = (Path(__file__).resolve().parents[3] / "crates" / "hotend-settings" / "src"
            / "facts.rs")
PIECE_KEYS = [
    "contact_mm2", "contact_perimeter_mm", "strip_width_mm", "contact_span_mm", "height_mm",
    "slenderness", "overhang_mm2", "fit_overhang_mm2", "overhang_over_part_mm2",
    "overhang_regions", "inner_loops", "fit_at_bed", "min_wall_mm",
]
REGION_KEYS = ["area_mm2", "kind", "span_mm", "diameter_mm", "reach_mm", "over_part"]
PLATE_KEYS = ["family", "piece_count", "total_contact_mm2", "pieces"]
# The length each kind of region states; the other two are null.
LENGTH_OF = {"bridge": "span_mm", "hole_ceiling": "diameter_mm", "free": "reach_mm"}


def facts(shape, up=(0.0, 0.0, 1.0), fit=None, mesh: bool = True, limit: float = 45.0) -> dict:
    part = NamedPart("p", shape, "#4c78a8")
    meshed = tessellate([part]).meshes["p"] if mesh else None
    return printfacts.piece_facts(prepare.PartGeometry(part, meshed), meshed, up, limit, fit)


def face_index(shape, kind: GeomType, radius: float | None = None) -> int:
    """The index of the one face of this kind (and radius) in `shape.faces()`."""
    found = [index for index, face in enumerate(shape.faces())
             if face.geom_type == kind and (radius is None or face.radius == pytest.approx(radius))]
    assert len(found) == 1, found
    return found[0]


def mating(shape, pair: str, kind: GeomType = GeomType.CYLINDER, radius: float | None = None):
    return prepare.PartFit(faces={face_index(shape, kind, radius): pair})


def assert_well_formed(piece: dict) -> None:
    """The shape the rules read: these keys in this order, numbers finite and not negative,
    regions largest first and summing to the overhang, each with the length of its kind."""
    assert list(piece) == PIECE_KEYS
    for key in PIECE_KEYS[:9]:
        assert isinstance(piece[key], float | int) and math.isfinite(piece[key]), key
        assert piece[key] >= 0.0, key
    wall = piece["min_wall_mm"]
    assert wall is None or (isinstance(wall, float) and 0.0 <= wall <= 1000.0)
    assert piece["slenderness"] <= 1e4 and piece["strip_width_mm"] <= 1000.0
    assert piece["overhang_over_part_mm2"] <= piece["overhang_mm2"] + 1e-6
    assert piece["fit_overhang_mm2"] <= piece["overhang_mm2"] + 1e-6
    # What the rules crate refuses as facts that contradict each other (`consistent` of
    # facts.rs): no outline is shorter than a circle's, no strip wider than that circle's
    # radius, no contact larger than the square on its span, no piece without a height.
    contact = piece["contact_mm2"]
    assert piece["contact_perimeter_mm"] >= 2.0 * math.sqrt(math.pi * contact) - 1e-6
    assert piece["strip_width_mm"] <= math.sqrt(contact / math.pi) + 1e-6
    assert piece["contact_span_mm"] >= math.sqrt(contact) - 1e-6
    assert contact <= 1e-6 or piece["height_mm"] > 0.0
    # The careful side of the two facts the rules cross-check: never a strip wider than
    # the piece's own ratio, never less slender than the height over the root of the contact.
    if piece["contact_perimeter_mm"] > 0.0:
        assert piece["strip_width_mm"] <= 2.0 * contact / piece["contact_perimeter_mm"] + 1e-6
    if contact > 1e-6:
        assert piece["slenderness"] >= min(piece["height_mm"] / math.sqrt(contact), 1e4) - 1e-5
    assert len(piece["inner_loops"]) <= 64 and len(piece["fit_at_bed"]) <= 64
    for loop in piece["inner_loops"]:
        assert list(loop) == ["min_width_mm", "fit"] and isinstance(loop["fit"], bool)
        assert 0.0 < loop["min_width_mm"] <= 1000.0
    for entry in piece["fit_at_bed"]:
        assert list(entry) == ["pair", "side"] and entry["side"] in ("inside", "outside")
        assert re.fullmatch(r"[a-z][a-z0-9_.-]{0,63}", entry["pair"])
    regions = piece["overhang_regions"]
    if regions is None:
        return
    assert len(regions) <= 64
    assert [r["area_mm2"] for r in regions] == sorted((r["area_mm2"] for r in regions),
                                                      reverse=True)
    assert sum(r["area_mm2"] for r in regions) == pytest.approx(
        piece["overhang_mm2"], rel=0.01, abs=0.01)
    for region in regions:
        assert list(region) == REGION_KEYS and isinstance(region["over_part"], bool)
        assert region["area_mm2"] > 0.0 and region["kind"] in LENGTH_OF
        for kind, key in LENGTH_OF.items():
            if kind == region["kind"]:
                assert 0.0 <= region[key] <= 1000.0
            else:
                assert region[key] is None


def one_region(piece: dict) -> dict:
    assert_well_formed(piece)
    (region,) = piece["overhang_regions"]
    return region


# --- the contact with the bed -----------------------------------------------------------------


def ring(outer: float = 74.0, inner: float = 70.0, height: float = 5.0):
    return Cylinder(outer / 2, height, align=BOTTOM) - Cylinder(inner / 2, height, align=BOTTOM)


def test_a_thin_ring_has_a_strip_width_of_its_wall():
    # Circles of 74 and 70 mm, a wall of 2 mm.
    piece = facts(ring())
    assert_well_formed(piece)
    assert piece["strip_width_mm"] == pytest.approx(2.0, rel=0.01)
    assert piece["contact_mm2"] == pytest.approx(math.pi * (37.0 ** 2 - 35.0 ** 2), rel=1e-6)
    # Both circles count: the inner loop is part of the perimeter.
    assert piece["contact_perimeter_mm"] == pytest.approx(math.pi * (74.0 + 70.0), rel=1e-6)
    assert piece["contact_span_mm"] == pytest.approx(74.0, abs=1e-3)
    assert piece["height_mm"] == pytest.approx(5.0, abs=1e-3)
    assert piece["inner_loops"] == [{"min_width_mm": 70.0, "fit": False}]
    assert piece["overhang_mm2"] == 0.0 and piece["overhang_regions"] == []
    assert piece["fit_at_bed"] == []


def test_the_thinnest_region_decides_the_strip_width():
    # A wide foot and a thin fin, joined above the bed: two contact regions.
    foot = Box(30, 30, 10, align=BOTTOM)
    fin = Pos(40, 0, 0) * Box(1.6, 30, 10, align=BOTTOM)
    bridge = Pos(20, 0, 6) * Box(40, 30, 4, align=BOTTOM)
    piece = facts(foot + fin + bridge)
    assert_well_formed(piece)
    assert piece["contact_mm2"] == pytest.approx(900.0 + 48.0)
    assert piece["contact_perimeter_mm"] == pytest.approx(120.0 + 63.2)
    # 2 x 48 / 63.2 for the fin; the foot alone would give 15.
    assert piece["strip_width_mm"] == pytest.approx(2 * 48.0 / 63.2, rel=1e-6)
    assert piece["contact_span_mm"] == pytest.approx(55.8, abs=1e-3)


def test_bed_faces_that_share_an_edge_are_one_region():
    # A slab of 40 x 20 x 5 whose bottom is two faces that meet along its middle.
    def face(*corners):
        return Face(Wire.make_polygon([Vector(*corner) for corner in corners], close=True))

    slab = Solid(Shell([
        face((-20, -10, 0), (-20, 10, 0), (0, 10, 0), (0, -10, 0)),
        face((0, -10, 0), (0, 10, 0), (20, 10, 0), (20, -10, 0)),
        face((-20, -10, 5), (20, -10, 5), (20, 10, 5), (-20, 10, 5)),
        face((-20, -10, 0), (0, -10, 0), (20, -10, 0), (20, -10, 5), (-20, -10, 5)),
        face((20, 10, 0), (0, 10, 0), (-20, 10, 0), (-20, 10, 5), (20, 10, 5)),
        face((-20, 10, 0), (-20, -10, 0), (-20, -10, 5), (-20, 10, 5)),
        face((20, -10, 0), (20, 10, 0), (20, 10, 5), (20, -10, 5)),
    ]))
    assert slab.is_valid and slab.volume == pytest.approx(4000.0)
    assert sum(1 for f in slab.faces() if f.normal_at(f.center()).Z < -0.9) == 2
    piece = facts(slab)
    assert_well_formed(piece)
    assert piece["contact_mm2"] == pytest.approx(800.0)
    # The edge the two faces share is inside the region, not part of its outline.
    assert piece["contact_perimeter_mm"] == pytest.approx(120.0)
    assert piece["strip_width_mm"] == pytest.approx(2 * 800.0 / 120.0)
    assert piece["inner_loops"] == []


def test_a_thin_wall_that_shares_its_region_with_a_wide_face_is_still_thin():
    # A thin ring with a pad or a handle grown onto it: one contact
    # region, and its `2 x area / perimeter` reads like a compact foot. The wall is as
    # thin as before, and that is what the brim rule must hear.
    alone = facts(ring())
    assert alone["strip_width_mm"] == pytest.approx(2.0, rel=0.01)
    pad = ring() + Pos(37 + 28, 0, 0) * Box(60, 60, 5, align=BOTTOM)
    handle = ring() + Pos(37 + 58, 0, 0) * Box(120, 30, 5, align=BOTTOM)
    fin = Box(40, 40, 10, align=BOTTOM) + Pos(20 + 75, 0, 0) * Box(150, 2, 10, align=BOTTOM)
    for shape, ratio in ((pad, 12.49), (handle, 11.42), (fin, 8.26)):
        piece = facts(shape)
        assert_well_formed(piece)
        # The ratio of the whole region is what the perimeter and the area still give.
        assert 2 * piece["contact_mm2"] / piece["contact_perimeter_mm"] == pytest.approx(
            ratio, abs=0.01)
        assert piece["strip_width_mm"] == pytest.approx(2.0, rel=0.01)
    # A wall of 3.5 mm on the same pad is not thin: the number is the wall, not a flag.
    thick = ring(74.0, 67.0) + Pos(37 + 28, 0, 0) * Box(60, 60, 5, align=BOTTOM)
    assert facts(thick)["strip_width_mm"] == pytest.approx(3.5, rel=0.01)


def test_the_local_width_never_reads_wider_than_the_ratio_and_ignores_short_necks():
    # Compact feet keep the specified measure exactly.
    assert facts(Box(20, 20, 5, align=BOTTOM))["strip_width_mm"] == pytest.approx(10.0)
    assert facts(Cylinder(10, 5, align=BOTTOM))["strip_width_mm"] == pytest.approx(10.0)
    assert facts(Box(40, 4, 5, align=BOTTOM))["strip_width_mm"] == pytest.approx(
        2 * 160.0 / 88.0)
    # A stub of 2 x 6 mm on a block is no wall to lift: under 10 mm of run it is not
    # counted. A fin of 2 x 12 mm is.
    block = Box(40, 40, 5, align=BOTTOM)
    stub = block + Pos(23, 0, 0) * Box(6, 2, 5, align=BOTTOM)
    assert facts(stub)["strip_width_mm"] == pytest.approx(
        2 * 1612.0 / 172.0, rel=1e-6)
    fin = block + Pos(26, 0, 0) * Box(12, 2, 5, align=BOTTOM)
    assert facts(fin)["strip_width_mm"] == pytest.approx(2.0, rel=0.01)
    # A hole 1 mm from an edge is a short neck, not a wall; the corner of a wedge of
    # 40 degrees is no wall either (its sides are not the two faces of one).
    holed = Box(40, 40, 5, align=BOTTOM) - Pos(15, 0, 0) * Cylinder(4.0, 5, align=BOTTOM)
    piece = facts(holed)
    assert piece["strip_width_mm"] == pytest.approx(
        2 * piece["contact_mm2"] / piece["contact_perimeter_mm"], rel=1e-6)
    # Short necks that lie apart do not add up to a wall: a row of holes 2 mm from the
    # edge of a plate leaves the plate as wide as its ratio says.
    row = Box(100, 40, 3, align=BOTTOM) - [
        Pos(-40 + 10 * i, 15, 0) * Cylinder(3.0, 3, align=BOTTOM) for i in range(9)]
    piece = facts(row)
    assert piece["strip_width_mm"] == pytest.approx(
        2 * piece["contact_mm2"] / piece["contact_perimeter_mm"], rel=1e-6)
    assert piece["strip_width_mm"] > 3.0
    # A wall drawn as several short edges end to end is one wall.
    bent = Box(40, 40, 5, align=BOTTOM)
    for i in range(4):
        bent += Pos(22 + 4 * i, 0, 0) * Box(4, 2, 5, align=BOTTOM)
    assert facts(bent)["strip_width_mm"] == pytest.approx(2.0, rel=0.01)
    # Turned on the bed, the wall is as thin: nothing depends on the bed's axes.
    askew = Rot(0, 0, 33) * (ring() + Pos(37 + 28, 0, 0) * Box(60, 60, 5, align=BOTTOM))
    assert facts(askew)["strip_width_mm"] == pytest.approx(2.0, rel=0.01)


def test_a_thin_standing_wall_is_slender_against_its_narrow_footing():
    # 60 x 2 mm on the bed and 40 mm tall: 20 times as tall as its footing is wide,
    # although `height / sqrt(contact)` reads 3.65.
    wall = facts(Box(60, 2, 40, align=BOTTOM))
    assert_well_formed(wall)
    assert 40.0 / math.sqrt(wall["contact_mm2"]) == pytest.approx(3.651, abs=1e-3)
    assert wall["slenderness"] == pytest.approx(20.0, rel=1e-3)
    # Askew to the bed's axes it is the same wall.
    assert facts(Rot(0, 0, 27) * Box(60, 2, 40, align=BOTTOM))["slenderness"] == pytest.approx(
        20.0, rel=1e-3)
    # A square or round foot keeps the specified measure; so does a wall that is braced:
    # an L of two such walls stands on a footing 40 mm wide.
    assert facts(Box(12, 12, 150, align=BOTTOM))["slenderness"] == pytest.approx(12.5)
    assert facts(Cylinder(3.0, 60.0, align=BOTTOM))["slenderness"] == pytest.approx(
        60.0 / math.sqrt(math.pi * 9.0), rel=1e-4)
    braced = Box(60, 2, 40, align=BOTTOM) + Pos(-29, 29, 0) * Box(2, 60, 40, align=BOTTOM)
    piece = facts(braced)
    assert piece["slenderness"] == pytest.approx(40.0 / math.sqrt(piece["contact_mm2"]),
                                                 rel=1e-6)
    # Feet far apart carry the piece between them: a ring is not a slender thing.
    assert facts(ring(height=30.0))["slenderness"] == pytest.approx(
        30.0 / math.sqrt(math.pi * (37.0 ** 2 - 35.0 ** 2)), rel=1e-4)


def test_the_thinnest_wall_is_measured_when_the_piece_is_a_closed_solid():
    # What the wall rule (3 walls for a wall of 2.4 mm or less) reads for walls that do
    # not stand on the bed: an open box with 2 mm walls on a floor.
    box = Box(80, 60, 40, align=BOTTOM) - Pos(0, 0, 2) * Box(76, 56, 40, align=BOTTOM)
    piece = facts(box)
    assert_well_formed(piece)
    assert piece["min_wall_mm"] == pytest.approx(2.0, abs=0.01)
    assert piece["strip_width_mm"] > 20.0  # its foot is the whole floor
    assert facts(Box(20, 20, 20, align=BOTTOM))["min_wall_mm"] == pytest.approx(20.0, abs=0.01)
    assert facts(ring())["min_wall_mm"] == pytest.approx(2.0, rel=0.03)
    # The measure does not depend on how the piece is turned.
    assert facts(box, up=(0, 0, -1))["min_wall_mm"] == piece["min_wall_mm"]
    # Without a mesh nothing can be sampled, and the fact says so.
    assert facts(box, mesh=False)["min_wall_mm"] is None


def test_a_mesh_too_large_to_sample_in_time_states_no_thinnest_wall(monkeypatch):
    box = Box(80, 60, 40, align=BOTTOM) - Pos(0, 0, 2) * Box(76, 56, 40, align=BOTTOM)
    assert facts(box)["min_wall_mm"] is not None
    monkeypatch.setattr(printfacts, "MAX_WALL_TRIANGLES", 8)
    piece = facts(box)
    assert_well_formed(piece)
    assert piece["min_wall_mm"] is None
    assert piece["strip_width_mm"] > 20.0  # every other fact is still measured


def test_a_tall_slender_peg_on_the_bed():
    peg = Cylinder(3.0, 60.0, align=BOTTOM)
    piece = facts(peg, fit=mating(peg, "axle.radial"))
    assert_well_formed(piece)
    assert piece["contact_mm2"] == pytest.approx(math.pi * 9.0, rel=1e-6)
    assert piece["height_mm"] == pytest.approx(60.0, abs=1e-3)
    # height / sqrt(contact): the quantity preparation scores for tipping.
    assert piece["slenderness"] == pytest.approx(60.0 / math.sqrt(math.pi * 9.0), rel=1e-4)
    assert piece["slenderness"] > 6.0
    assert piece["strip_width_mm"] == pytest.approx(3.0, rel=1e-6)
    # Its mating face rises from the bed and looks outward.
    assert piece["fit_at_bed"] == [{"pair": "axle.radial", "side": "outside"}]
    assert piece["inner_loops"] == []


def test_a_bore_on_the_bed_is_an_inner_loop_that_a_fit_owns():
    plate = Box(40, 30, 6, align=BOTTOM) - Pos(-8, 0, 0) * Cylinder(4.0, 6, align=BOTTOM) \
        - Pos(10, 0, 0) * Box(12, 5, 6, align=BOTTOM)
    piece = facts(plate, fit=mating(plate, "axle.radial"))
    assert_well_formed(piece)
    # Largest first: the plain slot of 12 x 5 (5 mm at its narrowest), then the bore.
    assert piece["inner_loops"] == [{"min_width_mm": 8.0, "fit": True},
                                    {"min_width_mm": 5.0, "fit": False}]
    assert piece["fit_at_bed"] == [{"pair": "axle.radial", "side": "inside"}]
    assert piece["contact_perimeter_mm"] == pytest.approx(140.0 + 8.0 * math.pi + 34.0)


def test_the_wall_of_an_opening_looks_inward_and_a_rail_outward():
    frame = Box(40, 30, 6, align=BOTTOM) - Box(30, 20, 6, align=BOTTOM)
    walls = {index: "lid.wall" for index, face in enumerate(frame.faces())
             if face.geom_type == GeomType.PLANE and abs(face.normal_at(face.center()).X) > 0.9
             and abs(face.center().X) < 16}
    assert len(walls) == 2
    piece = facts(frame, fit=prepare.PartFit(faces=walls))
    assert piece["fit_at_bed"] == [{"pair": "lid.wall", "side": "inside"}]
    assert piece["inner_loops"] == [{"min_width_mm": 20.0, "fit": True}]
    rail = Box(30, 3, 8, align=BOTTOM)
    sides = {index: "lid.wall" for index, face in enumerate(rail.faces())
             if abs(face.normal_at(face.center()).Y) > 0.9}
    assert facts(rail, fit=prepare.PartFit(faces=sides))["fit_at_bed"] == [
        {"pair": "lid.wall", "side": "outside"}]


def test_only_mating_faces_that_rise_from_the_bed_count():
    # A bore with a chamfer at the bed: its wall starts 1 mm up, so no foot can grow on it.
    plate = Box(30, 30, 6, align=BOTTOM) - Cylinder(4.0, 6, align=BOTTOM)
    relieved = chamfer(plate.edges().filter_by(GeomType.CIRCLE).group_by(Axis.Z)[0], 1.0)
    piece = facts(relieved, fit=mating(relieved, "axle.radial", GeomType.CYLINDER))
    assert piece["fit_at_bed"] == []
    # The loop on the bed is the chamfer's now, and the fit does not border it.
    assert piece["inner_loops"] == [{"min_width_mm": 10.0, "fit": False}]
    # A mating face that lies flat on the bed has no bottom edge to grow a foot.
    block = Box(20, 20, 5, align=BOTTOM)
    bottom = next(index for index, face in enumerate(block.faces())
                  if face.normal_at(face.center()).Z < -0.9)
    assert facts(block, fit=prepare.PartFit(faces={bottom: "seat.flat"}))["fit_at_bed"] == []
    # Upside down the same face is on top: still nothing.
    assert facts(block, up=(0, 0, -1),
                 fit=prepare.PartFit(faces={bottom: "seat.flat"}))["fit_at_bed"] == []


def test_a_pair_name_is_passed_on_only_when_it_fits_the_pattern():
    peg = Cylinder(3.0, 20.0, align=BOTTOM)
    index = face_index(peg, GeomType.CYLINDER)
    for label in ("Lid Snap.wall", "x\n;M104 S300", "", "a" * 65, "9lives.radial", "ok.radial"):
        (entry,) = facts(peg, fit=prepare.PartFit(faces={index: label}))["fit_at_bed"]
        assert entry == {"pair": "ok.radial" if label == "ok.radial" else "pair_1",
                         "side": "outside"}


def test_a_label_that_is_no_text_or_takes_a_replaced_name_cannot_break_the_facts():
    box = Box(30, 30, 20, align=BOTTOM) - Cylinder(5, 20, align=BOTTOM)
    bore = face_index(box, GeomType.CYLINDER)
    side = next(index for index, face in enumerate(box.faces())
                if face.geom_type == GeomType.PLANE and face.normal_at(face.center()).X > 0.9)
    for label in (5, None, 2.5, ["a"], {"a": 1}, b"axle"):
        assert facts(box, fit=prepare.PartFit(faces={bore: label}))["fit_at_bed"] == [
            {"pair": "pair_1", "side": "inside"}]
    # A real pair named like a replacement keeps its name; the replaced one moves on.
    piece = facts(box, fit=prepare.PartFit(faces={bore: "odd one", side: "pair_1"}))
    assert piece["fit_at_bed"] == [{"pair": "pair_1", "side": "outside"},
                                   {"pair": "pair_2", "side": "inside"}]
    # Also when the real pair of that name does not stand on the bed itself.
    top = next(index for index, face in enumerate(box.faces())
               if face.geom_type == GeomType.PLANE and face.normal_at(face.center()).Z > 0.9)
    # (Names go by the order of the faces: the side wall comes before the bore.)
    piece = facts(box, fit=prepare.PartFit(faces={bore: "Odd", side: 7, top: "pair_1"}))
    assert piece["fit_at_bed"] == [{"pair": "pair_2", "side": "outside"},
                                   {"pair": "pair_3", "side": "inside"}]
    assert_well_formed(piece)


@pytest.mark.parametrize("up", [(0.0, 0.0, 0.0), (float("nan"), 0.0, 1.0),
                                (float("inf"), 0.0, 0.0), (0.0, 1.0), (0.0, 0.0, 0.0, 1.0),
                                "up", None])
def test_a_direction_that_is_no_direction_is_refused(up):
    # Never a fact that is not a number: the rules would refuse the whole plate.
    with pytest.raises(ValueError, match="direction"):
        facts(Box(10, 10, 10), up=up)


def test_a_direction_of_any_length_is_a_direction():
    block = Box(10, 20, 30, align=BOTTOM)
    upright = facts(block)
    for up in ((0.0, 0.0, 1e-320), (0.0, 0.0, 1e308), (0, 0, 7)):
        assert facts(block, up=up) == upright
    askew = facts(block, up=(0.0, 1e308, 1e308))
    assert askew == facts(block, up=(0.0, 1.0, 1.0))
    assert_well_formed(askew)
    json.dumps(askew, allow_nan=False)


def test_the_family_is_a_known_token_that_agrees_with_the_material_id():
    def token(**material):
        return printfacts.family_token(Material.model_validate(material))

    assert token(id="pla-basic", family="PLA") == "PLA"
    assert token(id="petg-hf", family="PETG") == "PETG"
    assert token(id="tpu-95a", family="tpu") == "TPU"
    assert token(id="generic", family=" ASA ") == "ASA"
    # Not stated: the id says it, as everywhere in the fit library.
    assert token(id="pla-basic") == "PLA"
    assert token(id="tpu_90a") == "TPU"
    assert token(id="abs") == "ABS"
    # A family the rules have no card for is never replaced by a guess from the id.
    assert token(id="pla-basic", family="PETG-CF") is None
    assert token(id="petg", family="<script>") is None
    assert token(id="pla-basic", family="PLA; M104 S300") is None
    assert token(id="mystery", family="NYLON") is None
    # A family that contradicts the id: one of the two is wrong, and the bed temperature
    # of the wrong one must not be chosen.
    assert token(id="tpu-95a", family="PLA") is None
    assert token(id="abs-x", family="pla") is None
    assert token(id="pla-basic", family="PETG") is None
    # An id that merely begins with the letters is not that family.
    assert token(id="plasma") is None
    assert token(id="mystery") is None
    assert token(id="plaster-mix", family="PETG") == "PETG"
    assert printfacts.family_token(None) is None
    assert printfacts.family_token({"id": "pla-basic", "family": 5}) is None


def test_nothing_flat_on_the_bed_is_the_most_slender_a_piece_can_be():
    piece = facts(Sphere(10))
    assert_well_formed(piece)
    assert piece["contact_mm2"] == 0.0 and piece["contact_perimeter_mm"] == 0.0
    assert piece["strip_width_mm"] == 0.0 and piece["slenderness"] == 1e4
    assert piece["inner_loops"] == []
    # Nothing carries the underside of a ball: it reaches as far as it is large.
    (region,) = piece["overhang_regions"]
    assert region["kind"] == "free" and region["reach_mm"] > 10.0 and not region["over_part"]


def test_the_facts_follow_the_piece_when_it_is_turned():
    ledge = Box(20, 20, 30, align=BOTTOM) + Pos(25, 0, 25) * Box(30, 20, 5, align=BOTTOM)
    upright = facts(ledge)
    turn = Rot(30, 40, 50)
    up = tuple(float(v) for v in (turn * Pos(0, 0, 1)).position)
    turned = facts(turn * ledge, up=up)
    for key in PIECE_KEYS:
        if key == "contact_span_mm":
            continue  # the square foot lies askew to the bed axes now
        assert turned[key] == pytest.approx(upright[key], rel=1e-5, abs=1e-5) \
            if isinstance(upright[key], float) else turned[key] == upright[key], key
    assert turned["contact_span_mm"] > 20.0
    # Upside down it lies on its flat top, ledge and all, and nothing hangs.
    flipped = facts(ledge, up=(0, 0, -1))
    assert flipped["contact_mm2"] == pytest.approx(1000.0)
    assert flipped["contact_span_mm"] == pytest.approx(50.0)
    assert flipped["overhang_mm2"] == 0.0 and flipped["overhang_regions"] == []


# --- overhang regions ---------------------------------------------------------------------------


def test_a_slot_ceiling_is_a_bridge_of_its_width():
    block = Box(40, 30, 20, align=BOTTOM) - Box(20, 30, 8, align=BOTTOM)
    region = one_region(facts(block))
    assert region == {"area_mm2": 600.0, "kind": "bridge", "span_mm": 20.0,
                      "diameter_mm": None, "reach_mm": None, "over_part": False}
    # 21 mm is one too many for the rule's 20: the number must be the slot's width.
    wider = Box(40, 30, 20, align=BOTTOM) - Box(21, 30, 8, align=BOTTOM)
    assert one_region(facts(wider))["span_mm"] == pytest.approx(21.0, abs=1e-3)


def test_a_ceiling_carried_all_round_bridges_its_narrow_way():
    cup = Box(60, 40, 30, align=BOTTOM) - Box(56, 36, 28, align=BOTTOM)
    piece = facts(cup)
    region = one_region(piece)
    assert (region["kind"], region["span_mm"], region["over_part"]) == ("bridge", 36.0, False)
    assert region["area_mm2"] == pytest.approx(56.0 * 36.0)
    # The rim it stands on is a 2 mm strip around a hole.
    assert piece["strip_width_mm"] == pytest.approx(2.0)
    assert piece["inner_loops"] == [{"min_width_mm": 36.0, "fit": False}]


def test_a_cross_hole_is_a_hole_ceiling_of_its_diameter():
    block = Box(40, 20, 20, align=BOTTOM) - Pos(0, 0, 10) * Rot(90, 0, 0) * Cylinder(4.0, 20)
    piece = facts(block)
    region = one_region(piece)
    assert (region["kind"], region["diameter_mm"]) == ("hole_ceiling", 8.0)
    assert region["span_mm"] is None and region["reach_mm"] is None
    # The quarter of the bore that hangs steeper than 45 degrees, 20 mm long; the mesh
    # counts whole triangles, so a little more.
    assert region["area_mm2"] == pytest.approx(math.pi / 2 * 4.0 * 20.0, rel=0.03)
    # The floor of the hole is below it: supports would stand on the piece.
    assert region["over_part"] is True
    assert piece["overhang_over_part_mm2"] == pytest.approx(piece["overhang_mm2"])
    nine = Box(40, 20, 20, align=BOTTOM) - Pos(0, 0, 10) * Rot(90, 0, 0) * Cylinder(4.5, 20)
    assert one_region(facts(nine))["diameter_mm"] == pytest.approx(9.0, abs=1e-3)


def test_a_round_bar_is_not_a_hole():
    posts = Box(10, 10, 20, align=BOTTOM) + Pos(30, 0, 0) * Box(10, 10, 20, align=BOTTOM)
    bar = posts + Pos(15, 0, 15) * Rot(0, 90, 0) * Cylinder(3.0, 30)
    region = one_region(facts(bar))
    # Carried at its two ends only, 20 mm apart: its middle is 10 mm from either.
    assert region["kind"] == "free" and region["reach_mm"] == pytest.approx(10.0, abs=0.1)


def test_a_bottom_fillet_reaches_as_far_as_its_radius():
    # A lid: 100 x 60 mm with a 2 mm fillet round its bottom.
    lid = Box(100, 60, 8, align=BOTTOM)
    lid = fillet(lid.edges().group_by(Axis.Z)[0], 2.0)
    piece = facts(lid)
    region = one_region(piece)
    assert region["kind"] == "free" and region["over_part"] is False
    # 2 mm, at the square corners too: the rule's limit is "at most 2".
    assert region["reach_mm"] == pytest.approx(2.0, abs=1e-3) and region["reach_mm"] <= 2.0
    # Only the half of the curve that is flatter than 45 degrees counts as area.
    assert region["area_mm2"] == pytest.approx(500.0, rel=0.03)
    assert piece["contact_mm2"] == pytest.approx(96.0 * 56.0)
    larger = fillet(Box(100, 60, 8, align=BOTTOM).edges().group_by(Axis.Z)[0], 3.0)
    assert one_region(facts(larger))["reach_mm"] == pytest.approx(3.0, abs=1e-3)
    disc = fillet(Cylinder(20, 8, align=BOTTOM).edges().group_by(Axis.Z)[0], 2.0)
    assert one_region(facts(disc))["reach_mm"] == pytest.approx(2.0, abs=0.02)


def test_a_ledge_reaches_as_far_as_it_sticks_out():
    ledge = Box(20, 20, 30, align=BOTTOM) + Pos(25, 0, 25) * Box(30, 20, 5, align=BOTTOM)
    region = one_region(facts(ledge))
    assert region == {"area_mm2": 600.0, "kind": "free", "span_mm": None, "diameter_mm": None,
                      "reach_mm": 30.0, "over_part": False}
    # A rim that sticks out 10 mm all round a square stem reaches 10 mm, not the diagonal;
    # the foot of the piece lies under it.
    cap = Box(40, 40, 10, align=BOTTOM) + Pos(0, 0, 10) * Box(10, 10, 20, align=BOTTOM) \
        + Pos(0, 0, 30) * Box(30, 30, 4, align=BOTTOM)
    region = one_region(facts(cap))
    assert (region["kind"], region["reach_mm"], region["over_part"]) == ("free", 10.0, True)
    # Round a round stem the distance is the plain one.
    head = Box(20, 20, 3, align=BOTTOM) + Pos(0, 0, 3) * Cylinder(1.5, 10, align=BOTTOM) \
        + Pos(0, 0, 13) * Cylinder(3.0, 2, align=BOTTOM)
    assert one_region(facts(head))["reach_mm"] == pytest.approx(1.5, abs=0.05)


def test_a_ceiling_with_an_open_side_that_is_not_straight_is_free():
    # A roof with a round hole through it: lines across it would end in the hole.
    roof = Box(60, 40, 30, align=BOTTOM) - Box(56, 36, 28, align=BOTTOM) \
        - Pos(0, 0, 25) * Cylinder(5.0, 10, align=BOTTOM)
    region = one_region(facts(roof))
    assert region["kind"] == "free" and region["reach_mm"] == pytest.approx(18.0, abs=0.05)


def test_several_regions_are_listed_largest_first_with_their_kinds():
    piece = facts(all_kinds())
    assert_well_formed(piece)
    kinds = [(r["kind"], r[LENGTH_OF[r["kind"]]], r["over_part"])
             for r in piece["overhang_regions"]]
    assert kinds == [("bridge", 20.0, False), ("hole_ceiling", 8.0, True), ("free", 6.0, False)]
    assert piece["overhang_regions"][2]["area_mm2"] == pytest.approx(60.0)
    assert piece["overhang_over_part_mm2"] == pytest.approx(
        piece["overhang_regions"][1]["area_mm2"])


def all_kinds():
    """A block with a 20 mm slot under it, an 8 mm cross hole and a ledge reaching 6 mm."""
    block = Box(60, 30, 30, align=BOTTOM) - Pos(-15, 0, 0) * Box(20, 30, 6, align=BOTTOM)
    block -= Pos(15, 0, 15) * Rot(90, 0, 0) * Cylinder(4.0, 30)
    return block + Pos(33, 0, 20) * Box(6, 10, 4, align=BOTTOM)


def test_more_than_64_regions_are_merged_into_one_free_region():
    studs = Box(100, 100, 3, align=BOTTOM)
    studs += [Pos(x, y, 3) * Cylinder(1.5, 6, align=BOTTOM)
              + Pos(x, y, 9) * Cylinder(3.0, 2, align=BOTTOM)
              for x in range(-40, 41, 10) for y in range(-40, 41, 10)]
    studs += Pos(0, 0, 3) * Box(4, 4, 20, align=BOTTOM) + Pos(0, 0, 23) * Box(30, 30, 3, align=BOTTOM)
    piece = facts(studs)
    assert_well_formed(piece)
    regions = piece["overhang_regions"]
    # 81 stud heads and one large cap: the cap, 62 heads, and the other 19 heads as one.
    assert len(regions) == 64
    head = math.pi * (3.0 ** 2 - 1.5 ** 2)
    assert regions[0]["area_mm2"] == pytest.approx(900.0 - 16.0)
    assert regions[1]["area_mm2"] == pytest.approx(19 * head, rel=0.05)
    assert regions[1]["kind"] == "free" and regions[1]["reach_mm"] >= 1.5
    assert all(r["area_mm2"] == pytest.approx(head, rel=0.05) for r in regions[2:])
    assert all(r["over_part"] for r in regions)


def test_without_a_tessellation_the_regions_are_null_and_nothing_is_played_down():
    ledge = Box(20, 20, 30, align=BOTTOM) + Pos(25, 0, 25) * Box(30, 20, 5, align=BOTTOM)
    piece = facts(ledge, mesh=False)
    assert_well_formed(piece)
    assert piece["overhang_mm2"] == pytest.approx(600.0) and piece["overhang_regions"] is None
    # Unknown counts as "over the piece": the rules then keep supports everywhere allowed.
    assert piece["overhang_over_part_mm2"] == pytest.approx(600.0)
    # The contact is exact B-rep work and needs no mesh.
    assert piece["contact_mm2"] == pytest.approx(400.0) and piece["strip_width_mm"] == 10.0
    # Curved faces cannot be judged without triangles, so "no overhang" is not claimed.
    round_part = Cylinder(5, 10, align=BOTTOM)
    assert facts(round_part, mesh=False)["overhang_regions"] is None
    assert facts(round_part)["overhang_regions"] == []


def test_the_material_family_is_a_known_token_or_nothing():
    for family, token in (("PLA", "PLA"), ("petg", "PETG"), (" Tpu ", "TPU"), ("ABS", "ABS"),
                          ("ASA", "ASA")):
        assert printfacts.family_token(Material(id="x", family=family)) == token
    # Recognised by its id when the family is not given.
    assert printfacts.family_token(Material(id="tpu-95a")) == "TPU"
    for text in ("PLA; M104 S300", "Nylon", "", "pla\nPETG"):
        assert printfacts.family_token(Material(id="mystery", family=text)) is None
    assert printfacts.family_token(None) is None
    plate = printfacts.plate_facts("PLA", [facts(ring()), facts(Box(10, 10, 10, align=BOTTOM))])
    assert list(plate) == PLATE_KEYS and plate["piece_count"] == 2
    assert plate["total_contact_mm2"] == pytest.approx(math.pi * 144.0 + 100.0, rel=1e-6)


# --- the same names on the Rust side ----------------------------------------------------------


def rust_fields(source: str, struct: str) -> list[str]:
    body = re.search(rf"struct {struct} \{{\n(.*?)\n\}}", source, re.DOTALL).group(1)
    return re.findall(r"^    (?:pub )?([a-z0-9_]+):", body, re.MULTILINE)


def test_the_names_and_tokens_are_those_of_the_rules_crate():
    assert printfacts.FAMILIES == ("PLA", "PETG", "TPU", "ABS", "ASA")
    assert (printfacts.BRIDGE, printfacts.HOLE_CEILING, printfacts.FREE) == tuple(LENGTH_OF)
    if not FACTS_RS.exists():
        return  # without the crate's sources there is nothing to compare with
    source = FACTS_RS.read_text(encoding="utf-8")
    assert rust_fields(source, "PieceFacts") == PIECE_KEYS
    assert rust_fields(source, "OverhangRegion") == REGION_KEYS
    assert rust_fields(source, "InnerLoop") == ["min_width_mm", "fit"]
    assert rust_fields(source, "FitAtBed") == ["pair", "side"]
    assert rust_fields(source, "PlateWire") == PLATE_KEYS
    tokens = set(re.findall(r'=> "([a-z_]+)"', source))
    assert {"bridge", "hole_ceiling", "free", "inside", "outside"} <= tokens
    assert f"`^{printfacts.PAIR_PATTERN}$`" in source
    assert f"MAX_ENTRIES: usize = {printfacts.MAX_ENTRIES};" in source


def test_the_example_of_every_overhang_kind_is_what_the_engine_answers():
    example = json.loads((DATA / "overhang-kinds-facts.json").read_text(encoding="utf-8"))
    answer = printfacts.plate_facts("PLA", [facts(all_kinds())])
    assert_same_facts(answer, example)


def assert_same_facts(answer, example, path: str = "facts") -> None:
    """Equal shape, tokens and flags; numbers equal within what a mesher may differ by."""
    if isinstance(example, dict):
        assert isinstance(answer, dict) and list(answer) == list(example), path
        for key in example:
            assert_same_facts(answer[key], example[key], f"{path}.{key}")
    elif isinstance(example, list):
        assert isinstance(answer, list) and len(answer) == len(example), path
        for index, (mine, theirs) in enumerate(zip(answer, example, strict=True)):
            assert_same_facts(mine, theirs, f"{path}[{index}]")
    elif isinstance(example, float) and not isinstance(example, bool):
        assert answer == pytest.approx(example, rel=1e-3, abs=1e-3), path
    else:
        assert answer == example and type(answer) is type(example), path


# --- through the worker -------------------------------------------------------------------------

ZONE = {"id": "start_load_line", "min": [55.0, 0.0], "max": [145.0, 8.0]}


def drum(radius: float, height: float, sides: int = 96):
    """A plain round thing, written from a formula: the stand-in for what the ring fits."""
    angles = 2 * math.pi * np.arange(sides) / sides
    rim = np.stack([radius * np.cos(angles), radius * np.sin(angles)], axis=1)
    vertices = np.concatenate([
        np.column_stack([rim, np.zeros(sides)]), np.column_stack([rim, np.full(sides, height)]),
        [[0.0, 0.0, 0.0], [0.0, 0.0, height]]])
    triangles = []
    for k in range(sides):
        n = (k + 1) % sides
        triangles += [(k, n, sides + n), (k, sides + n, sides + k), (2 * sides, n, k),
                      (2 * sides + 1, sides + k, sides + n)]
    return vertices, np.array(triangles)


RING_CARDS = [{"card": "pocket", "name": "sample_rim",
               "args": {"reference": "sample", "fit": "cradle", "shape": "cylinder"}}]


@pytest.fixture(scope="module")
def jobs(tmp_path_factory, builds):
    root = tmp_path_factory.mktemp("printfacts")
    sample = {**copy.deepcopy(X2D_CONTEXT),
               "references": [{"name": "sample", "path": "sample.stl"}]}
    cable_box = builds["cable_box_asked"].dir
    requests = {
        # The gauge of a round thing of 60 mm: a ring of 64.5 and 68.5 mm, a 2 mm wall.
        "ring_orient": (None, {"op": "gauge", "views": [], "stage": "orient",
                               "context": sample, "cards": RING_CARDS}),
        "ring_all": (None, {"op": "gauge", "views": [], "clearance": 6.0, "context": sample,
                            "cards": RING_CARDS}),
        "ring_before": (None, {"op": "gauge", "views": [], "context": sample,
                               "cards": RING_CARDS}),
        # The same thing as a band of the silhouette's shape (a second kind of ring).
        "ring_box": (None, {"op": "gauge", "views": [], "stage": "orient", "context": sample,
                            "cards": [{"card": "pocket", "name": "sample_box", "args": {
                                "reference": "sample", "fit": "cradle", "shape": "box"}}]}),
        "card_gauges": (None, {"op": "gauge", "views": [], "stage": "all", "cards": [
            {"card": "snap_lid", "args": {"opening": [40, 30], "wall": 2.0}},
            {"card": "screw_boss", "args": {"screw": "M3"}}]}),
        "orient": (cable_box, {"op": "prepare", "stage": "orient", "copies": {"lid": 3},
                               "views": ["top"], "view_size": SMALL_VIEW}),
        "all": (cable_box, {"op": "prepare", "stage": "all", "views": []}),
        "before": (cable_box, {"op": "prepare", "views": []}),
        "unknown_family": (cable_box, {
            "op": "prepare", "stage": "orient", "views": [],
            "context": {**copy.deepcopy(X2D_CONTEXT),
                        "material": {"id": "mystery", "family": "PLA; M104 S300"}}}),
    }
    specs = {}
    for name, (built, request) in requests.items():
        request = {"protocol": 0, "context": copy.deepcopy(X2D_CONTEXT), **request}
        specs[name] = (prepare_job(root / name, request, copy_from=built), False)
        if name.startswith("ring"):
            write_stl(root / name / "sample.stl", *drum(30.0, 30.0))
    return run_all(specs)


def test_the_ring_gauge_states_its_facts(jobs):
    result = ok(jobs["ring_orient"])
    assert result["stage"] == "orient"
    (gauge,) = result["gauges"]
    assert gauge["parts"] == ["sample_rim.ring"] and list(gauge["facts"]) == gauge["parts"]
    piece = gauge["facts"]["sample_rim.ring"]
    assert_well_formed(piece)
    # The wall of the ring, whatever the size of the thing: what rule AD-1 reads.
    assert piece["strip_width_mm"] == pytest.approx(2.0, rel=0.01)
    inside = gauge["dimensions"][0]["drawn"]
    assert inside == pytest.approx(64.54, abs=0.05)
    assert piece["contact_mm2"] == pytest.approx(
        math.pi * ((inside / 2 + 2.0) ** 2 - (inside / 2) ** 2), rel=1e-4)
    assert piece["contact_perimeter_mm"] == pytest.approx(math.pi * (2 * inside + 4.0), rel=1e-4)
    # One inner loop, the hole the thing drops through, and its wall is the mating face.
    assert piece["inner_loops"] == [{"min_width_mm": pytest.approx(inside, abs=1e-3),
                                     "fit": True}]
    assert piece["fit_at_bed"] == [{"pair": "sample_rim", "side": "inside"}]
    assert piece["overhang_regions"] == [] and piece["height_mm"] == pytest.approx(5.0)
    # The object the rules read for the whole gauge.
    assert result["facts"] == {"family": "PLA", "piece_count": 1,
                               "total_contact_mm2": piece["contact_mm2"], "pieces": [piece]}
    example = json.loads((DATA / "ring-gauge-facts.json").read_text(encoding="utf-8"))
    assert_same_facts(result["facts"], example)


def test_the_orient_stage_lays_nothing_out_and_writes_no_file(jobs):
    ring = jobs["ring_orient"]
    assert ok(ring)["plates"] == [] and ring.result["unplaced"] == []
    assert ring.files() == {"request.json", "result.json", "sample.stl"}
    job = jobs["orient"]
    result = ok(job)
    assert result["stage"] == "orient" and result["plates"] == [] and result["unplaced"] == []
    # Views were asked for, but there is no plate to draw.
    assert job.files() == {"request.json", "result.json", "model.brep", "model.parts.json"}
    # The checks of the orientation are there; where a piece lies is the next pass.
    ids = {check["id"] for check in result["checks"]}
    assert {"overhangs", "bed_contact", "fit_orientation"} <= ids
    assert "plate_fit" not in ids and "keepout" not in ids


def test_prepare_states_the_facts_of_every_part_as_it_is_turned(jobs):
    result = ok(jobs["orient"])
    box, lid = result["orientations"]
    assert (box["part"], lid["part"], lid["turned"]) == ("box", "lid", "upside down")
    for entry in (box, lid):
        assert_well_formed(entry["facts"])
        # The measures preparation already reports, under the names the rules read.
        assert entry["facts"]["contact_mm2"] == pytest.approx(entry["measures"]["contact"])
        assert entry["facts"]["overhang_mm2"] == pytest.approx(entry["measures"]["overhang"])
        assert entry["facts"]["height_mm"] == pytest.approx(entry["measures"]["height"])
        assert entry["facts"]["fit_overhang_mm2"] == pytest.approx(
            entry["measures"]["fit_overhang"])
    # The box has two cable slots, 12 mm wide: bridges over its own floor.
    slots = box["facts"]["overhang_regions"]
    assert [(r["kind"], r["span_mm"], r["over_part"]) for r in slots] == [
        ("bridge", 12.0, True)] * 2
    # Upside down, the lid's screw holes stand on the bed: mating faces of the screw fit.
    assert lid["facts"]["fit_at_bed"] == [{"pair": "screw_boss_1.screw", "side": "inside"}]
    assert lid["facts"]["inner_loops"] and all(l["fit"] for l in lid["facts"]["inner_loops"])
    # The whole preparation, copies included, in the order of `orientations`.
    whole = result["facts"]
    assert list(whole) == PLATE_KEYS and whole["family"] == "PLA" and whole["piece_count"] == 4
    assert whole["pieces"] == [box["facts"]] + [lid["facts"]] * 3
    assert whole["total_contact_mm2"] == pytest.approx(
        box["facts"]["contact_mm2"] + 3 * lid["facts"]["contact_mm2"])


def test_every_plate_carries_the_facts_of_its_pieces(jobs):
    result = ok(jobs["all"])
    by_part = {entry["part"]: entry["facts"] for entry in result["orientations"]}
    (plate,) = result["plates"]
    assert plate["facts"] == {
        "family": "PLA", "piece_count": 2,
        "total_contact_mm2": pytest.approx(sum(f["contact_mm2"] for f in by_part.values())),
        "pieces": [by_part[item["part"]] for item in plate["items"]]}
    assert plate["layout"] == {"clearance": 0.0, "margin": 2.0, "anchor": "front_left",
                               "offset": [0.0, 0.0]}
    # Nothing but the new fields differs from the answer to a request without `stage`.
    before = ok(jobs["before"])
    for key in ("stage", "facts"):
        result.pop(key)
    for entry in result["orientations"]:
        entry.pop("facts")
    for check in result["checks"]:
        check["data"].pop("clearance", None)
    plate.pop("facts")
    plate.pop("layout")
    result.pop("timings")
    before.pop("timings")
    assert result == before


def test_gauges_of_cards_state_which_faces_are_fits(jobs):
    result = ok(jobs["card_gauges"])
    lid, boss = result["gauges"]
    box = lid["facts"]["snap_lid_1.box"]
    assert_well_formed(box)
    # The opening goes through the gauge: its four walls stand on the bed and look inward.
    assert box["inner_loops"] == [{"min_width_mm": pytest.approx(16.0, abs=1e-3), "fit": True}]
    assert {entry["side"] for entry in box["fit_at_bed"]} == {"inside"}
    assert {entry["pair"] for entry in box["fit_at_bed"]} == {
        "snap_lid_1.locate_minus", "snap_lid_1.locate_plus", "snap_lid_1.wall_minus",
        "snap_lid_1.wall_plus"}
    assert box["strip_width_mm"] == pytest.approx(2.0)
    # The lid half lies on its plate; its rails stand on the plate, not on the bed.
    assert lid["facts"]["snap_lid_1.lid"]["fit_at_bed"] == []
    plate = boss["facts"]["screw_boss_2.plate"]
    assert plate["fit_at_bed"] == [{"pair": "screw_boss_2.screw", "side": "inside"}]
    assert [loop["fit"] for loop in plate["inner_loops"]] == [True]
    names = [name for gauge in result["gauges"] for name in gauge["parts"]]
    assert result["facts"]["piece_count"] == 4
    assert result["facts"]["pieces"] == [
        gauge["facts"][name] for gauge in result["gauges"] for name in gauge["parts"]]
    (laid,) = result["plates"]
    assert [item["name"] for item in laid["items"]] == names
    assert laid["facts"]["pieces"] == result["facts"]["pieces"]
    # Where to put the caliper: never on the first layers, which the bed has widened.
    said = [dimension["what"] for gauge in result["gauges"] for dimension in gauge["dimensions"]]
    assert len(said) == 4
    assert all(text.endswith("; measure above the first millimetre from the bed")
               for text in said)


def test_every_gauge_with_a_mating_face_on_the_bed_says_where_to_measure(jobs):
    # FIT-3: the first layers are pressed wide, so no gauge whose mating face rises from
    # the bed may be measured there. The ring of a pocket is such a gauge by construction,
    # whichever way the request is written.
    sentence = "; measure above the first millimetre from the bed"
    seen = 0
    for name in ("ring_orient", "ring_all", "ring_before", "ring_box", "card_gauges"):
        for gauge in ok(jobs[name])["gauges"]:
            for dimension in gauge["dimensions"]:
                at_bed = gauge.get("facts", {}).get(dimension["object"], {}).get("fit_at_bed")
                if at_bed or gauge["card"] == "pocket":
                    seen += 1
                    assert dimension["what"].endswith(sentence), (name, dimension)
                    assert dimension["what"].count(sentence) == 1, (name, dimension)
    assert seen >= 10
    # With the facts asked for, every ring does report its inside as a fit on the bed.
    for name in ("ring_orient", "ring_all", "ring_box"):
        (gauge,) = ok(jobs[name])["gauges"]
        (piece,) = gauge["facts"].values()
        assert [entry["side"] for entry in piece["fit_at_bed"]] == ["inside"], name


def test_a_request_from_before_the_feature_is_answered_as_before(jobs):
    result = ok(jobs["before"])
    assert set(result) == {"protocol", "op", "status", "error", "timings", "orientations",
                           "plates", "unplaced", "checks", "warnings"}
    for entry in result["orientations"]:
        assert "facts" not in entry
    for plate in result["plates"]:
        assert set(plate) == {"index", "items", "size", "threemf"}
    for check in result["checks"]:
        assert "clearance" not in check["data"]
    gauge = ok(jobs["ring_before"])
    assert "facts" not in gauge and "stage" not in gauge and "checks" not in gauge
    assert "facts" not in gauge["gauges"][0]
    assert set(gauge["plates"][0]) == {"index", "items", "size", "threemf"}
    # The same gauge with the new fields lies where it lay, 6 mm further in for its brim.
    (old,), (new,) = gauge["plates"][0]["items"], ok(jobs["ring_all"])["plates"][0]["items"]
    assert new["bbox"]["min"][:2] == pytest.approx([c + 6.0 for c in old["bbox"]["min"][:2]])


def test_a_family_the_rules_do_not_know_is_not_passed_on(jobs):
    job = jobs["unknown_family"]
    result = ok(job)
    assert result["facts"]["family"] is None
    assert "M104" not in json.dumps(result["facts"])
