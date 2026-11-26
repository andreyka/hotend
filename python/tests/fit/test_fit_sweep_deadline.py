"""A sweep ends before the job's time limit with what it built so far, instead of being
killed with its whole report (STEP6 fix 9)."""

from __future__ import annotations

import time

import pytest
from fit_helpers import X2D_CONTEXT

from hotend_engine.request import SweepReport
from hotend_fit.sampler import sweep

pytestmark = pytest.mark.timeout(120)

# A block that takes `pause` seconds to build, and never finishes at a size of 55 mm.
SLOW = '''
import time
from build123d import *
PARAMS = {
    "size": {"default": 30.0, "min": 20.0, "max": 60.0, "provenance": "style"},
    "pause": {"default": 0.0, "min": 0.0, "max": 10.0, "provenance": "style"},
}
def build(p, ctx):
    while p.size == 55.0:
        time.sleep(0.05)
    time.sleep(p.pause)
    return Box(p.size, p.size, 10)
'''


@pytest.fixture
def slow_model(tmp_path):
    path = tmp_path / "model.py"
    path.write_text(SLOW, encoding="utf-8")
    return path


def test_a_setting_still_building_at_the_limit_is_stopped_and_the_rest_is_reported(
        slow_model):
    started = time.monotonic()
    report = sweep(slow_model, X2D_CONTEXT, base={},
                   settings=[{"size": 30.0}, {"size": 55.0}, {"size": 40.0}],
                   deadline=started + 100, hard_deadline=started + 4.0)
    took = time.monotonic() - started
    assert took < 10, took
    SweepReport.model_validate(report)
    assert report["truncated"] is True and report["builds"] == 1
    done, stopped, left = report["samples"]
    assert done["status"] == "ok" and done["parts"]
    assert stopped["status"] == "not_run"
    assert [f["check"] for f in stopped["failed"]] == ["time_limit"]
    assert left == {"params": {"size": 40.0}, "status": "not_run", "failed": [],
                    "requirements": [], "parts": {}}


def test_a_setting_that_would_not_fit_before_the_limit_is_not_started(slow_model):
    started = time.monotonic()
    # Each setting takes 3 s and more: after the first, a second would not fit in 5.5 s.
    report = sweep(slow_model, X2D_CONTEXT, base={},
                   settings=[{"pause": 3.0}, {"pause": 3.0}],
                   hard_deadline=started + 5.5)
    SweepReport.model_validate(report)
    assert report["builds"] == 1 and report["truncated"] is True
    assert [s["status"] for s in report["samples"]] == ["ok", "not_run"]
    # Not started, so not stopped either.
    assert report["samples"][1]["failed"] == []
    assert time.monotonic() - started < 5.5
