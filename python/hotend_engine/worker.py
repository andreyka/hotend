"""Engine worker entry point: `python -I -m hotend_engine.worker <job-dir>`.

One process, one request (docs/PROTOCOL.md section 3). The worker reads
`<job-dir>/request.json`, runs the op, writes its artefacts into the job folder and writes
`<job-dir>/result.json` last, through a temporary name and a rename.

Guarantees:
- This process never imports or executes a model script. For a build it starts the model
  process (`hotend_engine.model_run`), which runs the script and leaves data only: the
  B-rep of the parts and a record of what happened. When that process and everything it
  started is gone, the worker loads the B-rep and computes the part summary, the checks,
  the mesh, the views, the exports and result.json itself. A script can therefore make its
  build fail, but it cannot report a check outcome, a status or a size: those always
  describe the geometry it left behind, which is the geometry that is stored. The one
  exception is the `sweep` op, which stores nothing: there the model process runs the range
  sampler and its report is passed on, in a strict shape, as advice only (`run_sweep`).
- The `prepare`, `gauge` and `calibrate` ops (docs/PROTOCOL.md section 3.5) run no model
  code either: they work on the stored B-rep, on feature cards rebuilt from their kind and
  public arguments (data, validated like a build's declarations), and on numbers.
- Before the model process is given the go, the worker has read everything it needs from
  the job folder (the request, the script's PARAMS, the calibration) and loaded its
  libraries. Afterwards it reads only the model process's data files, strictly.
- On Linux the worker is a child subreaper: when the model process has ended, every process
  it left behind is found and killed before anything is read. If that cannot be done the
  worker exits with code 70 and writes no result. (Other systems have no such call; there a
  process a script detaches is the launcher's to stop.)
- Nothing is written outside the job folder. HOME, the temp directory and the XDG cache and
  config directories are pointed at a scratch folder inside the job folder before any CAD
  library is imported (some of them cache font lists under HOME), the working directory is the
  job folder, and Python writes no bytecode. The scratch folder is removed before the result
  is written.
- Exit code 0 means result.json was written; its `status` says how the op went. Exit code 2
  means a bad request: the reason is on stderr and, when the job folder is writable, in
  result.json with status `internal_error` and error type `bad_request`. When the model
  process dies without its record (the script killed the interpreter, or the geometry
  kernel crashed), the worker removes any result.json and exits with that process's exit
  code (128 + N for signal N; 70 when the code would read as one of the launcher's own).
- Stdout and stderr of this process are logs only.
"""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True

import json
import os
import shutil
import signal
import subprocess
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

from . import PROTOCOL_VERSION
from .errors import BadRequest, ModelError

REQUEST_NAME = "request.json"
RESULT_NAME = "result.json"
SCRATCH_NAME = ".engine-scratch"
MAX_REQUEST_BYTES = 1024 * 1024
MAX_RECORD_BYTES = 4 * 1024 * 1024
MAX_TRACEBACK_CHARS = 20_000
EXIT_OK, EXIT_BAD_REQUEST = 0, 2
# The model process died or could not be cleaned up: no result. Exit codes the launcher or
# the protocol give a meaning of their own (0, 2, 123 to 127) are never passed on.
EXIT_MODEL_PROCESS = 70
RESERVED_EXIT_CODES = (0, 2, 123, 124, 125, 126, 127)
PR_SET_CHILD_SUBREAPER = 36
REAP_SECONDS = 10.0
MAX_VIEW_THREADS = 4
# Two builds of a deterministic model agree to this relative tolerance.
DETERMINISM_TOLERANCE = 1e-6


def _log(message: str) -> None:
    print(f"[hotend_engine] {message}", file=sys.stderr, flush=True)


class ModelProcessFailed(Exception):
    """The model process ended without a usable record, or left processes behind that could
    not be removed. The worker writes no result and exits with `code`."""

    def __init__(self, message: str, code: int = EXIT_MODEL_PROCESS) -> None:
        super().__init__(message)
        self.code = code


def _confine_to(job_dir: Path, name: str = SCRATCH_NAME) -> Path:
    """Point every per-user and temporary location at a scratch folder in the job folder."""
    scratch = job_dir / name
    scratch.mkdir(exist_ok=True)
    for variable in ("HOME", "USERPROFILE", "XDG_CACHE_HOME", "XDG_CONFIG_HOME",
                     "XDG_DATA_HOME", "TMPDIR", "TEMP", "TMP", "MPLCONFIGDIR"):
        os.environ[variable] = str(scratch)
    os.chdir(job_dir)
    return scratch


def _write_result(job_dir: Path, result: dict) -> None:
    temporary = job_dir / (RESULT_NAME + ".tmp")
    # Never write through something that is already there (a link, a folder).
    _remove(temporary)
    if (job_dir / RESULT_NAME).is_dir() or (job_dir / RESULT_NAME).is_symlink():
        _remove(job_dir / RESULT_NAME)
    temporary.write_text(
        json.dumps(result, indent=1, allow_nan=False) + "\n", encoding="utf-8"
    )
    os.replace(temporary, job_dir / RESULT_NAME)


def _base_result(op: str | None, status: str = "ok", error: dict | None = None) -> dict:
    return {"protocol": PROTOCOL_VERSION, "op": op, "status": status, "error": error}


def _read_request(job_dir: Path):
    from pydantic import ValidationError

    from .request import decode_request, parse_request

    path = job_dir / REQUEST_NAME
    try:
        if path.stat().st_size > MAX_REQUEST_BYTES:
            raise BadRequest(f"{REQUEST_NAME} is larger than {MAX_REQUEST_BYTES} bytes")
        raw = decode_request(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise BadRequest(f"cannot read {REQUEST_NAME}: {exc}") from exc
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise BadRequest(f"{REQUEST_NAME} is not valid JSON: {exc}") from exc
    except ValueError as exc:
        # Valid JSON that no request may be: a key given twice, NaN, endless nesting.
        raise BadRequest(f"{REQUEST_NAME} is refused: {exc}") from exc
    try:
        return parse_request(raw)
    except ValidationError as exc:
        problems = "; ".join(
            f"{'.'.join(str(p) for p in e['loc']) or 'request'}: {e['msg']}" for e in exc.errors()[:8]
        )
        raise BadRequest(f"{REQUEST_NAME} does not match the protocol: {problems}") from exc
    except ValueError as exc:
        raise BadRequest(str(exc)) from exc


# --------------------------------------------------------------------------------------
# build
# --------------------------------------------------------------------------------------


def _same_geometry(first: list, second: list) -> str | None:
    """None when two builds agree, else a sentence saying where they differ."""
    if [p.name for p in first] != [p.name for p in second]:
        return "the two builds returned different part names"
    for a, b in zip(first, second, strict=True):
        va, vb = float(a.shape.volume), float(b.shape.volume)
        if abs(va - vb) > DETERMINISM_TOLERANCE * max(1.0, abs(va)):
            return f"part {a.name!r} had volume {va:.6f} mm3, then {vb:.6f} mm3"
        box_a, box_b = a.shape.bounding_box(), b.shape.bounding_box()
        corners_a = tuple(box_a.min) + tuple(box_a.max)
        corners_b = tuple(box_b.min) + tuple(box_b.max)
        scale = max(1.0, max(abs(c) for c in corners_a))
        if any(abs(x - y) > DETERMINISM_TOLERANCE * scale
               for x, y in zip(corners_a, corners_b, strict=True)):
            return f"part {a.name!r} had a different bounding box in the second build"
    return None


def _render_views(tessellation, parts, names, size, job_dir, note="", ghosts=()) -> dict:
    from .render import render_view

    views_dir = job_dir / "views"
    views_dir.mkdir(exist_ok=True)
    lows = [tuple(p.shape.bounding_box().min) for p in parts]
    highs = [tuple(p.shape.bounding_box().max) for p in parts]
    bbox = (tuple(min(c) for c in zip(*lows, strict=True)),
            tuple(max(c) for c in zip(*highs, strict=True)))
    # Placed references are drawn after the parts, pale, and named as references.
    meshes = [tessellation.meshes[p.name] for p in parts] + [g.part_mesh() for g in ghosts]
    for mesh in meshes:
        mesh.manifold()  # computed once here, not concurrently in every view
    unique = list(dict.fromkeys(names))
    # numpy releases the interpreter lock in the heavy steps, so views render in parallel.
    with ThreadPoolExecutor(max_workers=min(MAX_VIEW_THREADS, len(unique))) as pool:
        images = list(pool.map(lambda n: render_view(meshes, n, tuple(size), bbox, note), unique))
    written = {}
    for name, png in zip(unique, images, strict=True):
        (views_dir / f"{name}.png").write_bytes(png)
        written[name] = f"views/{name}.png"
    return written


# --------------------------------------------------------------------------------------
# the model process
# --------------------------------------------------------------------------------------


def _become_subreaper() -> bool:
    """Make this process the parent of every orphan among its descendants (Linux), so that
    nothing the model process starts can outlive the reaping below unnoticed."""
    if not sys.platform.startswith("linux"):
        return False
    try:
        import ctypes

        libc = ctypes.CDLL(None, use_errno=True)
        return libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0
    except (OSError, AttributeError):
        return False


def _child_pids() -> list[int] | None:
    """The pids of this process's children (Linux, /proc), or None when they cannot be read."""
    pids: list[int] = []
    try:
        for task in os.listdir("/proc/self/task"):
            text = Path("/proc/self/task", task, "children").read_text(encoding="ascii")
            pids += [int(pid) for pid in text.split()]
    except (OSError, ValueError):
        return None
    return pids


def _reap_descendants() -> None:
    """Kill and collect everything the model process left behind (Linux). Returns only when
    this process has no children at all; otherwise raises ModelProcessFailed."""
    deadline = time.monotonic() + REAP_SECONDS
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return  # no children: with the subreaper set, no descendant is alive
        if pid:
            continue
        pids = _child_pids()
        if pids is None:
            raise ModelProcessFailed(
                "the model script left processes running and they cannot be listed, so the "
                "build cannot be trusted"
            )
        for straggler in pids:
            try:
                os.kill(straggler, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        if time.monotonic() > deadline:
            raise ModelProcessFailed(
                "the model script left processes running that could not be stopped, so the "
                "build cannot be trusted"
            )
        time.sleep(0.005)


def _load_libraries() -> None:
    """Import what the rest of a build uses, while the model process imports its own copy."""
    import build123d  # noqa: F401
    import manifold3d  # noqa: F401
    import ocp_tessellate.convert  # noqa: F401

    from . import checks, export, parts, render, tessellate  # noqa: F401


def _remove(path: Path) -> None:
    try:
        if path.is_dir() and not path.is_symlink():
            shutil.rmtree(path, ignore_errors=True)
        else:
            path.unlink(missing_ok=True)
    except OSError:
        pass


def _read_record(job_dir: Path):
    """The model process's record, validated strictly, or None when there is no usable one."""
    from pydantic import ValidationError

    from .model_run import RECORD_NAME
    from .request import parse_build_record

    path = job_dir / RECORD_NAME
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_RECORD_BYTES:
            return None
        return parse_build_record(json.loads(path.read_bytes().decode("utf-8")))
    except (OSError, ValueError, ValidationError, RecursionError) as exc:
        _log(f"the model process's record is unusable: {type(exc).__name__}")
        return None


def _run_model_process(job_dir: Path, timings: dict):
    """Run the model script in its own process and return its record.

    The caller has already read everything it needs from the job folder. The go signal is
    sent only after this process has loaded its own libraries (timed as `import_s`), so
    nothing a script does can reach into what is loaded here.
    """
    from .model_run import RECORD_NAME
    from .model_run import SCRATCH_NAME as MODEL_SCRATCH_NAME
    from .parts import BREP_NAME, INDEX_NAME

    for name in (RECORD_NAME, RECORD_NAME + ".tmp", BREP_NAME, INDEX_NAME):
        _remove(job_dir / name)
    handed_over = (RECORD_NAME, BREP_NAME, INDEX_NAME)
    before = {entry.name for entry in job_dir.iterdir()}
    linux = sys.platform.startswith("linux")
    if linux and not _become_subreaper():
        raise ModelProcessFailed(
            "this system cannot make the worker a child subreaper, so a model script's "
            "processes could not be accounted for"
        )
    process = subprocess.Popen(
        [sys.executable, "-I", "-m", "hotend_engine.model_run", str(job_dir)],
        stdin=subprocess.PIPE, stdout=sys.stderr, cwd=job_dir,
    )
    try:
        started = time.perf_counter()
        _load_libraries()
        timings["import_s"] = round(time.perf_counter() - started, 3)
        started = time.perf_counter()
        try:
            process.stdin.write(b"g")
            process.stdin.close()
        except OSError:
            pass  # it is gone already; the exit code below says how
        code = process.wait()
        timings["model_process_s"] = round(time.perf_counter() - started, 3)
    except BaseException:
        process.kill()
        process.wait()
        raise
    finally:
        if linux:
            _reap_descendants()
        # Nothing a script may have put into either scratch folder is kept, and nothing else
        # it left in the job folder either: only the three files it hands over survive, and
        # only as plain files. Every artefact of this build is then written by this process.
        for name in (MODEL_SCRATCH_NAME, SCRATCH_NAME):
            _remove(job_dir / name)
        for entry in list(job_dir.iterdir()):
            if entry.name in handed_over:
                if entry.is_symlink() or not entry.is_file():
                    _remove(entry)
            elif entry.name not in before:
                _remove(entry)
        (job_dir / SCRATCH_NAME).mkdir(exist_ok=True)
    record = _read_record(job_dir)
    _remove(job_dir / RECORD_NAME)
    if record is None:
        how = f"signal {-code}" if code < 0 else f"exit code {code}"
        passed_on = 128 - code if code < 0 else code
        if passed_on in RESERVED_EXIT_CODES or not 0 < passed_on < 256:
            passed_on = EXIT_MODEL_PROCESS
        raise ModelProcessFailed(
            f"the model process ended with {how} before it reported a result (the script "
            "ended the interpreter, or the geometry kernel crashed)",
            passed_on,
        )
    if code != 0:
        _log(f"the model process reported a result and then ended with exit code {code}")
    return record


def _print_notes(model, ctx) -> dict:
    """What print preparation needs besides the solids (the `print` object of
    model.parts.json): the orientations the script's PRINT prefers and the feature cards of
    this build, written from the worker's own rebuilt cards."""
    from .context import MissingFit

    hints = model.print_hints.parts if model.print_hints else {}
    cards = [] if isinstance(ctx.fit, MissingFit) else [
        card.declaration() for card in ctx.fit.cards
    ]
    notes = {
        "orientations": {name: list(hint.vector) for name, hint in hints.items()},
        "fits": cards,
    }
    if not isinstance(ctx.fit, MissingFit) and ctx.fit.pockets:
        # The pockets of this build, so the gauge op can make their test prints.
        notes["pockets"] = [pocket.declaration() for pocket in ctx.fit.pockets]
        notes["components"] = sorted(ctx.fit.components)
    return notes


def _references_report(ctx) -> tuple[list, list]:
    """(references, components) of a build for result.json: the measures and provenance of
    every reference object the request lists, and of every component the model used."""
    from .context import MissingFit

    if isinstance(ctx.fit, MissingFit):
        return [], []
    pockets = ctx.fit.pockets
    references = []
    for reference in ctx.references.values():
        entry = reference.report()
        mine = [pocket for pocket in pockets if pocket.reference is reference]
        entry["pockets"] = [
            {"pocket": pocket.name, "fit_class": pocket.fit, "shape": pocket.shape,
             "gap": round(pocket.clearance.target, 4), "drawn_offset": round(pocket.offset, 4),
             "size": [round(v, 4) for v in pocket.size], "placed": pocket.at is not None}
            for pocket in mine
        ]
        entry["used"] = bool(mine)
        references.append(entry)
    components = []
    for component in ctx.fit.components.values():
        entry = component.report()
        entry["body"] = component.reference.report()
        components.append(entry)
    return references, components


def _clean_timings(raw: dict) -> dict[str, float]:
    return {
        name: round(float(raw[name]), 3)
        for name in ("build_s", "determinism_s")
        if isinstance(raw.get(name), (int, float)) and not isinstance(raw.get(name), bool)
        and 0 <= raw[name] < 1e6
    }


def run_build(request, job_dir: Path, timings: dict) -> dict:
    from . import checks as checks_module
    from .context import make_context, resolve_inside
    from .model import (
        STDOUT_LIMIT,
        check_declared_parts,
        load_model,
        merge_params,
        rule_error,
    )

    known = checks_module.ALL_CHECK_IDS
    unknown = [c for c in (request.checks.only or ()) if c not in known]
    if unknown:
        raise BadRequest(
            f"checks.only names unknown check(s) {', '.join(unknown)}; known: "
            f"{', '.join(known)}"
        )
    result = _base_result("build")
    result.update({
        "stdout": "", "stdout_truncated": False, "params": {}, "param_specs": {}, "parts": [],
        "checks": [], "artifacts": {}, "warnings": [], "timings": timings,
    })
    # Everything that can be known without running the script is settled here: the script is
    # parsed, never executed, in this process.
    try:
        model = load_model(resolve_inside(job_dir, request.model_path, "model_path"))
        result["param_specs"] = model.param_specs
        try:
            params = merge_params(model.param_specs, request.params)
        except ValueError as exc:
            raise rule_error(model, "ParameterError", str(exc), model.params_line) from None
        result["params"] = dict(params)
    except ModelError as exc:
        result["status"], result["error"] = "model_error", exc.to_json()
        return result
    # The calibration and the reference files are read now, before any script code runs;
    # the checks use this context and this process's own copy of every reference.
    started = time.perf_counter()
    ctx = make_context(request.context, job_dir,
                       max_reference_triangles=request.limits.max_reference_triangles)
    if request.context.references:
        timings["references_s"] = round(time.perf_counter() - started, 3)

    record = _run_model_process(job_dir, timings)
    timings.update(_clean_timings(record.timings))
    stdout = record.stdout.encode("utf-8", errors="replace")
    result["stdout"] = stdout[:STDOUT_LIMIT].decode("utf-8", errors="ignore")
    result["stdout_truncated"] = record.stdout_truncated or len(stdout) > STDOUT_LIMIT
    if record.status == "model_error":
        from .parts import BREP_NAME, INDEX_NAME

        for name in (BREP_NAME, INDEX_NAME):  # whatever it wrote before failing is no model
            _remove(job_dir / name)
        error = record.error
        result["status"] = "model_error"
        result["error"] = {
            "type": error.type[:200], "message": error.message[:4000], "line": error.line,
            "traceback": error.traceback[-MAX_TRACEBACK_CHARS:],
        }
        return result

    from .parts import BREP_NAME, INDEX_NAME, load_parts, save_parts, summarize

    try:
        parts = load_parts(job_dir)
    except BadRequest as exc:
        raise ModelProcessFailed(
            f"the model process reported a build but left no usable model ({exc})"
        ) from exc
    # From here on the script's process is gone and only its B-rep is used. The files it
    # wrote are replaced by this process's own, so what is stored is what was checked.
    _remove(job_dir / BREP_NAME)
    _remove(job_dir / INDEX_NAME)
    try:
        check_declared_parts(model, [part.name for part in parts])
    except ModelError as exc:
        result["status"], result["error"] = "model_error", exc.to_json()
        return result
    from .context import MissingFit

    if record.fits is not None and not isinstance(ctx.fit, MissingFit):
        ctx.fit.restore(record.fits)
    result["parts"] = [summarize(part, request.context.material) for part in parts]
    # Present only when the request lists references or the model used a component, so a
    # build without them answers exactly as before.
    references, components = _references_report(ctx)
    if references:
        result["references"] = references
    if components:
        result["components"] = components
        asked = [question for component in ctx.fit.components.values()
                 for question in component.questions()]
        if asked:
            result["questions"] = asked
    if references:
        from .checks.reference_fit import reference_questions

        asked = reference_questions(ctx, model.contract)
        if asked:
            result["questions"] = result.get("questions", []) + asked

    from .export import export_formats, write_step

    artifacts: dict[str, Any] = {}
    outputs = request.outputs
    started = time.perf_counter()
    if outputs.brep:
        if not save_parts(parts, job_dir, _print_notes(model, ctx)):
            result["warnings"].append(
                "The model declares so many feature cards that they were left out of "
                "model.parts.json; print preparation will not know its fit faces."
            )
        artifacts["brep"], artifacts["parts_index"] = BREP_NAME, INDEX_NAME
    if outputs.step:
        write_step(parts, job_dir / "model.step")
        artifacts["step"] = "model.step"
    timings["export_s"] = round(time.perf_counter() - started, 3)

    tessellation, tessellation_error, tessellation_crash = None, None, None
    if outputs.mesh or outputs.views or request.checks.enabled:
        from .tessellate import tessellate

        started = time.perf_counter()
        try:
            tessellation = tessellate(parts, request.limits.max_triangles)
        except Exception as exc:  # noqa: BLE001 - the build result is still worth returning
            traceback.print_exc(file=sys.stderr)
            tessellation_error = f"the model could not be tessellated ({str(exc)[:200]})"
            tessellation_crash = exc
            result["warnings"].append(f"No mesh or views were written: {tessellation_error}.")
        timings["tessellate_s"] = round(time.perf_counter() - started, 3)
        if tessellation is not None:
            unmapped = [name for name, mesh in tessellation.meshes.items()
                        if not mesh.edge_map_ok]
            if unmapped:
                result["warnings"].append(
                    "The edge numbers of mesh.json could not be matched to the solid for "
                    f"part(s) {', '.join(unmapped)}; measuring by edge index is unreliable "
                    "there."
                )

    if request.checks.enabled:
        started = time.perf_counter()
        env = checks_module.CheckEnv(
            parts, ctx, request.checks, tessellation, tessellation_error,
            contract=model.contract, print_hints=model.print_hints,
            param_specs=model.param_specs, tessellation_crash=tessellation_crash,
        )
        results = checks_module.run_checks(env)
        result["checks"] = [r.to_json() for r in results]
        if request.checks.contract:
            from .checks.contract import contract_questions

            result["questions"] = contract_questions(env) + result.get("questions", [])
        if checks_module.has_blocking_failure(results):
            result["status"] = "check_error"
        timings["checks_s"] = round(time.perf_counter() - started, 3)

    if tessellation is not None:
        ghosts = []
        if outputs.ghosts and not isinstance(ctx.fit, MissingFit) and ctx.fit.pockets:
            from .ghost import add_to_viewer, ghosts_of

            try:
                ghosts = ghosts_of(ctx.fit)
                if ghosts and outputs.mesh:
                    add_to_viewer(tessellation.viewer, ghosts)
            except Exception as exc:  # noqa: BLE001 - a ghost is display only
                traceback.print_exc(file=sys.stderr)
                ghosts = []
                result["warnings"].append(
                    f"The references could not be shown as ghosts: {str(exc)[:200]}.")
        if outputs.mesh:
            (job_dir / "mesh.json").write_text(tessellation.viewer_json(), encoding="utf-8")
            artifacts["mesh"] = "mesh.json"
        if outputs.views:
            started = time.perf_counter()
            try:
                artifacts["views"] = _render_views(
                    tessellation, parts, outputs.views, outputs.view_size, job_dir,
                    ghosts=ghosts,
                )
            except Exception as exc:  # noqa: BLE001 - the build result is still worth returning
                traceback.print_exc(file=sys.stderr)
                result["warnings"].append(f"The views could not be rendered: {str(exc)[:200]}.")
            timings["views_s"] = round(time.perf_counter() - started, 3)

    formats = tuple(f for f, wanted in (("stl", outputs.stl), ("3mf", outputs.threemf)) if wanted)
    if formats:
        started = time.perf_counter()
        exported, notes = export_formats(parts, formats, job_dir)
        artifacts.update(exported)
        result["warnings"] += notes
        timings["export_s"] = round(timings["export_s"] + time.perf_counter() - started, 3)
    result["artifacts"] = artifacts
    return result


# --------------------------------------------------------------------------------------
# measure, export, views, docs
# --------------------------------------------------------------------------------------


def run_measure(request, job_dir: Path, timings: dict) -> dict:
    from .measure import run_queries
    from .parts import load_parts

    parts = load_parts(job_dir)
    started = time.perf_counter()
    result = _base_result("measure")
    result["results"] = run_queries(parts, request.queries)
    timings["measure_s"] = round(time.perf_counter() - started, 3)
    return result


def run_export(request, job_dir: Path, timings: dict) -> dict:
    from .export import export_formats
    from .parts import load_parts, select_parts

    parts = select_parts(load_parts(job_dir), request.parts)
    started = time.perf_counter()
    result = _base_result("export")
    result["artifacts"], result["warnings"] = export_formats(parts, request.formats, job_dir)
    timings["export_s"] = round(time.perf_counter() - started, 3)
    return result


def _cut(parts: list, section) -> list:
    """Keep the half of every part behind the section plane, as seen from the iso camera."""
    from build123d import Box, Compound, Pos

    from .parts import NamedPart

    lows = [tuple(p.shape.bounding_box().min) for p in parts]
    highs = [tuple(p.shape.bounding_box().max) for p in parts]
    low = [min(c) - 1.0 for c in zip(*lows, strict=True)]
    high = [max(c) + 1.0 for c in zip(*highs, strict=True)]
    axis, keep_low = {"XY": (2, True), "XZ": (1, False), "YZ": (0, True)}[section.plane]
    if keep_low:
        high[axis] = min(high[axis], section.offset)
    else:
        low[axis] = max(low[axis], section.offset)
    size = [h - lo for lo, h in zip(low, high, strict=True)]
    if min(size) <= 0:
        return []
    keeper = Pos(*[(lo + h) / 2 for lo, h in zip(low, high, strict=True)]) * Box(*size)
    kept = []
    for part in parts:
        common = part.shape.intersect(keeper)
        solids = [s for item in (common or []) for s in item.solids()]
        if solids:
            shape = solids[0] if len(solids) == 1 else Compound(solids)
            kept.append(NamedPart(part.name, shape, part.color))
    return kept


def run_views(request, job_dir: Path, timings: dict) -> dict:
    from .parts import load_parts
    from .tessellate import tessellate

    parts = load_parts(job_dir)
    note = ""
    if request.section is not None:
        parts = _cut(parts, request.section)
        note = f"section {request.section.plane} at {request.section.offset:g} mm"
        if not parts:
            raise BadRequest("the section plane leaves nothing of the model to show")
    started = time.perf_counter()
    tessellation = tessellate(parts, request.limits.max_triangles)
    timings["tessellate_s"] = round(time.perf_counter() - started, 3)
    started = time.perf_counter()
    result = _base_result("views")
    result["artifacts"] = {
        "views": _render_views(tessellation, parts, request.views, request.size, job_dir, note)
    }
    timings["views_s"] = round(time.perf_counter() - started, 3)
    return result


def run_docs(request, job_dir: Path, timings: dict) -> dict:
    from . import docs

    started = time.perf_counter()
    entries, versions = docs.build_index()
    timings["index_s"] = round(time.perf_counter() - started, 3)
    result = _base_result("docs")
    result["versions"] = versions
    result["docs"] = docs.search(entries, request.query, request.limit)
    return result


# --------------------------------------------------------------------------------------
# prepare, gauge, calibrate
# --------------------------------------------------------------------------------------

PLATES_DIR = "plates"
BED_COLOR = "#e4e4e8"


def _nozzle_by_id(printer, nozzle_id: int | None):
    """The nozzle a request names (default: the main one). Raises BadRequest."""
    if printer is None or not printer.nozzles:
        raise BadRequest("context.printer with its nozzles is needed to lay out a plate")
    if nozzle_id is None:
        return printer.main_nozzle
    for nozzle in printer.nozzles:
        if nozzle.id == nozzle_id:
            return nozzle
    raise BadRequest(
        f"the printer has no nozzle with id {nozzle_id}; it has: "
        f"{', '.join(str(n.id) for n in printer.nozzles)}"
    )


def _bed_mesh(low, high):
    """A flat sheet at z = 0 over the reach of the nozzles, drawn under the parts."""
    import numpy as np

    from .tessellate import PartMesh

    corners = np.array([[low[0], low[1], 0.0], [high[0], low[1], 0.0],
                        [high[0], high[1], 0.0], [low[0], high[1], 0.0]])
    outline = np.stack([corners, np.roll(corners, -1, axis=0)], axis=1)
    return PartMesh("bed", BED_COLOR, "/bed", corners, np.tile([0.0, 0.0, 1.0], (4, 1)),
                    np.array([[0, 1, 2], [0, 2, 3]]), np.zeros(2, dtype=np.int64), outline, False)


def _keepout_check(placed: list, request):
    """The `keepout` check: no piece, grown by the clearance, touches a keep-out zone. The
    layout keeps pieces out of the zones, so a failure here is a fault of the layout; it
    is checked from the final boxes all the same, and it blocks."""
    from . import prepare
    from .checks.base import ERROR, FAIL, PASS, CheckResult

    collisions = [
        {"piece": p.item.name, "plate": p.plate, "zone": zone}
        for p in placed
        for zone in prepare.touched_zones(p.low[:2], p.high[:2], request.clearance,
                                          request.keepouts)
    ]
    zones = [{"id": zone.id, "min": list(zone.min), "max": list(zone.max)}
             for zone in request.keepouts]
    count = len(request.keepouts)
    return CheckResult(
        "keepout", FAIL if collisions else PASS, ERROR,
        (f"{len(collisions)} piece(s) lie on a part of the bed that must stay free. Lay the "
         "plate out again." if collisions else
         f"Every piece and its brim stay clear of the {count} part(s) of the bed that must "
         "stay free."),
        None, {"clearance": request.clearance, "zones": zones, "collisions": collisions},
    )


def _unplaced_entries(unplaced: list[dict], settings: bool) -> list[dict]:
    """The `unplaced` list of a result. The one-word `cause` goes only to a caller of the
    print settings interface; a caller from before it would refuse the unknown key."""
    if settings:
        return unplaced
    return [{key: value for key, value in entry.items() if key != "cause"}
            for entry in unplaced]


def _gauge_fit_check(placed: list, unplaced: list[dict], nozzle, request):
    """The `plate_fit` check of a gauge: every piece of every gauge lies on a plate."""
    from .checks.base import ERROR, FAIL, PASS, CheckResult

    where = f"{nozzle.role} nozzle" if nozzle.role else f"nozzle {nozzle.id}"
    data = {"nozzle_id": nozzle.id, "placed": len(placed),
            "unplaced": [entry["name"] for entry in unplaced],
            "plates": sorted({p.plate for p in placed}),
            "reach": {"x": list(nozzle.reach.x), "y": list(nozzle.reach.y),
                      "z": list(nozzle.reach.z)},
            "margin": request.margin, "clearance": request.clearance}
    if unplaced:
        return CheckResult(
            "plate_fit", FAIL, ERROR,
            f"{len(unplaced)} gauge piece(s) cannot be placed: {unplaced[0]['reason']}. A "
            "gauge is only a test with all of its pieces on the plate.",
            None, data)
    return CheckResult(
        "plate_fit", PASS, ERROR,
        f"All {len(placed)} gauge pieces lie inside the {where}'s reach.", None, data)


def _write_plates(placed: list, printer, request, job_dir: Path, result: dict,
                  timings: dict, facts: dict | None = None,
                  family: str | None = None) -> list[dict]:
    """Write every plate's 3MF and views; returns the `plates` list of the result. With
    `facts` (piece name -> print facts) every plate also says how it was laid out and
    carries the facts of its pieces, in the order of its items."""
    from . import printfacts
    from .export import write_plate_3mf
    from .plate3mf import PlateFileError, check_names
    from .render import render_view

    if request.threemf:
        # Piece names are the one text that reaches a plate file, and the slicer copies
        # them on. A name outside the fixed pattern stops the job before any plate exists.
        try:
            check_names([p.item.name for p in placed])
        except PlateFileError as exc:
            raise BadRequest(str(exc)) from exc
    reaches = [n.reach for n in printer.nozzles if n.reach is not None]
    low = (min(r.x[0] for r in reaches), min(r.y[0] for r in reaches))
    high = (max(r.x[1] for r in reaches), max(r.y[1] for r in reaches))
    count = max((p.plate for p in placed), default=0)
    plates = []
    timings["export_s"] = timings["views_s"] = 0.0
    for number in range(1, count + 1):
        members = [p for p in placed if p.plate == number]
        folder = job_dir / PLATES_DIR / str(number)
        folder.mkdir(parents=True, exist_ok=True)
        plate: dict[str, Any] = {
            "index": number, "items": [p.to_json() for p in members],
            "size": [high[0] - low[0], high[1] - low[1]],
        }
        if facts is not None:
            plate["layout"] = {
                "clearance": request.clearance, "margin": request.margin,
                "anchor": request.anchor,
                "offset": [round(v, 6) for v in members[0].offset],
            }
            plate["facts"] = printfacts.plate_facts(
                family, [facts[p.item.name] for p in members])
        if request.threemf:
            started = time.perf_counter()
            # Names and shapes only: a plate file carries no colour (see plate3mf).
            try:
                notes = write_plate_3mf(
                    [(p.item.name, p.shape) for p in members], folder / "plate.3mf")
            except PlateFileError as exc:
                raise BadRequest(f"plate {number}: {exc}") from exc
            result["warnings"] += [f"plate {number}: {note}" for note in notes]
            plate["threemf"] = f"{PLATES_DIR}/{number}/plate.3mf"
            timings["export_s"] += time.perf_counter() - started
        views = list(dict.fromkeys(request.views))
        meshes = [p.mesh for p in members if p.mesh is not None]
        if views and meshes:
            started = time.perf_counter()
            (folder / "views").mkdir(exist_ok=True)
            top = max(max(p.high[2] for p in members), 1.0)
            box = ((low[0], low[1], 0.0), (high[0], high[1], top))
            note = (f"plate {number} of {count}, bed {high[0] - low[0]:g} x "
                    f"{high[1] - low[1]:g} mm")
            for mesh in meshes:
                mesh.manifold()
            plate["views"] = {}
            for view in views:
                shown = meshes if view == "bottom" else [_bed_mesh(low, high), *meshes]
                png = render_view(shown, view, tuple(request.view_size), box, note)
                (folder / "views" / f"{view}.png").write_bytes(png)
                plate["views"][view] = f"{PLATES_DIR}/{number}/views/{view}.png"
            timings["views_s"] += time.perf_counter() - started
        plates.append(plate)
    timings["export_s"] = round(timings["export_s"], 3)
    timings["views_s"] = round(timings["views_s"], 3)
    return plates


def _restore_fits(ctx, notes: dict) -> None:
    from .context import MissingFit

    if notes.get("fits") and not isinstance(ctx.fit, MissingFit):
        ctx.fit.restore({"cards": notes["fits"], "loose": []})


def run_prepare(request, job_dir: Path, timings: dict) -> dict:
    from pydantic import ValidationError

    from . import prepare, printfacts
    from .checks import has_blocking_failure
    from .checks.base import ERROR, FAIL, PASS, WARN, WARNING, CheckResult
    from .checks.orientation import judge_bed_contact, judge_overhangs
    from .context import make_context
    from .parts import load_parts, load_print_notes, select_parts
    from .tessellate import tessellate

    parts = select_parts(load_parts(job_dir), request.parts)
    names = [part.name for part in parts]
    # The print settings interface: the facts the rules read, and a layout in two passes.
    settings = request.print_settings
    pinned = request.stage == "plate"
    lays_out = request.stage != "orient"
    if pinned:
        loose = [name for name in names if name not in request.orientations]
        if loose:
            raise BadRequest(
                f"stage \"plate\" lays out orientations that were chosen before, but "
                f"orientations gives none for {', '.join(loose)}; pass the `up` of every "
                "part from the \"orient\" result")
    for field_name in ("copies", "nozzles", "orientations"):
        unknown = [name for name in getattr(request, field_name) if name not in names]
        if unknown:
            raise BadRequest(f"{field_name} names unknown part(s) {', '.join(unknown)}; "
                             f"the plate has: {', '.join(names)}")
    if sum(request.copies.get(name, 1) for name in names) > prepare.MAX_INSTANCES:
        raise BadRequest(f"a preparation holds at most {prepare.MAX_INSTANCES} pieces")
    asked: dict[str, tuple] = {}
    if request.orientations:
        try:
            from hotend_fit.contract import describe_errors, parse_print

            asked = {name: hint.vector
                     for name, hint in parse_print(request.orientations).parts.items()}
        except ValidationError as exc:
            raise BadRequest(f"orientations is not valid: {describe_errors(exc)}") from exc
    printer, material = request.context.printer, request.context.material
    nozzles = {name: _nozzle_by_id(printer, request.nozzles.get(name)) for name in names}
    if any(nozzle.reach is None for nozzle in nozzles.values()):
        raise BadRequest("every nozzle that prints a part needs its reach in context.printer")
    ctx = make_context(request.context, job_dir, references=False)
    notes = load_print_notes(job_dir)
    _restore_fits(ctx, notes)

    started = time.perf_counter()
    tessellation = tessellate(parts, request.limits.max_triangles)
    timings["tessellate_s"] = round(time.perf_counter() - started, 3)

    started = time.perf_counter()
    limit = (material.max_overhang_deg if material and material.max_overhang_deg
             else prepare.DEFAULT_MAX_OVERHANG_DEG)
    features = prepare.fit_features(parts, ctx.fit)
    result = _base_result("prepare")
    result.update({"orientations": [], "plates": [], "unplaced": [], "checks": [],
                   "warnings": []})
    family = printfacts.family_token(material)
    part_facts: dict[str, dict] = {}
    if notes.get("fits_left_out"):
        result["warnings"].append(
            "The build left its feature cards out of model.parts.json, so the fit faces "
            "were not weighed when the orientations were chosen."
        )
    items, checks, turned = [], [], {}
    flexible = None
    if material is not None:
        from hotend_fit.materials import material_facts

        facts = material_facts(material)
        flexible = facts if facts.nozzle == "direct_drive" else None
    for part in parts:
        mesh = tessellation.meshes[part.name]
        nozzle = nozzles[part.name]
        preferred, by = None, None
        if part.name in asked:
            preferred, by = asked[part.name], "the request"
        elif part.name in notes.get("orientations", {}):
            preferred, by = notes["orientations"][part.name], "the model"
        geometry = prepare.PartGeometry(part, mesh if len(mesh.triangles) else None)
        # The clearance is room the piece needs too: an orientation that only fits the
        # reach without it is not one that prints with its brim.
        choice = prepare.choose_orientation(
            geometry, limit, features.get(part.name), preferred, by, nozzle.reach.size,
            request.margin + request.clearance, pinned,
        )
        moved, moved_mesh, translation = prepare.turned_part(part, mesh, choice.rotation)
        turned[part.name] = (moved, moved_mesh, choice)
        copies = request.copies.get(part.name, 1)
        size = tuple(float(v) for v in tuple(moved.shape.bounding_box().size))
        for copy in range(1, copies + 1):
            items.append(prepare.Item(
                name=part.name if copies == 1 else f"{part.name}.{copy}", part=part.name,
                copy=copy, shape=moved.shape, mesh=moved_mesh, color=part.color,
                rotation=choice.rotation, translation=translation, size=size, nozzle=nozzle,
            ))
        entry = choice.to_json()
        entry.update({"copies": copies, "nozzle_id": nozzle.id})
        if settings:
            part_facts[part.name] = entry["facts"] = printfacts.piece_facts(
                geometry, mesh, choice.up, limit, features.get(part.name), choice.measures)
        result["orientations"].append(entry)
    placed, unplaced = [], []
    if lays_out:
        placed, unplaced = prepare.lay_out(items, request.gap, request.margin,
                                           request.clearance, request.keepouts, request.anchor)
    result["unplaced"] = _unplaced_entries(unplaced, settings)
    if settings:
        result["stage"] = request.stage
        # Every piece of the preparation, copies included, in the order of `orientations`.
        result["facts"] = printfacts.plate_facts(
            family, [part_facts[item.part] for item in items])
    timings["prepare_s"] = round(time.perf_counter() - started, 3)

    # The checks of the plate: every part judged the way it will be printed.
    for part in parts:
        moved, moved_mesh, choice = turned[part.name]
        how = prepare.how_printed(choice.up)
        usable = moved_mesh if len(moved_mesh.triangles) else None
        checks.append(judge_overhangs(material, moved, usable, how))
        checks.append(judge_bed_contact(material, moved, usable, how))
        measures = choice.measures
        if part.name in features:
            troubled = measures["fit_overhang"] >= prepare.SUPPORT_NOTE_MM2
            weak = bool(measures["weak_snaps"])
            said = []
            if troubled:
                said.append(f"about {measures['fit_overhang']:.0f} mm2 of its mating faces "
                            "would print on support and may not keep their fit")
            if weak:
                said.append("its snap rails stand upright, across the layers, which is "
                            "their weaker direction; push the parts together gently")
            checks.append(CheckResult(
                "fit_orientation", WARN if said else PASS, WARNING,
                (f"Printed {how}, " + " and ".join(said) + "." if said else
                 f"Printed {how}, no mating face needs support and no snap is printed "
                 "across its layers."),
                part.name,
                {"fit_overhang": measures["fit_overhang"], "fit_faces": measures["fit_faces"],
                 "weak_snaps": measures["weak_snaps"]},
            ))
        mine = [p for p in placed if p.item.part == part.name]
        missing = [u for u in unplaced if u["part"] == part.name]
        nozzle = nozzles[part.name]
        where = f"{nozzle.role} nozzle" if nozzle.role else f"nozzle {nozzle.id}"
        if flexible is not None:
            # A flexible filament is fed only by the direct-drive main nozzle.
            main = printer.main_nozzle
            on_main = main is not None and nozzle.id == main.id
            checks.append(CheckResult(
                "material_nozzle", PASS if on_main else FAIL, ERROR,
                (f"'{part.name}' is printed in {flexible.family} on the {where}, the "
                 "direct-drive nozzle that feeds flexible filament."
                 if on_main else
                 f"'{part.name}' is given to the {where}, but {flexible.family} is flexible "
                 "and prints only on the main, direct-drive nozzle. Give the part to the "
                 "main nozzle."),
                part.name,
                {"nozzle_id": nozzle.id, "main_nozzle_id": None if main is None else main.id,
                 "material_family": flexible.family, "rule": flexible.nozzle,
                 "provenance": flexible.provenance},
            ))
        if not lays_out:
            continue  # where a piece lies is the business of the "plate" pass
        data = {"nozzle_id": nozzle.id, "placed": len(mine), "unplaced": len(missing),
                "plates": sorted({p.plate for p in mine}),
                "reach": {"x": list(nozzle.reach.x), "y": list(nozzle.reach.y),
                          "z": list(nozzle.reach.z)}, "margin": request.margin}
        # With a clearance the piece must keep it, and the margin, to the edge of the reach.
        keep = 0.0
        if settings:
            data["clearance"] = request.clearance
            keep = request.clearance + request.margin
        if missing:
            checks.append(CheckResult(
                "plate_fit", FAIL, ERROR,
                f"'{part.name}' cannot be placed: {missing[0]['reason']}. Make it smaller, "
                "split it, or give it to a nozzle with more reach.",
                part.name, data,
            ))
        else:
            inside = all(
                p.low[0] >= nozzle.reach.x[0] + keep - 1e-6
                and p.high[0] <= nozzle.reach.x[1] - keep + 1e-6
                and p.low[1] >= nozzle.reach.y[0] + keep - 1e-6
                and p.high[1] <= nozzle.reach.y[1] - keep + 1e-6
                for p in mine
            )
            pieces = "It lies" if len(mine) == 1 else f"All {len(mine)} copies lie"
            checks.append(CheckResult(
                "plate_fit", PASS if inside else FAIL, ERROR,
                f"{pieces} inside the {where}'s reach"
                + (f", on plates {', '.join(str(n) for n in data['plates'])}."
                   if len(data["plates"]) > 1 else "."),
                part.name, data,
            ))
    plate_count = max((p.plate for p in placed), default=0)
    if plate_count > 1:
        result["warnings"].append(
            f"The pieces do not fit on one plate: they are split over {plate_count} plates "
            f"(with {request.gap:g} mm between pieces)."
        )
    if unplaced:
        result["warnings"].append(
            f"{len(unplaced)} piece(s) fit no plate and were left out: "
            + "; ".join(f"{u['name']}: {u['reason']}" for u in unplaced[:3])
            + ("; ..." if len(unplaced) > 3 else "")
        )
    if lays_out:
        if settings and request.keepouts:
            checks.append(_keepout_check(placed, request))
        result["plates"] = _write_plates(
            placed, printer, request, job_dir, result, timings,
            {p.item.name: part_facts[p.item.part] for p in placed} if settings else None,
            family)
    result["checks"] = [check.to_json() for check in checks]
    if has_blocking_failure(checks):
        result["status"] = "check_error"
    return result


def run_gauge(request, job_dir: Path, timings: dict) -> dict:
    from types import SimpleNamespace

    import numpy as np

    from . import prepare, printfacts
    from .context import MissingFit, make_context
    from .parts import PALETTE, NamedPart, load_print_notes
    from .tessellate import tessellate

    settings = request.print_settings
    printer = request.context.printer
    nozzle = _nozzle_by_id(printer, request.nozzle_id)
    if nozzle.reach is None:
        raise BadRequest("the nozzle that prints the gauges needs its reach in context.printer")
    ctx = make_context(request.context, job_dir,
                       max_reference_triangles=request.limits.max_reference_triangles)
    if isinstance(ctx.fit, MissingFit):
        raise BadRequest("gauges need the hotend_fit package, which is not installed")
    from hotend_fit.cards import CARDS, restore_cards
    from hotend_fit.cards.base import ABOVE_THE_BED
    from hotend_fit.components import ComponentError, list_components, use_component
    from hotend_fit.reference import ReferenceProblem, parse_direction, restore_pockets

    problems: list[str] = []
    if request.cards is None:
        notes = load_print_notes(job_dir)
        declared = notes.get("fits") or []
        pockets = notes.get("pockets") or []
        if not declared and not pockets:
            raise BadRequest(
                "no feature cards to make gauges of, and no pockets: name them in `cards`, "
                "or put the model.parts.json of a build that used them in the job folder"
            )
        for component_id in notes.get("components", []):
            try:
                use_component(ctx.fit, component_id)
            except ComponentError as exc:
                problems.append(str(exc))
    else:
        unknown = [str(index + 1) for index, ref in enumerate(request.cards)
                   if ref.card not in CARDS and ref.card != "pocket"]
        if unknown:
            # Named by its place in `cards`: the text of the request is not repeated.
            raise BadRequest(
                (f"card {unknown[0]} is not a known card" if len(unknown) == 1 else
                 f"cards {', '.join(unknown)} are not known cards")
                + f"; known: {', '.join(CARDS)}, pocket")
        declared = [
            {"card": ref.card, "name": ref.name or f"{ref.card}_{index + 1}",
             "args": ref.args, "frames": {"a": None, "b": None}}
            for index, ref in enumerate(request.cards) if ref.card != "pocket"
        ]
        pockets = []
        for index, ref in enumerate(request.cards):
            if ref.card != "pocket":
                continue
            args = ref.args
            if not set(args) <= {"reference", "fit", "shape", "direction"} or not isinstance(
                    args.get("reference"), str):
                raise BadRequest("a pocket gauge takes the args reference (a name of "
                                 "context.references or a component id), fit, shape, direction")
            name = args["reference"]
            if name not in ctx.fit.references and name in list_components():
                use_component(ctx.fit, name)
            try:
                direction = list(parse_direction(args.get("direction", "+z")))
            except ReferenceProblem as exc:
                raise BadRequest(str(exc)) from exc
            pockets.append({
                "pocket": ref.name or f"pocket_{index + 1}", "reference": name,
                "fit": args.get("fit", "cradle"), "shape": args.get("shape", "silhouette"),
                "direction": direction, "removable": [], "frame": None, "through": False})
    problems += restore_cards(ctx.fit, declared)
    if pockets:
        problems += restore_pockets(ctx.fit, pockets)
    if problems and request.cards is not None:
        raise BadRequest("; ".join(problems))
    result = _base_result("gauge")
    result.update({"gauges": [], "plates": [], "unplaced": [], "warnings": list(problems)})

    # One gauge per distinct card: two equal joints are tested by one print.
    started = time.perf_counter()
    seen: set[str] = set()
    pieces: list[NamedPart] = []
    owners: list[tuple[dict, str]] = []
    # Per gauge: its pieces and what says which of their faces are mating faces (the
    # card's fit pairs, or the name of the pocket whose ring it is).
    mating: list[tuple[list[NamedPart], Any]] = []
    for card in ctx.fit.cards:
        key = json.dumps([card.kind, card._args()], sort_keys=True)
        if key in seen:
            continue
        seen.add(key)
        gauge = card.gauge_set()
        entry = {
            "card": card.kind, "name": card.name, "args": card._args(), "parts": [],
            "size": [round(v, 3) for v in gauge.size],
            "dimensions": gauge.dimensions,
            "clearances": [
                {"pair": pair.name, "fit_class": pair.clearance.fit_class,
                 "gap": round(pair.clearance.gap, 6), "provenance": pair.clearance.provenance}
                for pair in gauge.fits
            ],
        }
        for half, shape in zip((card.half_a, card.half_b), gauge.parts.values(), strict=True):
            name = f"{card.name}.{half}"
            entry["parts"].append(name)
            for dimension in entry["dimensions"]:
                if dimension["half"] == half:
                    dimension["object"] = name
            pieces.append(NamedPart(f"g{len(pieces)}", shape, PALETTE[len(result["gauges"])
                                                                      % len(PALETTE)]))
            owners.append((entry, name))
        mating.append((pieces[-2:], gauge.fits))
        result["gauges"].append(entry)
    # One gauge per distinct pocket: a ring the thing must drop through (a clearance fit)
    # or a band of the pocket's rim that must stretch over it (a stretch fit).
    for pocket in ctx.fit.pockets:
        key = json.dumps(["pocket", pocket._args()], sort_keys=True)
        if key in seen:
            continue
        seen.add(key)
        gauge = pocket.gauge_set()
        name = f"{pocket.name}.ring"
        entry = {
            "card": "pocket", "name": pocket.name, "args": pocket._args(), "parts": [name],
            "size": [round(v, 3) for v in gauge.size],
            "dimensions": gauge.dimensions,
            "clearances": [{"pair": pocket.name, "fit_class": pocket.fit,
                            "gap": round(pocket.clearance.gap, 6),
                            "provenance": pocket.clearance.provenance}],
        }
        for dimension in entry["dimensions"]:
            dimension["object"] = name
            # The ring's inside is its mating face and rises from the bed, where the
            # first layers print wide: the caliper belongs above them.
            if not dimension["what"].endswith(ABOVE_THE_BED):
                dimension["what"] += ABOVE_THE_BED
        pieces.append(NamedPart(f"g{len(pieces)}", gauge.parts["gauge_ring"],
                                PALETTE[len(result["gauges"]) % len(PALETTE)]))
        owners.append((entry, name))
        mating.append((pieces[-1:], pocket.name))
        result["gauges"].append(entry)
    timings["gauges_s"] = round(time.perf_counter() - started, 3)

    started = time.perf_counter()
    tessellation = tessellate(pieces, request.limits.max_triangles)
    timings["tessellate_s"] = round(time.perf_counter() - started, 3)
    facts: dict[str, dict] = {}
    family = printfacts.family_token(request.context.material)
    if settings:
        material = request.context.material
        limit = (material.max_overhang_deg if material and material.max_overhang_deg
                 else prepare.DEFAULT_MAX_OVERHANG_DEG)
        fits: dict[str, prepare.PartFit] = {}
        for members, source in mating:
            if isinstance(source, str):
                # A pocket's ring: its inside, the wall around the hole in its foot, is
                # what the thing must pass or stretch.
                ring = members[0]
                fits[ring.name] = prepare.PartFit(faces=dict.fromkeys(
                    printfacts.faces_on_inner_loops(ring.shape), source))
            else:
                fits.update(prepare.fit_features(
                    members, SimpleNamespace(declared_fits=source, cards=[])))
        for piece, (entry, name) in zip(pieces, owners, strict=True):
            mesh = tessellation.meshes[piece.name]
            geometry = prepare.PartGeometry(piece, mesh if len(mesh.triangles) else None)
            # A gauge half is drawn the way it is printed: +Z away from the bed.
            facts[name] = printfacts.piece_facts(
                geometry, mesh, (0.0, 0.0, 1.0), limit, fits.get(piece.name))
            entry.setdefault("facts", {})[name] = facts[name]
            if facts[name]["fit_at_bed"]:
                # Whatever the card's own text says: a piece measured as having a mating
                # face that rises from the bed is never to be measured at the bed.
                for dimension in entry["dimensions"]:
                    if (dimension.get("object") == name
                            and not dimension["what"].endswith(ABOVE_THE_BED)):
                        dimension["what"] += ABOVE_THE_BED
        result["stage"] = request.stage
        result["facts"] = printfacts.plate_facts(family, [facts[name] for _, name in owners])
    if request.stage == "orient":
        return result  # the facts only: nothing is laid out and no file is written
    items = []
    for piece, (_entry, name) in zip(pieces, owners, strict=True):
        # A gauge half is drawn lying on the bed already; it is only set at the origin.
        moved, mesh, translation = prepare.turned_part(
            piece, tessellation.meshes[piece.name], np.eye(3))
        size = tuple(float(v) for v in tuple(moved.shape.bounding_box().size))
        items.append(prepare.Item(
            name=name, part=name, copy=1, shape=moved.shape, mesh=mesh, color=piece.color,
            rotation=np.eye(3), translation=translation, size=size, nozzle=nozzle,
        ))
    placed, unplaced = prepare.lay_out(items, request.gap, request.margin, request.clearance,
                                       request.keepouts, request.anchor)
    result["unplaced"] = _unplaced_entries(unplaced, settings)
    if unplaced:
        result["warnings"].append(
            f"{len(unplaced)} gauge piece(s) fit no plate: "
            + "; ".join(f"{u['name']}: {u['reason']}" for u in unplaced[:3])
        )
    if settings:
        # A gauge tests a fit with all of its pieces: one that found no place fails the
        # job, so that a plate without it is never taken for the gauge.
        checks = [_gauge_fit_check(placed, unplaced, nozzle, request)]
        if request.keepouts:
            checks.append(_keepout_check(placed, request))
        result["checks"] = [check.to_json() for check in checks]
        if any(check.status != "pass" for check in checks):
            result["status"] = "check_error"
    result["plates"] = _write_plates(placed, printer, request, job_dir, result, timings,
                                     facts if settings else None, family)
    return result


def run_references(request, job_dir: Path, timings: dict) -> dict:
    """Say what the request's reference files hold, without building anything: every
    object of each file with its plate, parts and size, and the measures of the object a
    selection names. A file whose selection is missing or ambiguous is listed with status
    `needs_selection`, so the caller can ask the user which object is meant."""
    from . import reference_io
    from .context import MissingFit, load_reference, make_context, resolve_inside

    ctx = make_context(request.context, job_dir, references=False)
    if isinstance(ctx.fit, MissingFit):
        raise BadRequest("references need the hotend_fit package, which is not installed")
    from hotend_fit.components import ComponentError, list_components, load_component
    from hotend_fit.materials import material_facts

    result = _base_result("references")
    result.update({"references": [], "components": [],
                   "available_components": list_components(), "warnings": []})
    limit = request.limits.max_reference_triangles
    started = time.perf_counter()
    for spec in request.context.references:
        path = resolve_inside(job_dir, spec.path, f"context.references[{spec.name!r}].path")
        entry: dict = {"name": spec.name, "file": spec.path, "status": "ok", "message": ""}
        loaded = None
        try:
            reference, loaded = load_reference(spec, job_dir, limit)
            entry["reference"] = reference.report()
        except BadRequest as exc:
            entry["message"] = str(exc)
            try:
                loaded = reference_io.read_reference_file(path, spec.format,
                                                          max_triangles=limit)
                entry["status"] = "needs_selection"
            except reference_io.ReferenceFileError:
                entry["status"] = "unreadable"
        if loaded is not None:
            entry.update({
                "format": loaded.format, "bytes": loaded.bytes, "sha256": loaded.sha256,
                "unit": loaded.unit, "producer": loaded.producer, "notes": list(loaded.notes),
                "objects": [found.listing() for found in loaded.objects[:256]],
            })
        result["references"].append(entry)
    timings["references_s"] = round(time.perf_counter() - started, 3)
    for component_id in request.components:
        try:
            component = load_component(component_id)
        except ComponentError as exc:
            raise BadRequest(str(exc)) from exc
        entry = component.report(used=list(component.features))
        entry["rules"] = {name: datum.to_json() for name, datum in component.rules.items()}
        entry["envelope"] = {name: datum.to_json() for name, datum in component.envelope.items()}
        result["components"].append(entry)
    if request.context.material is not None:
        result["material"] = material_facts(request.context.material).to_json()
    return result


def run_calibrate(request, job_dir: Path, timings: dict) -> dict:
    from .context import resolve_inside

    try:
        from hotend_fit.calibration import (
            CalibrationError,
            calibrate_from_gauge,
            table_json,
        )
    except ImportError as exc:
        raise BadRequest("calibration needs the hotend_fit package") from exc
    printer, material = request.context.printer, request.context.material
    if printer is None or material is None:
        raise BadRequest("context.printer (with its nozzles) and context.material say "
                         "which calibration entry the readings belong to")
    nozzle = _nozzle_by_id(printer, request.nozzle_id)
    path = (resolve_inside(job_dir, request.context.calibration_path,
                           "context.calibration_path")
            if request.context.calibration_path else None)
    try:
        table, changes = calibrate_from_gauge(
            path, [reading.model_dump() for reading in request.readings],
            measured_at=request.measured_at, printer_id=printer.id, material=material.id,
            nozzle_id=nozzle.id, nozzle_diameter=nozzle.diameter,
        )
    except CalibrationError as exc:
        raise BadRequest(str(exc)) from exc
    result = _base_result("calibrate")
    result["table"] = table_json(table)
    result["changes"] = changes
    entry = table.find(material.id, nozzle.id)
    result["entry_provenance"] = entry.provenance
    return result


def run_sweep(request, job_dir: Path, timings: dict) -> dict:
    """The `sweep` op: the model at a few parameter settings, judged one after the other,
    with nothing stored (docs/PROTOCOL.md section 3.2).

    This process still runs no script code: it parses the script's PARAMS, refuses a
    setting that names an unknown parameter or a value outside its declared range before
    anything runs, and starts the model process, where the range sampler builds and checks
    every setting (`hotend_fit.sampler.sweep`). The report comes back in the model
    process's record and is passed on only in the strict shape of `SweepReport`: it is
    what that process claims, so the core gives it to the agent as advice and never makes
    a version, a check outcome or a print of it. A build of a version is always judged
    here, in this process."""
    from .context import resolve_inside
    from .model import load_model, merge_params, rule_error

    result = _base_result("sweep")
    result.update({"stdout": "", "params": {}, "param_specs": {}, "warnings": []})
    try:
        model = load_model(resolve_inside(job_dir, request.model_path, "model_path"))
        result["param_specs"] = model.param_specs
        try:
            result["params"] = dict(merge_params(model.param_specs, request.params))
            for setting in request.settings:
                merge_params(model.param_specs, {**request.params, **setting})
        except ValueError as exc:
            raise rule_error(model, "ParameterError", str(exc), model.params_line) from None
    except ModelError as exc:
        result["status"], result["error"] = "model_error", exc.to_json()
        return result
    record = _run_model_process(job_dir, timings)
    timings.update(_clean_timings(record.timings))
    if record.status == "model_error":
        error = record.error
        result["status"] = "model_error"
        result["error"] = {
            "type": error.type[:200], "message": error.message[:4000], "line": error.line,
            "traceback": error.traceback[-MAX_TRACEBACK_CHARS:],
        }
        return result
    if record.sweep is None:
        raise ModelProcessFailed("the model process reported a sweep without its report")
    result["sweep"] = record.sweep.model_dump(mode="json")
    return result


_OPS = {"build": run_build, "measure": run_measure, "export": run_export, "views": run_views,
        "docs": run_docs, "prepare": run_prepare, "gauge": run_gauge,
        "calibrate": run_calibrate, "references": run_references, "sweep": run_sweep}


def run_job(job_dir: Path) -> int:
    """Handle the request in `job_dir`. Returns the process exit code."""
    began = time.perf_counter()
    timings: dict[str, float] = {}
    op = None
    scratch = None
    try:
        scratch = _confine_to(job_dir)
        request = _read_request(job_dir)
        op = request.op
        result = _OPS[op](request, job_dir, timings)
        result.setdefault("timings", timings)
        code = EXIT_OK
    except ModelProcessFailed as exc:
        # No verdict can be given. Nothing the script may have written as a result is left.
        _log(str(exc))
        from .parts import BREP_NAME, INDEX_NAME

        for name in (RESULT_NAME, RESULT_NAME + ".tmp", BREP_NAME, INDEX_NAME):
            _remove(job_dir / name)
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)
        return exc.code
    except BadRequest as exc:
        _log(f"bad request: {exc}")
        result = _base_result(op, "internal_error", {
            "type": "bad_request", "message": str(exc), "line": None, "traceback": "",
        })
        code = EXIT_BAD_REQUEST
    except Exception as exc:  # noqa: BLE001 - every failure must still produce result.json
        trace = traceback.format_exc()
        _log(trace)
        result = _base_result(op, "internal_error", {
            "type": type(exc).__name__, "message": str(exc)[:2000], "line": None,
            "traceback": trace[-8000:],
        })
        code = EXIT_OK
    if scratch is not None:
        shutil.rmtree(scratch, ignore_errors=True)
    timings["total_s"] = round(time.perf_counter() - began, 3)
    result["timings"] = timings
    _write_result(job_dir, result)
    return code


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        _log("usage: python -I -m hotend_engine.worker <job-dir>")
        return EXIT_BAD_REQUEST
    job_dir = Path(args[0])
    if not job_dir.is_dir():
        _log(f"bad request: job folder {args[0]!r} does not exist")
        return EXIT_BAD_REQUEST
    return run_job(job_dir.resolve())


if __name__ == "__main__":
    sys.exit(main())
