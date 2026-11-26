"""The `sweep` op: the range sampler builds a model at a few parameter settings in the model
process, and the worker passes the report on in its strict shape. Nothing is stored."""

from __future__ import annotations

import copy
import json
from concurrent.futures import ThreadPoolExecutor

import pytest
from conftest import MODELS, X2D_CONTEXT, build_request, prepare_job, run_worker

from hotend_engine.request import SweepReport, parse_build_record, parse_request
from hotend_fit.sampler import SWEEP_CHECKS

DECLARED_WALL = '"wall": {"default": 2.0, "min": 1.6,'
WIDENED_WALL = '"wall": {"default": 2.0, "min": 0.8,'


def sweep_request(**overrides) -> dict:
    request = {
        "protocol": 0, "op": "sweep", "model_path": "model.py",
        "params": {"width": 120.0, "depth": 60.0, "height": 40.0, "wall": 2.0, "cable": 12.0},
        "settings": [{"wall": 1.0}, {"wall": 1.6}, {"wall": 2.5}],
        "context": copy.deepcopy(X2D_CONTEXT),
        "limits": {"wall_seconds": 240},
    }
    request.update(overrides)
    return request


@pytest.fixture(scope="module")
def sweeps(tmp_path_factory) -> dict:
    root = tmp_path_factory.mktemp("sweeps")
    specs = {
        "walls": sweep_request(),
        "outside": sweep_request(settings=[{"wall": 1.0}]),
        "unknown": sweep_request(settings=[{"colour": 1.0}]),
        "no_time": sweep_request(settings=[{"wall": 1.6}, {"wall": 2.5}],
                                 budget_seconds=0.001),
        # The same model built as a version would be (set_parameter), at two of the walls.
        **{f"build_{wall}": build_request(
            params={**sweep_request()["params"], "wall": wall},
            outputs={"step": False, "brep": True, "mesh": False, "views": []},
            checks={"enabled": True, "min_wall": 0.8, "contract": True})
           for wall in (1.0, 2.5)},
    }

    # The demo's planted mistake becomes reachable: a wall range that starts at 0.8 mm.
    source = (MODELS / "cable_box.py").read_text(encoding="utf-8")
    assert source.count(DECLARED_WALL) == 1
    widened = source.replace(DECLARED_WALL, WIDENED_WALL)

    def run(name: str):
        job_dir = prepare_job(root / name, specs[name])
        (job_dir / "model.py").write_text(widened if name != "outside" else source,
                                          encoding="utf-8")
        return run_worker(job_dir)

    with ThreadPoolExecutor(max_workers=4) as pool:
        return dict(zip(specs, pool.map(run, specs), strict=True))


def test_a_sweep_builds_every_setting_and_says_how_each_fares(sweeps):
    job = sweeps["walls"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["op"] == "sweep" and result["status"] == "ok", result.get("error")
    report = SweepReport.model_validate(result["sweep"])
    assert report.builds == 3 and not report.truncated
    thin, low, thick = report.samples
    assert [s.params for s in report.samples] == [{"wall": 1.0}, {"wall": 1.6}, {"wall": 2.5}]
    # The planted mistake of the demo: a 1 mm wall is cut too thin by the snap groove.
    assert thin.status == "check_error"
    assert "min_wall" in {failure.check for failure in thin.failed}
    walls = next(r for r in thin.requirements if r.id == "walls_print")
    assert walls.status == "fail" and walls.met is False
    for sample in (low, thick):
        assert sample.status == "ok" and sample.failed == ()
        strip = next(r for r in sample.requirements if r.id == "holds_power_strip")
        assert strip.met is True and strip.status == "warn"  # met, but a guessed size
        slot = next(r for r in sample.requirements if r.id == "cables_pass")
        assert slot.not_checkable and slot.met is None
    # The sizes of the parts: the outside grows with the wall, the inside does not; along X
    # the screw post stands as far out whatever the wall, so only the other end grows.
    assert thick.parts["box"][1] - low.parts["box"][1] == pytest.approx(1.8, abs=1e-3)
    assert thick.parts["box"][0] - low.parts["box"][0] == pytest.approx(0.9, abs=1e-3)
    assert thick.parts["box"][2] == low.parts["box"][2] == pytest.approx(40.0)
    # Nothing is stored: no geometry and no views are left in the job folder.
    assert not {"model.brep", "model.parts.json", "model.step", "mesh.json"} & job.files()


def test_a_sweep_setting_says_what_a_build_at_the_same_value_says(sweeps):
    """A setting of the sweep is advice for set_parameter: at the same value, a build of the
    version fails the same checks, measures the contract the same way and has parts of the
    same size."""
    report = SweepReport.model_validate(sweeps["walls"].result["sweep"])
    samples = {s.params["wall"]: s for s in report.samples}
    for wall in (1.0, 2.5):
        job = sweeps[f"build_{wall}"]
        assert job.returncode == 0, job.stderr
        built, sample = job.result, samples[wall]
        assert built["status"] in ("ok", "check_error"), built.get("error")
        failed = {c["id"] for c in built["checks"]
                  if c["status"] == "fail" and c["severity"] == "error"
                  and c["id"] in SWEEP_CHECKS}
        assert {f.check for f in sample.failed} == failed, wall
        assert sample.status == ("ok" if not failed else "check_error"), wall
        requirements = {
            c["data"]["requirement"]: (c["status"], c["data"].get("met"),
                                       bool(c["data"].get("not_checkable")))
            for c in built["checks"]
            if c["id"] == "contract" and c["data"].get("kind") == "requirement"}
        assert requirements, wall
        assert {r.id: (r.status, r.met, r.not_checkable)
                for r in sample.requirements} == requirements, wall
        sizes = {p["name"]: [mx - mn for mn, mx in zip(p["bbox"]["min"], p["bbox"]["max"],
                                                       strict=True)]
                 for p in built["parts"]}
        assert set(sample.parts) == set(sizes), wall
        for name, size in sizes.items():
            assert list(sample.parts[name]) == pytest.approx(size, abs=1e-3), (wall, name)


def test_a_setting_outside_the_declared_range_or_unknown_is_refused_before_anything_runs(
        sweeps):
    for name, words in (("outside", "wall"), ("unknown", "colour")):
        job = sweeps[name]
        assert job.returncode == 0, job.stderr
        result = job.result
        assert result["status"] == "model_error", name
        assert result["error"]["type"] == "ParameterError", name
        assert words in result["error"]["message"], name
        assert "sweep" not in result


def test_settings_past_the_time_budget_are_reported_as_not_run(sweeps):
    job = sweeps["no_time"]
    assert job.returncode == 0, job.stderr
    report = job.result["sweep"]
    assert report["truncated"] is True and report["builds"] == 0
    assert [s["status"] for s in report["samples"]] == ["not_run", "not_run"]


def test_the_sweep_request_and_record_are_strict():
    parse_request(sweep_request())
    for bad in (sweep_request(settings=[]), sweep_request(settings=[{"w": 1.0}] * 17),
                sweep_request(settings=[{"Wall": 1.0}]), sweep_request(budget_seconds=0),
                sweep_request(extra=1)):
        with pytest.raises(ValueError):
            parse_request(bad)
    record = {"protocol": 0, "status": "ok", "sweep": {
        "protocol": 0, "builds": 1, "truncated": False, "checks": ["min_wall"],
        "samples": [{"params": {"wall": 1.0}, "status": "ok", "failed": [],
                     "requirements": [], "parts": {"box": [1.0, 2.0, 3.0]}}]}}
    parse_build_record(record)
    # What the model process claims is held to the shape: no other status, no long text,
    # no other field.
    for change in ({"status": "passed"}, {"failed": [{"check": "x", "message": "m" * 301}]},
                   {"verdict": "print it"}):
        broken = json.loads(json.dumps(record))
        broken["sweep"]["samples"][0].update(change)
        with pytest.raises(ValueError):
            parse_build_record(broken)
