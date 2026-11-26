"""The measure, export, views and docs ops on a built model, through the worker process."""

from __future__ import annotations

import io

import numpy as np
import pytest
from PIL import Image


def _ok(job) -> dict:
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "ok", result["error"]
    assert result["error"] is None
    assert result["timings"]["total_s"] > 0
    return result


def test_measure(ops, builds):
    result = _ok(ops["measure"])
    assert result["op"] == "measure"
    answers = result["results"]
    assert len(answers) == 11
    parts = {p["name"]: p for p in builds["lidded_box"].result["parts"]}

    lid_box, whole_box, box_volume, whole_volume = answers[:4]
    assert lid_box["ok"] and lid_box["kind"] == "bbox"
    assert lid_box["min"] == parts["lid"]["bbox"]["min"]
    assert lid_box["max"] == parts["lid"]["bbox"]["max"]
    assert lid_box["size"] == [90.0, 60.0, 4.0]
    assert whole_box["min"] == [-45.0, -30.0, 0.0] and whole_box["max"] == [45.0, 30.0, 37.0]
    assert box_volume["volume"] == pytest.approx(parts["box"]["volume"], abs=1e-5)
    assert box_volume["area"] == pytest.approx(parts["box"]["area"], abs=1e-5)
    assert whole_volume["volume"] == pytest.approx(
        parts["box"]["volume"] + parts["lid"]["volume"], abs=1e-5)
    assert len(box_volume["center_of_mass"]) == 3

    touching, from_point = answers[4:6]
    # the lid rests on the rim of the box
    assert touching["distance"] == pytest.approx(0.0, abs=1e-6)
    # a point 63 mm above the top of the lid (z = 37)
    assert from_point["distance"] == pytest.approx(63.0, abs=1e-6)
    assert from_point["point_a"] == [0.0, 0.0, 100.0]
    assert from_point["point_b"][2] == pytest.approx(37.0)

    xz, xy = answers[6:8]
    assert xz["plane"] == "XZ" and {row["part"] for row in xz["parts"]} == {"box", "lid"}
    assert xz["area"] == pytest.approx(sum(row["area"] for row in xz["parts"]))
    # 10 mm up, the box is a ring: 90 x 60 outside, 86 x 56 inside, rounded corners
    (ring,) = xy["parts"]
    assert ring["part"] == "box" and ring["regions"] == 1
    assert 0 < ring["area"] < 90 * 60 - 86 * 56
    assert ring["bbox"]["min"][2] == pytest.approx(10.0)

    unknown_part, unknown_kind, bad_face = answers[8:]
    assert not unknown_part["ok"] and "'nope'" in unknown_part["error"]
    assert not unknown_kind["ok"] and "mystery" in unknown_kind["error"]
    assert not bad_face["ok"] and "index" in bad_face["error"]


def test_export_all_formats(ops):
    job = ops["export_all"]
    result = _ok(job)
    assert result["artifacts"] == {
        "step": "export/model.step",
        "threemf": "export/model.3mf",
        "stl": {"box": "export/box.stl", "lid": "export/lid.stl"},
    }
    step = (job.dir / "export/model.step").read_text(encoding="utf-8")
    assert step.startswith("ISO-10303-21;") and "'box'" in step and "'lid'" in step
    assert (job.dir / "export/model.3mf").read_bytes()[:2] == b"PK"
    for name in ("box", "lid"):
        stl = (job.dir / f"export/{name}.stl").read_bytes()
        assert len(stl) == 84 + 50 * int.from_bytes(stl[80:84], "little")


def test_3mf_holds_one_named_object_per_part(ops):
    import zipfile

    with zipfile.ZipFile(ops["export_all"].dir / "export/model.3mf") as archive:
        model = archive.read("3D/3dmodel.model").decode("utf-8")
    assert 'name="box"' in model and 'name="lid"' in model
    assert 'unit="millimeter"' in model


def test_export_selected_part_only(ops):
    job = ops["export_lid"]
    result = _ok(job)
    assert result["artifacts"] == {"stl": {"lid": "export/lid.stl"}}
    assert {p.name for p in (job.dir / "export").iterdir()} == {"lid.stl"}


def test_export_unknown_part_is_a_bad_request(ops):
    job = ops["export_unknown_part"]
    assert job.returncode == 2
    assert job.result["error"]["type"] == "bad_request"
    assert "'handle'" in job.result["error"]["message"]


def test_ops_need_a_built_model(ops):
    job = ops["measure_without_model"]
    assert job.returncode == 2
    assert "model.brep" in job.result["error"]["message"]


def _image(job, name: str) -> np.ndarray:
    data = (job.dir / job.result["artifacts"]["views"][name]).read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    return np.asarray(Image.open(io.BytesIO(data)).convert("RGB")).astype(int)


def test_views_op(ops):
    job = ops["views"]
    result = _ok(job)
    assert result["artifacts"] == {"views": {"iso": "views/iso.png", "top": "views/top.png"}}
    for name in ("iso", "top"):
        pixels = _image(job, name)
        assert pixels.shape == (240, 320, 3)
        assert (pixels < 250).any(axis=2).mean() > 0.05


def test_section_view_opens_the_model(ops, builds):
    whole, cut = _image(ops["views"], "iso"), _image(ops["views_section"], "iso")
    assert cut.shape == whole.shape
    # Cutting away the front half changes the picture and shows less of the model.
    assert (whole != cut).any()
    def orange(px):  # the lid's colour
        return ((px[:, :, 0] > 200) & (px[:, :, 2] < 90)).sum()

    assert builds["lidded_box"].result["parts"][1]["color"] == "#f58518"
    assert 0 < orange(cut) < orange(whole)


def test_docs_op(ops):
    result = _ok(ops["docs"])
    assert result["versions"]["build123d"] == "0.13.0"
    assert result["versions"]["bd_warehouse"] == "0.4.0"
    docs = result["docs"]
    assert len(docs) == 3
    best = docs[0]
    assert best["name"] == "build123d.fillet" and best["kind"] == "function"
    assert "radius" in best["signature"]
    assert "Fillet the given sequence of edges" in best["doc"]
    assert docs == sorted(docs, key=lambda d: -d["score"])
