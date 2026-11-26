"""The build op, driven through request.json / result.json exactly as the Rust core does."""

from __future__ import annotations

import base64
import io
import json
import re

import numpy as np
import pytest
from conftest import build_request, prepare_job, run_worker
from landlock_exec import landlock_abi
from PIL import Image

from hotend_engine.checks import CHECK_IDS

PER_PART_CHECKS = ("valid_solid", "manifold", "single_solid_per_part", "fits_build_volume",
                   "min_wall", "overhangs", "bed_contact", "small_features")


def test_box_builds_ok(builds):
    job = builds["box"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["protocol"] == 0
    assert result["op"] == "build"
    assert result["status"] == "ok"
    assert result["error"] is None
    assert result["warnings"] == []


def test_params_are_merged_with_defaults_and_specs_echoed(builds):
    result = builds["box"].result
    assert result["params"] == {"width": 90.0, "depth": 60.0, "height": 40.0, "wall": 2.0}
    spec = result["param_specs"]["width"]
    assert spec == {"default": 80.0, "min": 40.0, "max": 200.0, "unit": "mm", "label": "Width",
                    "provenance": "style"}


def test_part_summary_is_exact(builds):
    (part,) = builds["box"].result["parts"]
    assert part["name"] == "part"
    # 90 x 60 x 40 outer, 86 x 56 x 38 cavity
    assert part["volume"] == pytest.approx(90 * 60 * 40 - 86 * 56 * 38, abs=1e-6)
    assert part["bbox"] == {"min": [-45.0, -30.0, 0.0], "max": [45.0, 30.0, 40.0]}
    assert part["solids"] == 1
    assert part["faces"] == 11
    assert part["is_valid"] is True
    assert part["is_manifold"] is True
    # PLA at 1.24 g/cm3, solid
    assert part["mass_estimate_g"] == pytest.approx(part["volume"] / 1000 * 1.24, abs=1e-3)
    assert re.fullmatch(r"#[0-9a-f]{6}", part["color"])


def test_model_prints_are_captured(builds):
    result = builds["box"].result
    assert result["stdout"] == "box 90.0 x 60.0 x 40.0\n"
    assert result["stdout_truncated"] is False


def test_timings_are_reported(builds):
    timings = builds["box"].result["timings"]
    for key in ("import_s", "build_s", "export_s", "tessellate_s", "checks_s", "views_s",
                "total_s"):
        assert isinstance(timings[key], float) and timings[key] >= 0, key
    assert timings["total_s"] >= timings["build_s"]


def test_every_m1_check_is_reported(builds):
    result = builds["box"].result
    ids = [c["id"] for c in result["checks"]]
    assert sorted(set(ids)) == sorted(CHECK_IDS)
    for check in result["checks"]:
        assert check["status"] in ("pass", "warn", "fail", "skipped")
        assert check["severity"] in ("error", "warning")
        assert check["message"]
        assert isinstance(check["data"], dict)
    for check_id in PER_PART_CHECKS:
        assert builds["box"].check(check_id, "part")["status"] == "pass", check_id
    assert builds["box"].check("fit_pairs")["message"] == "Skipped: no declared fits."
    assert builds["box"].check("fit_pairs")["status"] == "skipped"
    assert builds["box"].check("parameter_ranges")["status"] == "skipped"
    assert builds["box"].check("parts_do_not_intersect")["status"] == "skipped"


def test_build_volume_message_uses_the_request_reach(builds):
    check = builds["box"].check("fits_build_volume")
    assert check["message"] == "Fits the main nozzle's reach with 166.0 mm to spare in X."
    reaches = {n["role"]: n["reach"] for n in check["data"]["nozzles"]}
    assert reaches == {"main": [256.0, 256.0, 260.0], "auxiliary": [235.5, 256.0, 256.0]}


def test_artifacts_are_relative_and_exist(builds):
    job = builds["box"]
    artifacts = job.result["artifacts"]
    assert artifacts == {
        "brep": "model.brep", "parts_index": "model.parts.json", "step": "model.step",
        "mesh": "mesh.json",
        "views": {v: f"views/{v}.png" for v in ("iso", "front", "top", "right")},
        "threemf": "export/model.3mf", "stl": {"part": "export/part.stl"},
    }
    expected = {"request.json", "model.py", "result.json", "model.brep", "model.parts.json",
                "model.step", "mesh.json", "export/model.3mf", "export/part.stl",
                *artifacts["views"].values()}
    # Nothing else is left behind: no scratch folder, no bytecode, no temp result.
    assert job.files() == expected


def test_step_has_part_names_and_a_fixed_timestamp(builds):
    step = (builds["lidded_box"].dir / "model.step").read_text(encoding="utf-8")
    assert step.startswith("ISO-10303-21;")
    assert "1970-01-01T00:00:00" in step
    assert "'box'" in step and "'lid'" in step


def test_parts_index_names_the_brep_children(builds):
    index = json.loads((builds["lidded_box"].dir / "model.parts.json").read_text())
    assert [p["name"] for p in index["parts"]] == ["box", "lid"]
    assert index["protocol"] == 0


def test_stl_and_3mf_are_written(builds):
    job = builds["box"]
    stl = (job.dir / "export/part.stl").read_bytes()
    triangles = int.from_bytes(stl[80:84], "little")
    assert triangles > 0 and len(stl) == 84 + 50 * triangles
    assert (job.dir / "export/model.3mf").read_bytes()[:2] == b"PK"


def _decode(buffer: dict) -> np.ndarray:
    assert buffer["codec"] == "b64"
    data = np.frombuffer(base64.b64decode(buffer["buffer"]), dtype=buffer["dtype"])
    assert list(data.shape) == buffer["shape"]
    return data


def test_mesh_json_is_the_instanced_viewer_form(builds):
    mesh = json.loads((builds["lidded_box"].dir / "mesh.json").read_text(encoding="utf-8"))
    assert set(mesh) == {"instances", "shapes", "hotend"}
    hotend = mesh["hotend"]
    assert hotend["protocol"] == 0
    assert hotend["format"] == "ocp-tessellate-instanced-b64"
    assert hotend["viewer"] == {"package": "three-cad-viewer", "version": "5.1.3"}
    assert hotend["parts"] == {"box": "/model/box", "lid": "/model/lid"}

    shapes = mesh["shapes"]
    assert shapes["version"] == 3 and shapes["id"] == "/model"
    assert set(shapes["bb"]) == {"xmin", "xmax", "ymin", "ymax", "zmin", "zmax"}
    leaves = {part["id"]: part for part in shapes["parts"]}
    assert set(leaves) == set(hotend["parts"].values())
    faces = {p["name"]: p["faces"] for p in builds["lidded_box"].result["parts"]}
    colors = {p["name"]: p["color"] for p in builds["lidded_box"].result["parts"]}
    for name, path in hotend["parts"].items():
        leaf = leaves[path]
        assert leaf["type"] == "shapes" and leaf["state"] == [1, 1]
        assert leaf["color"] == colors[name]
        instance = mesh["instances"][leaf["shape"]["ref"]]
        vertices, normals = _decode(instance["vertices"]), _decode(instance["normals"])
        triangles = _decode(instance["triangles"])
        per_face = _decode(instance["triangles_per_face"])
        assert vertices.dtype == np.float32 and triangles.dtype == np.int32
        assert len(vertices) % 3 == 0 and len(vertices) == len(normals)
        assert per_face.sum() * 3 == len(triangles)
        assert triangles.max() < len(vertices) // 3
        # per-face ids follow the part's B-rep faces
        assert len(per_face) == faces[name]
        assert _decode(instance["segments_per_edge"]).sum() * 6 == len(_decode(instance["edges"]))


def test_views_are_png_images_showing_the_part(builds):
    job = builds["box"]
    color = job.result["parts"][0]["color"]
    rgb = tuple(int(color[i:i + 2], 16) for i in (1, 3, 5))
    for name, path in job.result["artifacts"]["views"].items():
        data = (job.dir / path).read_bytes()
        assert data[:8] == b"\x89PNG\r\n\x1a\n", name
        image = Image.open(io.BytesIO(data)).convert("RGB")
        assert image.size == (768, 768)
        pixels = np.asarray(image).reshape(-1, 3).astype(int)
        # A good share of the picture is the part, in shades of its colour, on white.
        not_white = (pixels < 250).any(axis=1).mean()
        assert 0.08 < not_white < 0.9, (name, not_white)
        bluish = ((pixels[:, 2] > pixels[:, 0] + 20) if rgb[2] > rgb[0] else
                  (pixels[:, 0] > pixels[:, 2] + 20)).mean()
        assert bluish > 0.05, name


def test_same_request_gives_the_same_bytes(builds):
    first, second = builds["box"], builds["box_again"]
    assert second.result["status"] == "ok"
    for name in ("model.step", "model.brep", "model.parts.json", "mesh.json", "views/iso.png",
                 "views/front.png", "export/part.stl"):
        assert (first.dir / name).read_bytes() == (second.dir / name).read_bytes(), name
    drop = {"timings"}
    assert {k: v for k, v in first.result.items() if k not in drop} == {
        k: v for k, v in second.result.items() if k not in drop}


def test_checks_and_outputs_can_be_switched_off(builds):
    job = builds["box_no_checks"]
    result = job.result
    assert result["status"] == "ok"
    assert result["checks"] == []
    assert result["artifacts"] == {}
    assert job.files() == {"request.json", "model.py", "result.json"}
    assert result["parts"][0]["volume"] > 0


def test_bracket_builds_with_an_integer_count_parameter(builds):
    job = builds["bracket"]
    result = job.result
    assert result["status"] == "ok", result["error"]
    assert result["params"]["holes"] == 3 and isinstance(result["params"]["holes"], int)
    (part,) = result["parts"]
    assert part["name"] == "bracket"
    assert part["bbox"]["max"] == [60.0, 20.0, 60.0]
    # Holes through the upright arm have a ceiling: an honest overhang warning, not an error.
    overhang = job.check("overhangs", "bracket")
    assert overhang["status"] == "warn" and overhang["severity"] == "warning"
    assert overhang["data"]["overhang_area"] > 0
    assert job.check("min_wall", "bracket")["status"] == "pass"
    assert job.check("small_features", "bracket")["status"] == "pass"


def test_two_part_box(builds):
    job = builds["lidded_box"]
    result = job.result
    assert result["status"] == "ok", result["error"]
    assert [p["name"] for p in result["parts"]] == ["box", "lid"]
    assert result["parts"][0]["color"] != result["parts"][1]["color"]
    assert job.check("parts_do_not_intersect")["status"] == "pass"
    for part in ("box", "lid"):
        assert job.check("valid_solid", part)["status"] == "pass"
        assert job.check("fits_build_volume", part)["status"] == "pass"
    # verify_determinism built it twice: the prints appear twice, the timing is reported.
    assert result["stdout"].count("lid lip") == 2
    assert "determinism_s" in result["timings"]


# ---------------------------------------------------------------------------------------
# broken models
# ---------------------------------------------------------------------------------------


def _model_error(job) -> dict:
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "model_error"
    assert result["parts"] == [] and result["checks"] == [] and result["artifacts"] == {}
    error = result["error"]
    assert set(error) == {"type", "message", "line", "traceback"}
    # The traceback is about the model script only: no engine or library frames.
    assert "hotend_engine" not in error["traceback"]
    assert "site-packages" not in error["traceback"]
    return error


def test_syntax_error(builds):
    error = _model_error(builds["broken_syntax"])
    assert error["type"] == "SyntaxError"
    assert error["line"] == 9
    assert "never closed" in error["message"]
    assert 'File "model.py", line 9' in error["traceback"]


def test_exception_in_build(builds):
    job = builds["broken_exception"]
    error = _model_error(job)
    assert error["type"] == "ValueError"
    assert "fillet" in error["message"]
    # the innermost frame of the model script, with the calling frame above it
    assert error["line"] == 9
    assert 'File "model.py", line 15, in build' in error["traceback"]
    assert 'File "model.py", line 9, in rounded' in error["traceback"]
    assert "return fillet(block.edges(), radius=radius)" in error["traceback"]
    assert error["traceback"].rstrip().splitlines()[-1].startswith("ValueError: ")
    assert job.result["stdout"] == "about to fillet\n"


def test_non_solid_result(builds):
    error = _model_error(builds["broken_not_solid"])
    assert error["type"] == "NotASolid"
    assert "'plate'" in error["message"] and "Rectangle" in error["message"]
    assert error["line"] == 8


def test_bad_params(builds):
    job = builds["broken_params"]
    error = _model_error(job)
    assert error["type"] == "ParamsError"
    assert "'size'" in error["message"] and "'minimum'" in error["message"]
    assert error["line"] == 5
    # rejected before the script ran: build123d was never imported
    assert "import_s" not in job.result["timings"]


def test_out_of_range_parameter_is_not_clamped(builds):
    error = _model_error(builds["param_out_of_range"])
    assert error["type"] == "ParameterError"
    assert "'width'" in error["message"] and "500" in error["message"]
    assert "[40.0, 200.0]" in error["message"]


def test_unknown_parameter(builds):
    error = _model_error(builds["param_unknown"])
    assert error["type"] == "ParameterError"
    assert "'lenght'" in error["message"] and "width" in error["message"]


def test_non_deterministic_build_is_rejected(builds):
    error = _model_error(builds["broken_nondeterministic"])
    assert error["type"] == "NonDeterministicBuild"
    assert "volume" in error["message"]


def test_too_big_is_a_check_error_with_artifacts(builds):
    job = builds["broken_too_big"]
    result = job.result
    assert job.returncode == 0
    assert result["status"] == "check_error"
    assert result["error"] is None
    check = job.check("fits_build_volume", "beam")
    assert check["status"] == "fail" and check["severity"] == "error"
    assert "300.0 x 20.0 x 20.0 mm" in check["message"]
    assert "main nozzle by 44.0 mm in X" in check["message"]
    assert "auxiliary nozzle by 64.5 mm in X" in check["message"]
    assert all(not n["fits"] for n in check["data"]["nozzles"])
    # the model is still there to look at
    assert (job.dir / result["artifacts"]["mesh"]).is_file()
    assert (job.dir / result["artifacts"]["brep"]).is_file()


def test_thin_wall_is_a_check_error(builds):
    job = builds["broken_thin_wall"]
    assert job.result["status"] == "check_error"
    check = job.check("min_wall", "cup")
    assert check["status"] == "fail" and check["severity"] == "error"
    assert check["data"]["thinnest"] == pytest.approx(0.3, abs=0.01)
    assert check["data"]["min_wall"] == 0.8
    assert check["data"]["thin_area"] > 100
    assert "0.30 mm" in check["message"] and "0.80 mm" in check["message"]
    assert job.check("valid_solid", "cup")["status"] == "pass"


# ---------------------------------------------------------------------------------------
# bad requests and confinement
# ---------------------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("request_text", "expected"),
    [
        ("{not json", "not valid JSON"),
        ('{"protocol": 0, "op": "prepare", "clearance": 6, "clearance": 0}', "twice"),
        ('{"protocol": 0, "op": "prepare", "keepouts": ' + "[" * 100_000 + "]" * 100_000 + "}",
         "nested"),
        (json.dumps({"protocol": 0, "op": "frobnicate"}), "op"),
        (json.dumps({"protocol": 1, "op": "build"}), "protocol"),
        (json.dumps(build_request(model_path="../model.py")), "outside the job folder"),
        (json.dumps(build_request(model_path="/etc/passwd")), "relative to the job folder"),
        (json.dumps(build_request(paramz={})), "paramz"),
        (json.dumps(build_request(outputs={"views": ["sideways"]})), "views"),
        (json.dumps(build_request(checks={"only": ["no_such_check"]})), "no_such_check"),
    ],
)
def test_bad_request_exits_2(tmp_path, request_text, expected):
    job = run_worker(prepare_job(tmp_path / "job", request_text, "box.py"))
    assert job.returncode == 2
    assert expected in job.stderr
    result = job.result
    assert result["status"] == "internal_error"
    assert result["error"]["type"] == "bad_request"
    assert expected in result["error"]["message"]


def test_missing_request_exits_2(tmp_path):
    job = run_worker(prepare_job(tmp_path / "job", None))
    assert job.returncode == 2
    assert "request.json" in job.stderr


def test_missing_job_folder_exits_2(tmp_path):
    job = run_worker(tmp_path / "does-not-exist")
    assert job.returncode == 2
    assert not (tmp_path / "does-not-exist").exists()


def test_missing_model_is_a_model_error(tmp_path):
    job = run_worker(prepare_job(tmp_path / "job", build_request()))
    assert job.returncode == 0
    assert job.result["status"] == "model_error"
    assert job.result["error"]["type"] == "ModelNotFound"


@pytest.mark.skipif(landlock_abi() == 0, reason="needs Linux Landlock")
def test_build_succeeds_when_only_the_job_folder_is_writable(builds):
    """The whole build, with every write outside the job folder denied by the kernel."""
    job = builds["confined"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "ok", result["error"]
    assert result["warnings"] == []
    reference = builds["box"].result["artifacts"]
    assert result["artifacts"] == reference
    for path in ("model.step", "model.brep", "mesh.json", "views/iso.png", "export/model.3mf",
                 "export/part.stl"):
        assert (job.dir / path).stat().st_size > 0, path
    assert ".engine-scratch" not in {p.name for p in job.dir.iterdir()}


@pytest.mark.skipif(landlock_abi() == 0, reason="needs Linux Landlock")
def test_the_confinement_helper_really_denies_writes(tmp_path):
    """Guards the test above: the helper must block a write outside the allowed folder."""
    import subprocess
    import sys

    from conftest import LANDLOCK_EXEC

    allowed = tmp_path / "allowed"
    allowed.mkdir()
    script = (
        "import sys\n"
        "open(sys.argv[1] + '/inside.txt', 'w').close()\n"
        "try:\n"
        "    open(sys.argv[2], 'w').close()\n"
        "except PermissionError:\n"
        "    sys.exit(0)\n"
        "sys.exit(1)\n"
    )
    done = subprocess.run(
        [sys.executable, "-I", str(LANDLOCK_EXEC), str(allowed), "--", sys.executable, "-I",
         "-c", script, str(allowed), str(tmp_path / "outside.txt")],
        capture_output=True, text=True, timeout=60, check=False,
    )
    assert done.returncode == 0, done.stderr
    assert (allowed / "inside.txt").exists()
    assert not (tmp_path / "outside.txt").exists()
