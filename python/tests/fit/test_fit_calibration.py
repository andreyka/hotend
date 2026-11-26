"""The calibration table: strict loading, guessed defaults, material extremes."""

from __future__ import annotations

import copy
import json

import pytest
from fit_helpers import quantity, resolver, table, write_table

from hotend_fit import calibration
from hotend_fit.calibration import CalibrationError


def test_defaults_are_guessed_and_documented():
    entry = calibration.default_entry("pla-basic", 1, 0.4)
    assert entry.provenance == "guessed"
    for name in calibration.QUANTITIES:
        value = getattr(entry, name)
        assert value.provenance == "guessed" and value.measured_at is None and value.sigma > 0
    assert entry.hole_offset.value == pytest.approx(0.08)
    assert entry.peg_offset.value == pytest.approx(0.03)
    assert entry.min_clearance.value == pytest.approx(0.10)
    assert entry.xy_shrink.value == pytest.approx(0.003)
    assert entry.z_shrink.value == pytest.approx(0.003)


def test_default_offsets_scale_with_the_nozzle_but_shrinkage_does_not():
    fine, coarse = calibration.default_entry(None, None, 0.2), calibration.default_entry(None, None, 0.8)
    assert coarse.hole_offset.value == pytest.approx(4 * fine.hole_offset.value)
    assert coarse.min_clearance.sigma == pytest.approx(4 * fine.min_clearance.sigma)
    assert coarse.xy_shrink.value == fine.xy_shrink.value


def test_a_valid_table_loads_and_is_calibrated(tmp_path):
    path = write_table(tmp_path)
    entry, source = calibration.entry_for(
        path, printer_id="bambu-x2d", material="pla-basic", nozzle_id=1, nozzle_diameter=0.4
    )
    assert entry.provenance == "calibrated"
    assert entry.hole_offset.value == 0.06 and entry.hole_offset.sigma == 0.01
    assert entry.hole_offset.measured_at.year == 2026
    assert "bambu-x2d" in source


def test_one_guessed_quantity_makes_the_entry_guessed(tmp_path):
    path = write_table(tmp_path, table(z_shrink=quantity(0.003, 0.002, "guessed")))
    entry, _ = calibration.entry_for(path, material="pla-basic", nozzle_id=1)
    assert entry.provenance == "guessed"


def test_missing_table_or_entry_falls_back_to_guessed_defaults(tmp_path):
    entry, source = calibration.entry_for(None, material="pla-basic", nozzle_id=1)
    assert entry == calibration.default_entry("pla-basic", 1) and "guessed defaults" in source
    entry, source = calibration.entry_for(tmp_path / "absent.json", material="pla-basic")
    assert entry.provenance == "guessed" and "no calibration table" in source

    path = write_table(tmp_path)
    for material, nozzle in (("petg-hf", 1), ("pla-basic", 0)):
        entry, source = calibration.entry_for(path, material=material, nozzle_id=nozzle,
                                              nozzle_diameter=0.4)
        assert entry.provenance == "guessed" and "no entry" in source
        assert entry.hole_offset.value == pytest.approx(0.08)


@pytest.mark.parametrize("fitted", [0.2, 0.6, 0.8])
def test_an_entry_measured_with_another_nozzle_size_is_not_used(tmp_path, fitted):
    """After a nozzle swap on the same nozzle id the old offsets say nothing: the build falls
    back to the guessed defaults for the fitted size and no longer calls itself calibrated."""
    path = write_table(tmp_path)  # the entry was measured with a 0.4 mm nozzle
    entry, source = calibration.entry_for(path, printer_id="bambu-x2d", material="pla-basic",
                                          nozzle_id=1, nozzle_diameter=fitted)
    assert entry.provenance == "guessed"
    assert entry == calibration.default_entry("pla-basic", 1, fitted)
    assert "measured with a 0.4 mm nozzle" in source and f"{fitted:g} mm nozzle is fitted" in source
    assert "guessed defaults" in source
    # The size it was measured with (and no size at all, as before) still gets the entry.
    for same in (0.4, 0.4 + 1e-9, None):
        entry, source = calibration.entry_for(path, material="pla-basic", nozzle_id=1,
                                              nozzle_diameter=same)
        assert entry.provenance == "calibrated" and "calibration table" in source


def test_a_nozzle_swap_reaches_the_resolver_and_every_clearance(tmp_path):
    from fit_helpers import context

    from hotend_fit import fit

    path = write_table(tmp_path)
    swapped = context(calibration_path=str(path))
    swapped["printer"]["nozzles"][0]["diameter"] = 0.8
    r = fit.resolver_for(swapped)
    assert r.provenance == "guessed" and "0.8 mm nozzle is fitted" in r.source
    slide = r.clearance("slide", 6.0)
    assert slide.offsets_provenance == "guessed"
    without_table = context()
    without_table["printer"]["nozzles"][0]["diameter"] = 0.8
    assert slide.gap == pytest.approx(fit.resolver_for(without_table).clearance("slide", 6.0).gap)
    assert slide.gap == pytest.approx(0.42)  # not the 0.236 mm the 0.4 mm entry would give


def test_a_table_for_another_printer_is_refused(tmp_path):
    path = write_table(tmp_path)
    with pytest.raises(CalibrationError, match="is for printer 'bambu-x2d'"):
        calibration.entry_for(path, printer_id="prusa-core-one", material="pla-basic", nozzle_id=1)


def _broken(change) -> dict:
    data = copy.deepcopy(table())
    change(data)
    return data


def _set(path: list, value):
    def change(data):
        target = data
        for key in path[:-1]:
            target = target[key]
        target[path[-1]] = value
    return change


ENTRY = ["entries", 0]
BROKEN = {
    "unknown key in the table": (_set(["colour"], "red"), "colour"),
    "unknown key in an entry": (_set([*ENTRY, "bridge_offset"], quantity(0.1, 0.1)), "bridge_offset"),
    "unknown key in a quantity": (_set([*ENTRY, "hole_offset", "unit"], "mm"), "unit"),
    "number written as text": (_set([*ENTRY, "hole_offset", "value"], "0.06"), "hole_offset.value"),
    "boolean for a number": (_set([*ENTRY, "peg_offset", "sigma"], True), "peg_offset.sigma"),
    "negative sigma": (_set([*ENTRY, "peg_offset", "sigma"], -0.01), "peg_offset.sigma"),
    "implausible offset": (_set([*ENTRY, "hole_offset", "value"], 3.0), "outside"),
    "implausible shrinkage": (_set([*ENTRY, "xy_shrink", "value"], 0.2), "outside"),
    "negative minimum clearance": (_set([*ENTRY, "min_clearance", "value"], -0.1), "outside"),
    "measured without a date": (_set([*ENTRY, "hole_offset", "measured_at"], None), "measured_at"),
    "unknown provenance": (_set([*ENTRY, "hole_offset", "provenance"], "hearsay"), "provenance"),
    "missing quantity": (lambda data: data["entries"][0].pop("z_shrink"), "z_shrink"),
    "wrong protocol": (_set(["protocol"], 7), "protocol"),
    "missing printer id": (lambda data: data.pop("printer_id"), "printer_id"),
    "duplicate entry": (lambda data: data["entries"].append(copy.deepcopy(data["entries"][0])),
                        "two entries"),
    "zero nozzle": (_set([*ENTRY, "nozzle_diameter"], 0), "nozzle_diameter"),
}


@pytest.mark.parametrize("case", BROKEN)
def test_a_malformed_table_is_rejected_whole(tmp_path, case):
    change, needle = BROKEN[case]
    path = write_table(tmp_path, _broken(change))
    with pytest.raises(CalibrationError, match=needle):
        calibration.load_table(path)
    with pytest.raises(CalibrationError):
        calibration.entry_for(path, material="pla-basic", nozzle_id=1)


@pytest.mark.parametrize("text", ["", "{", "[1, 2]", "null",
                                  json.dumps(table()).replace("0.06", "NaN")])
def test_text_that_is_not_a_table_is_rejected(tmp_path, text):
    with pytest.raises(CalibrationError, match="not valid"):
        calibration.load_table(write_table(tmp_path, text))


def test_an_oversized_table_is_rejected_without_parsing(tmp_path):
    path = write_table(tmp_path, " " * (calibration.MAX_TABLE_BYTES + 1))
    with pytest.raises(CalibrationError, match="larger than"):
        calibration.load_table(path)


def test_extremes_are_value_plus_and_minus_k_sigma(tmp_path):
    entry, _ = calibration.entry_for(write_table(tmp_path), material="pla-basic", nozzle_id=1)
    tight, loose = calibration.extremes(entry, k=2.0)
    for name in calibration.QUANTITIES:
        base = getattr(entry, name)
        assert getattr(tight, name).value == pytest.approx(base.value + 2 * base.sigma)
        assert getattr(loose, name).value == pytest.approx(base.value - 2 * base.sigma)
        assert getattr(tight, name).sigma == base.sigma
    assert entry.at_extreme("nominal") is entry
    wide, narrow = calibration.extremes(entry, k=3.0)
    assert wide.hole_offset.value == pytest.approx(0.09) and narrow.hole_offset.value == pytest.approx(0.03)
    assert tight.provenance == "calibrated"


def test_extremes_stay_inside_the_plausible_range():
    entry = calibration.default_entry()
    _, loose = calibration.extremes(entry, k=10.0)
    assert loose.min_clearance.value == 0.0  # a gap cannot be negative
    tight, _ = calibration.extremes(entry, k=1000.0)
    assert tight.hole_offset.value == 1.0 and tight.xy_shrink.value == 0.05
    with pytest.raises(ValueError, match="extreme must be"):
        entry.at_extreme("snug")
    with pytest.raises(ValueError, match="k must be"):
        entry.at_extreme("tight", -1)


# --- calibration from a printed gauge ----------------------------------------------------------

GAUGE_AT = "2026-11-17T09:00:00Z"
WHO = {"printer_id": "bambu-x2d", "material": "pla-basic", "nozzle_id": 1,
       "nozzle_diameter": 0.4}


def test_a_gauge_reading_is_an_offset_per_side():
    # A hole drawn 6.40 that measures 6.20 came out 0.10 mm per side too tight.
    assert calibration.gauge_offset("hole", 6.4, 6.2) == pytest.approx(0.1)
    # A peg drawn 6.00 that measures 6.10 came out 0.05 mm per side too thick.
    assert calibration.gauge_offset("peg", 6.0, 6.1) == pytest.approx(0.05)
    assert calibration.gauge_offset("peg", 6.0, 5.9) == pytest.approx(-0.05)
    for bad in (("slot", 6.0, 6.0), ("hole", 0.0, 6.0), ("hole", 6.0, float("nan")),
                ("peg", True, 6.0), ("peg", 6.0, "6.1")):
        with pytest.raises(CalibrationError):
            calibration.gauge_offset(*bad)


def test_gauge_readings_replace_only_what_they_measure(tmp_path):
    path = write_table(tmp_path)  # a calibrated table: hole 0.06, peg 0.02
    readings = [{"feature": "hole", "drawn": 6.4, "measured": 6.2},
                {"feature": "hole", "drawn": 10.4, "measured": 10.24}]
    table_, changes = calibration.calibrate_from_gauge(path, readings, measured_at=GAUGE_AT,
                                                       **WHO)
    (change,) = changes
    assert change["quantity"] == "hole_offset" and change["before"] == 0.06
    assert change["after"] == pytest.approx(0.09) and change["readings"] == 2
    entry = table_.find("pla-basic", 1)
    assert entry.hole_offset.value == 0.09 and entry.hole_offset.provenance == "measured"
    # Two readings 0.02 apart: their own spread, never less than a caliper resolves.
    assert entry.hole_offset.sigma == pytest.approx(0.02)
    assert entry.hole_offset.measured_at.isoformat().startswith("2026-11-17T09:00:00")
    # The peg was not measured: its calibrated value stays, with its own date.
    assert entry.peg_offset.value == 0.02 and entry.peg_offset.provenance == "calibrated"
    assert entry.provenance == "calibrated"  # nothing in the entry is a guess
    # The file is not written by the function; the caller stores the table it gets back.
    assert calibration.load_table(path).find("pla-basic", 1).hole_offset.value == 0.06
    stored = write_table(tmp_path, calibration.table_json(table_), "new.json")
    assert calibration.load_table(stored) == table_


def test_a_gauge_starts_a_table_and_keeps_other_entries(tmp_path):
    table_, changes = calibration.calibrate_from_gauge(
        None, [{"feature": "peg", "drawn": 6.0, "measured": 6.1}], measured_at=GAUGE_AT, **WHO)
    (entry,) = table_.entries
    assert entry.peg_offset.value == 0.05 and entry.peg_offset.sigma == 0.04
    assert entry.hole_offset.provenance == "guessed" and entry.provenance == "guessed"
    assert changes[0]["before"] == calibration.default_entry("pla-basic", 1, 0.4).peg_offset.value

    # Another material's readings add an entry beside the first.
    stored = write_table(tmp_path, calibration.table_json(table_))
    both, _ = calibration.calibrate_from_gauge(
        stored, [{"feature": "hole", "drawn": 6.4, "measured": 6.3}], measured_at=GAUGE_AT,
        **{**WHO, "material": "petg-basic"})
    assert [(e.material, e.nozzle_id) for e in both.entries] == [
        ("pla-basic", 1), ("petg-basic", 1)]
    assert both.find("pla-basic", 1) == entry


@pytest.mark.parametrize(("readings", "kwargs", "fragment"), [
    ([], {}, "needs 1 to 64 gauge readings"),
    ([{"feature": "hole", "drawn": 6.4}], {}, "a gauge reading is {feature, drawn, measured}"),
    ([{"feature": "hole", "drawn": 6.4, "measured": 0.62}], {}, "implausible calibration"),
    ([{"feature": "hole", "drawn": 6.4, "measured": 6.2}], {"measured_at": "yesterday"},
     "measured_at must be the date and time"),
    ([{"feature": "hole", "drawn": 6.4, "measured": 6.2}], {"printer_id": "other-printer"},
     "is for printer 'bambu-x2d'"),
])
def test_unusable_gauge_readings_are_refused(tmp_path, readings, kwargs, fragment):
    options = {"measured_at": GAUGE_AT, **WHO, **kwargs}
    with pytest.raises(CalibrationError) as caught:
        calibration.calibrate_from_gauge(write_table(tmp_path), readings, **options)
    assert fragment in str(caught.value)


def test_every_card_says_what_to_measure_on_its_gauge():
    from hotend_fit import cards

    r = resolver()
    made = {
        "peg_hole": cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=r),
        "snap_lid": cards.snap_lid(opening=(60.0, 40.0), wall=2.0, ctx=r),
        "screw_boss": cards.screw_boss("M3", "self_tap", height=20.0, plate=2.4, ctx=r),
    }
    assert set(made) == set(cards.CARDS)
    for kind, card in made.items():
        gauge = card.gauge_set()
        # (The gauge of a snap lid is a short piece of it: its sizes are the gauge's own.)
        assert gauge.dimensions, kind
        for dimension in gauge.dimensions:
            assert set(dimension) == {"id", "half", "feature", "drawn", "what"}
            assert dimension["feature"] in ("hole", "peg") and dimension["drawn"] > 0
            assert dimension["half"] in (card.half_a, card.half_b)
