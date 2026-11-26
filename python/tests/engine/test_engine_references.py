"""Reference objects and bought components through the worker, the way the core drives
it: the request lists a reference file, the model fits a part to it, and the result says
what was measured and what was proven. The reference files are made by the tests."""

from __future__ import annotations

import copy
import json
import re
import sys
from pathlib import Path

import numpy as np
import pytest
from conftest import (
    X2D_CONTEXT,
    build_request,
    ok,
    prepare_job,
    read_3mf,
    run_all,
)

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reference_stand_in import stand_in_mesh, write_project_3mf, write_stl

from hotend_engine import checks
from hotend_engine.tessellate import _leaves
from hotend_fit.components import list_components

TPU = {"id": "tpu-95a", "family": "TPU"}
FIGURE = [{"name": "figure", "path": "figure.stl", "kind": "soft"}]
OUTPUTS = {"step": True, "brep": True, "mesh": True, "views": ["iso", "top"], "stl": True,
           "threemf": True, "view_size": [320, 240]}
QUESTION_KEYS = {"requirement", "text", "guessed_params"}
CHECKS = {"enabled": True, "min_wall": 0.8, "verify_determinism": False}


def context(**extra) -> dict:
    return {**copy.deepcopy(X2D_CONTEXT), **extra}


@pytest.fixture(scope="module")
def jobs(tmp_path_factory):
    root = tmp_path_factory.mktemp("references")
    figure = context(references=FIGURE)
    builds = {
        "basket": ("ref_basket.py", build_request(context=figure, outputs=OUTPUTS)),
        "trapped": ("ref_basket.py", build_request(context=figure, params={"rim": 6.0})),
        "case_tpu": ("phone_case.py", build_request(
            context=context(material=TPU),
            outputs={"step": True, "brep": True, "mesh": True, "views": [], "threemf": True})),
        "case_pla": ("phone_case.py", build_request(
            outputs={"step": False, "brep": False, "mesh": False, "views": []})),
        "escape": ("ref_basket.py", build_request(context=context(
            references=[{"name": "figure", "path": "../figure.stl"}]))),
        "ambiguous": ("ref_basket.py", build_request(context=context(
            references=[{"name": "figure", "path": "project.3mf"}]))),
        "plain_box": ("box.py", build_request()),
        "stated": ("ref_basket_contract.py", build_request(
            context=figure, checks={**CHECKS, "contract": True},
            outputs={"step": False, "brep": False, "mesh": False, "views": []})),
        "stated_trapped": ("ref_basket_contract.py", build_request(
            context=figure, params={"rim": 6.0}, checks={**CHECKS, "contract": True},
            outputs={"step": False, "brep": False, "mesh": False, "views": []})),
    }
    others = {
        "listing": {"protocol": 0, "op": "references", "components": ["sample_phone"],
                    "context": context(material=TPU, references=[
                        {"name": "figure", "path": "project.3mf"},
                        {"name": "eye", "path": "project.3mf", "object": 3},
                        {"name": "dome", "path": "figure.stl", "units": "cm"}])},
        "gauges": {"protocol": 0, "op": "gauge", "views": [], "context": context(
            material=TPU, references=FIGURE), "cards": [
                {"card": "pocket", "name": "figure_ring",
                 "args": {"reference": "figure", "fit": "cradle", "shape": "cylinder"}},
                {"card": "pocket", "args": {"reference": "sample_phone", "fit": "stretch"}}]},
        "gauge_unknown_reference": {"protocol": 0, "op": "gauge", "context": context(), "cards": [
            {"card": "pocket", "args": {"reference": "nothing", "fit": "cradle"}}]},
    }
    specs = {}
    for name, (model, request) in builds.items():
        specs[name] = (prepare_job(root / name, request, model), False)
    for name, request in others.items():
        specs[name] = (prepare_job(root / name, request), False)
    write_stl(root / "figure.stl", *stand_in_mesh())  # the file "escape" points at
    for name in specs:
        write_stl(root / name / "figure.stl", *stand_in_mesh())
        write_project_3mf(root / name / "project.3mf")
    done = run_all(specs)
    # Print preparation of the built basket in a flexible material, on each nozzle.
    second = {}
    for name, nozzle in (("prepare_main", 1), ("prepare_auxiliary", 0)):
        request = {"protocol": 0, "op": "prepare", "views": [], "threemf": True,
                   "context": context(material=TPU), "nozzles": {"basket": nozzle}}
        second[name] = (prepare_job(root / name, request, copy_from=done["basket"].dir), False)
    second["gauge_of_model"] = (prepare_job(
        root / "gauge_of_model", {"protocol": 0, "op": "gauge", "views": [], "context": figure},
        copy_from=done["basket"].dir), False)
    write_stl(root / "gauge_of_model" / "figure.stl", *stand_in_mesh())
    done.update(run_all(second))
    return done


def reference_fit(job) -> list[dict]:
    return job.checks("reference_fit")


def test_a_build_without_references_answers_exactly_as_before(jobs):
    result = ok(jobs["plain_box"])
    assert "references" not in result and "components" not in result and "questions" not in result
    assert reference_fit(jobs["plain_box"]) == []
    assert [c["id"] for c in result["checks"] if c["part"] is None] == [
        "parts_do_not_intersect", "fit_pairs", "parameter_ranges"]
    # The check is known by name and conditional: it is not one of the always-reported ids.
    assert "reference_fit" in checks.ALL_CHECK_IDS and "reference_fit" not in checks.CHECK_IDS
    mesh = json.loads((jobs["plain_box"].dir / "mesh.json").read_text())
    assert "references" not in mesh["hotend"]


def test_the_result_reports_the_reference_with_measures_and_provenance(jobs):
    result = ok(jobs["basket"])
    vertices, triangles = stand_in_mesh()
    (reference,) = result["references"]
    assert reference["name"] == "figure" and reference["kind"] == "soft"
    assert reference["provenance"] == "measured"
    assert reference["size"] == pytest.approx(list(np.ptp(vertices, axis=0)), abs=1e-3)
    assert reference["bbox"]["min"][2] == 0.0
    assert reference["closed"] and reference["triangles"] == len(triangles)
    assert 0 < reference["volume"] < reference["hull_volume"]
    assert reference["silhouette"]["radius"] > reference["size"][0] / 2
    source = reference["source"]
    assert source["file"] == "figure.stl" and source["format"] == "stl"
    assert re.fullmatch(r"[0-9a-f]{64}", source["sha256"])
    (pocket,) = reference["pockets"]
    assert pocket["fit_class"] == "cradle" and pocket["placed"] and pocket["gap"] > 1.5
    assert reference["used"] is True
    assert "references_s" in result["timings"]
    assert "figure [40.88, 41.43, 24.0]" in result["stdout"]  # the model saw the same numbers


def test_the_cavity_is_proven_with_the_resolvers_clearance_all_round(jobs):
    result = ok(jobs["basket"])
    (check,) = reference_fit(jobs["basket"])
    assert check["status"] == "pass" and check["severity"] == "error"
    data = check["data"]
    assert data["kind"] == "pocket" and data["reference"] == "figure"
    assert data["fit_class"] == "cradle" and data["clearance_proven"] and data["path_clear"]
    assert data["intrusions"] == [] and data["blocked"] == []
    # 1.5 mm plus 1 % of the 41.4 mm figure, once printed.
    assert data["target"] == pytest.approx(1.5 + 0.01 * 41.4349, abs=1e-3)
    assert data["provenance"] == "guessed"
    assert "goes in and out along +Z" in check["message"]
    # The cavity really is that large: the basket's inner radius is the pocket's.
    basket = result["parts"][0]
    radius = result["references"][0]["silhouette"]["radius"]
    outer = (basket["bbox"]["max"][0] - basket["bbox"]["min"][0]) / 2
    assert outer - 2.0 == pytest.approx(radius * 1.003 + data["offset"], abs=1e-3)


def test_a_deliberately_trapped_reference_fails_the_insertion_check(jobs):
    job = jobs["trapped"]
    assert job.returncode == 0 and job.result["status"] == "check_error"
    (check,) = reference_fit(job)
    assert check["status"] == "fail"
    assert "the thing is trapped" in check["message"] and "'basket'" in check["message"]
    assert check["data"]["clearance_proven"] is True and check["data"]["path_clear"] is False
    assert check["data"]["blocked"][0]["part"] == "basket"


def test_the_viewer_mesh_shows_the_reference_as_a_ghost(jobs):
    job = jobs["basket"]
    mesh = json.loads((job.dir / "mesh.json").read_text())
    assert mesh["hotend"]["parts"] == {"basket": "/model/basket"}
    (ghost,) = mesh["hotend"]["references"]
    assert ghost["name"] == "figure" and ghost["path"] == "/model/ghost.figure"
    assert ghost["alpha"] < 1 and ghost["mesh"] == "exact" and ghost["pocket"] == "pocket_1"
    leaves = {node["name"]: node for node in mesh["shapes"]["parts"]}
    assert list(leaves) == ["basket", "ghost.figure"]
    leaf = leaves["ghost.figure"]
    assert leaf["alpha"] == ghost["alpha"] and leaf["type"] == "shapes"
    assert set(leaf) == set(leaves["basket"])
    # The ghost's buffers decode like any part's and sit where the pocket put the figure:
    # on the basket's floor, 2 mm up.
    (found,) = list(_leaves(leaf, np.eye(3), np.zeros(3)))
    instance = mesh["instances"][found[0]["shape"]["ref"]]

    def decode(buffer):
        import base64

        return np.frombuffer(base64.b64decode(buffer["buffer"]), dtype=buffer["dtype"])

    points = decode(instance["vertices"]).reshape(-1, 3)
    assert len(decode(instance["triangles"])) == 3 * ghost["triangles"]
    assert points[:, 2].min() == pytest.approx(2.0, abs=1e-5)
    assert points[:, 2].max() == pytest.approx(26.0, abs=1e-5)
    assert set(instance) == set(mesh["instances"][0])
    for view in ("iso", "top"):
        assert (job.dir / "views" / f"{view}.png").stat().st_size > 2000


def test_the_reference_is_never_exported_for_printing(jobs):
    job = jobs["basket"]
    result = ok(job)
    basket = result["parts"][0]
    assert [part["name"] for part in result["parts"]] == ["basket"]
    objects = read_3mf(job.dir / result["artifacts"]["threemf"])
    assert list(objects) == ["basket"]
    assert objects["basket"]["volume"] == pytest.approx(basket["volume"], rel=2e-3)
    assert objects["basket"]["max"][2] == pytest.approx(basket["bbox"]["max"][2], abs=1e-3)
    index = json.loads((job.dir / "model.parts.json").read_text())
    assert [part["name"] for part in index["parts"]] == ["basket"]
    # The STEP and STL hold one solid of the basket's size: 16.4 mm tall, not the figure's 24.
    step = (job.dir / "model.step").read_text()
    assert step.count("MANIFOLD_SOLID_BREP") == 1 and "figure" not in step
    assert list(result["artifacts"]["stl"]) == ["basket"]
    raw = (job.dir / result["artifacts"]["stl"]["basket"]).read_bytes()
    count = int.from_bytes(raw[80:84], "little")
    record = np.dtype([("normal", "<f4", 3), ("points", "<f4", (3, 3)), ("attribute", "<u2")])
    corners = np.frombuffer(raw, dtype=record, count=count, offset=84)["points"]
    assert float(corners[:, :, 2].max()) == pytest.approx(basket["bbox"]["max"][2], abs=1e-3)
    # What print preparation lays on the plate is the basket alone.
    prepared = ok(jobs["prepare_main"])
    assert [entry["part"] for entry in prepared["orientations"]] == ["basket"]
    assert list(read_3mf(jobs["prepare_main"].dir / prepared["plates"][0]["threemf"])) == ["basket"]


def test_a_reference_outside_the_job_folder_or_an_unnamed_object_is_a_bad_request(jobs):
    escape = jobs["escape"]
    assert escape.returncode == 2
    assert escape.result["error"]["type"] == "bad_request"
    assert "points outside the job folder" in escape.result["error"]["message"]
    ambiguous = jobs["ambiguous"]
    assert ambiguous.returncode == 2
    message = ambiguous.result["error"]["message"]
    assert "holds 4 objects and they are never merged" in message
    assert "1: 'figure' on plate 1 'big', 45.0 x 45.0 x 22.5 mm" in message
    # No model ran and nothing was built for either.
    for job in (escape, ambiguous):
        assert not (job.dir / "model.brep").exists()


def test_the_references_op_lists_what_a_file_holds_without_building(jobs):
    result = ok(jobs["listing"])
    figure, eye, dome = result["references"]
    assert figure["status"] == "needs_selection" and "never merged" in figure["message"]
    assert [(o["index"], o["name"], o["plate"], o["plate_name"]) for o in figure["objects"]] == [
        (1, "figure", 1, "big"), (2, "eye", 2, "eyes"), (3, "eye", 2, "eyes"), (4, "eye", 2, "eyes")]
    assert [(p["name"], p["role"], p["geometry"]) for p in figure["objects"][0]["parts"]] == [
        ("DOME", "normal_part", True), ("BASE", "normal_part", True),
        ("more infill", "modifier_part", False), ("vent", "negative_part", False)]
    assert figure["format"] == "3mf" and "reference" not in figure
    assert eye["status"] == "ok" and eye["reference"]["size"] == [2.0, 4.0, 1.5]
    assert dome["status"] == "ok"
    assert dome["reference"]["size"][2] == pytest.approx(240.0)  # centimetres, as asked
    (phone,) = result["components"]
    assert phone["id"] == "sample_phone" and phone["size"] == [80.0, 130.0, 10.0]
    assert phone["proven"] is False and len(phone["questions"]) >= 6
    assert phone["rules"]["max_back_thickness"]["value"] == 2.4
    assert all(entry["source"].startswith("https://") for entry in phone["unverified"])
    assert result["available_components"] == list_components()
    assert "sample_phone" in result["available_components"]
    assert result["material"]["flexible"] is True and result["material"]["nozzle"] == "direct_drive"


def test_a_flexible_case_gets_a_stretch_fit_and_unverified_numbers_become_questions(jobs):
    job = jobs["case_tpu"]
    result = ok(job)
    pocket, clearance, provenance = reference_fit(job)
    assert pocket["status"] in ("pass", "warn")
    data = pocket["data"]
    assert data["fit_class"] == "stretch" and data["material_flexible"] is True
    assert data["reference"] == "sample_phone" and data["reference_provenance"] == "standard"
    assert data["target"] == pytest.approx(-0.05) and data["clearance_proven"]
    assert data["grip_coverage"] > 0.7
    assert 0 < data["stretch_strain"] < data["stretch_limit"]
    # The lip is drawn 1.2 mm in from the pocket's wall, which is 0.15 mm outside the phone
    # at its narrow side (the fit's offset and the shrinkage allowance).
    assert data["lip_depth"] == pytest.approx(1.05, abs=0.05)
    # Guessed until its gauge is printed.
    assert data["provenance"] == "guessed" and pocket["status"] == "warn"
    assert "gauge" in pocket["message"]
    assert clearance["status"] == "pass" and clearance["data"]["collisions"] == []
    assert clearance["data"]["keep_outs_covered"] == []
    # The model used numbers that are not verified: it is not reported as fully proven.
    assert provenance["status"] == "warn" and provenance["message"].startswith("Not fully proven")
    assert provenance["data"]["proven"] is False
    assert not all(check["status"] == "pass" for check in reference_fit(job))
    questions = result["questions"]
    assert {q["requirement"] for q in questions} >= {
        "sample_phone_usb_c", "sample_phone_camera_plateau"}
    for question in questions:
        assert set(question) == QUESTION_KEYS and question["guessed_params"] == []
        assert re.fullmatch(r"[a-z][a-z0-9_]{0,31}", question["requirement"])
    (component,) = result["components"]
    assert component["id"] == "sample_phone" and component["proven"] is False
    assert "usb_c" in component["used_features"]
    assert component["body"]["provenance"] == "standard"
    assert all(entry["source"].startswith("https://") for entry in component["unverified"])
    # The back stays under the maker's 2.4 mm and the phone is a ghost, not a part.
    (case,) = result["parts"]
    assert case["bbox"]["min"][2] == pytest.approx(-1.6)
    mesh = json.loads((job.dir / "mesh.json").read_text())
    assert [g["name"] for g in mesh["hotend"]["references"]] == ["sample_phone"]
    assert list(read_3mf(job.dir / result["artifacts"]["threemf"])) == ["case"]


def test_the_same_case_in_a_rigid_material_is_refused(jobs):
    job = jobs["case_pla"]
    assert job.returncode == 0 and job.result["status"] == "check_error"
    pocket = reference_fit(job)[0]
    assert pocket["status"] == "fail" and pocket["severity"] == "error"
    assert "a stretch fit needs a flexible material such as TPU, and PLA is rigid" in pocket["message"]
    assert pocket["data"]["material_flexible"] is False


def test_each_reference_fit_has_a_gauge_on_a_plate_of_its_own(jobs):
    result = ok(jobs["gauges"])
    ring, band = result["gauges"]
    assert ring["card"] == "pocket" and ring["name"] == "figure_ring"
    assert ring["parts"] == ["figure_ring.ring"] and ring["size"][2] == pytest.approx(5.0)
    assert ring["clearances"][0]["fit_class"] == "cradle"
    assert "drop through the ring" in ring["dimensions"][0]["what"]
    assert band["args"]["reference"] == "sample_phone" and band["args"]["fit"] == "stretch"
    assert "stretch over the thing and stay on" in band["dimensions"][0]["what"]
    # A thin frame of the case's rim: the phone's outline plus a 2 mm wall, 5 mm tall.
    assert band["size"] == pytest.approx([80.30 + 4, 130.45 + 4, 5.0], abs=0.1)
    for gauge in (ring, band):
        assert [d["feature"] for d in gauge["dimensions"]] == ["hole", "hole"]
        assert all(d["object"] == gauge["parts"][0] for d in gauge["dimensions"])
    plate = result["plates"][0]
    objects = read_3mf(jobs["gauges"].dir / plate["threemf"])
    assert sorted(objects) == sorted(ring["parts"] + band["parts"])
    assert result["unplaced"] == []
    # The gauges of a built model are found without naming them.
    (again,) = ok(jobs["gauge_of_model"])["gauges"]
    assert again["card"] == "pocket" and again["args"]["reference"] == "figure"
    assert again["args"]["shape"] == "cylinder"
    unknown = jobs["gauge_unknown_reference"]
    assert unknown.returncode == 2
    assert "no reference named 'nothing'" in unknown.result["error"]["message"]


def test_a_flexible_part_prints_only_on_the_main_nozzle(jobs):
    main = ok(jobs["prepare_main"])
    (check,) = [c for c in main["checks"] if c["id"] == "material_nozzle"]
    assert check["status"] == "pass" and check["data"]["nozzle_id"] == 1
    other = jobs["prepare_auxiliary"]
    assert other.returncode == 0 and other.result["status"] == "check_error"
    (check,) = [c for c in other.result["checks"] if c["id"] == "material_nozzle"]
    assert check["status"] == "fail" and check["severity"] == "error"
    assert "prints only on the main, direct-drive nozzle" in check["message"]
    assert check["data"]["main_nozzle_id"] == 1 and check["data"]["provenance"] == "guessed"


def test_a_build_fitted_to_a_reference_file_asks_about_its_guesses_by_itself(jobs):
    """No contract, no hand-written note: the result of a build around a reference file
    asks whether the file is the real thing at its real size, and whether the guessed
    room suits it."""
    result = ok(jobs["basket"])
    questions = result["questions"]
    assert all(set(question) == QUESTION_KEYS for question in questions)
    assert [question["requirement"] for question in questions] == ["reference_figure", "pocket_1"]
    size = " x ".join(f"{value:.1f}" for value in result["references"][0]["size"])
    assert "figure.stl" in questions[0]["text"] and size in questions[0]["text"]
    assert "caliper" in questions[0]["text"]
    assert "cradle" in questions[1]["text"] and "ring gauge" in questions[1]["text"]
    gap = result["references"][0]["pockets"][0]["gap"]
    assert f"{gap:.2f} mm per side" in questions[1]["text"]
    # A build without references asks nothing (test above); a trapped figure still asks.
    assert [q["requirement"] for q in jobs["trapped"].result["questions"]] == [
        "reference_figure", "pocket_1"]


def test_a_contract_can_state_the_fit_to_a_reference_and_a_guess_is_never_called_calibrated(jobs):
    result = jobs["stated"].result
    assert result["status"] == "check_error"  # only for the requirement nothing answers
    contract = {c["data"]["requirement"]: c for c in result["checks"] if c["id"] == "contract"}
    # The reference fit is measured by the pocket's own proof.
    stated = contract["figure_drops_in"]
    assert stated["data"]["met"] is True and stated["data"]["requirement_kind"] == "reference_fit"
    assert [p["status"] for p in stated["data"]["pockets"]] == ["pass"]
    # Both fits rest on clearances nobody has tried on this printer: whatever the script's
    # tag says, they are warnings with a question, and the questions reach the result.
    for requirement, word in (("figure_drops_in", "ring gauge"), ("lid_is_pegged", "calibrated")):
        check = contract[requirement]
        assert check["status"] == "warn" and "rests on a guess" in check["message"]
        assert word in check["data"]["question"]["text"]
    # A requirement for a reference the model made no pocket for is not met.
    assert contract["no_such_thing"]["status"] == "fail"
    assert "no pocket for 'other'" in contract["no_such_thing"]["message"]
    asked = [question["requirement"] for question in result["questions"]]
    # The contract's question stands for the pocket's own, so the gauge is asked for once.
    assert asked == ["figure_drops_in", "lid_is_pegged", "reference_figure"]
    # With a rim over the opening the figure is trapped: the stated fit is not met.
    trapped = {c["data"]["requirement"]: c
               for c in jobs["stated_trapped"].result["checks"] if c["id"] == "contract"}
    assert trapped["figure_drops_in"]["data"]["met"] is False
    assert "NOT met" in trapped["figure_drops_in"]["message"]
