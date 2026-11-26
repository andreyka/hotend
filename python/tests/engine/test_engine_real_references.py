"""Reference import on real files. They are other people's designs and neither they nor
what is expected of them is in the repository: these tests find both through HOTEND_REFS_DIR
(local_references) and skip cleanly when it is unset. Only numbers leave the child process,
never a mesh or an outline.

Each file is handled in a process of its own with the engine sandbox's limits (120 s,
4096 MB), reading the folder in place: load, measure, fit an envelope, cut a holder
around it and run the insertion check, once open and once deliberately trapped.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from local_references import FOLDER, KEYS, WHY_NOT, entry_of

WALL_SECONDS = 120
MEMORY_MB = 4096

pytestmark = pytest.mark.skipif(bool(WHY_NOT), reason=WHY_NOT)

CHILD = r'''
import json, resource, sys, time
limit = int(sys.argv[3]) * 1024 * 1024
resource.setrlimit(resource.RLIMIT_AS, (limit, limit))
started = time.perf_counter()
from pathlib import Path
from hotend_engine.context import load_reference
from hotend_engine.errors import BadRequest
from hotend_engine.ghost import display_mesh
from hotend_engine.request import ReferenceSpec
from hotend_fit import fit
from hotend_fit.reference import verify_pocket

folder, name, plate = Path(sys.argv[1]), sys.argv[2], json.loads(sys.argv[4])
out = {}
try:
    load_reference(ReferenceSpec(name="unselected", path=name), folder)
    out["unselected"] = None
except BadRequest as exc:
    out["unselected"] = str(exc)
began = time.perf_counter()
reference, loaded = load_reference(
    ReferenceSpec(name="thing", path=name, kind="soft", plate=plate), folder)
out["load_s"] = time.perf_counter() - began
out["objects"] = [found.listing() for found in loaded.objects]
out["notes"] = list(loaded.notes) + list(reference.notes)
out["report"] = reference.report()
out["section_points"] = sum(len(p) for p in reference.section_points(reference.size[2] / 2))
out["ghost"] = display_mesh(reference)[2]
out["ghost_triangles"] = len(display_mesh(reference)[1])

from build123d import Align, Cylinder, Pos
bottom = (Align.CENTER, Align.CENTER, Align.MIN)
out["pockets"] = {}
for shape in ("cylinder", "silhouette", "hull"):
    resolver = fit.resolver_for({"material": {"id": "petg-hf", "family": "PETG"}})
    began = time.perf_counter()
    pocket = reference.pocket("cradle", ctx=resolver, shape=shape)
    envelope = pocket.envelope
    cx, cy, _ = pocket.center
    floor, wall = 2.0, 2.4
    open_height = floor + 0.5 * pocket.depth
    holder = pocket.cut(Pos(cx, cy, 0) * Cylinder(pocket.radius + wall, open_height, align=bottom),
                        at=Pos(0, 0, floor))
    report = verify_pocket(pocket, [("holder", holder)])
    entry = {"gap": pocket.gap, "target": pocket.clearance.target, "radius": pocket.radius,
             "size": list(pocket.size), "tolerance": pocket.envelope_tolerance,
             "envelope_valid": bool(envelope.is_valid), "envelope_volume": envelope.volume,
             "status": report.status, "message": report.message,
             "clearance_proven": report.data.get("clearance_proven"),
             "path_clear": report.data.get("path_clear")}
    if shape == "cylinder":
        # The same holder, taller, with a rim that closes in above the thing.
        other = fit.resolver_for({"material": {"id": "petg-hf", "family": "PETG"}})
        trapped = reference.pocket("cradle", ctx=other, shape=shape)
        tall = floor + trapped.size[2] + 0.5
        body = trapped.cut(Pos(cx, cy, 0) * Cylinder(trapped.radius + wall, tall, align=bottom),
                           at=Pos(0, 0, floor))
        rim = Pos(cx, cy, tall) * (Cylinder(trapped.radius + wall, 2.0, align=bottom)
                                   - Cylinder(trapped.radius - 8.0, 2.0, align=bottom))
        again = verify_pocket(trapped, [("holder", body + rim)])
        entry["trapped_status"], entry["trapped_message"] = again.status, again.message
        gauge = pocket.gauge()
        entry["gauge_valid"] = bool(gauge.is_valid)
        entry["gauge_size"] = list(tuple(gauge.bounding_box().size))
    entry["seconds"] = time.perf_counter() - began
    out["pockets"][shape] = entry
out["total_s"] = time.perf_counter() - started
out["max_rss_mb"] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024
print(json.dumps(out))
'''


def measured(entry: dict, tmp_path: Path) -> dict:
    done = subprocess.run(
        [sys.executable, "-I", "-c", CHILD, str(FOLDER), entry["file"], str(MEMORY_MB),
         json.dumps(entry.get("plate"))],
        capture_output=True, text=True, timeout=WALL_SECONDS, cwd=tmp_path, check=False)
    assert done.returncode == 0, done.stderr[-2000:]
    return json.loads(done.stdout)


def check_fit(out: dict) -> None:
    assert out["total_s"] < WALL_SECONDS and out["max_rss_mb"] < MEMORY_MB
    assert out["report"]["provenance"] == "measured" and out["report"]["closed"] is True
    for shape, pocket in out["pockets"].items():
        assert pocket["envelope_valid"], shape
        assert pocket["envelope_volume"] > out["report"]["hull_volume"] * 0.999 or shape == "silhouette"
        assert pocket["status"] == "pass", (shape, pocket["message"])
        assert pocket["clearance_proven"] and pocket["path_clear"], shape
        assert pocket["target"] > 1.5 and pocket["tolerance"] <= 0.5
    cylinder = out["pockets"]["cylinder"]
    assert cylinder["trapped_status"] == "fail" and "trapped" in cylinder["trapped_message"]
    assert cylinder["gauge_valid"] and cylinder["gauge_size"][2] == pytest.approx(5.0)


@pytest.mark.parametrize("key", KEYS)
def test_a_real_file_is_measured_as_printed_and_fitted(key, tmp_path):
    """The object as printed, with its placement scale and without what is not geometry."""
    entry = entry_of(key)
    out = measured(entry, tmp_path)
    report, expect = out["report"], entry["engine"]
    assert report["size"] == pytest.approx(expect["size_mm"], abs=0.05)
    objects = expect["objects_in_file"]
    if objects == 1:
        assert out["unselected"] is None  # one object: no selection needed
    else:
        assert report["source"]["plate"] == entry["plate"]
        assert report["source"]["objects_in_file"] == objects
        # Several objects: never merged silently, the request must name one.
        assert f"holds {objects} objects and they are never merged" in out["unselected"]
    if parts := expect.get("parts"):
        chosen = out["objects"][report["source"]["object_index"] - 1]
        geometry = [part for part in chosen["parts"] if part["geometry"]]
        ignored = [part for part in chosen["parts"] if not part["geometry"]]
        assert len(geometry) == parts["geometry"]
        assert [part["role"] for part in ignored] == parts["ignored_roles"]
        # The modifier is larger than the figure: had it counted, the size would be its own.
        assert ignored[0]["size"][0] > parts["ignored_wider_than_mm"] > report["size"][0]
        assert any("only change print settings" in note for note in out["notes"])
    if elsewhere := expect.get("elsewhere"):
        others = [found for found in out["objects"] if found["plate"] == elsewhere["plate"]]
        assert len(others) == elsewhere["count"]
        assert all(found["size"] == elsewhere["size_mm"] for found in others)
    if mesh := expect.get("mesh"):
        assert report["triangles"] > mesh["more_triangles_than"]
        assert report["volume"] == pytest.approx(mesh["volume_mm3"], rel=1e-3)
        assert out["ghost"].startswith("simplified")
        assert out["ghost_triangles"] <= mesh["most_ghost_triangles"]
        assert out["section_points"] > mesh["more_section_points_than"]
    if "base_z_mm" in expect:
        assert report["bbox"]["min"][2] == expect["base_z_mm"]
    check_fit(out)
    print(f"\n{key} measured:", json.dumps({
        "size": report["size"], "volume": report["volume"], "hull_volume": report["hull_volume"],
        "silhouette": report["silhouette"], "load_s": round(out["load_s"], 2),
        "total_s": round(out["total_s"], 2), "max_rss_mb": round(out["max_rss_mb"]),
        "notes": out["notes"],
        "pockets": {s: {"gap": round(p["gap"], 3), "size": [round(v, 2) for v in p["size"]],
                        "seconds": round(p["seconds"], 2)} for s, p in out["pockets"].items()}}))
