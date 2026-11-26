"""What the two acceptance-scenario tests share: running the engine worker the way the
product does, and measuring what it wrote without asking the engine.

The measurements here are deliberately not the engine's own: rays through the exact solids
of `model.brep` (the kernel's intersector, nothing of hotend_engine or hotend_fit), the
smallest circle around a set of points, and the triangles of the exported 3MF files read
back with lib3mf. A scenario test states what a user relies on (the thing fits its room
with the clearance the result declares, the openings are where the device's data puts
them, the plate can be printed) and proves it with these."""

from __future__ import annotations

import base64
import importlib.util
import json
import math
import random
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import lib3mf
import numpy as np

TESTS = Path(__file__).resolve().parents[1]
MODELS = TESTS / "models"
sys.path.insert(0, str(TESTS))               # reference_stand_in
sys.path.insert(0, str(TESTS / "engine"))    # landlock_exec, which the engine helpers import

from reference_stand_in import stand_in_mesh, write_plain_3mf, write_stl
from result_contract import check_job_result

__all__ = ["stand_in_mesh", "write_plain_3mf", "write_stl"]


def _engine_helpers():
    """tests/engine/conftest.py as a plain module: its job-folder helpers are reused here,
    whichever folder pytest was started on."""
    name = "hotend_engine_test_helpers"
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, TESTS / "engine" / "conftest.py")
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]


_helpers = _engine_helpers()
WORKER, Job, ok, prepare_job = _helpers.WORKER, _helpers.Job, _helpers.ok, _helpers.prepare_job
X2D_CONTEXT = _helpers.X2D_CONTEXT
WORKER_TIMEOUT_S = 480


def run_all(folders: dict[str, Path], at_a_time: int = 4) -> dict:
    """The worker on each job folder. A scenario build takes 15 to 60 seconds by itself, so
    it gets more time than the engine tests' helper allows when the machine is busy."""
    def run(job_dir: Path):
        done = subprocess.run([*WORKER, str(job_dir)], capture_output=True, text=True,
                              timeout=WORKER_TIMEOUT_S, cwd=job_dir.parent, check=False)
        check_job_result(job_dir)
        return Job(job_dir, done.returncode, done.stderr)

    with ThreadPoolExecutor(max_workers=at_a_time) as pool:
        return dict(zip(folders, pool.map(run, folders.values()), strict=True))


def checks(result: dict, check_id: str, **data) -> list[dict]:
    return [c for c in result["checks"] if c["id"] == check_id
            and all(c["data"].get(key) == value for key, value in data.items())]


def part(result: dict, name: str) -> dict:
    (found,) = [p for p in result["parts"] if p["name"] == name]
    return found


# ---- the exact solids ------------------------------------------------------------------


def solids(job_dir: Path) -> dict:
    """The parts of a build as exact solids, by name (the order of model.parts.json)."""
    from build123d import import_brep

    names = [p["name"] for p in json.loads((job_dir / "model.parts.json").read_text())["parts"]]
    found = import_brep(str(job_dir / "model.brep")).solids()
    assert len(found) == len(names), (len(found), names)
    return dict(zip(names, found, strict=True))


class Rays:
    """Rays through one or more exact solids."""

    def __init__(self, *shapes) -> None:
        from OCP.IntCurvesFace import IntCurvesFace_ShapeIntersector

        self._casters = []
        for shape in shapes:
            caster = IntCurvesFace_ShapeIntersector()
            caster.Load(shape.wrapped, 1e-7)
            self._casters.append(caster)

    def hits(self, origin, direction, reach: float = 1000.0) -> list[float]:
        """Distances from `origin` along `direction` at which the ray crosses a surface,
        nearest first."""
        from OCP.gp import gp_Dir, gp_Lin, gp_Pnt

        line = gp_Lin(gp_Pnt(*map(float, origin)), gp_Dir(*map(float, direction)))
        found = []
        for caster in self._casters:
            caster.Perform(line, 0.0, reach)
            found += [caster.WParameter(i + 1) for i in range(caster.NbPnt())]
        return sorted(found)

    def first(self, origin, direction, reach: float = 1000.0) -> float:
        """The first crossing, or infinity when the ray meets nothing."""
        found = self.hits(origin, direction, reach)
        return found[0] if found else math.inf

    def filled(self, origin, direction, reach: float = 1000.0) -> bool:
        """Whether `origin` is in material: a ray from inside a closed solid leaves it an
        odd number of times."""
        return len(self.hits(origin, direction, reach)) % 2 == 1


# ---- points ----------------------------------------------------------------------------


def hull_2d(points: np.ndarray) -> np.ndarray:
    """The corners of the convex outline of 2D points, counter-clockwise."""
    ordered = sorted({(float(x), float(y)) for x, y in points})

    def half(sequence):
        chain = []
        for p in sequence:
            while len(chain) >= 2 and ((chain[-1][0] - chain[-2][0]) * (p[1] - chain[-2][1])
                                       - (chain[-1][1] - chain[-2][1]) * (p[0] - chain[-2][0])) <= 0:
                chain.pop()
            chain.append(p)
        return chain[:-1]

    return np.array(half(ordered) + half(reversed(ordered)))


def smallest_circle(points: np.ndarray) -> tuple[np.ndarray, float]:
    """Centre and radius of the smallest circle that holds every 2D point (exact, by the
    move-to-front construction on the convex outline)."""
    corners = [tuple(p) for p in hull_2d(points)]
    random.Random(7).shuffle(corners)

    def through(a, b, c=None):
        if c is None:
            centre = ((a[0] + b[0]) / 2, (a[1] + b[1]) / 2)
        else:
            d = 2 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]))
            if abs(d) < 1e-12:
                return None
            sq = [p[0] ** 2 + p[1] ** 2 for p in (a, b, c)]
            centre = ((sq[0] * (b[1] - c[1]) + sq[1] * (c[1] - a[1]) + sq[2] * (a[1] - b[1])) / d,
                      (sq[0] * (c[0] - b[0]) + sq[1] * (a[0] - c[0]) + sq[2] * (b[0] - a[0])) / d)
        return centre, math.dist(centre, a)

    def holds(circle, p):
        return circle is not None and math.dist(circle[0], p) <= circle[1] + 1e-9

    circle = None
    for i, p in enumerate(corners):
        if holds(circle, p):
            continue
        circle = (p, 0.0)
        for j, q in enumerate(corners[:i]):
            if holds(circle, q):
                continue
            circle = through(p, q)
            for r in corners[:j]:
                if not holds(circle, r):
                    circle = through(p, q, r)
    return np.array(circle[0]), float(circle[1])


def ghost_vertices(job_dir: Path, name: str) -> tuple[np.ndarray, str]:
    """Where the build put a reference: the vertices of its ghost in mesh.json (world
    coordinates) and how exact that mesh is (`exact` or `simplified to within x mm`)."""
    mesh = json.loads((job_dir / "mesh.json").read_text(encoding="utf-8"))
    (entry,) = [g for g in mesh["hotend"]["references"] if g["name"] == name]
    (leaf,) = [p for p in mesh["shapes"]["parts"] if p["id"] == entry["path"]]
    assert leaf["loc"] == [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]]
    buffer = mesh["instances"][leaf["shape"]["ref"]]["vertices"]
    assert buffer["dtype"] == "float32" and buffer["codec"] == "b64"
    flat = np.frombuffer(base64.b64decode(buffer["buffer"]), dtype="<f4").astype(float)
    return flat.reshape(-1, 3), entry["mesh"]


def top_points(points: np.ndarray, cell: float = 1.5) -> np.ndarray:
    """The highest point of every `cell` x `cell` column: what a cover above must clear."""
    keys = np.floor(points[:, :2] / cell).astype(np.int64)
    order = np.lexsort((points[:, 2], keys[:, 1], keys[:, 0]))
    keys, points = keys[order], points[order]
    last = np.ones(len(points), dtype=bool)
    last[:-1] = (keys[1:] != keys[:-1]).any(axis=1)
    return points[last]


# ---- exported meshes -------------------------------------------------------------------


def read_meshes(path: Path) -> dict[str, tuple[np.ndarray, np.ndarray]]:
    """Every object of a 3MF file as it lies on the plate: name -> (vertices, triangles),
    millimetres, build-item transform applied. Each mesh must be closed and oriented."""
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
    items = model.GetBuildItems()
    while items.MoveNext():
        item = items.GetCurrent()
        name, vertices, triangles = meshes[item.GetObjectResourceID()]
        if item.HasObjectTransform():
            matrix = np.array([list(row) for row in item.GetObjectTransform().Fields])
            vertices = vertices @ matrix[:3] + matrix[3]
        assert name not in objects, f"two objects are named {name}"
        objects[name] = (vertices, triangles)
    assert len(objects) == len(meshes)        # every object is on the plate exactly once
    return objects


def mesh_measures(vertices: np.ndarray, triangles: np.ndarray, limit_deg: float = 45.0,
                  slack_deg: float = 1.5) -> dict:
    """What a slicer would see of a mesh lying on the bed (z = 0): the volume it encloses,
    the area lying on the bed, and the area that hangs over more steeply than `limit_deg`
    from upright (facets within `slack_deg` of the limit are a surface drawn at the limit)
    and is not on the bed."""
    a, b, c = (vertices[triangles[:, i]] for i in range(3))
    cross = np.cross(b - a, c - a)
    area = np.linalg.norm(cross, axis=1) / 2
    keep = area > 1e-12
    down = np.zeros(len(area))
    down[keep] = -cross[keep, 2] / (2 * area[keep])       # 1 for a facet that faces the bed
    on_bed = (np.abs(np.stack([a[:, 2], b[:, 2], c[:, 2]])) < 1e-6).all(axis=0) & (down > 0.999)
    hangs = (down > math.cos(math.radians(limit_deg - slack_deg))) & ~on_bed
    return {"volume": float(np.einsum("ij,ij->i", a, np.cross(b, c)).sum() / 6.0),
            "min": vertices.min(axis=0), "max": vertices.max(axis=0),
            "contact": float(area[on_bed].sum()), "overhang": float(area[hangs].sum())}


def boxes_apart(objects: dict[str, dict]) -> float:
    """The smallest gap between the bounding boxes of the objects of a plate, seen from
    above (negative when two overlap)."""
    names, least = list(objects), math.inf
    for i, first in enumerate(names):
        for second in names[i + 1:]:
            gaps = [max(objects[first]["min"][k] - objects[second]["max"][k],
                        objects[second]["min"][k] - objects[first]["max"][k]) for k in (0, 1)]
            least = min(least, max(gaps))
    return least


def step_is_exact_only(path: Path, names: set[str]) -> None:
    """A STEP file that holds the named products as exact solids and no mesh at all."""
    text = path.read_text(errors="replace")
    products = set()
    for line in text.splitlines():
        if "=PRODUCT(" in line.replace(" ", ""):
            products.add(line.split("'")[1])
    assert names <= products and products - names <= {"model"}, products
    upper = text.upper()
    for entity in ("TESSELLATED", "TRIANGULATED", "COMPLEX_TRIANGULATED", "POLY_LOOP"):
        assert entity not in upper, entity
    assert upper.count("MANIFOLD_SOLID_BREP(") == len(names)
