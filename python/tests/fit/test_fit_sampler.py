"""The range sampler: finds failing regions, clamps the ranges, and is deterministic."""

from __future__ import annotations

import functools
import json

import pytest
from fit_helpers import MODELS, X2D_CONTEXT, context, quantity, table, write_table

from hotend_engine.errors import ModelError
from hotend_fit.sampler import ERROR_CHECKS, sample_ranges

PLANTED = MODELS / "fit_planted_failure.py"

# A cup whose wall is a slider: min_wall fails below 0.8 mm, wherever the other slider is.
THIN_WALL = '''
from build123d import *
PARAMS = {
    "size": {"default": 30.0, "min": 20.0, "max": 60.0, "provenance": "style"},
    "wall": {"default": 2.0, "min": 0.4, "max": 4.0, "provenance": "guessed"},
}
def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    return Box(p.size, p.size, 20, align=bottom) - Pos(0, 0, p.wall) * Box(
        p.size - 2 * p.wall, p.size - 2 * p.wall, 20, align=bottom)
'''

# Five sliders and a count; a solid block that always passes.
MANY = '''
from build123d import *
PARAMS = {
    "a": {"default": 20.0, "min": 10.0, "max": 30.0, "provenance": "style"},
    "b": {"default": 20.0, "min": 10.0, "max": 30.0, "provenance": "style"},
    "c": {"default": 20.0, "min": 10.0, "max": 30.0, "provenance": "style"},
    "d": {"default": 2.0, "min": 1.0, "max": 3.0, "provenance": "style"},
    "holes": {"default": 2, "min": 1, "max": 4, "unit": "count", "provenance": "style"},
    "fixed": {"default": 5.0, "provenance": "style"},
}
def build(p, ctx):
    assert isinstance(p.holes, int)
    block = Box(p.a + 10 * p.holes, p.b, p.c + p.d + p.fixed)
    if p.holes == 4 and p.d > 2.5:
        raise ValueError("planted: four holes do not fit a thick plate")
    return block
'''

# A tray whose floor is 0.3 mm in a band of widths in the middle of the range: both ends of
# the range, every corner and the default pass.
BAND = '''
from build123d import *
PARAMS = {
    "width": {"default": 60.0, "min": 20.0, "max": 100.0, "provenance": "style"},
    "height": {"default": 30.0, "min": 10.0, "max": 60.0, "provenance": "style"},
}
def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    floor = 0.3 if 68 < p.width < 80 else 2.0
    return Box(p.width, 30.0, p.height, align=bottom) - Pos(0, 0, floor) * Box(
        p.width - 4, 26.0, p.height, align=bottom)
'''

# A divider tray whose walls are thin for exactly one pocket count inside the range.
ONE_BAD_COUNT = '''
from build123d import *
PARAMS = {
    "pockets": {"default": 3, "min": 2, "max": 9, "unit": "count", "provenance": "style"},
    "width": {"default": 100.0, "min": 80.0, "max": 120.0, "provenance": "style"},
}
def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    tray = Box(p.width, 30.0, 12.0, align=bottom)
    wall = 0.4 if p.pockets == 6 else 2.0
    pocket = (p.width - wall * (p.pockets + 1)) / p.pockets
    for i in range(p.pockets):
        x = -p.width / 2 + wall + pocket / 2 + i * (pocket + wall)
        tray -= Pos(x, 0, 2.0) * Box(pocket, 26.0, 12.0, align=bottom)
    return tray
'''

# A press-fitted peg on a cover, drawn apart: the only thing that can go wrong is the fit.
PRESS = '''
from build123d import *
from hotend_fit import cards
PARAMS = {"diameter": {"default": 5.0, "min": 4.0, "max": 8.0, "provenance": "standard"}}
def build(p, ctx):
    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    joint = cards.peg_hole(diameter=p.diameter, length=6.0, fit="press", ctx=ctx)
    base = joint.apply_a(Box(30, 30, 10, align=bottom), at=Pos(0, 0, 10))
    cover = joint.apply_b(Pos(50, 0, 0) * Box(30, 30, 3, align=bottom), at=Pos(50, 0, 0))
    return {"base": base, "cover": cover}
'''


def model(tmp_path, source: str):
    path = tmp_path / "model.py"
    path.write_text(source, encoding="utf-8")
    return path


def inside(report: dict, params: dict) -> bool:
    return all(low <= params[name] <= high
               for name, (low, high) in report["passed_ranges"].items())


def assert_no_failing_setting_in_the_reported_ranges(report: dict) -> None:
    assert report["verified"]
    for setting in report["failed_settings"]:
        assert not inside(report, setting["params"]), setting


@functools.cache
def planted_report(seed: int = 7) -> dict:
    return sample_ranges(PLANTED, X2D_CONTEXT, seed=seed)


def test_the_planted_failure_region_is_found_and_clamped_away():
    report = planted_report()
    assert report["status"] == "warn" and report["verified"] and not report["truncated"]
    (failure,) = report["failures"]
    assert failure["check"] == "min_wall"
    assert "0.30 mm" in failure["example"]["message"]
    # Every failing setting lies in the planted region (width > 80 and height < 20) ...
    assert report["samples"]["failed"] == failure["count"] >= 1
    for setting in report["failed_settings"]:
        assert setting["params"]["width"] > 80 and setting["params"]["height"] < 20
        assert setting["failed"] == ["min_wall"]
    assert failure["box"]["width"][0] > 80 and failure["box"]["height"][1] < 20
    # ... and the suggested ranges keep the defaults and leave the region out.
    low_w, high_w = report["passed_ranges"]["width"]
    low_h, high_h = report["passed_ranges"]["height"]
    assert not (high_w > 80 and low_h < 20)
    assert low_w <= 60 <= high_w and low_h <= 30 <= high_h
    assert (low_w, high_w) == (20.0, 100.0) and (low_h, high_h) == (20.0, 60.0)
    assert report["clamped"] == ["height"]
    height = dict(report["parameters"]["height"])
    assert height.pop("values_checked") >= 9
    assert height == {
        "default": 30.0, "unit": "mm", "sampled": True, "declared": [10.0, 60.0],
        "passed": [20.0, 60.0], "clamped": True, "swept": 9}
    assert report["parameters"]["wall"] == {"default": 2.0, "unit": "mm", "sampled": False}
    assert "clamped" in report["message"] and "height 20 to 60" in report["message"]
    assert "guarantee" not in report["message"].lower()
    assert_no_failing_setting_in_the_reported_ranges(report)


def test_the_report_says_what_was_sampled():
    report = planted_report()
    samples = report["samples"]
    assert samples["declared_corners_checked"] == samples["declared_corners_total"] == 4
    kinds = samples["by_kind"]
    # The default, nine values of each of two parameters (the default is one of them
    # neither time), four corners, eight inner points, then the clamped box again.
    assert kinds["default"] == 1 and kinds["corner"] == 4 and kinds["sweep"] == 17
    assert kinds["inner"] == 8 and kinds["confirm_corner"] >= 1 and kinds["confirm_inner"] == 8
    assert samples["settings"] == sum(kinds.values()) == samples["passed"] + samples["failed"]
    assert samples["builds"] == samples["settings"]  # one drawing per setting: the printed one
    assert report["extremes"] == ["tight", "loose"] and report["k"] == 2.0
    assert report["checks"] == list(ERROR_CHECKS)
    # One part and no cards: these two checks had nothing to check and are listed as such,
    # and the report does not claim that fits were proven.
    assert report["skipped_checks"] == ["fit_pairs", "parts_do_not_intersect"]
    assert report["fits"] == {"declared": False, "proven": False, "unproven_settings": 0,
                              "example": None}
    assert "declares no fits" in report["message"]


def test_the_same_seed_gives_the_same_report_and_another_seed_other_samples():
    first = planted_report()
    again = sample_ranges(PLANTED, X2D_CONTEXT, seed=7)
    assert json.dumps(first, sort_keys=True) == json.dumps(again, sort_keys=True)
    other = sample_ranges(PLANTED, X2D_CONTEXT, seed=8)
    assert json.dumps(other, sort_keys=True) != json.dumps(first, sort_keys=True)
    assert other["passed_ranges"] == first["passed_ranges"] and other["verified"]
    json.loads(json.dumps(first, allow_nan=False))  # the report is plain JSON


def test_a_one_parameter_threshold_is_named_and_the_bound_is_found_by_bisection(tmp_path):
    report = sample_ranges(model(tmp_path, THIN_WALL), X2D_CONTEXT, seed=3)
    (failure,) = report["failures"]
    assert failure["check"] == "min_wall"
    (split,) = failure["separated_by"]
    assert split["param"] == "wall" and split["side"] == "below"
    assert split["fails_up_to"] < 0.8 <= split["passes_from"]
    low, high = report["passed_ranges"]["wall"]
    # The sweep passes at 0.85 and fails at 0.4; three bisections look for a lower bound
    # that passes and find none above 0.8 that is nearer than 0.85.
    assert 0.8 <= low <= 0.85 and high == 4.0
    assert report["passed_ranges"]["size"] == [20.0, 60.0] and report["clamped"] == ["wall"]
    assert report["samples"]["by_kind"]["refine"] == 3
    assert_no_failing_setting_in_the_reported_ranges(report)
    # Without bisection the bound stays at the last passing sweep value.
    plain = sample_ranges(model(tmp_path, THIN_WALL), X2D_CONTEXT, seed=3, refine_steps=0)
    assert plain["passed_ranges"]["wall"] == [0.85, 4.0] and plain["verified"]
    assert "refine" not in plain["samples"]["by_kind"]


def test_ranges_can_be_narrowed_to_what_passed(tmp_path):
    path = model(tmp_path, THIN_WALL)
    report = sample_ranges(path, X2D_CONTEXT, seed=3, ranges={"wall": (1.2, 4.0)})
    assert report["status"] == "pass" and report["failures"] == [] and report["verified"]
    assert report["passed_ranges"] == {"size": [20.0, 60.0], "wall": [1.2, 4.0]}
    assert "Every sampled size passed" in report["message"] and report["clamped"] == []
    with pytest.raises(ValueError, match="does not contain its default"):
        sample_ranges(path, X2D_CONTEXT, ranges={"wall": (2.5, 4.0)})
    with pytest.raises(ValueError, match="unknown parameter"):
        sample_ranges(path, X2D_CONTEXT, ranges={"depth": (1.0, 2.0)})


def test_a_model_that_fails_at_its_defaults_has_no_checked_range(tmp_path):
    source = THIN_WALL.replace('"default": 2.0, "min": 0.4', '"default": 0.5, "min": 0.4')
    report = sample_ranges(model(tmp_path, source), X2D_CONTEXT, random_samples=0)
    assert report["status"] == "fail" and report["passed_ranges"] is None
    assert not report["verified"] and "default sizes" in report["message"]
    assert report["parameters"]["wall"]["passed"] is None


def test_corners_are_capped_counts_stay_whole_and_build_errors_are_failures(tmp_path):
    report = sample_ranges(model(tmp_path, MANY), X2D_CONTEXT, seed=5, max_corners=6,
                           random_samples=6, checks=("valid_solid", "fits_build_volume"))
    samples = report["samples"]
    assert samples["declared_corners_checked"] == 6 and samples["declared_corners_total"] == 32
    # Five sampled parameters: the inner set grows to four points per parameter.
    assert samples["by_kind"]["inner"] == 20
    assert report["parameters"]["fixed"]["sampled"] is False
    (failure,) = report["failures"]
    assert failure["check"] == "build:ValueError" and "planted" in failure["example"]["message"]
    for setting in report["failed_settings"]:
        assert setting["params"]["holes"] == 4 and setting["params"]["d"] > 2.5
        assert isinstance(setting["params"]["holes"], int)
    low, high = report["passed_ranges"]["holes"]
    assert isinstance(low, int) and isinstance(high, int)
    assert not (high == 4 and report["passed_ranges"]["d"][1] > 2.5)
    assert_no_failing_setting_in_the_reported_ranges(report)
    assert report["checks"] == ["valid_solid", "fits_build_volume"]
    assert report["fits"]["proven"] is False and "fits were not checked" in report["message"]
    # Only the two parameters that are involved were looked at for a cut: a, b and c, which
    # have nothing to do with the failure, keep their whole range.
    assert set(report["clamped"]) <= {"d", "holes"} and report["clamped"]
    for name in ("a", "b", "c"):
        assert report["passed_ranges"][name] == [10.0, 30.0]


def test_a_failing_band_inside_a_range_is_found_and_left_out(tmp_path):
    """Both ends of the range, every corner and the default pass; the floor is thin only for
    widths between 68 and 80. The sweep steps through the range and finds it."""
    report = sample_ranges(model(tmp_path, BAND), X2D_CONTEXT, seed=1)
    assert report["status"] == "warn"
    (failure,) = report["failures"]
    assert failure["check"] == "min_wall"
    swept = [s["params"]["width"] for s in report["failed_settings"] if s["kind"] == "sweep"]
    assert swept and all(68 < width < 80 for width in swept)
    low, high = report["passed_ranges"]["width"]
    assert low == 20.0 and 60.0 <= high <= 68.0
    assert report["clamped"] == ["width"] and report["passed_ranges"]["height"] == [10.0, 60.0]
    assert_no_failing_setting_in_the_reported_ranges(report)
    # The same holds for any seed: the sweep does not depend on it.
    for seed in (2, 3):
        other = sample_ranges(model(tmp_path, BAND), X2D_CONTEXT, seed=seed, random_samples=0)
        assert other["passed_ranges"]["width"][1] <= 68.0
        assert_no_failing_setting_in_the_reported_ranges(other)


def test_every_value_of_a_count_parameter_is_built(tmp_path):
    """Six pockets is the one bad count of eight; it is neither end of the range."""
    report = sample_ranges(model(tmp_path, ONE_BAD_COUNT), X2D_CONTEXT, seed=1,
                           random_samples=0)
    assert report["parameters"]["pockets"]["swept"] == 8  # 2, 3, ... 9: all of them
    failed = {s["params"]["pockets"] for s in report["failed_settings"]}
    assert failed == {6}
    assert report["passed_ranges"]["pockets"] == [2, 5]
    assert report["parameters"]["pockets"]["values_checked"] == 4
    assert_no_failing_setting_in_the_reported_ranges(report)


def test_the_build_budget_truncates_honestly(tmp_path):
    path = model(tmp_path, THIN_WALL)
    starved = sample_ranges(path, X2D_CONTEXT, seed=3, max_builds=10)
    assert starved["truncated"] and not starved["verified"] and starved["status"] == "warn"
    assert starved["samples"]["builds"] <= 10
    assert "budget ran out" in starved["message"]
    # Not enough to confirm the clamped box: nothing is called verified.
    full = sample_ranges(path, X2D_CONTEXT, seed=3)
    limited = sample_ranges(path, X2D_CONTEXT, seed=3,
                            max_builds=full["samples"]["builds"] - 2)
    assert limited["truncated"] and not limited["verified"]


def test_a_fit_that_leaves_its_class_at_a_material_extreme_fails_the_setting(tmp_path):
    """The drawing is built once; each of its clearances is evaluated on the tightest and
    the loosest printer the calibration allows. A press fit on a printer whose hole offset
    is known only to +/- 0.16 mm prints loose at one extreme: every setting fails."""
    wide = write_table(tmp_path, table(hole_offset=quantity(0.10, 0.08)), name="wide.json")
    report = sample_ranges(model(tmp_path, PRESS), context(calibration_path=str(wide)),
                           random_samples=2)
    assert report["status"] == "fail"  # it fails at the default diameter already
    (failure,) = report["failures"]
    assert failure["check"] == "fit_pairs"
    message = failure["example"]["message"]
    assert "press" in message and "loosest printer" in message and "would not hold" in message
    assert report["skipped_checks"] == [] and report["passed_ranges"] is None
    # With k = 0 both "extremes" are the printer as measured, and the same model passes.
    calm = sample_ranges(model(tmp_path, PRESS), context(calibration_path=str(wide)),
                         random_samples=2, k=0.0)
    assert calm["status"] == "pass" and calm["k"] == 0.0 and calm["fits"]["proven"]


def test_a_well_measured_printer_proves_the_fit_and_a_guessed_one_cannot(tmp_path):
    path = write_table(tmp_path, table(hole_offset=quantity(0.10, 0.01)))
    with_table = sample_ranges(model(tmp_path, PRESS),
                               context(calibration_path=str(path)), random_samples=0)
    # Sigma 0.01 mm: at +/- 2 sigma the pressed peg still interferes by 0.01 to 0.15 mm.
    assert with_table["status"] == "pass" and with_table["fits"]["proven"]
    assert with_table["calibration"]["provenance"] == "calibrated"
    assert "bambu-x2d" in with_table["calibration"]["source"]
    assert "tightest and the loosest printer" in with_table["message"]
    # The guessed defaults (sigma 0.08 mm) cannot prove any press fit. Nothing failed, and
    # the report still is not a pass: it says the fits are unproven.
    guessed = sample_ranges(model(tmp_path, PRESS), X2D_CONTEXT, random_samples=0)
    assert guessed["calibration"]["provenance"] == "guessed"
    assert "guessed defaults" in guessed["calibration"]["source"]
    assert guessed["status"] == "warn" and guessed["failures"] == []
    fits = guessed["fits"]
    assert fits["declared"] and not fits["proven"]
    assert fits["unproven_settings"] == guessed["samples"]["settings"]
    assert "would not hold" in fits["example"] and "not calibrated" in guessed["message"]


def test_a_script_that_cannot_be_loaded_is_a_model_error(tmp_path):
    with pytest.raises(ModelError):
        sample_ranges(model(tmp_path, "PARAMS = {}\n"), X2D_CONTEXT)
    with pytest.raises(ModelError):
        sample_ranges(tmp_path / "absent.py", X2D_CONTEXT)


def test_a_model_without_ranges_is_checked_at_its_defaults_only(tmp_path):
    source = "from build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    return Box(10, 10, 10)\n"
    report = sample_ranges(model(tmp_path, source), X2D_CONTEXT)
    assert report["status"] == "pass" and report["passed_ranges"] == {}
    assert report["samples"]["settings"] == 1 and report["samples"]["builds"] == 1


def test_library_caches_are_emptied_before_every_build(tmp_path, monkeypatch):
    from ocp_tessellate import tessellator
    from ocp_tessellate.ocp_utils import bbox

    from hotend_engine import model as engine_model

    seen = []
    real_build = engine_model.run_build

    def watched(*args, **kwargs):
        # What a library kept from the setting before, at the moment the model runs.
        seen.append((len(bbox.cache), len(tessellator.cache)))
        return real_build(*args, **kwargs)

    monkeypatch.setattr(engine_model, "run_build", watched)
    report = sample_ranges(model(tmp_path, THIN_WALL), X2D_CONTEXT, seed=3, random_samples=0,
                           refine_steps=0)
    assert len(seen) == report["samples"]["builds"] > 5
    assert set(seen) == {(0, 0)}
    assert len(tessellator.cache) > 0  # the cache is in use: the last build left its mesh


def test_a_second_process_gives_the_same_report(tmp_path):
    """The limit of what is promised: same machine, same libraries, a deterministic script."""
    import subprocess
    import sys

    path = model(tmp_path, THIN_WALL)
    here = sample_ranges(path, X2D_CONTEXT, seed=3)
    script = (
        "import json, sys\n"
        "from hotend_fit.sampler import sample_ranges\n"
        "context = json.loads(sys.argv[2])\n"
        "print(json.dumps(sample_ranges(sys.argv[1], context, seed=3), sort_keys=True))\n"
    )
    done = subprocess.run(
        [sys.executable, "-c", script, str(path), json.dumps(X2D_CONTEXT)],
        capture_output=True, text=True, timeout=300, check=True,
    )
    assert done.stdout.strip().splitlines()[-1] == json.dumps(here, sort_keys=True)
