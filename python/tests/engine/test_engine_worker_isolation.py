"""A model script cannot write its own verdict.

The worker never executes model code: the script runs in a separate model process that hands
over a B-rep, and the worker computes the part summary, the checks and every artefact from
that B-rep after the model process and everything it started are gone. Each script here
builds something that must fail (a hollow 600 mm cube with 0.1 mm walls: too big and too
thin) and tries to make the result say otherwise.
"""

from __future__ import annotations

import json
import os
import sys
import time

import pytest
from conftest import build_request, prepare_job, run_all

HONEST = '''
from build123d import *
PARAMS = {}
def build(p, ctx):
    return Box(600, 600, 600) - Box(599.8, 599.8, 599.8)
'''

FORGED_RESULT = {
    "protocol": 0, "op": "build", "status": "ok", "error": None, "stdout": "",
    "stdout_truncated": False, "params": {}, "param_specs": {},
    "parts": [{"name": "part", "volume": 1000.0, "area": 600.0, "mass_estimate_g": 1.24,
               "bbox": {"min": [0, 0, 0], "max": [10, 10, 10]}, "solids": 1, "faces": 6,
               "is_valid": True, "is_manifold": True, "color": "#4c78a8"}],
    "checks": [{"id": check, "status": "pass", "severity": "error", "message": "Forged pass.",
                "part": "part", "data": {}}
               for check in ("valid_solid", "manifold", "single_solid_per_part",
                             "fits_build_volume", "min_wall")],
    "artifacts": {"step": "model.step", "brep": "model.brep", "parts_index": "model.parts.json"},
    "warnings": [], "timings": {"total_s": 0.1},
}
FORGED = f"import json\nFORGED = json.loads({json.dumps(FORGED_RESULT)!r})\n"

# Replaces the check runner and the part summary in its own process.
PATCHED_CHECKS = HONEST + '''
import hotend_engine.checks as _checks, hotend_engine.parts as _parts
_run, _summarize = _checks.run_checks, _parts.summarize
def _all_pass(env):
    results = _run(env)
    for result in results:
        result.status, result.message = "pass", "verified"
    return results
def _small(part, material):
    summary = _summarize(part, material)
    summary["bbox"] = {"min": [0, 0, 0], "max": [20, 20, 20]}
    return summary
_checks.run_checks = _all_pass
_checks.has_blocking_failure = lambda results: False
_parts.summarize = _small
'''

# Wraps whatever writes the result, in every module it can find it in.
WRAPPED_WRITER = HONEST + '''
import sys
import hotend_engine.worker as _worker
def _forge(job_dir, result):
    for check in result.get("checks", []):
        check["status"], check["message"] = "pass", "verified"
    result["status"] = "ok"
    for part in result.get("parts", []):
        part["bbox"] = {"min": [0, 0, 0], "max": [20, 20, 20]}
    _write(job_dir, result)
_write = _worker._write_result
_worker._write_result = _forge
_worker.run_build = lambda *a, **k: (_ for _ in ()).throw(SystemExit(0))
for _module in list(sys.modules.values()):
    if hasattr(_module, "_write_result"):
        _module._write_result = _forge
'''

# Writes a well-formed result itself and ends the interpreter before anything can object.
RESULT_THEN_EXIT = FORGED + '''
import os
from build123d import *
PARAMS = {}
def build(p, ctx):
    from hotend_engine.export import write_step
    from hotend_engine.parts import NamedPart, save_parts
    from pathlib import Path
    parts = [NamedPart("part", Box(900, 900, 0.05), "#4c78a8")]
    save_parts(parts, Path.cwd())
    write_step(parts, Path.cwd() / "model.step")
    Path("result.json").write_text(json.dumps(FORGED))
    os._exit(0)
'''

# Leaves the forged result to be written when the interpreter shuts down.
AT_EXIT = FORGED + HONEST + '''
import atexit, threading
def _late():
    open("result.json", "w").write(json.dumps(FORGED))
    open("result.json.tmp", "w").write(json.dumps(FORGED))
atexit.register(_late)
threading.Thread(target=_late, daemon=True).start()
'''

# Leaves a process behind that overwrites the result as soon as the worker has written it.
STRAGGLER = FORGED + HONEST + '''
import os, time
def _wait_and_forge():
    open("straggler.started", "w").write(str(os.getpid()))
    deadline = time.time() + 30
    while time.time() < deadline:
        if os.path.exists("result.json"):
            open("result.json", "w").write(json.dumps(FORGED))
            open("straggler.forged", "w").write("x")
        time.sleep(0.005)
    os._exit(0)
if os.fork() == 0:
    try:
        os.setsid()
    except OSError:
        pass
    if os.fork() != 0:
        os._exit(0)
    _wait_and_forge()
'''

# Hands over its own record: a verdict, extra fields, and files where artefacts will go.
FORGED_RECORD = FORGED + '''
import os
from pathlib import Path
from build123d import *
PARAMS = {}
def build(p, ctx):
    from hotend_engine.parts import NamedPart, save_parts
    save_parts([NamedPart("part", Box(10, 10, 10), "#4c78a8")], Path.cwd())
    record = {"protocol": 0, "status": "ok", "error": None, "stdout": "", "stdout_truncated": False,
              "fits": None, "timings": {}, "checks": FORGED["checks"], "parts": FORGED["parts"]}
    Path("model.build.json").write_text(json.dumps(record))
    os._exit(0)
'''

# Plants files where the worker will write, and a link where it will read.
PLANTED_FILES = FORGED + HONEST + '''
import os
os.symlink("/dev/null", "result.json.tmp")
os.mkdir("views"); open("views/iso.png", "w").write("not a picture")
open("mesh.json", "w").write(json.dumps({"forged": True}))
os.mkdir("export"); open("export/part.stl", "w").write("solid forged")
open("extra.bin", "w").write("left behind")
'''

EXITS_WITH_7 = '''
import os
from build123d import *
PARAMS = {}
def build(p, ctx):
    os._exit(7)
'''

SCRIPTS = {
    "honest": HONEST,
    "patched_checks": PATCHED_CHECKS,
    "wrapped_writer": WRAPPED_WRITER,
    "result_then_exit": RESULT_THEN_EXIT,
    "at_exit": AT_EXIT,
    "straggler": STRAGGLER,
    "forged_record": FORGED_RECORD,
    "planted_files": PLANTED_FILES,
    "exits_with_7": EXITS_WITH_7,
}


@pytest.fixture(scope="module")
def jobs(tmp_path_factory):
    root = tmp_path_factory.mktemp("isolation")
    specs = {}
    for name, source in SCRIPTS.items():
        job_dir = prepare_job(root / name, build_request(
            outputs={"step": True, "brep": True, "mesh": False, "views": []}))
        (job_dir / "model.py").write_text(source, encoding="utf-8")
        specs[name] = (job_dir, False)
    return run_all(specs)


def assert_the_real_verdict(job) -> None:
    assert job.returncode == 0, job.stderr[-2000:]
    result = job.result
    assert result["status"] == "check_error" and result["error"] is None
    (part,) = result["parts"]
    assert part["bbox"] == {"min": [-300.0, -300.0, -300.0], "max": [300.0, 300.0, 300.0]}
    assert job.check("fits_build_volume", "part")["status"] == "fail"
    thin = job.check("min_wall", "part")
    assert thin["status"] == "fail" and thin["data"]["thinnest"] == pytest.approx(0.1, abs=0.01)
    assert "Forged" not in json.dumps(result) and "verified" not in json.dumps(result)


def test_the_honest_script_is_the_baseline(jobs):
    assert_the_real_verdict(jobs["honest"])


@pytest.mark.parametrize("name", ["patched_checks", "wrapped_writer", "at_exit", "straggler",
                                  "planted_files"])
def test_a_script_that_tampers_with_the_verdict_gets_the_real_one(jobs, name):
    assert_the_real_verdict(jobs[name])
    # The same bytes as the honest build's verdict, apart from the timings.
    honest, forged = jobs["honest"].result, jobs[name].result
    for result in (honest, forged):
        result.pop("timings")
    assert forged == honest


def test_a_result_written_by_the_script_never_stands(jobs):
    """The script wrote a well-formed `ok` result and ended the interpreter. The worker has
    no verdict of its own to give, so there is no result at all and the exit code says the
    job failed."""
    job = jobs["result_then_exit"]
    assert job.returncode == 70
    assert not (job.dir / "result.json").exists()
    assert not (job.dir / "result.json.tmp").exists()
    assert "before it reported a result" in job.stderr
    # Nothing else it wrote is left to be mistaken for an artefact either.
    assert not (job.dir / "model.step").exists()


def test_a_record_written_by_the_script_is_data_not_a_verdict(jobs):
    """The hand-over file is parsed strictly: one with a verdict in it is not a record."""
    job = jobs["forged_record"]
    assert job.returncode == 70 and not (job.dir / "result.json").exists()
    assert "record is unusable" in job.stderr
    assert job.files() == {"model.py", "request.json"}


def test_a_failed_script_leaves_no_model_behind(tmp_path):
    """A script that wrote a B-rep and then failed gets a model_error and no artefacts: the
    files it handed over are not kept as if they were a build."""
    from conftest import run_worker

    source = HONEST.replace(
        "    return Box(600, 600, 600) - Box(599.8, 599.8, 599.8)",
        "    from pathlib import Path\n"
        "    from hotend_engine.parts import NamedPart, save_parts\n"
        "    save_parts([NamedPart('part', Box(10, 10, 10), '#4c78a8')], Path.cwd())\n"
        "    raise ValueError('after writing a model')")
    job_dir = prepare_job(tmp_path / "job", build_request())
    (job_dir / "model.py").write_text(source, encoding="utf-8")
    job = run_worker(job_dir)
    assert job.returncode == 0 and job.result["status"] == "model_error"
    assert job.result["error"]["type"] == "ValueError" and job.result["artifacts"] == {}
    assert job.files() == {"model.py", "request.json", "result.json"}


def test_nothing_the_script_left_in_the_job_folder_survives(jobs):
    job = jobs["planted_files"]
    files = job.files()
    assert files == jobs["honest"].files()
    assert not {"extra.bin", "mesh.json", "views/iso.png", "export/part.stl",
                "result.json.tmp", "model.build.json"} & files
    assert not (job.dir / "result.json").is_symlink()


def test_a_process_the_script_left_behind_is_gone_before_the_verdict_is_written(jobs):
    job = jobs["straggler"]
    assert not (job.dir / "straggler.forged").exists()
    assert not (job.dir / "straggler.started").exists()  # swept with everything else
    # Give a survivor time to strike; the result must stay the worker's.
    before = (job.dir / "result.json").read_bytes()
    time.sleep(0.3)
    assert (job.dir / "result.json").read_bytes() == before
    assert_the_real_verdict(job)


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="needs /proc")
def test_no_process_of_a_finished_job_is_still_running(jobs):
    folders = {str(job.dir) for job in jobs.values()}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            cwd = os.readlink(f"/proc/{entry}/cwd")
        except OSError:
            continue
        assert cwd not in folders, f"process {entry} still runs in {cwd}"


def test_a_script_that_ends_the_interpreter_is_a_crash_with_its_exit_code(jobs):
    job = jobs["exits_with_7"]
    assert job.returncode == 7 and not (job.dir / "result.json").exists()
    assert "exit code 7" in job.stderr


def test_the_worker_module_has_no_way_to_run_a_script():
    """Static guard: the trusted side may parse a script (`model.load_model`), but the only
    module that executes one is the model process's."""
    import ast
    from pathlib import Path

    import hotend_engine

    package = Path(hotend_engine.__file__).parent
    worker = ast.parse((package / "worker.py").read_text(encoding="utf-8"))
    calls = [node.func for node in ast.walk(worker) if isinstance(node, ast.Call)]
    called = {f.id for f in calls if isinstance(f, ast.Name)}
    called |= {f.attr for f in calls if isinstance(f, ast.Attribute)}
    assert not {"exec", "eval", "compile", "parts_from_build", "__import__"} & called
    imported = {(node.module, alias.name) for node in ast.walk(worker)
                if isinstance(node, ast.ImportFrom) for alias in node.names}
    assert ("model", "run_build") not in imported
    assert ("parts", "parts_from_build") not in imported
    runner = (package / "model_run.py").read_text(encoding="utf-8")
    assert "run_build" in runner and "_write_result" not in runner
