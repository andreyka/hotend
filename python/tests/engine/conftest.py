"""Shared fixtures for the engine tests.

The worker is always driven the way the Rust core drives it: a job folder with request.json,
`python -I -m hotend_engine.worker <job-dir>`, then result.json. Starting a worker costs a few
seconds (importing build123d), so every worker run the tests need is started once per session,
four at a time, and the tests only read the results.
"""

from __future__ import annotations

import copy
import json
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path

import lib3mf
import numpy as np
import pytest
from landlock_exec import landlock_abi

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from result_contract import check_job_result

MODELS = Path(__file__).resolve().parents[1] / "models"
LANDLOCK_EXEC = Path(__file__).resolve().with_name("landlock_exec.py")
WORKER = (sys.executable, "-I", "-m", "hotend_engine.worker")
WORKER_TIMEOUT_S = 120

X2D_CONTEXT = {
    "printer": {"id": "bambu-x2d", "nozzles": [
        {"id": 1, "role": "main", "diameter": 0.4,
         "reach": {"x": [0, 256], "y": [0, 256], "z": [0, 260]}},
        {"id": 0, "role": "auxiliary", "diameter": 0.4,
         "reach": {"x": [20.5, 256], "y": [0, 256], "z": [0, 256]}}]},
    "material": {"id": "pla-basic", "family": "PLA"},
}


def build_request(**overrides) -> dict:
    request = {
        "protocol": 0,
        "op": "build",
        "model_path": "model.py",
        "params": {},
        "context": copy.deepcopy(X2D_CONTEXT),
        "outputs": {"step": True, "brep": True, "mesh": True, "views": []},
        "checks": {"enabled": True, "min_wall": 0.8, "verify_determinism": False},
        "limits": {"wall_seconds": 120, "max_triangles": 2_000_000},
    }
    request.update(overrides)
    return request


@dataclass
class Job:
    dir: Path
    returncode: int
    stderr: str

    @property
    def result(self) -> dict:
        return json.loads((self.dir / "result.json").read_text(encoding="utf-8"))

    def files(self) -> set[str]:
        return {p.relative_to(self.dir).as_posix() for p in self.dir.rglob("*") if p.is_file()}

    def checks(self, check_id: str) -> list[dict]:
        return [c for c in self.result["checks"] if c["id"] == check_id]

    def check(self, check_id: str, part: str | None = None) -> dict:
        found = [c for c in self.checks(check_id) if part is None or c["part"] == part]
        assert len(found) == 1, f"expected one {check_id} result for {part}, got {found}"
        return found[0]


def prepare_job(job_dir: Path, request: dict | str | None, model: str | None = None,
                copy_from: Path | None = None) -> Path:
    """Lay out a job folder: request.json, optionally a model script and a built model."""
    job_dir.mkdir(parents=True)
    if model is not None:
        shutil.copy(MODELS / model, job_dir / "model.py")
    if copy_from is not None:
        for name in ("model.brep", "model.parts.json"):
            shutil.copy(copy_from / name, job_dir / name)
    if isinstance(request, str):
        (job_dir / "request.json").write_text(request, encoding="utf-8")
    elif request is not None:
        (job_dir / "request.json").write_text(json.dumps(request), encoding="utf-8")
    return job_dir


def run_worker(job_dir: Path, confined: bool = False, core_reads: bool = True) -> Job:
    """Run the worker on a job folder. `core_reads=False` only for a request the core never
    makes whose answer the core would refuse; the test says why."""
    command = [*WORKER, str(job_dir)]
    if confined:
        command = [sys.executable, "-I", str(LANDLOCK_EXEC), str(job_dir), "--", *command]
    done = subprocess.run(
        command, capture_output=True, text=True, timeout=WORKER_TIMEOUT_S, cwd=job_dir.parent,
        check=False,
    )
    if core_reads:
        check_job_result(job_dir)  # the core must be able to read what the worker wrote
    return Job(job_dir, done.returncode, done.stderr)


def run_all(specs: dict[str, tuple[Path, bool]]) -> dict[str, Job]:
    with ThreadPoolExecutor(max_workers=4) as pool:
        jobs = pool.map(lambda item: run_worker(item[1][0], item[1][1]), specs.items())
        return dict(zip(specs, jobs, strict=True))


FULL_OUTPUTS = {"step": True, "brep": True, "mesh": True,
                "views": ["iso", "front", "top", "right"], "stl": True, "threemf": True}

ASKED_CHECKS = {"enabled": True, "min_wall": 0.8, "orientation": "prepared", "contract": True,
                "undeclared_joints": True}
SMALL_VIEW = [320, 240]

# name -> (model file, request overrides)
BUILDS: dict[str, tuple[str, dict]] = {
    "box": ("box.py", {"params": {"width": 90.0}, "outputs": FULL_OUTPUTS}),
    "box_again": ("box.py", {"params": {"width": 90.0}, "outputs": FULL_OUTPUTS}),
    "box_no_checks": ("box.py", {
        "checks": {"enabled": False},
        "outputs": {"step": False, "brep": False, "mesh": False, "views": []}}),
    "bracket": ("bracket.py", {
        "params": {"holes": 3},
        "outputs": {"step": True, "brep": True, "mesh": True, "views": ["iso"]}}),
    "lidded_box": ("lidded_box.py", {
        "checks": {"enabled": True, "min_wall": 0.8, "verify_determinism": True}}),
    "broken_syntax": ("broken_syntax.py", {}),
    "broken_exception": ("broken_exception.py", {}),
    "broken_not_solid": ("broken_not_solid.py", {}),
    "broken_too_big": ("broken_too_big.py", {}),
    "broken_thin_wall": ("broken_thin_wall.py", {}),
    "broken_params": ("broken_params.py", {}),
    "broken_nondeterministic": ("broken_nondeterministic.py", {
        "checks": {"enabled": True, "verify_determinism": True}}),
    "param_out_of_range": ("box.py", {"params": {"width": 500.0}}),
    # Milestone 2: the options a caller sets when it prints the prepared plate.
    "cable_box_asked": ("cable_box.py", {
        "checks": ASKED_CHECKS,
        "outputs": {"step": True, "brep": True, "mesh": True, "views": [], "threemf": True}}),
    "lidded_box_asked": ("lidded_box.py", {"checks": ASKED_CHECKS}),
    "print_hint": ("print_hint.py", {"checks": ASKED_CHECKS}),
    "broken_contract": ("broken_contract.py", {}),
    # 254 x 20 x 5 mm: longer than the bed less its margin, but it fits standing on its end.
    "plank_stands": ("plank.py", {"checks": ASKED_CHECKS}),
    # 254 x 254 x 5 mm: inside the bare reach, too large for the plate whichever way up.
    "plank_too_wide": ("plank.py", {"params": {"width": 254.0}, "checks": ASKED_CHECKS}),
    "param_unknown": ("box.py", {"params": {"lenght": 50.0}}),
}


@pytest.fixture(scope="session")
def builds(tmp_path_factory) -> dict[str, Job]:
    """Every build the tests look at, run once."""
    root = tmp_path_factory.mktemp("builds")
    specs = {
        name: (prepare_job(root / name, build_request(**overrides), model), False)
        for name, (model, overrides) in BUILDS.items()
    }
    if landlock_abi() > 0:
        confined = prepare_job(root / "confined", build_request(outputs=FULL_OUTPUTS), "box.py")
        specs["confined"] = (confined, True)
    return run_all(specs)


@pytest.fixture(scope="session")
def ops(tmp_path_factory, builds) -> dict[str, Job]:
    """The measure, export, views and docs ops, run once on the built lidded box."""
    root = tmp_path_factory.mktemp("ops")
    built = builds["lidded_box"].dir
    requests = {
        "measure": {"op": "measure", "queries": [
            {"kind": "bbox", "part": "lid"},
            {"kind": "bbox"},
            {"kind": "volume", "part": "box"},
            {"kind": "volume"},
            {"kind": "distance", "a": {"part": "box"}, "b": {"part": "lid"}},
            {"kind": "distance", "a": {"point": [0, 0, 100]}, "b": {"part": "lid"}},
            {"kind": "section", "plane": "XZ", "offset": 0},
            {"kind": "section", "plane": "XY", "offset": 10, "part": "box"},
            {"kind": "bbox", "part": "nope"},
            {"kind": "mystery"},
            {"kind": "distance", "a": {"part": "box", "face": 9999}, "b": {"part": "lid"}},
        ]},
        "export_all": {"op": "export", "formats": ["3mf", "stl", "step"]},
        "export_lid": {"op": "export", "formats": ["stl"], "parts": ["lid"]},
        "export_unknown_part": {"op": "export", "formats": ["stl"], "parts": ["handle"]},
        "views": {"op": "views", "views": ["iso", "top"], "size": [320, 240]},
        "views_section": {"op": "views", "views": ["iso"], "size": [320, 240],
                          "section": {"plane": "XZ", "offset": 0}},
        "docs": {"op": "docs", "query": "fillet edges", "limit": 3},
    }
    specs = {
        name: (prepare_job(root / name, {"protocol": 0, **request},
                           copy_from=None if name == "docs" else built), False)
        for name, request in requests.items()
    }
    specs["measure_without_model"] = (
        prepare_job(root / "measure_without_model",
                    {"protocol": 0, "op": "measure", "queries": [{"kind": "volume"}]}), False)
    return run_all(specs)


# name -> (the build whose model.brep and model.parts.json it works on, request)
PLATE_OPS: dict[str, tuple[str | None, dict]] = {
    "prepare_cable_box": ("cable_box_asked", {
        "op": "prepare", "views": ["top"], "view_size": SMALL_VIEW}),
    "prepare_twelve_lids": ("cable_box_asked", {
        "op": "prepare", "parts": ["lid"], "copies": {"lid": 12}, "nozzles": {"lid": 0},
        "views": []}),
    "prepare_hints": ("print_hint", {"op": "prepare", "views": [], "threemf": False}),
    "prepare_user_choice": ("print_hint", {
        "op": "prepare", "parts": ["post"], "orientations": {"post": {"up": "+x"}},
        "views": [], "threemf": False}),
    "prepare_small_bed": ("box", {
        "op": "prepare", "views": [], "context": {
            "printer": {"id": "tiny", "nozzles": [
                {"id": 1, "role": "main", "diameter": 0.4,
                 "reach": {"x": [0, 60], "y": [0, 60], "z": [0, 60]}}]},
            "material": {"id": "pla-basic", "family": "PLA"}}}),
    "prepare_plank_stands": ("plank_stands", {"op": "prepare", "views": []}),
    "prepare_plank_too_wide": ("plank_too_wide", {
        "op": "prepare", "views": [], "threemf": False}),
    "prepare_unknown_part": ("box", {"op": "prepare", "copies": {"handle": 2}}),
    "prepare_bad_orientation": ("box", {
        "op": "prepare", "orientations": {"part": {"up": "sideways"}}}),
    "prepare_without_printer": ("box", {"op": "prepare", "context": {}}),
    "gauge_of_model": ("cable_box_asked", {
        "op": "gauge", "views": ["top"], "view_size": SMALL_VIEW}),
    "gauge_of_cards": (None, {"op": "gauge", "views": [], "cards": [
        {"card": "peg_hole", "args": {"diameter": 6.0, "length": 6.0, "fit": "slide"}}]}),
    "gauge_unknown_card": (None, {"op": "gauge", "cards": [{"card": "velcro"}]}),
    "gauge_without_cards": ("box", {"op": "gauge"}),
    "calibrate": (None, {
        "op": "calibrate", "measured_at": "2026-11-17T09:00:00Z", "readings": [
            {"feature": "hole", "drawn": 6.4, "measured": 6.2},
            {"feature": "hole", "drawn": 6.4, "measured": 6.16},
            {"feature": "peg", "drawn": 6.0, "measured": 6.1}]}),
    "calibrate_misread": (None, {
        "op": "calibrate", "measured_at": "2026-11-17T09:00:00Z", "readings": [
            {"feature": "hole", "drawn": 6.4, "measured": 0.62}]}),
    "calibrate_without_date": (None, {
        "op": "calibrate", "readings": [{"feature": "hole", "drawn": 6.4, "measured": 6.2}]}),
}


@pytest.fixture(scope="session")
def plates(tmp_path_factory, builds) -> dict[str, Job]:
    """The prepare, gauge and calibrate ops, run once."""
    root = tmp_path_factory.mktemp("plates")
    specs = {}
    for name, (built, request) in PLATE_OPS.items():
        request = {"protocol": 0, "context": copy.deepcopy(X2D_CONTEXT), **request}
        specs[name] = (prepare_job(root / name, request,
                                   copy_from=builds[built].dir if built else None), False)
    if landlock_abi() > 0:
        # The same two requests with every write outside the job folder denied.
        for name in ("prepare_cable_box", "gauge_of_model"):
            built, request = PLATE_OPS[name]
            request = {"protocol": 0, "context": copy.deepcopy(X2D_CONTEXT), **request}
            specs[f"{name}_confined"] = (
                prepare_job(root / f"{name}_confined", request, copy_from=builds[built].dir),
                True)
    return run_all(specs)


def ok(job) -> dict:
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "ok", (result["error"], result.get("checks"))
    return result


def read_3mf(path) -> dict[str, dict]:
    """Every object on the plate of a 3MF file, read back with lib3mf: name -> volume and
    bounding box, in millimetres, with the build item's transform applied."""
    model = lib3mf.get_wrapper().CreateModel()
    reader = model.QueryReader("3mf")
    reader.ReadFromFile(str(path))
    assert reader.GetWarningCount() == 0
    assert model.GetUnit() == lib3mf.ModelUnit.MilliMeter
    meshes = {}
    iterator = model.GetMeshObjects()
    while iterator.MoveNext():
        mesh = iterator.GetCurrentMeshObject()
        assert mesh.IsManifoldAndOriented(), mesh.GetName()
        vertices = np.array([v.Coordinates[:] for v in mesh.GetVertices()], dtype=float)
        triangles = np.array([t.Indices[:] for t in mesh.GetTriangleIndices()], dtype=int)
        meshes[mesh.GetResourceID()] = (mesh.GetName(), vertices, triangles)
    objects = {}
    build_items = model.GetBuildItems()
    while build_items.MoveNext():
        item = build_items.GetCurrent()
        name, vertices, triangles = meshes[item.GetObjectResourceID()]
        if item.HasObjectTransform():
            # 3MF: four rows of three, applied to row vectors.
            matrix = np.array([list(row) for row in item.GetObjectTransform().Fields])
            vertices = vertices @ matrix[:3] + matrix[3]
        a, b, c = (vertices[triangles[:, i]] for i in range(3))
        assert name not in objects, f"two objects are named {name}"
        objects[name] = {"volume": float(np.einsum("ij,ij->i", a, np.cross(b, c)).sum() / 6.0),
                         "min": vertices.min(axis=0), "max": vertices.max(axis=0)}
    # Every object is on the plate exactly once.
    assert len(objects) == len(meshes)
    return objects
