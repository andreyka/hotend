"""The model process: `python -I -m hotend_engine.model_run <job-dir>`.

This is the only process that executes a model script. The worker (`hotend_engine.worker`)
starts it for a build and trusts nothing it says about the geometry. It writes data only:

- `model.brep` and `model.parts.json`: the parts `build` returned;
- `model.build.json`: what happened (`ok` or a `model_error`), the script's prints, how long
  the build took and which feature cards it applied where (`FitResolver.declarations()`).

The worker then loads the B-rep in its own process and computes the part summary, the
checks, the mesh, the exports and `result.json` from it. Whatever a script does in here -
patching the engine's modules, writing its own `result.json`, exiting early - changes no
check outcome, status or size that the core reads: at worst the build fails.

Sequence: read the request, import the CAD libraries, then wait for one byte on stdin. The
worker sends it once it has finished everything it reads from the job folder, so the
libraries of both processes load side by side and no script code runs before that point.

For a `sweep` request the process instead runs the range sampler's `sweep` over the settings
of the request (`run_sweep`) and writes its report into the record; no B-rep is written.

Exit code 0 means `model.build.json` was written.
"""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True

import json
import os
import time
from pathlib import Path

from . import PROTOCOL_VERSION
from .errors import BadRequest, ModelError

RECORD_NAME = "model.build.json"
SCRATCH_NAME = ".model-scratch"
EXIT_OK, EXIT_BAD_REQUEST, EXIT_NO_GO = 0, 2, 3
PR_SET_PDEATHSIG = 1
# When this process started, and how long before the job's time limit a sweep must be done:
# time for the worker's start before this process, writing the record and the worker passing
# it on. A sweep that ran into the limit would be killed and lose its whole report.
_STARTED = time.monotonic()
SWEEP_MARGIN_S = 20.0


def _die_with_parent() -> None:
    """Ask the kernel to kill this process when the worker dies (Linux; best effort), so a
    worker that is killed never leaves a model running. Not a security boundary: the worker
    reaps this process tree itself before it trusts anything."""
    if not sys.platform.startswith("linux"):
        return
    try:
        import ctypes
        import signal

        ctypes.CDLL(None, use_errno=True).prctl(PR_SET_PDEATHSIG, int(signal.SIGKILL), 0, 0, 0)
    except (OSError, AttributeError, ValueError):
        pass


def _write_record(job_dir: Path, record: dict) -> None:
    temporary = job_dir / (RECORD_NAME + ".tmp")
    temporary.write_text(json.dumps(record, allow_nan=False), encoding="utf-8")
    os.replace(temporary, job_dir / RECORD_NAME)


def _fits(ctx) -> dict | None:
    declarations = getattr(ctx.fit, "declarations", None)
    return declarations() if callable(declarations) else None


def run(job_dir: Path) -> int:
    from .worker import _confine_to, _log, _read_request, _same_geometry

    _confine_to(job_dir, SCRATCH_NAME)
    request = _read_request(job_dir)
    if request.op == "sweep":
        return run_sweep(job_dir, request)
    if request.op != "build":
        raise BadRequest("the model process only serves the build and sweep ops")

    from .context import make_context, resolve_inside
    from .model import StdoutCapture, load_model, merge_params, rule_error, run_build

    timings: dict[str, float] = {}
    record: dict = {
        "protocol": PROTOCOL_VERSION, "status": "ok", "error": None, "stdout": "",
        "stdout_truncated": False, "fits": None, "timings": timings,
    }
    capture = StdoutCapture()
    try:
        model = load_model(resolve_inside(job_dir, request.model_path, "model_path"))
        try:
            params = merge_params(model.param_specs, request.params)
        except ValueError as exc:
            raise rule_error(model, "ParameterError", str(exc), model.params_line) from None

        started = time.perf_counter()
        import build123d  # noqa: F401 - loaded before the go signal, timed apart from the build

        from .parts import parts_from_build, save_parts

        timings["import_s"] = round(time.perf_counter() - started, 3)
        limit = request.limits.max_reference_triangles
        # The reference files of the request are read here, before the go.
        ctx = make_context(request.context, job_dir, max_reference_triangles=limit)

        # Nothing of the script has run yet. Wait for the worker's go.
        if sys.stdin.buffer.read(1) != b"g":
            _log("the worker went away before the build started")
            return EXIT_NO_GO

        started = time.perf_counter()
        parts = parts_from_build(model, run_build(model, params, ctx, capture))
        timings["build_s"] = round(time.perf_counter() - started, 3)
        if request.checks.verify_determinism:
            started = time.perf_counter()
            again = parts_from_build(
                model, run_build(model, params, make_context(
                    request.context, job_dir, max_reference_triangles=limit), capture)
            )
            timings["determinism_s"] = round(time.perf_counter() - started, 3)
            difference = _same_geometry(parts, again)
            if difference:
                raise rule_error(
                    model, "NonDeterministicBuild",
                    f"build is not deterministic: {difference}. It must not depend on random "
                    "numbers, the clock, the environment or files.",
                )
        save_parts(parts, job_dir)
        record["fits"] = _fits(ctx)
    except ModelError as exc:
        record["status"], record["error"] = "model_error", exc.to_json()
    record["stdout"], record["stdout_truncated"] = capture.text()
    try:
        _write_record(job_dir, record)
    except ValueError:
        # A number that is not finite (a card applied at a NaN position) cannot be written.
        record["fits"] = None
        record["status"] = "model_error"
        record["error"] = ModelError(
            "NonFiniteNumber",
            "a feature card was built or placed with a number that is not finite (NaN or "
            "infinity); check the sizes and positions passed to the cards",
        ).to_json()
        _write_record(job_dir, record)
    return EXIT_OK


def run_sweep(job_dir: Path, request) -> int:
    """The `sweep` op: the range sampler's `sweep` builds the script at every setting of the
    request, in this process, and its report is written into the record. The worker passes
    it on as advice only (docs/PROTOCOL.md section 3.2): it is what this process claims."""
    from .context import resolve_inside
    from .worker import _log

    timings: dict[str, float] = {}
    record: dict = {
        "protocol": PROTOCOL_VERSION, "status": "ok", "error": None, "stdout": "",
        "stdout_truncated": False, "fits": None, "timings": timings, "sweep": None,
    }
    try:
        model_path = resolve_inside(job_dir, request.model_path, "model_path")
        context = request.context.model_dump(mode="json", exclude_none=True)
        if request.context.calibration_path:
            context["calibration_path"] = str(resolve_inside(
                job_dir, request.context.calibration_path, "calibration_path"))
        started = time.perf_counter()
        import build123d  # noqa: F401 - loaded before the go signal

        from hotend_fit.sampler import sweep

        timings["import_s"] = round(time.perf_counter() - started, 3)
        if sys.stdin.buffer.read(1) != b"g":
            _log("the worker went away before the sweep started")
            return EXIT_NO_GO
        started = time.perf_counter()
        now = time.monotonic()
        record["sweep"] = sweep(
            model_path, context, base=dict(request.params),
            settings=[dict(setting) for setting in request.settings],
            min_wall=request.min_wall, deadline=now + request.budget_seconds,
            hard_deadline=_STARTED + float(request.limits.wall_seconds) - SWEEP_MARGIN_S,
        )
        timings["build_s"] = round(time.perf_counter() - started, 3)
    except ModelError as exc:
        record["status"], record["error"] = "model_error", exc.to_json()
    try:
        _write_record(job_dir, record)
    except ValueError:
        record["sweep"] = None
        record["status"] = "model_error"
        record["error"] = ModelError(
            "NonFiniteNumber", "a setting of the sweep gave a size that is not finite").to_json()
        _write_record(job_dir, record)
    return EXIT_OK


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1 or not Path(args[0]).is_dir():
        print("usage: python -I -m hotend_engine.model_run <job-dir>", file=sys.stderr)
        return EXIT_BAD_REQUEST
    _die_with_parent()
    try:
        return run(Path(args[0]).resolve())
    except BadRequest as exc:
        print(f"[hotend_engine.model_run] bad request: {exc}", file=sys.stderr, flush=True)
        return EXIT_BAD_REQUEST


if __name__ == "__main__":
    sys.exit(main())
