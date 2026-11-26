"""What a declared fit must survive beyond its measured clearance: the two parts can be put
together (nothing in the way, the whole mating face there, all joints between two parts in
agreement), and the fit keeps its class across the printer's spread. Each case here passed
`fit_pairs` when only one probe point per pair was measured."""

from __future__ import annotations

import json

import pytest
from build123d import Align, Box, Cylinder, Pos
from fit_helpers import (
    check_env,
    context,
    fit_results,
    quantity,
    resolver,
    table,
    write_table,
)

from hotend_engine.checks import run_checks
from hotend_engine.checks.intersect import parts_do_not_intersect
from hotend_fit import cards, fit

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)


def statuses(shapes: dict, r, kind: str) -> list[str]:
    return [result.status for result in fit_results(shapes, r, kind)]


def failures(shapes: dict, r, kind: str | None = None) -> list[str]:
    return [result.message for result in fit_results(shapes, r, kind) if result.status == "fail"]


def peg_parts(r, fit_class: str = "slide", apart: float = 50.0):
    """A base with a hole and a cover with a peg, the cover drawn `apart` mm to the side."""
    base = Box(30, 30, 10, align=BOTTOM)
    cover = Pos(apart, 0, 10) * Box(30, 30, 3, align=BOTTOM)
    card = cards.peg_hole(diameter=6.0, length=6.0, fit=fit_class, ctx=r)
    return card.apply_a(base, at=Pos(0, 0, 10)), card.apply_b(cover, at=Pos(apart, 0, 10)), card


def test_an_untouched_joint_passes_its_pairs_and_its_assembly():
    r = resolver()
    base, cover, _ = peg_parts(r)
    shapes = {"base": base, "cover": cover}
    assert statuses(shapes, r, "pair") == ["pass", "pass"]
    (joint,) = fit_results(shapes, r, "joint")
    assert joint.status == "pass" and "goes together" in joint.message
    assert joint.data["card"] == "peg_hole" and joint.data["parts"] == ["base", "cover"]
    assert joint.data["assembled_overlap"] == pytest.approx(0.0, abs=1e-6)


# --- finding: joints that measure right at the probe and still cannot be assembled ---------


def test_a_rib_on_the_peg_away_from_the_probe_fails():
    r = resolver()
    base, cover, card = peg_parts(r)
    # 1.5 mm proud of the peg on its far side: the probe on +X still measures the clearance.
    rib = Pos(50 - card.peg_radius - 0.5, 0, 5) * Box(2.0, 1.0, 5, align=BOTTOM)
    shapes = {"base": base, "cover": cover + rib}
    (radial, _relief) = fit_results(shapes, r, "pair")
    assert radial.status == "fail" and "measures 0.266 mm as asked, but" in radial.message
    assert "% of the mating face of half b exists" in radial.message
    (joint,) = fit_results(shapes, r, "joint")
    assert joint.status == "fail" and "cannot be assembled as drawn" in joint.message
    assert "brought to its place" in joint.message and joint.data["assembled_overlap"] > 1.0


def test_a_key_in_the_hole_fails():
    r = resolver()
    base, cover, card = peg_parts(r)
    key = Pos(-card.hole_radius + 0.5, 0, 5) * Box(2.0, 1.0, 5, align=BOTTOM)
    shapes = {"base": base + key, "cover": cover}
    (joint,) = fit_results(shapes, r, "joint")
    assert joint.status == "fail" and "must leave free" in joint.message
    assert max(joint.data["keep_out"].values()) > 1.0
    assert any("mating face of half a exists" in message for message in failures(shapes, r, "pair"))


def test_a_hole_that_runs_off_the_edge_of_its_host_fails():
    r = resolver()
    narrow = Pos(2.5, 0, 0) * Box(5, 30, 10, align=BOTTOM)  # x from 0 to 5: half a hole
    card = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r)
    shapes = {"base": card.apply_a(narrow, at=Pos(0, 0, 10)),
              "cover": card.apply_b(Pos(50, 0, 10) * Box(30, 30, 3, align=BOTTOM),
                                    at=Pos(50, 0, 10))}
    radial, relief = fit_results(shapes, r, "pair")
    assert radial.status == relief.status == "fail"
    assert "Only 50 % of the mating face of half a exists" in radial.message
    assert radial.data["measured"] == pytest.approx(radial.data["expected"], abs=1e-3)


def test_a_blind_hole_deeper_than_its_plate_fails():
    r = resolver()
    card = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r)
    shapes = {"base": card.apply_a(Box(30, 30, 3, align=BOTTOM), at=Pos(0, 0, 3)),
              "cover": card.apply_b(Pos(50, 0, 3) * Box(30, 30, 3, align=BOTTOM),
                                    at=Pos(50, 0, 3))}
    messages = failures(shapes, r, "pair")
    assert len(messages) == 2  # half the wall is missing, and the hole has no floor
    assert any("% of the mating face of half a exists" in message for message in messages)
    assert any("no part has the mating face of half A" in message for message in messages)


def snap_box(r, rib: bool = False, lid_apart: float = 0.0):
    width, depth, wall, height = 80.0, 60.0, 2.0, 30.0
    box = Box(width + 2 * wall, depth + 2 * wall, height, align=BOTTOM) - Pos(0, 0, wall) * Box(
        width, depth, height, align=BOTTOM)
    if rib:
        # a stiffening rib across the box up to the rim, where the lid's rails go
        box += Pos(0, 0, wall) * Box(2.0, depth, height - wall, align=BOTTOM)
    lid = Pos(0, 0, height + lid_apart) * Box(width + 2 * wall, depth + 2 * wall, 2.0,
                                               align=BOTTOM)
    card = cards.snap_lid(opening=(width, depth), wall=wall, ctx=r)
    return {"box": card.apply_a(box, at=Pos(0, 0, height)),
            "lid": card.apply_b(lid, at=Pos(0, 0, height + lid_apart))}


def test_a_rib_where_the_lids_rails_go_fails_even_with_the_lid_drawn_apart():
    r = resolver()
    assert failures(snap_box(r, lid_apart=30.0), r) == []
    blocked = resolver()
    shapes = snap_box(blocked, rib=True, lid_apart=30.0)
    # Drawn 30 mm apart the parts do not overlap, so only the fit check can see it.
    assert parts_do_not_intersect(check_env(shapes, blocked, mesh=False)).status == "pass"
    (joint,) = fit_results(shapes, blocked, "joint")
    assert joint.status == "fail" and "cannot be assembled as drawn" in joint.message


# --- finding: several joints between the same two parts were never compared ----------------


def test_a_peg_placed_off_its_hole_on_parts_drawn_assembled_fails():
    r = resolver()
    base = Box(30, 30, 10, align=BOTTOM)
    cover = Pos(0, 0, 10) * Box(30, 30, 3, align=BOTTOM)
    card = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r)
    shapes = {"base": card.apply_a(base, at=Pos(0, 0, 10)),
              "cover": card.apply_b(cover, at=Pos(0.3, 0, 10))}
    # Each half is exactly as its card drew it, so every pair measures right ...
    assert statuses(shapes, r, "pair") == ["pass", "pass"]
    # ... but the parts are drawn touching while the two halves sit 0.3 mm apart.
    (joint,) = fit_results(shapes, r, "joint")
    assert joint.status == "fail" and "drawn touching" in joint.message
    assert "0.300 mm apart" in joint.message


def two_pegs(r, pitch_of_the_pegs: float):
    base = Box(60, 30, 10, align=BOTTOM)
    cover = Pos(0, 0, 30) * Box(60, 30, 3, align=BOTTOM)
    first = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r)
    second = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r)
    base = second.apply_a(first.apply_a(base, at=Pos(-10, 0, 10)), at=Pos(10, 0, 10))
    cover = first.apply_b(cover, at=Pos(-10, 0, 30))
    cover = second.apply_b(cover, at=Pos(-10 + pitch_of_the_pegs, 0, 30))
    return {"base": base, "cover": cover}


def test_two_joints_between_the_same_parts_must_agree_on_where_the_parts_sit():
    r = resolver()
    assert statuses(two_pegs(r, 20.0), r, "joint") == ["pass", "pass"]
    off = resolver()
    shapes = two_pegs(off, 21.0)  # holes 20 mm apart, pegs 21 mm apart
    assert statuses(shapes, off, "pair") == ["pass"] * 4
    first, second = fit_results(shapes, off, "joint")
    assert first.status == second.status == "fail"
    assert "joints between the same two parts ('base' and 'cover')" in first.message
    assert "disagree by 1.000 mm" in first.message and "peg_hole_2" in first.message


def test_a_screw_hole_away_from_its_boss_fails_against_the_other_joint():
    """The cable box's two joints: the lid's screw hole 8 mm from the boss it belongs to.
    Hole and boss are each drawn right and nothing overlaps."""
    r = resolver()
    shapes = snap_box(r)
    screw = cards.screw_boss("M3", "self_tap", height=28.0, plate=2.0, ctx=r)
    corner = Pos(40 - 3.0, 30 - 3.0, 30)
    shapes["box"] = screw.apply_a(shapes["box"], at=corner)
    shapes["lid"] = screw.apply_b(shapes["lid"], at=Pos(-8.0, -6.0, 0) * corner)
    assert set(statuses(shapes, r, "pair")) == {"pass"}
    assert parts_do_not_intersect(check_env(shapes, r, mesh=False)).status == "pass"
    snap, boss = fit_results(shapes, r, "joint")
    assert snap.status == boss.status == "fail"
    assert "different places" in boss.message and "disagree by 10.000 mm" in boss.message
    # The same model with the hole over the boss passes.
    good = resolver()
    shapes = snap_box(good)
    screw = cards.screw_boss("M3", "self_tap", height=28.0, plate=2.0, ctx=good)
    shapes["box"] = screw.apply_a(shapes["box"], at=corner)
    shapes["lid"] = screw.apply_b(shapes["lid"], at=corner)
    assert failures(shapes, good) == []


# --- findings: the material extremes, and interference fits on a guessed printer -----------


def test_the_printed_gap_at_both_extremes_is_computed_from_the_drawing():
    r = resolver()  # guessed defaults: hole offset 0.08 +/- 0.08, peg offset 0.03 +/- 0.04
    press = r.clearance("press", 6.0)
    assert press.gap == pytest.approx(0.06)  # drawn with clearance on a guessed printer
    report = r.robustness(press)
    assert report["printed"]["tight"] == pytest.approx(-0.21)
    assert report["printed"]["loose"] == pytest.approx(0.11)
    assert report["window"] == [-0.15, -0.01] and not report["ok"]
    assert "would not hold" in report["reason"] and "would split the hole" in report["reason"]
    assert (report["offsets_provenance"], report["target_provenance"]) == ("guessed", "guessed")
    # The printer as measured prints the class target: that is what the drawing is for.
    assert r.printed_gap(press, "nominal") == pytest.approx(press.target)
    loose = r.robustness(r.clearance("loose", 6.0))
    assert loose["ok"] and loose["reason"] == ""


def test_an_interference_fit_on_a_guessed_printer_is_a_warning_not_a_pass():
    r = resolver()
    base, cover, _ = peg_parts(r, "press")
    shapes = {"base": base, "cover": cover}
    assert statuses(shapes, r, "pair") == ["pass", "pass"]
    (unproven,) = fit_results(shapes, r, "robustness")
    assert unproven.status == "warn" and unproven.severity == "error"
    assert "not calibrated" in unproven.message and "would not hold" in unproven.message
    assert "print the card's gauge or calibrate the printer" in unproven.message
    (entry,) = unproven.data["unproven"]
    assert entry["fit_class"] == "press" and entry["pairs"] == ["peg_hole_1.radial"]


def test_a_fit_that_leaves_its_class_on_a_measured_printer_fails(tmp_path):
    # Measured, but the hole offset is only known to +/- 0.16 mm (2 sigma).
    wide = write_table(tmp_path, table(hole_offset=quantity(0.10, 0.08)), name="wide.json")
    r = fit.resolver_for(context(calibration_path=str(wide)))
    base, cover, _ = peg_parts(r, "press")
    (failed,) = fit_results({"base": base, "cover": cover}, r, "robustness")
    assert failed.status == "fail" and "not across its spread" in failed.message
    assert "loosest printer" in failed.message and "would not hold" in failed.message
    # A well-measured printer (2 sigma = 0.02 mm) keeps the press fit at both extremes.
    tight = write_table(tmp_path, table(hole_offset=quantity(0.10, 0.01)), name="tight.json")
    r = fit.resolver_for(context(calibration_path=str(tight)))
    base, cover, _ = peg_parts(r, "press")
    (passed,) = fit_results({"base": base, "cover": cover}, r, "robustness")
    assert passed.status == "pass" and "tightest and the loosest printer" in passed.message


def test_a_press_fit_drawn_assembled_is_the_one_overlap_that_is_allowed(tmp_path):
    # A printer whose holes and pegs come out almost as drawn: a press fit is then drawn
    # with the peg larger than the hole.
    exact = write_table(tmp_path, table(hole_offset=quantity(0.01, 0.005),
                                        peg_offset=quantity(0.01, 0.005)), name="exact.json")
    tight = exact
    r = fit.resolver_for(context(calibration_path=str(tight)))
    base, cover, card = peg_parts(r, "press", apart=0.0)
    assert card.radial.gap == pytest.approx(-0.03)
    shapes = {"base": base, "cover": cover}
    env = check_env(shapes, r, mesh=False)
    results = {result.id: result for result in run_checks(env)
               if result.id == "parts_do_not_intersect"}
    overlap = results["parts_do_not_intersect"]
    assert overlap.status == "pass" and "interference" in overlap.message
    (intended,) = overlap.data["intended"]
    assert 0 < intended["volume"] <= intended["intended"] + 1e-3
    assert failures(shapes, r) == []
    # Any other overlap between the same two parts still fails.
    r = fit.resolver_for(context(calibration_path=str(tight)))
    base, cover, _ = peg_parts(r, "press", apart=0.0)
    clash = cover + Pos(10, 10, 8) * Cylinder(2, 4, align=BOTTOM)
    assert parts_do_not_intersect(
        check_env({"base": base, "cover": clash}, r, mesh=False)).status == "fail"


# --- the declared fits as data: what the worker verifies after the model's process --------


def rebuilt(r, declarations) -> fit.FitResolver:
    """A fresh resolver with the same calibration, filled from declared data only (after a
    trip through JSON, as between the model's process and the worker)."""
    fresh = fit.resolver_for(context())
    fresh.restore(json.loads(json.dumps(declarations)))
    return fresh


def test_declared_fits_survive_as_data_and_are_verified_on_the_other_side():
    r = resolver()
    shapes = snap_box(r)
    screw = cards.screw_boss("M3", "self_tap", height=28.0, plate=2.0, ctx=r)
    corner = Pos(37, 27, 30)
    shapes = {"box": screw.apply_a(shapes["box"], at=corner),
              "lid": screw.apply_b(shapes["lid"], at=corner)}
    r.clearance("loose", 12.0)  # taken without a card
    declared = r.declarations()
    assert [card["card"] for card in declared["cards"]] == ["snap_lid", "screw_boss"]
    assert declared["loose"] == [{"fit_class": "loose", "nominal": 12.0, "axis": "xy"}]

    other = rebuilt(r, declared)
    assert other.restored and other.restore_problems == []
    here = [(x.status, x.data.get("kind"), x.message) for x in fit_results(shapes, r, None)]
    there = [(x.status, x.data.get("kind"), x.message) for x in fit_results(shapes, other, None)]
    assert there == here and len(there) == 12 + 2 + 1 + 1
    assert [kind for _, kind, _ in there].count("unverified") == 1


def test_declared_data_cannot_make_a_wrong_solid_pass():
    r = resolver()
    base, cover, card = peg_parts(r)
    declared = r.declarations()
    # The solids the worker loads are not the ones the declaration describes.
    wider = base - Pos(0, 0, 4) * Cylinder(card.hole_radius + 0.1, 6, align=BOTTOM)
    other = rebuilt(r, declared)
    radial = fit_results({"base": wider, "cover": cover}, other, "pair")[0]
    assert radial.status == "fail" and radial.data["difference"] == pytest.approx(0.1, abs=1e-3)
    # A declaration cannot carry a clearance, a probe or a tolerance: only the card's own
    # arguments and where it was placed.
    assert set(declared["cards"][0]) == {"card", "name", "args", "frames"}
    assert set(declared["cards"][0]["args"]) == {"diameter", "length", "fit", "through"}


@pytest.mark.parametrize(("tamper", "needle"), [
    (lambda d: d["cards"][0].update(card="magic_fit"), "not a known card"),
    (lambda d: d["cards"][0]["args"].update(gap=0.5), "arguments are not those of the card"),
    (lambda d: d["cards"][0]["args"].update(ctx="x"), "arguments are not those of the card"),
    (lambda d: d["cards"][0]["args"].update(diameter={"x": 1}), "arguments are not those"),
    (lambda d: d["cards"][0]["args"].update(diameter=-3.0), "could not be rebuilt"),
    (lambda d: d["cards"][0]["frames"].update(a=[1.0] * 12), "rigid placement"),
    (lambda d: d["cards"][0]["frames"].update(a=[2, 0, 0, 0, 0, 2, 0, 0, 0, 0, 2, 0]),
     "rigid placement"),
    (lambda d: d["cards"][0]["frames"].update(a=[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -1, 0]),
     "must not mirror"),
    (lambda d: d["cards"][0]["frames"].update(b=[1, 0, 0, 1e9, 0, 1, 0, 0, 0, 0, 1, 0]),
     "implausibly far"),
    (lambda d: d["cards"][0]["frames"].update(a="here"), "12 finite numbers"),
    (lambda d: d["cards"][0].update(extra=1), "not a card declaration"),
    (lambda d: d.update(cards="none"), "not a list of cards"),
    (lambda d: d.update(note="trust me"), "malformed"),
])
def test_a_declaration_that_cannot_be_rebuilt_is_a_failure_never_a_pass(tamper, needle):
    r = resolver()
    base, cover, _ = peg_parts(r)
    declared = json.loads(json.dumps(r.declarations()))
    tamper(declared)
    other = rebuilt(r, declared)
    assert other.restore_problems and needle in other.restore_problems[0]
    results = fit_results({"base": base, "cover": cover}, other, None)
    assert [x.status for x in results] == ["fail"]
    assert results[0].data["kind"] == "declaration" and needle in results[0].message


def test_a_flood_of_declared_cards_is_cut_off_and_reported():
    r = resolver()
    peg_parts(r)
    declared = json.loads(json.dumps(r.declarations()))
    declared["cards"] = declared["cards"] * 300
    other = fit.resolver_for(context())
    other.restore(declared)
    assert len(other.cards) == 256
    assert "only the first 256 are verified" in other.restore_problems[0]
