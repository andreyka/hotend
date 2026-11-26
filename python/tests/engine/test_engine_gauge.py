"""Gauges and calibration through the worker: the `gauge` op exports the test gauge of
feature cards as a plate of its own, the `calibrate` op turns caliper readings of the
printed gauge into the updated calibration table, and the next clearances follow."""

from __future__ import annotations

import copy
import json

import pytest
from conftest import X2D_CONTEXT, ok, prepare_job, read_3mf, run_worker

from hotend_fit import calibration, cards, fit

PEG = {"card": "peg_hole", "args": {"diameter": 6.0, "length": 6.0, "fit": "slide"}}
MEASURED_AT = "2026-11-17T09:00:00Z"


def test_the_gauge_of_named_cards_is_a_plate_of_its_own(plates):
    job = plates["gauge_of_cards"]
    result = ok(job)
    assert result["op"] == "gauge" and not result["unplaced"] and not result["warnings"]
    (gauge,) = result["gauges"]
    assert gauge["card"] == "peg_hole" and gauge["name"] == "peg_hole_1"
    assert gauge["args"] == {"diameter": 6.0, "length": 6.0, "fit": "slide", "through": False}
    assert gauge["parts"] == ["peg_hole_1.hole", "peg_hole_1.peg"]
    # What to measure with a caliper, and on which object of the plate.
    hole, peg = gauge["dimensions"]
    assert (hole["id"], hole["feature"], hole["object"]) == (
        "hole_diameter", "hole", "peg_hole_1.hole")
    assert (peg["id"], peg["feature"], peg["object"]) == (
        "peg_diameter", "peg", "peg_hole_1.peg")
    assert hole["what"] and peg["what"]
    radial = next(c for c in gauge["clearances"] if c["pair"] == "radial")
    assert radial["fit_class"] == "slide" and radial["provenance"] == "guessed"
    # The drawn sizes differ by the drawn gap, on the diameter.
    assert hole["drawn"] - peg["drawn"] == pytest.approx(2 * radial["gap"], abs=1e-9)

    (plate,) = result["plates"]
    assert plate["threemf"] == "plates/1/plate.3mf" and "views" not in plate
    objects = read_3mf(job.dir / plate["threemf"])
    assert set(objects) == set(gauge["parts"])
    for item in plate["items"]:
        assert item["bbox"]["min"][2] == 0.0 and item["nozzle_id"] == 1
        assert objects[item["name"]]["min"] == pytest.approx(item["bbox"]["min"], abs=0.02)
        assert objects[item["name"]]["volume"] > 100


def test_the_gauges_of_every_card_a_built_model_uses(builds, plates):
    job = plates["gauge_of_model"]
    result = ok(job)
    used = {entry["card"] for entry in result["gauges"]}
    assert used == {"snap_lid", "screw_boss"}
    # Four equal screw bosses are tested by one gauge.
    assert len(result["gauges"]) == 2
    names = [name for gauge in result["gauges"] for name in gauge["parts"]]
    assert len(names) == len(set(names)) == 4
    (plate,) = result["plates"]
    assert [item["name"] for item in plate["items"]] == names
    assert set(read_3mf(job.dir / plate["threemf"])) == set(names)
    assert plate["views"] == {"top": "plates/1/views/top.png"}
    assert (job.dir / plate["views"]["top"]).read_bytes()[:8] == b"\x89PNG\r\n\x1a\n"
    features = {}
    for gauge in result["gauges"]:
        features[gauge["card"]] = sorted(d["feature"] for d in gauge["dimensions"])
        assert all(d["object"] in gauge["parts"] and d["drawn"] > 0
                   for d in gauge["dimensions"])
    # The snap lid has an opening and a rail; a screw boss has two holes and no peg.
    assert features == {"snap_lid": ["hole", "peg"], "screw_boss": ["hole", "hole"]}
    # No model code ran: the job folder holds the request, the built model and the plate.
    assert "model.py" not in job.files()


def test_measuring_a_printed_gauge_moves_the_next_clearances_by_what_was_found(plates, tmp_path):
    """The round trip: gauge, simulated caliper readings with a known error, calibrate, and
    the resolver's next clearances (and the next gauge) move by exactly that error."""
    (gauge,) = ok(plates["gauge_of_cards"])["gauges"]
    before = fit.resolver_for(copy.deepcopy(X2D_CONTEXT))
    assumed = calibration.default_entry("pla-basic", 1, 0.4)
    # The simulated printer: its holes come out 0.07 mm per side tighter than the guessed
    # table assumes, its pegs 0.02 mm per side thinner.
    hole_error, peg_error = 0.07, -0.02
    true_hole = assumed.hole_offset.value + hole_error
    true_peg = assumed.peg_offset.value + peg_error
    readings = []
    for dimension in gauge["dimensions"]:
        printed = (dimension["drawn"] - 2 * true_hole if dimension["feature"] == "hole"
                   else dimension["drawn"] + 2 * true_peg)
        readings.append({"feature": dimension["feature"], "drawn": dimension["drawn"],
                         "measured": round(printed, 3)})

    request = {"protocol": 0, "op": "calibrate", "context": copy.deepcopy(X2D_CONTEXT),
               "measured_at": MEASURED_AT, "readings": readings}
    calibrated = ok(run_worker(prepare_job(tmp_path / "calibrate", request)))
    changes = {change["quantity"]: change for change in calibrated["changes"]}
    assert changes["hole_offset"]["before"] == assumed.hole_offset.value
    assert changes["hole_offset"]["after"] == pytest.approx(true_hole, abs=1e-9)
    assert changes["peg_offset"]["after"] == pytest.approx(true_peg, abs=1e-9)
    (entry,) = calibrated["table"]["entries"]
    for name in ("hole_offset", "peg_offset"):
        assert entry[name]["provenance"] == "measured"
        assert entry[name]["measured_at"] == MEASURED_AT  # the caller's date, not a clock
    # What the gauge does not measure stays a guess, and the entry says so.
    assert entry["xy_shrink"]["provenance"] == "guessed"
    assert calibrated["entry_provenance"] == "guessed"

    # The table is valid as the next request's calibration: the resolver's next clearance
    # of the same fit moves by the error that was found, per side.
    folder = tmp_path / "gauge_again"
    job_dir = prepare_job(folder, {
        "protocol": 0, "op": "gauge", "cards": [PEG], "views": [], "threemf": False,
        "context": {**copy.deepcopy(X2D_CONTEXT), "calibration_path": "calibration.json"}})
    (job_dir / "calibration.json").write_text(json.dumps(calibrated["table"]), encoding="utf-8")
    after = fit.resolver_for({**copy.deepcopy(X2D_CONTEXT),
                              "calibration_path": str(job_dir / "calibration.json")})
    old, new = (r.clearance("slide", 6.0) for r in (before, after))
    assert new.hole_delta - old.hole_delta == pytest.approx(hole_error, abs=1e-9)
    assert new.peg_delta - old.peg_delta == pytest.approx(peg_error, abs=1e-9)
    assert new.gap - old.gap == pytest.approx(hole_error + peg_error, abs=1e-9)
    assert new.target == old.target  # the printed gap aimed for is the same
    assert new.offsets_provenance == "guessed"  # the weakest quantity of the entry

    # ... and the next gauge is drawn with it: a hole 0.14 mm wider, a peg 0.04 mm thicker.
    (again,) = ok(run_worker(job_dir))["gauges"]
    drawn = {d["id"]: d["drawn"] for d in again["dimensions"]}
    first = {d["id"]: d["drawn"] for d in gauge["dimensions"]}
    assert drawn["hole_diameter"] - first["hole_diameter"] == pytest.approx(2 * hole_error,
                                                                            abs=1e-6)
    assert drawn["peg_diameter"] - first["peg_diameter"] == pytest.approx(-2 * peg_error,
                                                                          abs=1e-6)
    # A second gauge printed on the same simulated printer now measures as the table says:
    # calibrating again changes nothing.
    second = [{"feature": d["feature"], "drawn": d["drawn"],
               "measured": d["drawn"] - 2 * true_hole if d["feature"] == "hole"
               else d["drawn"] + 2 * true_peg} for d in again["dimensions"]]
    _table, changes = calibration.calibrate_from_gauge(
        job_dir / "calibration.json", second, measured_at=MEASURED_AT,
        printer_id="bambu-x2d", material="pla-basic", nozzle_id=1, nozzle_diameter=0.4)
    assert all(change["after"] == pytest.approx(change["before"], abs=1e-6)
               for change in changes)


def test_the_calibrate_op_averages_readings_and_reports_the_change(plates):
    result = ok(plates["calibrate"])
    assert result["op"] == "calibrate"
    hole, peg = result["changes"]
    # Two hole readings: (6.4 - 6.2) / 2 and (6.4 - 6.16) / 2; one peg reading.
    assert hole == {"quantity": "hole_offset", "before": 0.08, "after": 0.11, "sigma": 0.02,
                    "readings": 2}
    assert peg == {"quantity": "peg_offset", "before": 0.03, "after": 0.05, "sigma": 0.04,
                   "readings": 1}
    assert result["table"]["printer_id"] == "bambu-x2d"
    (entry,) = result["table"]["entries"]
    assert (entry["material"], entry["nozzle_id"], entry["nozzle_diameter"]) == (
        "pla-basic", 1, 0.4)
    # The table that comes back loads as a calibration table.
    calibration.CalibrationTable.model_validate(result["table"])
    assert plates["calibrate"].files() == {"request.json", "result.json"}


@pytest.mark.parametrize(("name", "fragment"), [
    ("calibrate_misread", "implausible calibration"),
    ("calibrate_without_date", "calibrate.measured_at: Field required"),
    # The card is named by its place in the request; its own text is not repeated.
    ("gauge_unknown_card", "card 1 is not a known card; known: " + ", ".join(cards.CARDS)),
    ("gauge_without_cards", "no feature cards to make gauges of"),
])
def test_bad_gauge_and_calibrate_requests_are_refused(plates, name, fragment):
    job = plates[name]
    assert job.returncode == 2
    assert job.result["error"]["type"] == "bad_request"
    assert fragment in job.result["error"]["message"]
