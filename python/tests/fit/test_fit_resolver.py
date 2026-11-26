"""The fit resolver: fit classes, calibration, the record of what was handed out."""

from __future__ import annotations

import pytest
from fit_helpers import EXTREMES, context, quantity, resolver, table, write_table

from hotend_fit import calibration, fit
from hotend_fit.fit import FIT_CLASSES, FitError

PROTOCOL_CLASSES = ("press", "snug", "slide", "loose", "snap", "thread_tap", "thread_free")


def test_the_fit_classes_are_those_of_the_protocol():
    assert tuple(FIT_CLASSES) == PROTOCOL_CLASSES
    for spec in FIT_CLASSES.values():
        assert spec.provenance == "guessed" and spec.meaning


def test_a_clearance_is_the_class_target_plus_both_printer_offsets():
    r = resolver()
    entry = calibration.default_entry("pla-basic", 1, 0.4)
    offsets = entry.hole_offset.value + entry.peg_offset.value
    for name, spec in FIT_CLASSES.items():
        c = r.clearance(name, 10.0)
        target = spec.base + spec.per_mm * 10.0
        if spec.floor:
            target = max(target, entry.min_clearance.value)
        assert c.target == pytest.approx(target)
        assert c.gap == pytest.approx(target + offsets)
        assert c.hole_delta + c.peg_delta == pytest.approx(c.gap)
        assert c.diametral == pytest.approx(2 * c.gap)
        assert c.provenance == "guessed" and c.extreme == "nominal" and c.target_source == "class"


def test_printed_targets_have_the_right_sign_and_order():
    r = resolver()
    target = {name: r.clearance(name, 5.0).target for name in FIT_CLASSES}
    assert target["press"] < 0 < target["snug"] <= target["slide"] < target["loose"]
    assert target["snap"] < 0 and target["thread_tap"] < 0 < target["thread_free"]


def test_clearance_classes_never_aim_below_the_printers_minimum_clearance(tmp_path):
    path = write_table(tmp_path, table(min_clearance=quantity(0.25, 0.0)))
    r = fit.resolver_for(context(calibration_path=str(path)))
    assert r.clearance("snug", 5.0).target == 0.25
    assert r.clearance("slide", 5.0).target == 0.25
    assert r.clearance("loose", 5.0).target == pytest.approx(0.31)
    assert r.clearance("press", 5.0).target == pytest.approx(-0.05)  # interference: no floor


def test_thread_classes_follow_the_standards_they_cite():
    """thread_free reproduces ISO 273 medium holes, read here from bd_warehouse's own table;
    thread_tap is a pilot hole of 0.8 x the screw diameter."""
    from bd_warehouse.fastener import read_fastener_parameters_from_csv

    holes = read_fastener_parameters_from_csv("clearance_hole_sizes.csv")
    r = resolver()
    for size in ("M2.5", "M3", "M4", "M5", "M6"):
        diameter = float(size[1:])
        free = r.clearance("thread_free", diameter, printed_peg=False)
        assert diameter + 2 * free.target == pytest.approx(float(holes[size]["Normal"]), abs=0.08)
        tap = r.clearance("thread_tap", diameter, printed_peg=False)
        assert diameter + 2 * tap.target == pytest.approx(0.8 * diameter)


def test_every_clearance_is_recorded_by_identity():
    r = resolver()
    first = r.clearance("slide", 5.0)
    second = r.clearance("slide", 5.0)
    assert first is not second and first.gap == second.gap
    assert [c.index for c in r.issued] == [0, 1] and r.issued[0] is first
    assert r.was_issued(first) and r.was_issued(second)
    assert not r.was_issued(resolver().clearance("slide", 5.0))
    assert r.summary()["issued"][1]["fit_class"] == "slide"
    assert r.declared_fits == [] and r.cards == []


@pytest.mark.parametrize("fit_class", PROTOCOL_CLASSES)
def test_the_tight_printer_gets_the_larger_drawn_gap(fit_class):
    gaps = {extreme: resolver(extreme).clearance(fit_class, 8.0).gap for extreme in EXTREMES}
    assert gaps["tight"] > gaps["nominal"] > gaps["loose"]
    wider = fit.resolver_for(context(fit_extreme="tight", fit_k=3.0)).clearance(fit_class, 8.0)
    assert wider.gap > gaps["tight"] and wider.extreme == "tight"


def test_a_bought_part_gets_no_offset_but_shows_shrinkage():
    r = resolver()
    entry = r.calibration
    both = r.clearance("thread_free", 4.0)
    screw = r.clearance("thread_free", 4.0, printed_peg=False)
    shrink = entry.xy_shrink.value * 4.0 / 2
    assert screw.peg_delta == 0.0
    assert screw.gap == pytest.approx(both.gap - entry.peg_offset.value + shrink)
    sleeve = r.clearance("slide", 4.0, printed_hole=False)
    assert sleeve.hole_delta == pytest.approx(sleeve.target)
    assert sleeve.peg_delta == pytest.approx(entry.peg_offset.value - shrink)
    with pytest.raises(FitError, match="two bought parts"):
        r.clearance("slide", 4.0, printed_hole=False, printed_peg=False)


def test_gaps_along_z_ignore_the_hole_and_peg_offsets():
    r = resolver()
    relief = r.clearance("loose", 10.0, axis="z")
    assert relief.gap == pytest.approx(relief.target) and relief.axis == "z"
    assert relief.hole_delta == pytest.approx(relief.target + r.calibration.z_shrink.value * 5.0)
    with pytest.raises(FitError, match="axis"):
        r.clearance("loose", 10.0, axis="x")


def test_a_datasheet_gap_replaces_the_class_target_and_is_marked():
    r = resolver()
    c = r.clearance("press", 4.6, printed_peg=False, datasheet_gap=-0.25)
    assert c.target == -0.25 and c.target_source == "datasheet"
    assert c.gap == pytest.approx(-0.25 + r.calibration.hole_offset.value
                                  + r.calibration.xy_shrink.value * 2.3)
    for bad in (5.0, float("nan"), "0.1", True):
        with pytest.raises(FitError, match="datasheet_gap"):
            r.clearance("press", 4.6, datasheet_gap=bad)


@pytest.mark.parametrize(("fit_class", "nominal", "needle"), [
    ("free", 5.0, "unknown fit class"),
    ("slide", 0.0, "nominal"),
    ("slide", -3.0, "nominal"),
    ("slide", float("inf"), "nominal"),
    ("slide", "5", "nominal"),
    ("slide", True, "nominal"),
    ("slide", 5000.0, "nominal"),
])
def test_unanswerable_requests_are_refused_and_not_recorded(fit_class, nominal, needle):
    r = resolver()
    with pytest.raises(FitError, match=needle):
        r.clearance(fit_class, nominal)
    assert r.issued == []


def test_resolver_for_reads_the_table_for_the_main_nozzle_and_material(tmp_path):
    path = write_table(tmp_path)
    r = fit.resolver_for(context(calibration_path=str(path)))
    assert r.provenance == "calibrated" and "calibration table" in r.source
    c = r.clearance("slide", 10.0)
    # The offsets are measured; the target of a fit class is a guess until the gauge is printed.
    assert c.offsets_provenance == "calibrated"
    assert c.target_provenance == "guessed"
    assert c.provenance == "guessed"
    # A maker's datasheet gap is not a guess, so with measured offsets nothing is guessed.
    bought = r.clearance("snug", 4.0, printed_peg=False, datasheet_gap=-0.1)
    assert (bought.target_provenance, bought.provenance) == ("standard", "calibrated")
    assert c.gap == pytest.approx(0.16 + 0.06 + 0.02)
    assert c.gap != resolver().clearance("slide", 10.0).gap
    # Another material falls back to the guessed defaults, and says so.
    other = context(calibration_path=str(path))
    other["material"]["id"] = "petg-hf"
    fallback = fit.resolver_for(other)
    assert fallback.provenance == "guessed" and "guessed defaults" in fallback.source
    assert fallback.clearance("slide", 10.0).gap == resolver().clearance("slide", 10.0).gap


def test_a_clearance_is_only_as_proven_as_its_weaker_half(tmp_path):
    """The offsets come from the calibration, the target from the fit class. Class targets
    are guesses until a gauge is printed, so a measured table alone does not make a press
    interference or a snap undercut `calibrated`."""
    for fit_class in fit.FIT_CLASSES:
        assert fit.FIT_CLASSES[fit_class].provenance == "guessed"
    measured = fit.resolver_for(context(calibration_path=str(write_table(tmp_path))))
    for fit_class in ("press", "snap", "slide"):
        c = measured.clearance(fit_class, 10.0)
        assert (c.offsets_provenance, c.target_provenance, c.provenance) == (
            "calibrated", "guessed", "guessed")
        assert c.to_json()["offsets_provenance"] == "calibrated"
        assert c.to_json()["target_provenance"] == "guessed"
    # A maker's datasheet hole is a standard, not a guess.
    insert = measured.clearance("snug", 4.0, printed_peg=False, datasheet_gap=0.0)
    assert (insert.target_provenance, insert.provenance) == ("standard", "calibrated")
    # Without a table both halves are guessed.
    c = resolver().clearance("slide", 10.0)
    assert (c.offsets_provenance, c.target_provenance, c.provenance) == (
        "guessed", "guessed", "guessed")
    insert = resolver().clearance("snug", 4.0, printed_peg=False, datasheet_gap=0.0)
    assert (insert.target_provenance, insert.provenance) == ("standard", "guessed")


def test_resolver_for_defaults_and_context_options():
    bare = fit.resolver_for(None)
    assert bare.provenance == "guessed" and bare.extreme == "nominal" and bare.k == 2.0
    assert fit.resolver_for({}).clearance("slide", 5.0).gap == bare.clearance("slide", 5.0).gap
    with pytest.raises(FitError, match="fit_extreme"):
        fit.resolver_for({"fit_extreme": "average"})
    with pytest.raises(FitError, match="fit_k"):
        fit.resolver_for({"fit_k": -1})
    with pytest.raises(calibration.CalibrationError):
        fit.resolver_for({"printer": {"id": "other"}, "calibration_path": __file__})


def test_the_engine_builds_ctx_fit_from_the_request(tmp_path):
    """The hook the engine uses: make_context -> hotend_fit.fit.resolver_for."""
    from hotend_engine.context import make_context
    from hotend_engine.request import Context

    write_table(tmp_path)
    ctx = make_context(Context.model_validate(context(calibration_path="calibration.json")), tmp_path)
    assert isinstance(ctx.fit, fit.FitResolver) and ctx.fit.provenance == "calibrated"
    assert ctx.declared_fits == []
    assert fit.clearance("slide", 5.0, ctx) is ctx.fit.issued[0]
    assert fit.clearance("slide", 5.0, ctx.fit) is ctx.fit.issued[1]
    extreme = make_context(Context.model_validate(context(fit_extreme="loose")), tmp_path)
    assert extreme.fit.extreme == "loose" and extreme.fit.provenance == "guessed"
    with pytest.raises(FitError, match="pass ctx"):
        fit.clearance("slide", 5.0, None)


def test_a_printer_measured_in_part_is_said_to_be_measured_in_part(tmp_path):
    """The gauge measures hole and peg sizes only. The fit lines then
    name what was measured and what is still general, and never say the printer is not
    measured."""
    from hotend_fit.verify import _basis

    partly = table(xy_shrink=quantity(0.002, 0.0005, "guessed"),
                   z_shrink=quantity(0.001, 0.0005, "guessed"),
                   min_clearance=quantity(0.08, 0.01, "guessed"))
    r = fit.resolver_for(context(calibration_path=str(write_table(tmp_path, partly))))
    c = r.clearance("slide", 10.0)
    assert r.provenance == c.offsets_provenance == "guessed"
    assert c.measured == ("hole_offset", "peg_offset")
    words = _basis(c)
    assert "not measured yet" not in words
    assert "measured hole sizes and peg sizes" in words
    assert "shrinkage across the plate, shrinkage in height and the smallest gap" in words
    # Nothing measured: the old words, which are true then.
    nothing = resolver().clearance("slide", 10.0)
    assert nothing.measured == ()
    assert "not measured yet" in _basis(nothing)
    # Everything measured: the offsets are calibrated, and the words say so.
    full = fit.resolver_for(context(calibration_path=str(write_table(tmp_path, name="full.json"))))
    assert full.clearance("slide", 10.0).measured == calibration.QUANTITIES
    assert "measured offsets" in _basis(full.clearance("slide", 10.0))


def test_the_calibration_state_says_no_more_and_no_less_than_was_measured():
    from hotend_fit.verify import calibration_state

    assert calibration_state(()) == "is not calibrated"
    assert calibration_state(calibration.QUANTITIES) == "is calibrated"
    partly = calibration_state(("hole_offset", "peg_offset"))
    assert partly.startswith("is measured only in part (hole sizes and peg sizes; general")
    assert "not calibrated" not in partly
