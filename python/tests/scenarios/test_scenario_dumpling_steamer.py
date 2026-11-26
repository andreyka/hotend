"""The first acceptance scenario (docs/IMPLEMENTATION_BRIEF.md section 10): a soft figure in a
steamer basket with a lid. The figure (`dumpling`) is given as a reference file.

The model script is the one the scenario delivered (tests/models/scenarios/dumpling-steamer).
It is built through the worker around a procedural stand-in of an invented size, so the test
always runs; with HOTEND_REFS_DIR set it is also built, unchanged, around the real
files of local_references, which are never in the repository (a copy is put in the job
folder for the run and removed again, with the viewer mesh that shows it).

What a user relies on is measured here and not taken from the model's own checks
(scenario_harness): the dumpling as the build placed it, against the exact solids, with
rays; the exported plate, triangle by triangle.

Where the basket's edge is, is a parameter. Its default puts the edge at 71 % of the figure's
height, so that 29 % stands above it. An entry of a real file may name features that are to be
below the edge (`face_mm`)."""

from __future__ import annotations

import copy
import hashlib
import json
import math
import re
import shutil
from pathlib import Path

import numpy as np
import pytest
from scenario_harness import (
    MODELS,
    Rays,
    boxes_apart,
    checks,
    ghost_vertices,
    mesh_measures,
    ok,
    part,
    prepare_job,
    read_meshes,
    run_all,
    smallest_circle,
    solids,
    stand_in_mesh,
    step_is_exact_only,
    top_points,
    write_plain_3mf,
    write_stl,
)

# isort: split
# After scenario_harness, which puts tests/ on the import path.
from local_references import ENTRIES, FOLDER, KEYS, WHY_NOT, entry_of

SCENARIO = "scenarios/dumpling-steamer"
DATA = json.loads((MODELS / SCENARIO / "reference.json").read_text(encoding="utf-8"))
PARTS = {"basket", "lid", "knob"}
ERROR_CHECKS = ("valid_solid", "manifold", "single_solid_per_part", "fits_build_volume",
                "min_wall", "parts_do_not_intersect")
MAIN_REACH = DATA["printer"]["nozzles"][0]["reach"]
# A second dumpling, nobody's model either: smaller, oval and in another format.
OTHER = {"radius": 18.0, "height": 28.0, "pleats": 5, "stretch_x": 1.25}
# The default of `show`: the share of the dumpling that stands above the edge.
STANDS_ABOVE = 0.29


def request(reference: dict, params: dict | None = None, **outputs) -> dict:
    return {
        "protocol": 0, "op": "build", "model_path": "model.py", "params": params or {},
        "context": {"printer": copy.deepcopy(DATA["printer"]), "material": DATA["material"],
                    "references": [{**DATA["reference"], **reference}]},
        "outputs": {"step": True, "brep": True, "mesh": True, "views": [], "stl": False,
                    "threemf": True, **outputs},
        "checks": {**DATA["checks"], "verify_determinism": False},
        "limits": {"wall_seconds": 400, "max_triangles": 2_000_000},
    }


def stand_in(job_dir: Path) -> None:
    write_stl(job_dir / "dumpling.stl", *stand_in_mesh(**DATA["stand_in"]["args"]))


def other_mesh() -> tuple[np.ndarray, np.ndarray]:
    vertices, triangles = stand_in_mesh(OTHER["radius"], OTHER["height"], OTHER["pleats"])
    return vertices * np.array([OTHER["stretch_x"], 1.0, 1.0]), triangles


@pytest.fixture(scope="module")
def jobs(tmp_path_factory):
    root = tmp_path_factory.mktemp("dumpling_steamer")
    file = {"path": "dumpling.stl", "format": "stl"}
    builds = {
        "one": request(file, views=["iso"], view_size=[320, 240]),
        "three_lifted": request(file, {"count": 3, "lid_lift": 40.0}),
        "close_low_thin": request(file, {"snug": 1, "show": 0.75, "wall": 3.0, "stack": 0}),
        "deep_thick": request(file, {"show": 0.0, "wall": 4.8}),
        "low_tray": request(file, {"show": 0.7}),
        "aside": request(file, {"lid_aside": 1}),
        "other": request({"path": "other.3mf", "format": "3mf"}),
    }
    folders = {}
    for name, body in builds.items():
        folders[name] = prepare_job(root / name, body, f"{SCENARIO}/model.py")
        if name == "other":
            write_plain_3mf(folders[name] / "other.3mf", *other_mesh())
        else:
            stand_in(folders[name])
    done = run_all(folders)
    plate = {"protocol": 0, "op": "prepare", "views": [], "threemf": True,
             "context": {"printer": DATA["printer"], "material": DATA["material"]}}
    done.update(run_all({
        name: prepare_job(root / name, plate, copy_from=done[build].dir)
        for name, build in (("plate", "one"), ("plate_other", "other"))}))
    return done


def measured_fit(job_dir: Path, result: dict) -> dict:
    """The dumpling in its holder, measured without the engine's checks: the vertices of
    the reference where the build placed it, against the exact solids. Only numbers leave
    this function, so it may be used on somebody else's file."""
    points, exactness = ghost_vertices(job_dir, "dumpling")
    simplified = re.fullmatch(r"simplified to within ([0-9.]+) mm", exactness)
    assert exactness == "exact" or simplified, exactness
    shapes = solids(job_dir)
    box = shapes["basket"].bounding_box()
    axis = np.array([(box.min.X + box.max.X) / 2, (box.min.Y + box.max.Y) / 2])
    basket = Rays(shapes["basket"])
    # The floor's top, where the slats are: the highest thing a ray from above meets.
    floor = max(10.0 - basket.first((axis[0] + dx, axis[1] + dy, 10.0), (0, 0, -1))
                for dx in np.linspace(-20, 20, 41) for dy in (-7.0, 0.0, 7.0))
    # The inside of the wall, from the floor to the edge, all round.
    bore = [basket.first((axis[0], axis[1], z), (math.cos(turn), math.sin(turn), 0))
            for z in np.linspace(floor + 0.05, box.max.Z - 0.05, 25)
            for turn in np.linspace(0, 2 * math.pi, 73)[:-1]]
    centre, radius = smallest_circle(points[:, :2])
    # With the cover on: how far every point of the dumpling is from the nearest wall
    # outwards, and how far its top is from whatever hangs above it.
    everything = Rays(*shapes.values())
    away = points[:, :2] - axis
    reach = np.hypot(away[:, 0], away[:, 1])
    sideways = [everything.first(p, (d[0] / r, d[1] / r, 0)) for p, d, r in
                zip(points, away, reach, strict=True) if r > 1.0 and p[2] > floor + 0.05]
    cover = Rays(shapes["lid"], shapes["knob"])
    above = [cover.first(p, (0, 0, 1)) for p in top_points(points)]
    one_in = max(1, len(points) // 400)
    buried = sum(Rays(shape).filled(p, (0.31, 0.52, 0.8))
                 for shape in shapes.values() for p in points[::one_in])
    low, high = points[:, 2].min(), points[:, 2].max()
    return {
        "slack": float(simplified[1]) if simplified else 0.0,
        "size": (points.max(axis=0) - points.min(axis=0)).tolist(),
        "circle_radius": radius, "off_centre": float(np.hypot(*(centre - axis))),
        "floor": floor, "seat": float(low - floor), "edge": box.max.Z,
        "bore_min": min(bore), "bore_max": max(bore),
        "air_centred": min(bore) - radius, "air_sideways": min(sideways),
        "air_above": min(above), "buried_points": buried,
        "shows": float((high - box.max.Z) / (high - low)),
        "outside": box.max.X - box.min.X,
    }


def fits_as_declared(measured: dict, result: dict) -> None:
    """The room that was measured is the room the result declares: the drawn clearance of
    the pocket all round (never less; at most a quarter millimetre more, the price of a
    round wall around an outline), a clear way in, nothing above the dumpling's top closer
    than the script states, and no point of the dumpling inside a part."""
    (fit,) = checks(result, "reference_fit", kind="pocket")
    declared, slack = fit["data"]["expected"], measured["slack"]
    assert fit["data"]["target"] < declared                 # the printed aim is the smaller
    assert declared - 0.02 - slack <= measured["air_centred"] <= declared + 0.25 + slack
    assert measured["air_sideways"] >= declared - 0.02 - slack
    assert measured["off_centre"] < 0.05 + slack           # it sits in the middle of its room
    # One plain bore from the floor to the edge: nothing narrows on the way out.
    assert measured["bore_max"] - measured["bore_min"] < 0.005
    assert abs(measured["seat"]) < 0.01 + slack             # it rests on the slats
    stated = float(re.search(r"is ([0-9.]+) mm above the dumpling's top", result["stdout"])[1])
    assert measured["air_above"] == pytest.approx(stated, abs=0.02 + slack)
    assert measured["air_above"] > 2.0
    assert measured["buried_points"] == 0


def statuses(result: dict, check_id: str) -> list[str]:
    return [c["status"] for c in result["checks"] if c["id"] == check_id]


def proven(result: dict, pockets: int = 1) -> None:
    """Every error check passes and each dumpling's room is proven free with a way out."""
    for check_id in ERROR_CHECKS:
        assert set(statuses(result, check_id)) == {"pass"}, check_id
    fits = [c for c in result["checks"] if c["id"] == "reference_fit"]
    assert len(fits) == pockets and all(c["status"] == "pass" for c in fits), fits
    assert all(c["data"]["clearance_proven"] and c["data"]["path_clear"] for c in fits)
    assert "fail" not in statuses(result, "fit_pairs")
    assert "fail" not in statuses(result, "contract")


def test_the_basket_is_sized_from_the_reference_and_the_fit_is_proven(jobs):
    result = ok(jobs["one"])
    assert {part["name"] for part in result["parts"]} == PARTS
    proven(result)
    (reference,) = result["references"]
    assert reference["provenance"] == "measured" and reference["kind"] == "soft"
    (pocket,) = reference["pockets"]
    assert pocket["fit_class"] == "cradle" and pocket["shape"] == "hull" and pocket["placed"]
    # The inside of the wall is the circle around the dumpling plus the resolver's gap:
    # basket outside = 2 * (that radius + wall 3.2 + band 1.2).
    fit = next(c for c in result["checks"] if c["id"] == "reference_fit")["data"]
    inside = 2 * (reference["silhouette"]["radius"] * fit["scale"] + fit["offset"])
    basket = next(part for part in result["parts"] if part["name"] == "basket")
    width = basket["bbox"]["max"][0] - basket["bbox"]["min"][0]
    assert width == pytest.approx(inside + 2 * (3.2 + 1.2), abs=0.02)
    assert fit["target"] > 1.5  # a cradle: visible room, never a squeeze
    # The basket's edge is at 71 % of the dumpling's height, so 29 % of it stands above the
    # edge, and the closed lid is above the dumpling's top.
    lid = next(part for part in result["parts"] if part["name"] == "lid")
    tall = reference["size"][2]
    assert result["params"]["show"] == STANDS_ABOVE
    assert basket["bbox"]["max"][2] == pytest.approx(2.4 + (1 - STANDS_ABOVE) * tall, abs=0.01)
    assert 2.4 + tall < lid["bbox"]["max"][2]
    # The basket is the tall piece and the cover the low one: a rim band and the dome.
    assert basket["bbox"]["max"][2] > 1.4 * (lid["bbox"]["max"][2] - lid["bbox"]["min"][2])
    assert "2 tier(s)" in result["stdout"] and "0 tier(s)" in result["stdout"]
    assert "cradle room per side" in result["stdout"]
    # The script states the lid's lowest point over the dumpling (the boss under the
    # knob), not only its ceiling: the room's top, where the boss ends, is the gap higher.
    lowest = float(re.search(r"is ([0-9.]+) mm above the dumpling's top", result["stdout"])[1])
    assert lowest == pytest.approx(tall * (fit["scale"] - 1) + fit["offset"], abs=0.006)
    assert lowest > 2.0


@pytest.fixture(scope="module")
def measured_one(jobs) -> dict:
    return measured_fit(jobs["one"].dir, ok(jobs["one"]))


def test_the_dumpling_fits_its_room_with_the_declared_clearance_measured_on_the_solids(
        jobs, measured_one):
    job = jobs["one"]
    result = ok(job)
    measured = measured_one
    fits_as_declared(measured, result)
    # What was placed is the file's dumpling, moved and nothing else: every vertex of the
    # stand-in this test wrote is found again, shifted by one and the same vector.
    mine, _ = stand_in_mesh(**DATA["stand_in"]["args"])
    placed, exactness = ghost_vertices(job.dir, "dumpling")
    assert exactness == "exact"
    shift = placed.min(axis=0) - mine.min(axis=0)
    for row in mine + shift:
        assert np.abs(placed - row).max(axis=1).min() < 1e-3
    assert measured["size"] == pytest.approx(result["references"][0]["size"], abs=1e-3)
    # 29 % of the dumpling stands above the basket's edge, measured.
    assert measured["shows"] == pytest.approx(STANDS_ABOVE, abs=0.002)
    assert measured["outside"] == pytest.approx(
        2 * (measured["bore_min"] + 3.2 + 1.2), abs=0.02)        # wall 3.2 and band 1.2


def test_swapping_the_reference_for_another_dumpling_re_fits_the_holder(jobs, measured_one):
    """The same script, byte for byte, around a dumpling of another size, shape and file
    format: every size of the holder follows, and the fit is proven and measured again."""
    script = hashlib.sha256((MODELS / SCENARIO / "model.py").read_bytes()).hexdigest()
    for name in ("one", "other"):
        assert hashlib.sha256((jobs[name].dir / "model.py").read_bytes()).hexdigest() == script
    first, second = ok(jobs["one"]), ok(jobs["other"])
    proven(second)
    (reference,) = second["references"]
    mine, _ = other_mesh()
    size = mine.max(axis=0) - mine.min(axis=0)                  # about 45 x 37 x 28 mm
    assert reference["size"] == pytest.approx(size.tolist(), abs=1e-3)
    assert reference["source"]["format"] == "3mf" and reference["provenance"] == "measured"
    was, now = measured_one, measured_fit(jobs["other"].dir, second)
    fits_as_declared(now, second)
    # The room is the smallest circle around the new dumpling plus the same kind of gap.
    assert now["circle_radius"] == pytest.approx(smallest_circle(mine[:, :2])[1], abs=1e-3)
    assert now["circle_radius"] < was["circle_radius"] - 5.0
    # (The cradle gap itself grows with the thing, by about 1.3 % of its size.)
    assert was["bore_min"] - now["bore_min"] == pytest.approx(
        was["circle_radius"] - now["circle_radius"], abs=0.3)
    assert now["outside"] < was["outside"] - 10.0
    assert now["shows"] == pytest.approx(was["shows"], abs=0.002)
    # The closed holder is lower by what the dumpling is lower (and a little more: the
    # narrower cover's dome is lower too).
    heights = [part(result, "knob")["bbox"]["max"][2] for result in (first, second)]
    lower = first["references"][0]["size"][2] - size[2]
    assert lower < heights[0] - heights[1] < lower + 4.0
    # The questions name the new file, and it prints on one plate like the first.
    asked = {question["requirement"]: question["text"] for question in second["questions"]}
    assert "other.3mf" in asked["reference_dumpling"]
    plate = ok(jobs["plate_other"])
    assert len(plate["plates"]) == 1 and plate["unplaced"] == []
    assert {c["status"] for c in plate["checks"]} == {"pass"}
    assert set(read_meshes(jobs["plate_other"].dir / "plates" / "1" / "plate.3mf")) == PARTS


def test_the_result_itself_asks_about_every_guess(jobs):
    """Nothing that was guessed is left to a hand-written note: the size read from the
    file, the room for a soft thing and the press fit on a printer nobody calibrated are
    questions of the result."""
    result = ok(jobs["one"])
    asked = {question["requirement"]: question["text"] for question in result["questions"]}
    assert set(asked) == {"dumpling_drops_in_freely", "knob_presses_into_lid",
                          "reference_dumpling"}
    assert "ring gauge" in asked["dumpling_drops_in_freely"]
    assert "not been calibrated" in asked["knob_presses_into_lid"]
    assert "caliper" in asked["reference_dumpling"] and "dumpling.stl" in asked["reference_dumpling"]
    contract = {c["data"]["requirement"]: c for c in result["checks"] if c["id"] == "contract"}
    for requirement in ("dumpling_drops_in_freely", "knob_presses_into_lid"):
        assert contract[requirement]["status"] == "warn"
        assert contract[requirement]["data"]["met"] is True
        assert "rests on a guess" in contract[requirement]["message"]


def test_the_lid_joint_is_reported_as_not_card_verified_and_the_knob_as_verified(jobs):
    result = ok(jobs["one"])
    messages = [c["message"] for c in result["checks"] if c["id"] == "fit_pairs"]
    assert any("knob_peg" in m and "goes together" in m for m in messages)
    # The lid sits on the rim with a resolver clearance but no card makes a round lid yet.
    assert any("without a feature card" in m for m in messages)
    contract = [c for c in result["checks"] if c["id"] == "contract"]
    assert [c["status"] for c in contract] == ["warn", "warn", "pass", "pass"]


def test_the_reference_is_shown_as_a_ghost_and_is_in_no_export(jobs):
    job = jobs["one"]
    mesh = json.loads((job.dir / "mesh.json").read_text())
    assert [g["name"] for g in mesh["hotend"]["references"]] == ["dumpling"]
    assert set(mesh["hotend"]["parts"]) == PARTS
    step = (job.dir / "model.step").read_text(errors="replace")
    assert "ghost" not in step.lower() and "dumpling" not in step.lower()
    step_is_exact_only(job.dir / "model.step", PARTS)
    # The exported meshes are the three parts and enclose exactly their volumes: nothing
    # of the dumpling (about 90 cm3 of it) is hidden in an object under another name.
    result = ok(job)
    for file in (job.dir / "export" / "model.3mf", jobs["plate"].dir / "plates" / "1" / "plate.3mf"):
        exported = {name: mesh_measures(*mesh) for name, mesh in read_meshes(file).items()}
        assert set(exported) == PARTS
        for name, measures in exported.items():
            assert measures["volume"] == pytest.approx(part(result, name)["volume"], rel=1e-3)
    assert (job.dir / "views" / "iso.png").stat().st_size > 5000


def test_the_count_makes_one_basket_for_several_dumplings(jobs):
    one, three = ok(jobs["one"]), ok(jobs["three_lifted"])
    proven(three, pockets=3)
    width = {name: next(p for p in result["parts"] if p["name"] == "basket")["bbox"]["max"][0]
             for name, result in (("one", one), ("three", three))}
    assert width["three"] > 2 * width["one"] * 0.95
    assert len(three["references"][0]["pockets"]) == 3


def test_the_ends_of_the_sliders_still_pass(jobs):
    close = ok(jobs["close_low_thin"])
    proven(close)
    assert close["references"][0]["pockets"][0]["fit_class"] == "loose"
    proven(ok(jobs["deep_thick"]))


def test_the_depth_is_a_slider_and_the_closed_holder_does_not_change_with_it(jobs):
    """`show` moves the joint between basket and cover and nothing else: a low tray (70 %
    of the dumpling above the edge, the wall drawn as tiers of the cover) is one setting
    away, closes to the same size and keeps the same room and the same clearance above the
    dumpling."""
    deep, low = ok(jobs["one"]), ok(jobs["low_tray"])
    proven(low)
    tall = low["references"][0]["size"][2]
    assert part(low, "basket")["bbox"]["max"][2] == pytest.approx(2.4 + 0.3 * tall, abs=0.01)
    assert "1 tier(s)" in low["stdout"] and "2 tier(s)" in low["stdout"]
    for name in ("basket", "knob"):
        boxes = [part(result, name)["bbox"] for result in (deep, low)]
        assert boxes[0]["max"][0] - boxes[0]["min"][0] == pytest.approx(
            boxes[1]["max"][0] - boxes[1]["min"][0], abs=1e-6)
    assert part(deep, "knob")["bbox"]["max"][2] == pytest.approx(
        part(low, "knob")["bbox"]["max"][2], abs=0.01)
    stated = [float(re.search(r"is ([0-9.]+) mm above the dumpling's top", r["stdout"])[1])
              for r in (deep, low)]
    assert stated[0] == stated[1]
    measured = measured_fit(jobs["low_tray"].dir, low)
    fits_as_declared(measured, low)
    assert measured["shows"] == pytest.approx(0.70, abs=0.002)


def test_the_cover_can_be_set_aside_for_a_picture_of_the_open_basket(jobs):
    aside, closed = ok(jobs["aside"]), ok(jobs["one"])
    proven(aside)
    box = {name: {p["name"]: p["bbox"] for p in result["parts"]}
           for name, result in (("aside", aside), ("closed", closed))}
    # The cover stands on the table beside the basket, unchanged in size.
    assert box["aside"]["lid"]["min"][2] == pytest.approx(0.0, abs=1e-6)
    assert box["aside"]["lid"]["min"][0] > box["aside"]["basket"]["max"][0] + 5.0
    for moved in ("lid", "knob"):
        sizes = [[hi - lo for lo, hi in zip(b[moved]["min"], b[moved]["max"], strict=True)]
                 for b in (box["aside"], box["closed"])]
        assert sizes[0] == pytest.approx(sizes[1], abs=1e-4)


def test_every_piece_prints_without_support_on_one_plate(jobs):
    result = ok(jobs["plate"])
    turned = {o["part"]: o["turned"] for o in result["orientations"]}
    assert turned == {"basket": "as modelled", "lid": "upside down", "knob": "upside down"}
    assert all(o["measures"]["overhang"] < 4.0 for o in result["orientations"])
    assert len(result["plates"]) == 1 and result["unplaced"] == []
    assert {c["status"] for c in result["checks"]} == {"pass"}
    assert {item["nozzle_id"] for item in result["plates"][0]["items"]} == {1}
    # The same, measured on the triangles of the plate file as a slicer gets it: every
    # piece lies on the bed inside the main nozzle's reach, apart from the others, with a
    # real first layer and next to nothing hanging over more steeply than 45 degrees.
    pieces = {name: mesh_measures(*mesh) for name, mesh in
              read_meshes(jobs["plate"].dir / "plates" / "1" / "plate.3mf").items()}
    assert set(pieces) == PARTS
    for name, piece in pieces.items():
        assert piece["min"][2] == pytest.approx(0.0, abs=1e-6), name
        for axis, key in enumerate("xyz"):
            assert MAIN_REACH[key][0] <= piece["min"][axis], name
            assert piece["max"][axis] <= MAIN_REACH[key][1], name
        assert piece["overhang"] < 4.0, name
    assert pieces["basket"]["contact"] > 3000 and pieces["lid"]["contact"] > 1200
    assert pieces["knob"]["contact"] > 80
    assert boxes_apart(pieces) >= 4.99


# ---- real files, when they are there -------------------------------------------------


@pytest.fixture(scope="module")
def real(tmp_path_factory):
    if WHY_NOT:
        pytest.skip(WHY_NOT)
    root = tmp_path_factory.mktemp("dumpling_steamer_real")
    folders, copies = {}, []
    for key in KEYS:
        entry = ENTRIES.get(key)
        if entry is None or not (FOLDER / entry["file"]).is_file():
            continue
        select = {name: entry[name] for name in ("plate", "object") if name in entry}
        job = prepare_job(root / key, request(
            {"path": "reference.3mf", "format": "3mf", **select}), f"{SCENARIO}/model.py")
        shutil.copy(FOLDER / entry["file"], job / "reference.3mf")
        copies += [job / "reference.3mf", job / "mesh.json"]
        folders[key] = job
    if not folders:
        pytest.skip("the reference files are not in HOTEND_REFS_DIR")
    results = {}
    try:
        for name, job in run_all(folders, at_a_time=2).items():
            result = job.result if job.returncode == 0 else None
            measured, exports = None, None
            if result and result["status"] == "ok":
                measured = measured_fit(job.dir, result)        # numbers only
                exports = {piece: mesh_measures(*mesh)["volume"] for piece, mesh in
                           read_meshes(job.dir / "export" / "model.3mf").items()}
                step_is_exact_only(job.dir / "model.step", PARTS)
            results[name] = (job.returncode, job.stderr, result, measured, exports)
    finally:
        # Nothing of the reference stays behind: neither the file nor the mesh that shows it.
        for path in copies:
            path.unlink(missing_ok=True)
    return results


@pytest.mark.skipif(bool(WHY_NOT), reason=WHY_NOT)
@pytest.mark.parametrize("key", KEYS)
def test_the_same_script_fits_each_of_the_real_files(real, key):
    entry = entry_of(key)
    code, stderr, result, measured, exports = real[key]
    assert code == 0, stderr
    assert result["status"] == "ok", (
        [c for c in result["checks"] if c["status"] == "fail"], stderr)
    proven(result)
    (reference,) = result["references"]
    assert reference["size"] == pytest.approx(entry["measured_mm"], abs=0.01)
    # The dumpling as printed and nothing else: the object the request names, not a
    # modifier volume, not the eyes of another plate.
    assert measured["size"] == pytest.approx(entry["measured_mm"], abs=0.02 + 2 * measured["slack"])
    fits_as_declared(measured, result)
    assert measured["shows"] == pytest.approx(STANDS_ABOVE, abs=0.005)
    # Features the entry names (plain numbers, measured on the file) are below the edge of
    # the basket.
    face = entry.get("face_mm")
    if face:
        edge = measured["edge"] - measured["floor"]             # above the dumpling's base
        highest = max(high for _, high in face["features"].values())
        lowest = min(low for low, _ in face["features"].values())
        assert 0 < lowest < highest < edge
        assert edge - highest == pytest.approx(face["edge_above_highest_mm"], abs=0.05)
    # The exports are the holder alone, with the volumes of its three parts.
    assert set(exports) == PARTS
    for part_name, volume in exports.items():
        assert volume == pytest.approx(part(result, part_name)["volume"], rel=1e-3)
    asked = {question["requirement"] for question in result["questions"]}
    assert {"reference_dumpling", "dumpling_drops_in_freely", "knob_presses_into_lid"} <= asked
    expect = entry.get("expect")
    if expect:
        sizes = {p["name"]: p for p in result["parts"]}
        basket = sizes["basket"]["bbox"]
        assert basket["max"][0] - basket["min"][0] == pytest.approx(expect["basket_outside_mm"], abs=0.01)
        assert 2 * measured["bore_min"] == pytest.approx(expect["basket_inside_mm"], abs=0.01)
        assert sizes["knob"]["bbox"]["max"][2] == pytest.approx(expect["closed_height_mm"], abs=0.01)
        for part_name, volume in expect["volumes_mm3"].items():
            assert sizes[part_name]["volume"] == pytest.approx(volume, abs=1.0)


def test_no_reference_geometry_is_in_the_repository():
    """Real reference files are other people's designs: the repository holds nothing of
    them. No mesh, solid or picture lies beside the scenario's script, and its data file is
    a few numbers."""
    folder = MODELS / SCENARIO
    assert {path.name for path in folder.iterdir() if path.name != "__pycache__"} == {
        "model.py", "reference.json"}
    assert (folder / "reference.json").stat().st_size < 4000
    geometry = {".3mf", ".stl", ".step", ".stp", ".obj", ".ply", ".brep", ".npz", ".npy", ".glb"}
    assert [path for path in MODELS.joinpath("scenarios").rglob("*")
            if path.suffix.lower() in geometry] == []
    # The script draws from the reference it is handed, never from numbers of its own:
    # no long run of coordinates is written into it.
    script = (folder / "model.py").read_text(encoding="utf-8")
    assert not re.search(r"(?:-?\d+\.\d+\s*,\s*){12,}", script)
