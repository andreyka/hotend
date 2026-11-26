"""Room for the brim: the layout with a clearance, keep-out zones and an anchor, and the two
passes of a preparation (hotend_engine.prepare.lay_out, the `stage` of a request).

The print settings rules choose a brim after they have seen the facts of the pieces, so a
preparation runs twice: `stage: "orient"` (orientations and facts), then `stage: "plate"`
with the orientations pinned and the clearance the chosen brim needs. The engine makes the
room and decides nothing: a piece and its brim stay `clearance + margin` inside the reach,
`2 x clearance + gap` from the next piece and off every keep-out zone, or the piece is
reported as not placed.

The first half lays out boxes in this process; the second half reads worker jobs.
"""

from __future__ import annotations

import copy
import itertools
import math

import numpy as np
import pytest
from build123d import Align, Box
from conftest import X2D_CONTEXT, ok, prepare_job, read_3mf, run_all
from pydantic import ValidationError

from hotend_engine import prepare
from hotend_engine.parts import NamedPart
from hotend_engine.request import (
    Context,
    GaugeRequest,
    Keepout,
    PrepareRequest,
    decode_request,
    parse_request,
)
from hotend_engine.tessellate import tessellate

PRINTER = Context.model_validate(X2D_CONTEXT).printer
MAIN, AUXILIARY = PRINTER.nozzles
# The line the printer draws at the front of the bed before a print, as the printer data
# give it.
LOAD_LINE = {"id": "start_load_line", "min": [55.0, 0.0], "max": [145.0, 8.0]}
ZONE = Keepout.model_validate(LOAD_LINE)
GAP, MARGIN = 5.0, 5.0
# No brim, and brims of 2, 5 and 8 mm with 1 mm to spare.
CLEARANCES = (0.0, 3.0, 6.0, 9.0)


def items(count: int, size: tuple[float, float, float], nozzle=MAIN, name: str = "p"):
    shape = Box(*size, align=Align.MIN)
    return [prepare.Item(name=f"{name}.{n}", part=name, copy=n, shape=shape, mesh=None,
                         color="#4c78a8", rotation=np.eye(3), translation=np.zeros(3),
                         size=size, nozzle=nozzle) for n in range(1, count + 1)]


def assert_room(placed, clearance: float, zones=(ZONE,), gap: float = GAP,
                margin: float = MARGIN) -> None:
    """Every piece where the layout says it is, with room for its brim on all sides."""
    for entry in placed:
        reach = entry.item.nozzle.reach
        keep = clearance + margin
        assert entry.low[0] >= reach.x[0] + keep - 1e-9, entry.item.name
        assert entry.high[0] <= reach.x[1] - keep + 1e-9, entry.item.name
        assert entry.low[1] >= reach.y[0] + keep - 1e-9, entry.item.name
        assert entry.high[1] <= reach.y[1] - keep + 1e-9, entry.item.name
        box = entry.shape.bounding_box()
        assert tuple(box.min) == pytest.approx(entry.low, abs=1e-6)
        assert tuple(box.max) == pytest.approx(entry.high, abs=1e-6)
        for zone in zones:
            # The piece grown by the clearance does not touch the zone: off it in X or Y.
            off_x = max(zone.min[0] - entry.high[0], entry.low[0] - zone.max[0])
            off_y = max(zone.min[1] - entry.high[1], entry.low[1] - zone.max[1])
            assert max(off_x, off_y) > clearance, (entry.item.name, zone.id)
        assert prepare.touched_zones(entry.low[:2], entry.high[:2], clearance, zones) == []
    for a, b in itertools.combinations(placed, 2):
        if a.plate != b.plate:
            continue
        apart_x = max(a.low[0] - b.high[0], b.low[0] - a.high[0])
        apart_y = max(a.low[1] - b.high[1], b.low[1] - a.high[1])
        assert max(apart_x, apart_y) >= 2 * clearance + gap - 1e-9, (a.item.name, b.item.name)


def grown_box(placed, clearance: float) -> tuple[float, float, float, float]:
    return (min(p.low[0] for p in placed) - clearance, min(p.low[1] for p in placed) - clearance,
            max(p.high[0] for p in placed) + clearance, max(p.high[1] for p in placed) + clearance)


# --- the layout ---------------------------------------------------------------------------------


@pytest.mark.parametrize("anchor", ["front_left", "centre"])
@pytest.mark.parametrize("clearance", CLEARANCES)
def test_pieces_keep_the_clearance_to_each_other_the_edge_and_the_zone(clearance, anchor):
    pieces = items(7, (60.0, 40.0, 10.0)) + items(5, (25.0, 18.0, 6.0), name="q")
    placed, unplaced = prepare.lay_out(pieces, GAP, MARGIN, clearance, (ZONE,), anchor)
    assert not unplaced and len(placed) == 12
    assert_room(placed, clearance)
    # The same request gives the same layout.
    again, _ = prepare.lay_out(pieces, GAP, MARGIN, clearance, (ZONE,), anchor)
    assert [(p.plate, p.low, p.high, p.offset) for p in again] == [
        (p.plate, p.low, p.high, p.offset) for p in placed]


def test_neighbours_in_a_row_lie_exactly_two_clearances_and_a_gap_apart():
    for clearance in CLEARANCES:
        first, second, third = prepare.lay_out(items(3, (60.0, 40.0, 10.0)), GAP, MARGIN,
                                               clearance)[0]
        assert first.low[:2] == pytest.approx((MARGIN + clearance, MARGIN + clearance))
        assert second.low[0] - first.high[0] == pytest.approx(2 * clearance + GAP)
        assert third.low[0] - second.high[0] == pytest.approx(2 * clearance + GAP)
    # The next row starts the same distance behind the deepest piece of the row before.
    placed, _ = prepare.lay_out(items(4, (100.0, 40.0, 10.0)), GAP, MARGIN, 6.0)
    assert placed[2].low[1] - placed[0].high[1] == pytest.approx(2 * 6.0 + GAP)


def test_centre_puts_the_group_in_the_middle_of_the_reach():
    # A ring gauge: 74 mm round, a 5 mm brim and 1 mm to spare.
    (ring,), _ = prepare.lay_out(items(1, (74.0, 74.0, 5.0)), GAP, MARGIN, 6.0, (ZONE,),
                                 "centre")
    assert ring.low[:2] == pytest.approx((91.0, 91.0)) and ring.high[:2] == pytest.approx(
        (165.0, 165.0))
    assert ring.offset == pytest.approx((80.0, 80.0))
    for clearance in CLEARANCES:
        placed, _ = prepare.lay_out(items(5, (60.0, 40.0, 10.0)), GAP, MARGIN, clearance,
                                    (ZONE,), "centre")
        assert_room(placed, clearance)
        x0, y0, x1, y1 = grown_box(placed, clearance)
        assert ((x0 + x1) / 2, (y0 + y1) / 2) == pytest.approx((128.0, 128.0))
        # The rows are not broken around the zone: the group is as compact as without it.
        plain, _ = prepare.lay_out(items(5, (60.0, 40.0, 10.0)), GAP, MARGIN, clearance)
        assert [np.subtract(p.low, placed[0].low) for p in placed] == [
            pytest.approx(np.subtract(q.low, plain[0].low)) for q in plain]
        assert {p.offset for p in placed} == {placed[0].offset}
    # Without `centre` nothing is moved, and the first piece lies at the front left.
    placed, _ = prepare.lay_out(items(5, (60.0, 40.0, 10.0)), GAP, MARGIN, 6.0)
    assert placed[0].low[:2] == (11.0, 11.0) and {p.offset for p in placed} == {(0.0, 0.0)}


def test_a_piece_is_centred_inside_its_own_nozzles_reach():
    # The auxiliary nozzle starts at X 20.5: a piece as wide as its reach cannot be moved.
    (wide,), _ = prepare.lay_out(items(1, (225.5, 30.0, 5.0), AUXILIARY), GAP, 0.0, 5.0, (),
                                 "centre")
    assert wide.low[0] == pytest.approx(25.5) and wide.high[0] == pytest.approx(251.0)
    assert (wide.low[1] + wide.high[1]) / 2 == pytest.approx(128.0)
    # Two nozzles on one plate: the group is centred as far as both reaches allow.
    both = items(1, (200.0, 30.0, 5.0), MAIN, "a") + items(1, (225.5, 30.0, 5.0), AUXILIARY, "b")
    placed, unplaced = prepare.lay_out(both, GAP, 0.0, 5.0, (), "centre")
    assert not unplaced
    assert_room(placed, 5.0, zones=(), margin=0.0)


def test_a_full_plate_stays_clear_of_the_zone_when_it_is_centred():
    # Seven pieces in three rows fill the bed from front to back: centred as they are, the
    # front row would lie on the load line, so the rows are laid around the zone instead.
    pieces = items(7, (72.0, 72.0, 10.0))
    placed, unplaced = prepare.lay_out(pieces, GAP, MARGIN, 3.0, (ZONE,), "centre")
    assert not unplaced
    assert_room(placed, 3.0)
    assert {p.plate for p in placed} == {1}
    # Without the zone the same pieces are centred with their front row across it.
    free, _ = prepare.lay_out(pieces, GAP, MARGIN, 3.0, (), "centre")
    assert [p.low for p in free] != [p.low for p in placed]
    assert prepare.touched_zones(free[1].low[:2], free[1].high[:2], 3.0, (ZONE,)) == [
        "start_load_line"]


def test_from_the_front_left_a_piece_goes_past_the_zone_or_behind_it():
    # 60 mm wide with a 6 mm clearance: no room left of the zone (X 5 to 55), so past it.
    (piece,), _ = prepare.lay_out(items(1, (60.0, 40.0, 10.0)), GAP, MARGIN, 6.0, (ZONE,))
    assert piece.low[0] == pytest.approx(145.0 + 6.0, abs=0.01) and piece.low[1] == 11.0
    # A small one fits beside the zone.
    (small,), _ = prepare.lay_out(items(1, (20.0, 20.0, 10.0)), GAP, MARGIN, 6.0, (ZONE,))
    assert small.low[:2] == (11.0, 11.0)
    # A piece wider than the room either side starts its row behind the zone.
    (wide,), unplaced = prepare.lay_out(items(1, (200.0, 40.0, 10.0)), GAP, MARGIN, 6.0, (ZONE,))
    assert not unplaced
    assert wide.low[0] == 11.0 and wide.low[1] == pytest.approx(8.0 + 6.0, abs=0.01)
    assert wide.plate == 1


def test_a_piece_that_does_not_fit_with_its_clearance_is_not_placed():
    piece = items(1, (240.0, 236.0, 10.0))
    (placed,), unplaced = prepare.lay_out(piece, GAP, MARGIN, 0.0)
    assert not unplaced and placed.high[:2] == pytest.approx((245.0, 241.0))
    placed, (refused,) = prepare.lay_out(piece, GAP, MARGIN, 6.0)
    assert placed == []
    assert refused["name"] == "p.1" and refused["part"] == "p" and refused["copy"] == 1
    assert refused["reason"] == (
        "it is 240.0 x 236.0 x 10.0 mm as turned for printing, larger than the main nozzle's "
        "reach of 246.0 x 246.0 x 260.0 mm (with a 5 mm margin and 6 mm of clearance around it)")
    # Without a clearance the sentence is the one of before.
    _, (plain,) = prepare.lay_out(items(1, (260.0, 100.0, 10.0)), GAP, 2.0)
    assert plain["reason"].endswith("(with a 2 mm margin)")
    # The others are still laid out, from plate 1.
    mixed = piece + items(2, (30.0, 30.0, 5.0), name="q")
    placed, unplaced = prepare.lay_out(mixed, GAP, MARGIN, 6.0, (ZONE,), "centre")
    assert [u["name"] for u in unplaced] == ["p.1"]
    assert [p.item.name for p in placed] == ["q.1", "q.2"] and {p.plate for p in placed} == {1}
    assert_room(placed, 6.0)


def test_a_wider_brim_needs_more_of_the_bed():
    pieces = items(9, (70.0, 70.0, 10.0))
    plates = {}
    for clearance in CLEARANCES:
        placed, unplaced = prepare.lay_out(pieces, GAP, MARGIN, clearance, (ZONE,), "centre")
        assert not unplaced
        assert_room(placed, clearance)
        plates[clearance] = max(p.plate for p in placed)
    # Nine pieces of 70 mm lie three by three on a 256 mm bed with up to 3 mm round each;
    # with a wider brim only two by two fit a plate.
    assert plates == {0.0: 1, 3.0: 1, 6.0: 3, 9.0: 3}


def test_a_zone_that_leaves_no_room_refuses_the_piece_and_says_so():
    across = Keepout(id="strip", min=(0.0, 100.0), max=(256.0, 110.0))
    tall = items(1, (150.0, 150.0, 10.0)) + items(2, (40.0, 40.0, 10.0), name="q")
    placed, (refused,) = prepare.lay_out(tall, GAP, MARGIN, 3.0, (across,))
    assert refused["name"] == "p.1" and "keep-out" in refused["reason"]
    assert "3 mm of clearance" in refused["reason"]
    assert [p.item.name for p in placed] == ["q.1", "q.2"] and placed[0].plate == 1
    assert_room(placed, 3.0, zones=(across,))


def test_an_unplaced_piece_says_in_one_word_what_kept_it_off_the_plate():
    # The rules must tell "the brim does not fit" from "the piece does not fit": only the
    # first is theirs to solve, with a narrower brim or a bare plate.
    def cause(pieces, *layout):
        _, (refused,) = prepare.lay_out(pieces, *layout)
        assert set(refused) == {"name", "part", "copy", "reason", "cause"}
        return refused["cause"]

    # 240 mm lies in the 246 mm between the margins, but not with 6 mm round it.
    assert cause(items(1, (240.0, 236.0, 10.0)), GAP, MARGIN, 6.0) == "clearance"
    # Too long for the reach with or without a brim; too tall for it.
    assert cause(items(1, (260.0, 100.0, 10.0)), GAP, 2.0) == "too_large"
    assert cause(items(1, (260.0, 100.0, 10.0)), GAP, 2.0, 6.0) == "too_large"
    assert cause(items(1, (30.0, 30.0, 300.0)), GAP, MARGIN, 6.0) == "too_large"
    # A strip across the bed leaves 141 mm behind it: 136 mm fit there bare, not with 3 mm
    # round them; 150 mm fit on neither side of it.
    across = Keepout(id="strip", min=(0.0, 100.0), max=(256.0, 110.0))
    assert cause(items(1, (136.0, 136.0, 10.0)), GAP, MARGIN, 3.0, (across,)) == "clearance"
    assert cause(items(1, (150.0, 150.0, 10.0)), GAP, MARGIN, 3.0, (across,)) == "keepout"
    assert cause(items(1, (150.0, 150.0, 10.0)), GAP, MARGIN, 0.0, (across,)) == "keepout"
    # Centred, the answer is the same.
    assert cause(items(1, (136.0, 136.0, 10.0)), GAP, MARGIN, 3.0, (across,),
                 "centre") == "clearance"
    reachless = PRINTER.nozzles[0].model_copy(update={"reach": None})
    assert cause(items(1, (10.0, 10.0, 10.0), nozzle=reachless), GAP, MARGIN) == "no_reach"


def test_without_a_clearance_or_a_zone_the_layout_is_the_one_of_before():
    nine = items(9, (60.0, 40.0, 10.0))
    placed, unplaced = prepare.lay_out(nine, gap=5.0, margin=2.0)
    assert not unplaced
    assert [p.low[:2] for p in placed] == [
        (2.0, 2.0), (67.0, 2.0), (132.0, 2.0), (2.0, 47.0), (67.0, 47.0), (132.0, 47.0),
        (2.0, 92.0), (67.0, 92.0), (132.0, 92.0)]
    stated, _ = prepare.lay_out(nine, 5.0, 2.0, 0.0, (), "front_left")
    assert [(p.plate, p.low, p.high, p.quarter_turn) for p in stated] == [
        (p.plate, p.low, p.high, p.quarter_turn) for p in placed]
    assert all(np.array_equal(a.translation, b.translation)
               for a, b in zip(stated, placed, strict=True))


def test_a_pinned_orientation_is_used_whatever_it_scores():
    # A pillar taller than wide is laid down by its score, and stands when it is pinned.
    part = NamedPart("p", Box(20, 20, 60, align=(Align.CENTER, Align.CENTER, Align.MIN)),
                     "#4c78a8")
    geometry = prepare.PartGeometry(part, tessellate([part]).meshes["p"])
    free = prepare.choose_orientation(geometry, 45.0)
    assert free.source == "face" and free.measures["height"] == pytest.approx(20.0)
    pinned = prepare.choose_orientation(geometry, 45.0, None, (0.0, 0.0, 1.0), "the request",
                                        pinned=True)
    assert tuple(pinned.up) == (0.0, 0.0, 1.0) and pinned.overridden is None
    # Even one a free choice would refuse: on an edge, with nothing flat on the bed.
    askew = (1.0, 1.0, 0.0)
    refused = prepare.choose_orientation(geometry, 45.0, None, askew, "the request")
    assert refused.overridden == "leaves nothing flat on the bed"
    kept = prepare.choose_orientation(geometry, 45.0, None, askew, "the request", pinned=True)
    assert kept.overridden is None and kept.source == "preferred"
    assert tuple(kept.up) == pytest.approx((0.7071068, 0.7071068, 0.0))


# --- the request ----------------------------------------------------------------------------------


def test_the_layout_fields_of_a_request():
    plain = parse_request({"op": "prepare"})
    assert isinstance(plain, PrepareRequest)
    assert (plain.stage, plain.clearance, plain.anchor, plain.keepouts) == (
        "all", 0.0, "front_left", ())
    assert plain.print_settings is False
    asked = parse_request({"op": "prepare", "stage": "plate", "clearance": 6.0,
                           "anchor": "centre", "margin": 5.0, "keepouts": [LOAD_LINE]})
    assert (asked.stage, asked.clearance, asked.anchor, asked.margin) == (
        "plate", 6.0, "centre", 5.0)
    assert asked.keepouts == (ZONE,) and asked.print_settings is True
    gauge = parse_request({"op": "gauge", "stage": "orient"})
    assert isinstance(gauge, GaugeRequest) and gauge.print_settings is True
    assert parse_request({"op": "gauge"}).print_settings is False
    # Naming any one of the four fields speaks the interface, also with its default value.
    for field in ({"stage": "all"}, {"clearance": 0}, {"anchor": "front_left"}, {"keepouts": []}):
        assert parse_request({"op": "prepare", **field}).print_settings is True
    # A margin or a gap alone is a request of before.
    assert parse_request({"op": "prepare", "margin": 5.0, "gap": 3.0}).print_settings is False


@pytest.mark.parametrize("bad", [
    {"stage": "slice"},
    {"stage": None},
    {"clearance": -0.1},
    {"clearance": 20.5},
    {"clearance": float("nan")},
    {"clearance": float("inf")},
    {"clearance": "6; M104 S300"},
    {"anchor": "middle"},
    {"keepouts": [{"id": "Load Line", "min": [0, 0], "max": [1, 1]}]},
    {"keepouts": [{"id": "a\nb", "min": [0, 0], "max": [1, 1]}]},
    {"keepouts": [{"id": "a" * 33, "min": [0, 0], "max": [1, 1]}]},
    {"keepouts": [{"id": "a", "min": [0, 0], "max": [0, 1]}]},
    {"keepouts": [{"id": "a", "min": [5, 5], "max": [1, 9]}]},
    {"keepouts": [{"id": "a", "min": [0, 0], "max": [1, 1e9]}]},
    {"keepouts": [{"id": "a", "min": [0, 0], "max": [1, float("nan")]}]},
    {"keepouts": [{"id": "a", "min": [0, 0, 0], "max": [1, 1, 1]}]},
    {"keepouts": [{"id": "a", "min": [0, 0], "max": [1, 1], "reason": "the load line"}]},
    {"keepouts": [{"id": "a", "min": [0, 0], "max": [1, 1]}] * 2},
    {"keepouts": [{"id": f"z{n}", "min": [0, 0], "max": [1, 1]} for n in range(17)]},
    {"keepouts": {"id": "a", "min": [0, 0], "max": [1, 1]}},
    {"brim": 5.0},
    # A number is a number: no text and no flag is read as one.
    {"clearance": "6"},
    {"clearance": True},
    {"clearance": [6]},
    {"clearance": None},
    {"margin": "5"},
    {"margin": True},
    {"gap": "5"},
    {"gap": False},
    {"keepouts": [{"id": "a", "min": ["0", "0"], "max": ["1", "1"]}]},
    {"keepouts": [{"id": "a", "min": [False, False], "max": [True, True]}]},
    {"keepouts": [{"id": "a", "min": [0, 0], "max": [1, "9"]}]},
])
def test_layout_fields_outside_their_bounds_are_refused(bad):
    for op in ("prepare", "gauge"):
        with pytest.raises((ValidationError, ValueError)):
            parse_request({"op": op, **bad})


def test_whole_numbers_are_lengths_and_a_minus_zero_is_zero():
    asked = parse_request({"op": "prepare", "clearance": 6, "margin": 5, "gap": 3,
                           "keepouts": [{"id": "a", "min": [55, 0], "max": [145, 8]}]})
    assert (asked.clearance, asked.margin, asked.gap) == (6.0, 5.0, 3.0)
    assert all(isinstance(v, float) for v in (asked.clearance, asked.margin, asked.gap,
                                              *asked.keepouts[0].min, *asked.keepouts[0].max))
    zero = parse_request({"op": "prepare", "clearance": -0.0, "margin": -0.0, "gap": -0.0,
                          "keepouts": [{"id": "a", "min": [-0.0, -0.0], "max": [1, 1]}]})
    for value in (zero.clearance, zero.margin, zero.gap, *zero.keepouts[0].min):
        assert value == 0.0 and math.copysign(1.0, value) == 1.0


def test_a_request_text_names_every_key_once_and_is_not_nested_without_end():
    assert decode_request('{"protocol": 0, "op": "prepare", "clearance": 6}') == {
        "protocol": 0, "op": "prepare", "clearance": 6}
    # The later of two equal keys used to win silently: a brim's room could be asked for
    # and taken back in one request.
    for text in (
        '{"protocol":0,"op":"prepare","clearance":6,"clearance":0}',
        '{"protocol":0,"op":"prepare","keepouts":[{"id":"a","min":[0,0],"max":[1,1]}],"keepouts":[]}',
        '{"op":"prepare","keepouts":[{"id":"a","id":"b","min":[0,0],"max":[9,9]}]}',
        '{"op":"prepare","context":{"printer":{"id":"x","id":"y"}}}',
    ):
        with pytest.raises(ValueError, match="twice"):
            decode_request(text)
    deep = '{"protocol":0,"op":"prepare","keepouts":' + "[" * 100_000 + "]" * 100_000 + "}"
    with pytest.raises(ValueError, match="nested"):
        decode_request(deep)
    for text in ("{not json", '{"clearance": NaN}', '{"clearance": Infinity}',
                 '{"clearance": -Infinity}'):
        with pytest.raises(ValueError):
            decode_request(text)


# --- through the worker -------------------------------------------------------------------------

SMALL_PRINTER = {"id": "small", "nozzles": [
    {"id": 1, "role": "main", "diameter": 0.4,
     "reach": {"x": [0, 140], "y": [0, 80], "z": [0, 100]}}]}
SMALL = {"printer": SMALL_PRINTER, "material": {"id": "pla-basic", "family": "PLA"}}
PLATE = {"op": "prepare", "stage": "plate", "anchor": "centre", "margin": MARGIN,
         "keepouts": [LOAD_LINE], "views": []}


def bed(side: float) -> dict:
    return {"printer": {"id": "small", "nozzles": [
        {"id": 1, "role": "main", "diameter": 0.4,
         "reach": {"x": [0, side], "y": [0, side], "z": [0, 100]}}]},
        "material": {"id": "pla-basic", "family": "PLA"}}


# The gauges of a screw boss (two pieces of 12.6 mm square) and of a snap lid (two of
# 28 x 20 mm). Between the 5 mm margins of a 46 mm bed lie 36 mm: room for all four bare,
# and with 6 mm round each only for the boss's. A 30 mm bed holds the boss's alone.
TWO_GAUGES = [{"card": "screw_boss", "args": {"screw": "M3"}},
              {"card": "snap_lid", "args": {"opening": [40, 30], "wall": 2.0}}]


@pytest.fixture(scope="module")
def jobs(tmp_path_factory, builds):
    """The two passes the core makes: `orient`, then `plate` with the orientations of the
    first pass pinned and a clearance for the brim."""
    root = tmp_path_factory.mktemp("layout")
    cable_box, hints = builds["cable_box_asked"].dir, builds["print_hint"].dir

    def job(name, built, request):
        request = {"protocol": 0, "context": copy.deepcopy(X2D_CONTEXT), **request}
        return prepare_job(root / name, request, copy_from=built), False

    first = run_all({"orient": job("orient", cable_box, {
        "op": "prepare", "stage": "orient", "margin": MARGIN, "views": []})})
    pinned = {entry["part"]: {"up": entry["up"]}
              for entry in ok(first["orient"])["orientations"]}
    peg = [{"card": "peg_hole", "args": {"diameter": 6.0, "length": 6.0, "fit": "slide"}}]
    second = run_all({
        "plate": job("plate", cable_box, {**PLATE, "clearance": 6.0, "orientations": pinned}),
        "plate_wide": job("plate_wide", cable_box, {
            **PLATE, "clearance": 9.0, "copies": {"lid": 5}, "orientations": pinned}),
        "small_bare": job("small_bare", cable_box, {
            **PLATE, "keepouts": [], "clearance": 0.0, "orientations": pinned,
            "context": SMALL}),
        "small_brim": job("small_brim", cable_box, {
            **PLATE, "keepouts": [], "clearance": 6.0, "orientations": pinned,
            "context": SMALL}),
        "unpinned": job("unpinned", cable_box, {**PLATE, "clearance": 6.0,
                                                "orientations": {"box": pinned["box"]}}),
        # The model asks for the dome on its round side, which a free choice refuses.
        "pinned_odd": job("pinned_odd", hints, {
            **PLATE, "clearance": 3.0, "parts": ["dome"],
            "orientations": {"dome": {"up": [0, 0, -1]}}, "threemf": False}),
        "free_odd": job("free_odd", hints, {
            "op": "prepare", "stage": "all", "parts": ["dome"], "views": [],
            "orientations": {"dome": {"up": [0, 0, -1]}}, "threemf": False}),
        "gauge_plate": job("gauge_plate", None, {
            "op": "gauge", "stage": "plate", "clearance": 6.0, "anchor": "centre",
            "margin": MARGIN, "keepouts": [LOAD_LINE], "views": [], "cards": peg}),
        "gauge_orient": job("gauge_orient", None, {
            "op": "gauge", "stage": "orient", "clearance": 6.0, "keepouts": [LOAD_LINE],
            "views": ["top"], "cards": peg}),
        # A bed that must stay free everywhere: no gauge piece finds a place.
        "gauge_blocked": job("gauge_blocked", None, {
            "op": "gauge", "stage": "plate", "clearance": 6.0, "margin": MARGIN, "views": [],
            "keepouts": [{"id": "whole_bed", "min": [0.0, 0.0], "max": [256.0, 256.0]}],
            "cards": peg}),
        # A bed just wide enough for one of the two pieces of the gauge with its brim.
        "gauge_half": job("gauge_half", None, {
            "op": "gauge", "stage": "all", "clearance": 6.0, "margin": MARGIN, "views": [],
            "cards": TWO_GAUGES, "context": bed(46.0)}),
        "gauge_half_bare": job("gauge_half_bare", None, {
            "op": "gauge", "stage": "all", "clearance": 0.0, "margin": MARGIN, "views": [],
            "cards": TWO_GAUGES, "context": bed(46.0)}),
        "gauge_half_before": job("gauge_half_before", None, {
            "op": "gauge", "margin": MARGIN, "views": [], "cards": TWO_GAUGES,
            "context": bed(30.0)}),
    })
    return {**first, **second}


def boxes(plate: dict) -> list[tuple[list[float], list[float]]]:
    return [(item["bbox"]["min"], item["bbox"]["max"]) for item in plate["items"]]


def assert_plate_room(plate: dict, clearance: float, reach=((0.0, 256.0), (0.0, 256.0)),
                      zones=(LOAD_LINE,), margin: float = MARGIN) -> None:
    """The same promises as `assert_room`, read from the result the core gets."""
    laid = boxes(plate)
    for low, high in laid:
        for axis in (0, 1):
            assert low[axis] >= reach[axis][0] + clearance + margin - 1e-6
            assert high[axis] <= reach[axis][1] - clearance - margin + 1e-6
        for zone in zones:
            off_x = max(zone["min"][0] - high[0], low[0] - zone["max"][0])
            off_y = max(zone["min"][1] - high[1], low[1] - zone["max"][1])
            assert max(off_x, off_y) > clearance
    for (a_low, a_high), (b_low, b_high) in itertools.combinations(laid, 2):
        apart = max(max(a_low[k] - b_high[k], b_low[k] - a_high[k]) for k in (0, 1))
        assert apart >= 2 * clearance + GAP - 1e-6


def test_the_second_pass_lays_the_pinned_orientations_out_with_room_for_the_brim(jobs):
    first, job = ok(jobs["orient"]), jobs["plate"]
    result = ok(job)
    assert result["stage"] == "plate" and not result["unplaced"] and not result["warnings"]
    # The orientations are the ones of the first pass, and so are the facts.
    for before, after in zip(first["orientations"], result["orientations"], strict=True):
        assert after["up"] == before["up"] and after["facts"] == before["facts"]
        assert after["source"] in ("preferred", "as_modelled") and after["overridden"] is None
    assert result["facts"] == first["facts"]
    (plate,) = result["plates"]
    assert_plate_room(plate, 6.0)
    layout = plate["layout"]
    assert list(layout) == ["clearance", "margin", "anchor", "offset"]
    assert (layout["clearance"], layout["margin"], layout["anchor"]) == (6.0, 5.0, "centre")
    # Centred: the two pieces with their clearance sit round the middle of the 256 mm bed,
    # moved there together from the front left.
    laid = boxes(plate)
    lows, highs = np.array([b[0] for b in laid]), np.array([b[1] for b in laid])
    assert (lows.min(axis=0)[:2] + highs.max(axis=0)[:2]) / 2 == pytest.approx([128.0, 128.0])
    assert lows.min(axis=0)[:2] - 6.0 - MARGIN == pytest.approx(layout["offset"])
    # The plate file holds the pieces where the result says they are.
    objects = read_3mf(job.dir / plate["threemf"])
    for item in plate["items"]:
        assert objects[item["name"]]["min"] == pytest.approx(item["bbox"]["min"], abs=0.02)
        assert objects[item["name"]]["max"] == pytest.approx(item["bbox"]["max"], abs=0.02)
    for name in ("box", "lid"):
        fit = job.check("plate_fit", name)
        assert fit["status"] == "pass" and fit["data"]["clearance"] == 6.0
        assert fit["data"]["margin"] == 5.0
    zone = job.check("keepout")
    assert zone["status"] == "pass" and zone["severity"] == "error" and zone["part"] is None
    assert zone["data"] == {"clearance": 6.0, "zones": [LOAD_LINE], "collisions": []}


def test_a_wider_brim_and_more_copies_are_split_over_plates_with_the_same_room(jobs):
    result = ok(jobs["plate_wide"])
    assert not result["unplaced"]
    # A box and five lids of 124 x 64 mm, each with 9 mm round it: two to a plate.
    assert [len(plate["items"]) for plate in result["plates"]] == [2, 2, 2]
    assert sum(len(plate["items"]) for plate in result["plates"]) == 6
    for plate in result["plates"]:
        assert_plate_room(plate, 9.0)
        assert plate["layout"]["clearance"] == 9.0
        assert plate["facts"]["piece_count"] == len(plate["items"])
    assert jobs["plate_wide"].check("keepout")["status"] == "pass"
    assert result["facts"]["piece_count"] == 6


def test_pieces_that_have_no_room_for_their_brim_fail_the_preparation(jobs):
    # A bed of 140 x 80 mm holds the 124 x 64 mm box and lid, one per plate.
    bare = ok(jobs["small_bare"])
    assert [len(plate["items"]) for plate in bare["plates"]] == [1, 1]
    for plate in bare["plates"]:
        assert_plate_room(plate, 0.0, reach=((0.0, 140.0), (0.0, 80.0)), zones=())
    # With 6 mm round each piece for a brim there is no room: nothing is laid out, the
    # check fails and no plate file is written.
    job = jobs["small_brim"]
    assert job.returncode == 0
    result = job.result
    assert result["status"] == "check_error" and result["plates"] == []
    assert [u["name"] for u in result["unplaced"]] == ["box", "lid"]
    for entry in result["unplaced"]:
        assert set(entry) == {"name", "part", "copy", "reason", "cause"}
        assert "5 mm margin and 6 mm of clearance around it" in entry["reason"]
        # They lay on the bare bed: it is the brim that has no room.
        assert entry["cause"] == "clearance"
    for name in ("box", "lid"):
        fit = job.check("plate_fit", name)
        assert fit["status"] == "fail" and fit["severity"] == "error"
        assert "cannot be placed" in fit["message"] and fit["data"]["clearance"] == 6.0
    assert not any(path.startswith("plates/") for path in job.files())
    # The facts are still there: the rules can say what does not fit and why.
    assert result["facts"]["piece_count"] == 2


def test_the_second_pass_needs_every_orientation(jobs):
    job = jobs["unpinned"]
    assert job.returncode == 2
    error = job.result["error"]
    assert error["type"] == "bad_request"
    assert "orientations gives none for lid" in error["message"]
    assert not any(path.startswith("plates/") for path in job.files())


def test_a_pinned_orientation_is_not_second_guessed(jobs):
    # Asked for in one pass, the orientation without a flat face is refused ...
    (free,) = ok(jobs["free_odd"])["orientations"]
    assert free["overridden"] == "leaves nothing flat on the bed" and free["up"] != [0, 0, -1]
    # ... and pinned in the second pass it is what the plan was made for, so it is kept.
    (pinned,) = jobs["pinned_odd"].result["orientations"]
    assert pinned["up"] == [0.0, 0.0, -1.0] and pinned["overridden"] is None
    assert pinned["source"] == "preferred" and pinned["preferred_by"] == "the request"
    # What the rules get to read about it: nothing flat on the bed.
    assert pinned["facts"]["contact_mm2"] == 0.0 and pinned["facts"]["slenderness"] == 1e4


def test_a_gauge_plate_is_laid_out_the_same_way(jobs):
    job = jobs["gauge_plate"]
    result = ok(job)
    assert result["stage"] == "plate"
    (plate,) = result["plates"]
    assert_plate_room(plate, 6.0)
    assert plate["layout"]["anchor"] == "centre" and plate["layout"]["clearance"] == 6.0
    laid = boxes(plate)
    lows, highs = np.array([b[0] for b in laid]), np.array([b[1] for b in laid])
    assert (lows.min(axis=0)[:2] + highs.max(axis=0)[:2]) / 2 == pytest.approx([128.0, 128.0])
    fit, check = result["checks"]
    assert check["id"] == "keepout" and check["status"] == "pass"
    assert check["data"]["zones"] == [LOAD_LINE]
    # Every piece of the gauge lies on the plate, and a check says so.
    assert (fit["id"], fit["status"], fit["severity"], fit["part"]) == (
        "plate_fit", "pass", "error", None)
    assert fit["data"]["placed"] == 2 and fit["data"]["unplaced"] == []
    assert fit["data"]["clearance"] == 6.0 and fit["data"]["margin"] == MARGIN
    objects = read_3mf(job.dir / plate["threemf"])
    for item in plate["items"]:
        assert objects[item["name"]]["min"] == pytest.approx(item["bbox"]["min"], abs=0.02)
    # The first pass of a gauge: facts, no plate, no file, no check of a layout.
    first = jobs["gauge_orient"]
    result = ok(first)
    assert result["plates"] == [] and "checks" not in result
    assert set(result["gauges"][0]["facts"]) == {"peg_hole_1.hole", "peg_hole_1.peg"}
    assert first.files() == {"request.json", "result.json"}


def test_a_gauge_that_does_not_lie_on_the_plate_whole_fails(jobs):
    # A gauge tests a fit with all of its pieces. When one finds no room, the answer is
    # not "ok" with a plate that lacks it.
    job = jobs["gauge_blocked"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "check_error" and result["plates"] == []
    assert [u["name"] for u in result["unplaced"]] == ["peg_hole_1.hole", "peg_hole_1.peg"]
    assert {u["cause"] for u in result["unplaced"]} == {"keepout"}
    fit = job.check("plate_fit")
    assert fit["status"] == "fail" and fit["severity"] == "error" and fit["part"] is None
    assert fit["data"]["placed"] == 0
    assert fit["data"]["unplaced"] == ["peg_hole_1.hole", "peg_hole_1.peg"]
    assert "cannot be placed" in fit["message"]
    assert job.check("keepout")["status"] == "pass"  # nothing lies on the zone
    assert not any(path.startswith("plates/") for path in job.files())
    # The facts are still there for the rules.
    assert result["facts"]["piece_count"] == 2

    # Bare, all four pieces of two gauges lie on a small bed. With room for a brim only
    # the smaller gauge does, and the other is named with what kept it off.
    assert not ok(jobs["gauge_half_bare"])["unplaced"]
    assert ok(jobs["gauge_half_bare"])["checks"][0]["status"] == "pass"
    half = jobs["gauge_half"]
    assert half.returncode == 0, half.stderr
    result = half.result
    assert result["status"] == "check_error"
    assert [u["name"] for u in result["unplaced"]] == ["snap_lid_2.box", "snap_lid_2.lid"]
    assert {u["cause"] for u in result["unplaced"]} == {"clearance"}
    fit = half.check("plate_fit")
    assert fit["status"] == "fail" and fit["data"]["placed"] == 2
    assert fit["data"]["unplaced"] == ["snap_lid_2.box", "snap_lid_2.lid"]

    # A request from before the feature is answered as before: a warning, no check, and
    # no key a reader of before does not know.
    before = jobs["gauge_half_before"]
    result = ok(before)
    assert result["status"] == "ok" and "checks" not in result
    assert [u["name"] for u in result["unplaced"]] == ["snap_lid_2.box", "snap_lid_2.lid"]
    for entry in result["unplaced"]:
        assert set(entry) == {"name", "part", "copy", "reason"}
    assert any("fit no plate" in warning for warning in result["warnings"])
