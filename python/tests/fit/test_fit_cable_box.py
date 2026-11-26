"""The end-to-end example: tests/models/cable_box.py through the real engine worker, its
CONTRACT, and the range sampler over its declared ranges."""

from __future__ import annotations

import ast
import functools
import json
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest
from fit_helpers import MODELS, X2D_CONTEXT, context, table, write_table

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from result_contract import check_job_result

from hotend_engine.model import load_model
from hotend_fit import fit
from hotend_fit.contract import open_questions, parse_contract
from hotend_fit.sampler import sample_ranges

CABLE_BOX = MODELS / "cable_box.py"
WORKER = (sys.executable, "-I", "-m", "hotend_engine.worker")


def run_job(job_dir: Path, model: Path, with_table: bool = False, **overrides) -> dict:
    """One build the way the core runs it: job folder, request.json, isolated worker."""
    job_dir.mkdir(parents=True)
    shutil.copy(model, job_dir / "model.py")
    if with_table:
        write_table(job_dir)
    request = {
        "protocol": 0, "op": "build", "model_path": "model.py", "params": {},
        "context": context(**({"calibration_path": "calibration.json"} if with_table else {})),
        "outputs": {"step": True, "brep": True, "mesh": True, "views": []},
        "checks": {"enabled": True, "min_wall": 0.8, "verify_determinism": True},
    }
    request.update(overrides)
    (job_dir / "request.json").write_text(json.dumps(request), encoding="utf-8")
    done = subprocess.run([*WORKER, str(job_dir)], capture_output=True, text=True, timeout=120,
                          cwd=job_dir.parent, check=False)
    assert done.returncode == 0, done.stderr[-2000:]
    check_job_result(job_dir)
    return json.loads((job_dir / "result.json").read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def jobs(tmp_path_factory) -> dict[str, dict]:
    """Every worker run this module looks at, started together."""
    root = tmp_path_factory.mktemp("fit_jobs")
    wrong = MODELS / "fit_wrong_clearance.py"
    specs = {
        "default": lambda: run_job(root / "default", CABLE_BOX),
        "calibrated": lambda: run_job(root / "calibrated", CABLE_BOX, with_table=True),
        "wrong": lambda: run_job(root / "wrong", wrong),
        "wrong_fixed": lambda: run_job(root / "wrong_fixed", wrong, params={"extra": 0.0}),
    }
    with ThreadPoolExecutor(max_workers=4) as pool:
        return dict(zip(specs, pool.map(lambda start: start(), specs.values()), strict=True))


def fit_rows(result: dict, kind: str) -> list[dict]:
    return [c for c in result["checks"] if c["id"] == "fit_pairs" and c["data"]["kind"] == kind]


def fit_checks(result: dict) -> dict[str, dict]:
    """The measured clearances: one `fit_pairs` row per mating pair of a card."""
    return {f"{c['data']['instance']}.{c['data']['pair']}": c for c in fit_rows(result, "pair")}


def test_the_cable_box_builds_and_passes_every_error_check(jobs):
    result = jobs["default"]
    assert result["status"] == "ok" and result["error"] is None and result["warnings"] == []
    assert [p["name"] for p in result["parts"]] == ["box", "lid"]
    assert all(p["solids"] == 1 and p["is_valid"] and p["is_manifold"] for p in result["parts"])
    blocking = [c for c in result["checks"] if c["status"] == "fail"]
    assert blocking == []
    # The one thing short of a pass: on a printer nobody measured the fits are unproven.
    (unproven,) = [c for c in result["checks"]
                   if c["status"] == "warn" and c["id"] == "fit_pairs"]
    assert unproven["data"]["kind"] == "robustness"
    assert "not calibrated" in unproven["message"] and "snap" in unproven["message"]
    assert {"step", "brep", "mesh"} <= set(result["artifacts"])


def test_every_fit_of_the_cable_box_is_measured_on_the_solids(jobs):
    checks = fit_checks(jobs["default"])
    assert sorted(checks) == sorted(
        [f"snap_lid_1.{pair}_{side}" for pair in ("wall", "snap", "seat", "end", "locate")
         for side in ("plus", "minus")] + ["screw_boss_1.thread", "screw_boss_1.screw"])
    expected = fit.resolver_for(context())
    for check in checks.values():
        data = check["data"]
        assert check["status"] == "pass" and check["severity"] == "error"
        assert data["measured"] == pytest.approx(data["expected"], abs=1e-3)
        assert data["provenance"] == "guessed" and "not measured yet" in check["message"]
        assert data["offsets_provenance"] == "guessed"
    # The numbers are the resolver's for this printer, not ones the model could have typed.
    assert checks["snap_lid_1.wall_plus"]["data"]["expected"] == pytest.approx(
        expected.clearance("slide", 60.0).gap, abs=1e-4)
    assert checks["snap_lid_1.snap_minus"]["data"]["measured"] == pytest.approx(
        expected.clearance("snap", 60.0).gap, abs=1e-3)
    assert checks["snap_lid_1.snap_minus"]["data"]["measured"] < 0
    assert checks["screw_boss_1.thread"]["data"]["measured"] == pytest.approx(
        expected.clearance("thread_tap", 3.0, printed_peg=False).gap, abs=1e-3)
    assert checks["screw_boss_1.screw"]["data"]["parts"] == [None, "lid"]
    assert checks["snap_lid_1.wall_plus"]["data"]["parts"] == ["box", "lid"]
    # Each card's joint was also put together: the lid reaches its place without colliding.
    joints = fit_rows(jobs["default"], "joint")
    assert [(j["data"]["instance"], j["status"]) for j in joints] == [
        ("snap_lid_1", "pass"), ("screw_boss_1", "pass")]
    assert all("goes together" in j["message"] for j in joints)


def test_a_calibration_table_changes_both_halves_without_touching_the_model(jobs):
    default, calibrated = fit_checks(jobs["default"]), fit_checks(jobs["calibrated"])
    assert jobs["calibrated"]["status"] == "ok" and sorted(calibrated) == sorted(default)
    # Slide at 60 mm (0.15 + 0.001 * 60) plus the table's hole and peg offsets.
    table_resolver_gap = 0.21 + 0.06 + 0.02
    for name, check in calibrated.items():
        data = check["data"]
        assert check["status"] == "pass"
        # Measured offsets, but the target of a fit class is a guess until a gauge is
        # printed: the row is not labelled `calibrated`, and says which half is which.
        assert data["offsets_provenance"] == "calibrated"
        assert data["target_provenance"] == "guessed" and data["provenance"] == "guessed"
        assert "offsets calibrated, target guessed" in check["message"]
        assert data["measured"] == pytest.approx(data["expected"], abs=1e-3)
        assert abs(data["expected"] - default[name]["data"]["expected"]) > 0.01
    assert calibrated["snap_lid_1.wall_plus"]["data"]["measured"] == pytest.approx(
        table_resolver_gap, abs=1e-3)
    entry = table()["entries"][0]
    assert (entry["hole_offset"]["value"], entry["peg_offset"]["value"]) == (0.06, 0.02)
    # On this well-measured printer every clearance keeps its class at both extremes.
    (robust,) = fit_rows(jobs["calibrated"], "robustness")
    assert robust["status"] == "pass" and "tightest and the loosest" in robust["message"]
    assert [c for c in jobs["calibrated"]["checks"]
            if c["status"] == "warn" and c["id"] == "fit_pairs"] == []


def test_a_deliberately_wrong_clearance_fails_fit_pairs_with_numbers(jobs):
    result = jobs["wrong"]
    assert result["status"] == "check_error"
    checks = fit_checks(result)
    radial, relief = checks["peg_hole_1.radial"], checks["peg_hole_1.relief"]
    assert radial["status"] == "fail" and relief["status"] == "pass"
    data = radial["data"]
    assert data["difference"] == pytest.approx(0.1, abs=1e-3)
    assert data["measured"] == pytest.approx(data["expected"] + 0.1, abs=1e-3)
    assert f"{data['measured']:.3f} mm" in radial["message"]
    assert f"{data['expected']:.3f} mm" in radial["message"]
    # The same script without the hand-typed bore is fine.
    fixed = jobs["wrong_fixed"]
    assert fixed["status"] == "ok"
    assert [c["status"] for c in fit_checks(fixed).values()] == ["pass", "pass"]


def contract_of(path: Path) -> dict:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    (node,) = [n for n in tree.body if isinstance(n, ast.Assign)
               and any(getattr(t, "id", None) == "CONTRACT" for t in n.targets)]
    return ast.literal_eval(node.value)


def test_the_cable_box_contract_validates_and_asks_about_its_guesses(jobs):
    contract = parse_contract(contract_of(CABLE_BOX))
    assert list(contract.parts) == [p["name"] for p in jobs["default"]["parts"]]
    assert {r.kind for r in contract.requirements} == {"cavity_min", "dimension", "fit", "wall_min"}
    specs = load_model(CABLE_BOX).param_specs
    for requirement in contract.requirements:
        assert set(requirement.params) <= set(specs)
    questions = {q.requirement: q for q in open_questions(contract, specs)}
    # Guessed: the size of the power strip, the cable slot, and a snap fit nobody has printed.
    assert sorted(questions) == ["cables_pass", "holds_power_strip", "lid_snaps"]
    assert questions["holds_power_strip"].guessed_params == ("width", "depth")
    assert "fit gauge" in questions["lid_snaps"].text
    # The standard screw and the style choices are not asked about.
    assert "lid_screwed" not in questions and "walls_print" not in questions


@functools.cache
def declared_ranges_report() -> dict:
    return sample_ranges(CABLE_BOX, X2D_CONTEXT, seed=1)


def test_the_cable_box_survives_the_sampler_over_its_declared_ranges():
    report = declared_ranges_report()
    assert report["failures"] == [] and report["verified"] and not report["truncated"]
    # Nothing failed, and still no plain pass: the printer is not calibrated, so the snap
    # and snug fits are unproven at the extremes of the guessed spread.
    assert report["status"] == "warn" and not report["fits"]["proven"]
    assert report["fits"]["unproven_settings"] == report["samples"]["settings"]
    assert "snap" in report["fits"]["example"]
    specs = load_model(CABLE_BOX).param_specs
    assert report["passed_ranges"] == {
        name: [specs[name]["min"], specs[name]["max"]]
        for name in ("width", "depth", "height", "wall")
    }
    samples = report["samples"]
    assert samples["declared_corners_checked"] == samples["declared_corners_total"] == 16
    # The default, 9 values of each of 4 sliders, 16 corners and 16 inner points.
    assert samples["builds"] == samples["settings"] == 69 and samples["failed"] == 0
    assert report["skipped_checks"] == [] and report["clamped"] == []


# What fails just outside each declared range (the comments in the model say the same):
# the range to sample, the declared range and its widened replacement, the first check that
# fails, and a value that passes although it is outside the declared range.
OUTSIDE = {
    "wall": ((1.0, 2.0), '"min": 1.6, "max": 3.0', '"min": 1.0, "max": 3.0', "min_wall"),
    "height": ((18.0, 40.0), '"min": 26.0, "max": 80.0', '"min": 18.0, "max": 80.0',
               "fit_pairs"),
    "depth": ((4.0, 60.0), '"min": 30.0, "max": 80.0', '"min": 4.0, "max": 80.0',
              "build:CardError"),
    "width": ((120.0, 256.0), '"min": 60.0, "max": 200.0', '"min": 60.0, "max": 256.0',
              "fits_build_volume"),
}


@pytest.mark.parametrize("name", OUTSIDE)
def test_outside_the_declared_ranges_the_sampler_finds_what_breaks(name, tmp_path):
    span, declared, widened, check = OUTSIDE[name]
    source = CABLE_BOX.read_text(encoding="utf-8")
    assert source.count(declared) == 1
    wide = tmp_path / "cable_box_wide.py"
    wide.write_text(source.replace(declared, widened), encoding="utf-8")
    specs = load_model(CABLE_BOX).param_specs
    fixed = {other: (specs[other]["default"],) * 2
             for other in ("width", "depth", "height", "wall") if other != name}
    report = sample_ranges(wide, X2D_CONTEXT, seed=1, random_samples=0,
                           ranges={**fixed, name: span})
    assert report["status"] == "warn" and report["verified"]
    assert check in [failure["check"] for failure in report["failures"]]
    outside = span[0] if span[0] < specs[name]["min"] else span[1]
    assert outside in [setting["params"][name] for setting in report["failed_settings"]]
    low, high = report["passed_ranges"][name]
    assert low <= specs[name]["default"] <= high and not low <= outside <= high
    assert list(report["passed_ranges"]) == [name] and report["clamped"] == [name]
    for setting in report["failed_settings"]:
        assert not low <= setting["params"][name] <= high


GUESSED_SIZE = ('"size": [120.0, 60.0, 36.0], "provenance": "guessed"',
                '"size": [120.0, 60.0, 36.0], "provenance": "measured"')
GUESSED_INSIDE = (('"label": "Inside width", "provenance": "guessed"',
                   '"label": "Inside width", "provenance": "measured"'),
                  ('"label": "Inside depth", "provenance": "guessed"',
                   '"label": "Inside depth", "provenance": "measured"'))


def _measured(source: str) -> str:
    """The example with its inside size taken as measured, so that no guess is left."""
    for old, new in (GUESSED_SIZE, *GUESSED_INSIDE):
        assert source.count(old) == 1, old
        source = source.replace(old, new)
    return source


def test_the_bundled_example_meets_its_own_inside_size(tmp_path):
    """The example once failed its own requirement: a corner screw boss stood in the inside,
    so only about 114 x 54 mm of the required 120 x 60 mm was free. The boss now stands in a
    post outside the corner. With the size taken as measured (so that the guess does not
    turn the result into a warning) the requirement is a plain pass, at the default sizes
    and at the smallest wall and largest height the ranges allow."""
    source = _measured(CABLE_BOX.read_text(encoding="utf-8"))
    measured = tmp_path / "cable_box_measured.py"
    measured.write_text(source, encoding="utf-8")
    for name, params in (("default", {}), ("thin_tall", {"wall": 1.6, "height": 80.0})):
        result = run_job(tmp_path / name, measured, params=params,
                         checks={"enabled": True, "min_wall": 0.8, "contract": True})
        assert result["status"] == "ok", name
        (strip,) = [c for c in result["checks"] if c["id"] == "contract"
                    and c["data"]["requirement"] == "holds_power_strip"]
        assert strip["status"] == "pass" and strip["data"]["met"] is True, strip
        assert strip["message"] == ("Requirement 'holds_power_strip' (measured) is met: a "
                                    "120 x 60 x 36 mm cuboid fits inside 'box'."), name
    # The inside is exactly the declared width x depth: a tenth more does not go in.
    wider = tmp_path / "cable_box_wider.py"
    wider.write_text(source.replace(
        '"size": [120.0, 60.0, 36.0]', '"size": [120.1, 60.0, 36.0]'), encoding="utf-8")
    result = run_job(tmp_path / "wider", wider,
                     checks={"enabled": True, "min_wall": 0.8, "contract": True})
    (strip,) = [c for c in result["checks"] if c["id"] == "contract"
                and c["data"]["requirement"] == "holds_power_strip"]
    assert strip["status"] == "fail" and strip["data"]["met"] is False
