"""Print preparation: orientation by a stated score, shelf plating inside each nozzle's reach,
plate 3MF files and the checks of the plate. Real geometry throughout.

The first half measures shapes in this process; the second half reads worker jobs that the
session fixtures ran once (`builds`, `plates` in conftest.py).
"""

from __future__ import annotations

import io

import numpy as np
import pytest
from build123d import Align, Box, Cylinder, Pos, Sphere
from conftest import X2D_CONTEXT, ok, read_3mf
from landlock_exec import landlock_abi
from PIL import Image

from hotend_engine import prepare
from hotend_engine.parts import NamedPart
from hotend_engine.request import Context
from hotend_engine.tessellate import tessellate

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)
PRINTER = Context.model_validate(X2D_CONTEXT).printer
MAIN, AUXILIARY = PRINTER.nozzles


def choose(shape, **options) -> prepare.Choice:
    part = NamedPart("p", shape, "#4c78a8")
    mesh = tessellate([part]).meshes["p"]
    return prepare.choose_orientation(prepare.PartGeometry(part, mesh), 45.0, **options)


def candidate(choice: prepare.Choice, source: str) -> dict:
    return next(c for c in choice.candidates if c["source"] == source)


# --- orientation -------------------------------------------------------------------------------


def test_the_score_is_the_stated_sum_of_its_terms():
    measures = {"contact": 400.0, "overhang": 12.0, "height": 60.0, "fit_overhang": 5.0,
                "weak_snaps": ["snap lid 'a'"]}
    score, terms = prepare.score_of(measures)
    assert terms == {"support": 12.0, "fit_support": 15.0, "weak_snaps": 150.0,
                     "tipping": 100.0, "height": 60.0}
    assert score == 337.0
    # Under 20 mm2 on the bed the part is taken to fall over.
    assert prepare.score_of({**measures, "contact": 5.0})[1]["tipping"] == 1000.0


def test_a_pillar_taller_than_wide_is_laid_down_because_its_score_says_so():
    choice = choose(Box(20, 20, 60, align=BOTTOM))
    standing = candidate(choice, "as_modelled")
    # Standing: 60 mm tall on 400 mm2, three times as tall as its footing is wide.
    assert standing["score"] == pytest.approx(100.0 + 60.0)
    assert choice.source == "face" and choice.score == pytest.approx(20.0)
    assert choice.measures["height"] == pytest.approx(20.0)
    assert choice.measures["contact"] == pytest.approx(1200.0)
    assert choice.measures["overhang"] == 0.0
    assert choice.score == min(c["score"] for c in choice.candidates)
    assert standing["score"] - choice.score > prepare.KEEP_MARGIN
    assert "as modelled it would stand 60 mm tall on 400 mm2" in choice.sentence
    assert sum(choice.terms.values()) == pytest.approx(choice.score)


def test_a_tall_cup_stays_upright_because_lying_down_needs_support():
    cup = Box(30, 30, 60, align=BOTTOM) - Pos(0, 0, 2) * Box(26, 26, 60, align=BOTTOM)
    choice = choose(cup)
    assert choice.source == "as_modelled" and choice.score == pytest.approx(60.0)
    lying = [c for c in choice.candidates if c["source"] == "face" and c["height"] < 31]
    # Every way of laying it down puts the inside of a wall (26 x 58) over air.
    assert len(lying) == 4
    assert all(c["overhang"] == pytest.approx(26 * 58) and c["score"] > 1500 for c in lying)
    assert choice.sentence == ("'p' is printed as modelled: 900 mm2 lies flat on the bed and "
                               "nothing needs support.")


def test_a_part_is_not_turned_for_a_gain_inside_the_margin():
    choice = choose(Box(20, 20, 30, align=BOTTOM))
    best = min(c["score"] for c in choice.candidates)
    assert best == pytest.approx(20.0) and choice.score == pytest.approx(30.0)
    assert choice.source == "as_modelled" and choice.score - best < prepare.KEEP_MARGIN


def test_a_mushroom_is_turned_onto_its_cap():
    # A stem under a wide cap: as modelled the underside of the cap hangs over air.
    mushroom = Cylinder(5, 20, align=BOTTOM) + Pos(0, 0, 20) * Cylinder(25, 4, align=BOTTOM)
    choice = choose(mushroom)
    hanging = np.pi * (25**2 - 5**2)
    assert choice.as_modelled["overhang"] == pytest.approx(hanging, rel=1e-6)
    assert choice.source == "face" and choice.measures["overhang"] == 0.0
    assert tuple(choice.up) == (0.0, 0.0, -1.0)
    assert choice.to_json()["turned"] == "upside down"
    assert f"(as modelled about {hanging:.0f} mm2 would)" in choice.sentence
    assert "nothing needs support" in choice.sentence


def test_an_asked_orientation_is_used_when_it_can_print():
    choice = choose(Box(20, 20, 60, align=BOTTOM), preferred=(0.0, 0.0, 1.0),
                    preferred_by="the model")
    assert choice.source == "preferred" and choice.overridden is None
    assert choice.measures["height"] == pytest.approx(60.0)
    assert choice.sentence.endswith(", as the model asks.")
    # ... although another orientation scores better: the stated score is still reported.
    assert choice.score > min(c["score"] for c in choice.candidates) + prepare.KEEP_MARGIN


def test_an_asked_orientation_that_cannot_print_is_overridden_and_said():
    dome = Sphere(15) & Box(40, 40, 15, align=BOTTOM)
    choice = choose(dome, preferred=(0.0, 0.0, -1.0), preferred_by="the model")
    assert choice.source == "as_modelled" and choice.preferred_by == "the model"
    assert choice.overridden == "leaves nothing flat on the bed"
    assert choice.sentence.endswith(
        "; the model asked for another orientation, which leaves nothing flat on the bed.")

    # Lying along X it is 300 mm long: beyond the 256 mm reach, whichever way it is turned.
    beam = Box(20, 20, 300, align=BOTTOM)
    choice = choose(beam, preferred=(1.0, 0.0, 0.0), preferred_by="the request",
                    reach_size=(256.0, 256.0, 400.0), margin=2.0)
    assert choice.overridden == "does not fit the nozzle's reach"
    assert choice.source == "as_modelled" and choice.measures["height"] == pytest.approx(300.0)
    assert "the request asked for another orientation" in choice.sentence


@pytest.mark.parametrize(("size", "height"), [((254, 20, 5), 254.0), ((258, 20, 20), 258.0)])
def test_a_part_that_fits_only_another_way_up_is_stood_that_way(size, height):
    # Lying down it is longer than the 256 mm bed less 2 mm at each edge, either way round;
    # standing on its end it is inside the 260 mm of height. (The as-modelled candidate is
    # then not among those that fit: comparing candidates by value once raised here.)
    choice = choose(Box(*size, align=BOTTOM), reach_size=MAIN.reach.size, margin=2.0)
    assert choice.source == "face" and choice.fits_reach and not choice.modelled_fits_reach
    assert choice.measures["height"] == pytest.approx(height)
    assert candidate(choice, "as_modelled")["fits_reach"] is False
    assert choice.sentence.endswith("(as modelled it does not fit the nozzle's reach).")
    # With no margin asked for, 254 mm lies on the bed and the part stays as modelled.
    flat = choose(Box(254, 20, 5, align=BOTTOM), reach_size=MAIN.reach.size, margin=0.0)
    assert flat.source == "as_modelled" and flat.modelled_fits_reach


def test_a_part_that_fits_no_way_up_keeps_its_best_score_and_says_it_does_not_fit():
    choice = choose(Box(254, 254, 5, align=BOTTOM), reach_size=MAIN.reach.size, margin=2.0)
    assert choice.source == "as_modelled" and not choice.fits_reach
    assert not any(c["fits_reach"] for c in choice.candidates)


def test_one_rule_says_what_fits_a_reach():
    room = prepare.reach_room((256.0, 256.0, 260.0), 2.0)
    assert room == (252.0, 252.0, 260.0)
    spare, quarter = prepare.reach_spare((100.0, 250.0, 30.0), room)
    assert spare == (152.0, 2.0, 230.0) and not quarter and prepare.spare_fits(spare)
    # Only a quarter turn makes it fit the auxiliary nozzle's shorter X.
    spare, quarter = prepare.reach_spare((250.0, 20.0, 20.0),
                                         prepare.reach_room(AUXILIARY.reach.size, 2.0))
    assert quarter and spare == (211.5, 2.0, 236.0) and prepare.spare_fits(spare)
    # Too large either way: the shortfall of the piece as it lies, no turn reported.
    spare, quarter = prepare.reach_spare((254.0, 254.0, 5.0), room)
    assert spare == (-2.0, -2.0, 255.0) and not quarter and not prepare.spare_fits(spare)
    # The height has no margin.
    assert prepare.spare_fits(prepare.reach_spare((20.0, 20.0, 260.0), room)[0])
    assert not prepare.spare_fits(prepare.reach_spare((20.0, 20.0, 260.1), room)[0])


def test_turning_keeps_the_solid_and_stands_it_on_the_bed():
    part = NamedPart("p", Pos(7, -3, 12) * Box(10, 20, 30), "#4c78a8")
    mesh = tessellate([part]).meshes["p"]
    for up in ((0, 0, 1), (0, 0, -1), (1, 0, 0), (0, -1, 0), (0.6, 0.0, 0.8)):
        rotation = prepare.rotation_to_up(np.array(up, dtype=float))
        assert np.allclose(rotation @ rotation.T, np.eye(3)) and np.linalg.det(rotation) > 0.99
        assert np.allclose(rotation @ np.array(up, dtype=float), (0, 0, 1))
        moved, moved_mesh, translation = prepare.turned_part(part, mesh, rotation)
        box = moved.shape.bounding_box()
        assert tuple(box.min) == pytest.approx((0, 0, 0), abs=1e-6)
        assert moved.shape.volume == pytest.approx(6000.0)
        # The mesh went through the same transform as the solid.
        assert moved_mesh.vertices.min(axis=0) == pytest.approx((0, 0, 0), abs=1e-6)
        assert np.allclose(moved_mesh.vertices, mesh.vertices @ rotation.T + translation)


# --- plating -----------------------------------------------------------------------------------


def items(count: int, size: tuple[float, float, float], nozzle, name: str = "p"):
    shape = Box(*size, align=Align.MIN)
    return [prepare.Item(name=f"{name}.{n}", part=name, copy=n, shape=shape, mesh=None,
                         color="#4c78a8", rotation=np.eye(3), translation=np.zeros(3),
                         size=size, nozzle=nozzle) for n in range(1, count + 1)]


def assert_apart_and_inside(placed, gap: float, margin: float) -> None:
    for entry in placed:
        reach = entry.item.nozzle.reach
        assert entry.low[0] >= reach.x[0] + margin - 1e-9
        assert entry.high[0] <= reach.x[1] - margin + 1e-9
        assert entry.low[1] >= reach.y[0] + margin - 1e-9
        assert entry.high[1] <= reach.y[1] - margin + 1e-9
        box = entry.shape.bounding_box()
        assert tuple(box.min) == pytest.approx(entry.low, abs=1e-6)
        assert tuple(box.max) == pytest.approx(entry.high, abs=1e-6)
    for a in placed:
        for b in placed:
            if a is b or a.plate != b.plate:
                continue
            apart_x = max(a.low[0] - b.high[0], b.low[0] - a.high[0])
            apart_y = max(a.low[1] - b.high[1], b.low[1] - a.high[1])
            assert max(apart_x, apart_y) >= gap - 1e-9, (a.item.name, b.item.name)


def test_the_shelf_layout_keeps_the_gap_and_the_margin():
    placed, unplaced = prepare.lay_out(items(9, (60.0, 40.0, 10.0), MAIN), gap=5.0, margin=2.0)
    assert not unplaced and {p.plate for p in placed} == {1}
    assert_apart_and_inside(placed, 5.0, 2.0)
    # Rows from the front left: three fit across 252 mm, the fourth starts the next row.
    assert [p.low[:2] for p in placed[:4]] == [(2.0, 2.0), (67.0, 2.0), (132.0, 2.0),
                                               (2.0, 47.0)]
    again, _ = prepare.lay_out(items(9, (60.0, 40.0, 10.0), MAIN), gap=5.0, margin=2.0)
    assert [(p.plate, p.low, p.high) for p in again] == [(p.plate, p.low, p.high)
                                                         for p in placed]


def test_the_auxiliary_nozzle_never_prints_left_of_its_reach():
    both = items(3, (60.0, 40.0, 10.0), AUXILIARY, "aux") + items(3, (60.0, 40.0, 10.0), MAIN)
    placed, unplaced = prepare.lay_out(both, gap=5.0, margin=0.0)
    assert not unplaced
    assert_apart_and_inside(placed, 5.0, 0.0)
    by_name = {p.item.name: p for p in placed}
    # The first piece of the plate is the auxiliary nozzle's: it starts at X = 20.5, not 0.
    assert by_name["aux.1"].low == (20.5, 0.0, 0.0)
    assert min(p.low[0] for p in placed if p.item.part == "aux") == 20.5
    # A piece of the main nozzle may use the strip the auxiliary nozzle cannot reach.
    assert min(p.low[0] for p in placed if p.item.part == "p") == 0.0

    # 250 mm wide fits the main nozzle (256 mm) and not the auxiliary one (235.5 mm);
    # turned a quarter it fits the auxiliary nozzle along Y.
    (wide,), _ = prepare.lay_out(items(1, (250.0, 30.0, 5.0), AUXILIARY), 5.0, 0.0)
    assert wide.quarter_turn and wide.low == (20.5, 0.0, 0.0) and wide.high == (50.5, 250.0, 5.0)
    assert wide.shape.volume == pytest.approx(250 * 30 * 5)
    (flat,), _ = prepare.lay_out(items(1, (250.0, 30.0, 5.0), MAIN), 5.0, 0.0)
    assert not flat.quarter_turn and flat.high == (250.0, 30.0, 5.0)


def test_pieces_that_do_not_fit_one_plate_are_split_and_a_too_large_one_is_reported():
    twelve = items(12, (120.0, 100.0, 10.0), MAIN)
    placed, unplaced = prepare.lay_out(twelve, gap=5.0, margin=2.0)
    assert not unplaced and len(placed) == 12
    assert [sum(p.plate == n for p in placed) for n in (1, 2, 3)] == [4, 4, 4]
    assert_apart_and_inside(placed, 5.0, 2.0)

    placed, unplaced = prepare.lay_out(
        items(1, (300.0, 30.0, 5.0), MAIN, "long") + items(1, (30.0, 30.0, 300.0), MAIN, "tall")
        + items(1, (30.0, 30.0, 5.0), MAIN, "fine"), gap=5.0, margin=2.0)
    assert [p.item.name for p in placed] == ["fine.1"]
    assert [u["name"] for u in unplaced] == ["long.1", "tall.1"]
    assert "larger than the main nozzle's reach of 252.0 x 252.0 x 260.0 mm" in (
        unplaced[0]["reason"])


# --- the prepare op, through the worker --------------------------------------------------------


def orientation(result: dict, part: str) -> dict:
    return next(o for o in result["orientations"] if o["part"] == part)


def test_the_cable_box_lid_is_flipped_rails_up_and_needs_no_support(builds, plates):
    built = ok(builds["cable_box_asked"])
    result = ok(plates["prepare_cable_box"])
    assert result["op"] == "prepare" and not result["unplaced"]
    lid, box = orientation(result, "lid"), orientation(result, "box")

    assert lid["source"] == "face" and lid["up"] == [0.0, 0.0, -1.0]
    assert lid["turned"] == "upside down"
    # As modelled the lid rests on its rails with its whole plate over air.
    assert lid["as_modelled"]["overhang"] > 5000
    assert lid["measures"]["overhang"] < prepare.SUPPORT_NOTE_MM2
    assert lid["measures"]["fit_overhang"] == 0.0
    # The rails stand upright either way up; laying the lid on its side to spare them
    # would put its whole plate on support, and the score says that is worse.
    assert lid["measures"]["weak_snaps"] == ["snap lid 'snap_lid_1'"]
    assert lid["terms"]["weak_snaps"] == prepare.SNAP_PENALTY
    assert "the snap rails stand upright, across the layers" in lid["explanation"]
    assert lid["measures"]["contact"] > 7000
    assert lid["score"] == min(c["score"] for c in lid["candidates"])
    assert lid["explanation"].startswith("'lid' is turned upside down: ")
    assert "nothing needs support (as modelled about" in lid["explanation"]
    assert box["source"] == "as_modelled"
    assert box["explanation"].startswith("'box' is printed as modelled: ")

    # The rails point up: the top of the lid's plate (the highest face as modelled) is on
    # the bed, so the transform sends the model's +Z to the plate's -Z.
    (plate,) = result["plates"]
    placed = {item["name"]: item for item in plate["items"]}
    transform = np.array(placed["lid"]["transform"]).reshape(3, 4)
    assert transform[:, :3] @ np.array([0.0, 0.0, 1.0]) == pytest.approx([0.0, 0.0, -1.0])
    assert placed["lid"]["bbox"]["min"][2] == 0.0 and placed["box"]["bbox"]["min"][2] == 0.0
    modelled = {p["name"]: p for p in built["parts"]}
    model_min, model_max = (np.array(modelled["lid"]["bbox"][k]) for k in ("min", "max"))
    corners = np.array([[x, y, z] for x in (model_min[0], model_max[0])
                        for y in (model_min[1], model_max[1])
                        for z in (model_min[2], model_max[2])])
    on_plate = corners @ transform[:, :3].T + transform[:, 3]
    assert on_plate.min(axis=0) == pytest.approx(placed["lid"]["bbox"]["min"], abs=1e-5)
    assert on_plate.max(axis=0) == pytest.approx(placed["lid"]["bbox"]["max"], abs=1e-5)

    # The checks of the plate judge the lid as it is printed.
    checks = {(c["id"], c["part"]): c for c in result["checks"]}
    assert checks[("overhangs", "lid")]["status"] == "pass"
    assert "turned upside down" in checks[("overhangs", "lid")]["message"]
    assert checks[("bed_contact", "lid")]["status"] == "pass"
    snap = checks[("fit_orientation", "lid")]
    assert snap["status"] == "warn" and snap["severity"] == "warning"
    assert "would print on support" not in snap["message"]
    assert "its snap rails stand upright" in snap["message"]
    assert checks[("fit_orientation", "box")]["status"] == "pass"
    assert checks[("plate_fit", "lid")]["status"] == "pass"
    assert checks[("plate_fit", "box")]["data"]["plates"] == [1]


def test_a_build_asked_to_judge_the_prepared_plate_stops_warning_about_the_lid(builds):
    asked = ok(builds["cable_box_asked"])
    lid = next(c for c in asked["checks"] if c["id"] == "overhangs" and c["part"] == "lid")
    assert lid["status"] == "pass" and lid["data"]["orientation"]["turned"] == "upside down"
    assert "turned upside down" in lid["message"]
    # The box really has overhangs (its cable slot) whichever way it lies: still said.
    box = next(c for c in asked["checks"] if c["id"] == "overhangs" and c["part"] == "box")
    assert box["status"] == "warn" and "as modelled" in box["message"]

    plain = ok(builds["lidded_box"])
    lid = next(c for c in plain["checks"] if c["id"] == "overhangs" and c["part"] == "lid")
    assert lid["status"] == "warn" and "orientation" not in lid["data"]
    turned = ok(builds["lidded_box_asked"])
    lid = next(c for c in turned["checks"] if c["id"] == "overhangs" and c["part"] == "lid")
    assert lid["status"] == "pass"


def test_the_plate_3mf_reimports_with_named_objects_and_equal_volumes(builds, plates):
    built = ok(builds["cable_box_asked"])
    job = plates["prepare_cable_box"]
    (plate,) = ok(job)["plates"]
    assert plate["threemf"] == "plates/1/plate.3mf"
    objects = read_3mf(job.dir / plate["threemf"])
    assert set(objects) == {"box", "lid"}
    volumes = {p["name"]: p["volume"] for p in built["parts"]}
    for item in plate["items"]:
        found = objects[item["name"]]
        # The tessellation of a curved solid is slightly smaller than the solid.
        assert found["volume"] == pytest.approx(volumes[item["part"]], rel=2e-3)
        assert found["min"] == pytest.approx(item["bbox"]["min"], abs=0.02)
        assert found["max"] == pytest.approx(item["bbox"]["max"], abs=0.02)

    # The same meshes as the build's own 3MF, only moved: equal volumes, object by object.
    as_built = read_3mf(builds["cable_box_asked"].dir / built["artifacts"]["threemf"])
    assert set(as_built) == set(objects)
    for name, found in objects.items():
        # (3MF stores single-precision coordinates, so moving a mesh rounds it anew.)
        assert found["volume"] == pytest.approx(as_built[name]["volume"], rel=1e-6)


def test_the_plate_views_show_the_bed_and_the_parts(plates):
    job = plates["prepare_cable_box"]
    (plate,) = ok(job)["plates"]
    assert plate["views"] == {"top": "plates/1/views/top.png"}
    image = Image.open(io.BytesIO((job.dir / plate["views"]["top"]).read_bytes()))
    assert image.size == (320, 240)
    pixels = np.asarray(image.convert("RGB")).reshape(-1, 3)
    assert len(np.unique(pixels, axis=0)) > 20  # not one flat colour
    assert job.files() >= {"plates/1/plate.3mf", "plates/1/views/top.png", "result.json"}


def test_twelve_lids_on_the_auxiliary_nozzle_split_into_plates_right_of_its_limit(plates):
    job = plates["prepare_twelve_lids"]
    result = ok(job)
    assert len(result["plates"]) > 1 and not result["unplaced"]
    assert any("do not fit on one plate" in w and f"{len(result['plates'])} plates" in w
               for w in result["warnings"])
    names = [item["name"] for plate in result["plates"] for item in plate["items"]]
    assert sorted(names) == sorted(f"lid.{n}" for n in range(1, 13))
    for plate in result["plates"]:
        for item in plate["items"]:
            assert item["nozzle_id"] == 0
            # 20.5 mm is the auxiliary nozzle's limit; the layout keeps 2 mm more.
            assert item["bbox"]["min"][0] >= 22.5 - 1e-9
            assert item["bbox"]["max"][0] <= 254.0 + 1e-9
            assert 2.0 - 1e-9 <= item["bbox"]["min"][1] and item["bbox"]["max"][1] <= 254.0 + 1e-9
        objects = read_3mf(job.dir / plate["threemf"])
        assert set(objects) == {item["name"] for item in plate["items"]}
        volumes = [found["volume"] for found in objects.values()]
        assert max(volumes) == pytest.approx(min(volumes), rel=1e-6)
    fit = next(c for c in result["checks"] if c["id"] == "plate_fit")
    assert fit["status"] == "pass" and fit["data"]["placed"] == 12
    assert fit["data"]["plates"] == [p["index"] for p in result["plates"]]
    assert "auxiliary nozzle" in fit["message"]
    assert orientation(result, "lid")["copies"] == 12


def test_the_models_print_hints_are_honoured_or_overridden_with_a_sentence(builds, plates):
    built = builds["print_hint"]
    notes = built.dir / "model.parts.json"
    assert '"print"' in notes.read_text(encoding="utf-8")
    result = ok(plates["prepare_hints"])
    post, dome, tray = (orientation(result, name) for name in ("post", "dome", "tray"))
    assert post["source"] == "preferred" and post["preferred_by"] == "the model"
    assert post["measures"]["height"] == pytest.approx(60.0)
    assert post["explanation"].endswith(", as the model asks.")
    assert dome["source"] == "as_modelled"
    assert dome["overridden"] == "leaves nothing flat on the bed"
    assert "the model asked for another orientation" in dome["explanation"]
    assert tray["source"] == "as_modelled" and tray["preferred_by"] is None
    assert result["plates"][0].get("threemf") is None and "views" not in result["plates"][0]

    # The user's choice in the request comes before the model's.
    chosen = orientation(ok(plates["prepare_user_choice"]), "post")
    assert chosen["preferred_by"] == "the request" and chosen["source"] == "preferred"
    assert chosen["measures"]["height"] == pytest.approx(20.0)


def test_a_part_larger_than_the_reach_is_unplaced_and_fails_the_plate(plates):
    job = plates["prepare_small_bed"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "check_error" and result["plates"] == []
    (left_out,) = result["unplaced"]
    # The entry of a request from before the print settings: these keys and no other.
    assert set(left_out) == {"name", "part", "copy", "reason"}
    # (A model that returns one unnamed shape has the part `part`.)
    assert left_out["part"] == "part" and "larger than the main nozzle's reach" in (
        left_out["reason"])
    fit = next(c for c in result["checks"] if c["id"] == "plate_fit")
    assert fit["status"] == "fail" and fit["severity"] == "error" and fit["part"] == "part"
    assert any("fit no plate" in w for w in result["warnings"])


def test_a_plank_longer_than_the_bed_is_built_prepared_and_plated_standing(builds, plates):
    # The whole way of the product: a build that judges the prepared plate, then prepare.
    build = ok(builds["plank_stands"])
    assert all(c["data"].get("could_not_run") is not True for c in build["checks"])
    fit = builds["plank_stands"].check("fits_build_volume", "plank")
    assert fit["status"] == "pass"
    assert fit["message"] == ("Fits the main nozzle's reach when printed turned onto its +X "
                              "side with 6.0 mm to spare in Z.")
    assert fit["data"]["orientation"]["turned"] == "onto its +X side"
    for check_id in ("overhangs", "bed_contact"):
        judged = builds["plank_stands"].check(check_id, "plank")
        assert judged["status"] == "pass", judged
        assert judged["data"]["orientation"]["turned"] == "onto its +X side"

    result = ok(plates["prepare_plank_stands"])
    chosen = orientation(result, "plank")
    assert chosen["turned"] == fit["data"]["orientation"]["turned"]
    assert chosen["explanation"] == fit["data"]["orientation"]["explanation"]
    assert "as modelled it does not fit the nozzle's reach" in chosen["explanation"]
    (plate,) = result["plates"]
    (item,) = plate["items"]
    assert item["bbox"]["max"][2] == pytest.approx(254.0)
    assert result["unplaced"] == []
    assert next(c for c in result["checks"] if c["id"] == "plate_fit")["status"] == "pass"
    (placed,) = read_3mf(plates["prepare_plank_stands"].dir / plate["threemf"]).values()
    assert placed["max"][2] - placed["min"][2] == pytest.approx(254.0, abs=1e-3)


def test_the_build_and_the_plate_agree_that_a_part_inside_the_bare_reach_does_not_fit(
        builds, plates):
    # 254 x 254 mm is inside the 256 mm reach but not inside the 2 mm kept at each edge.
    job = builds["plank_too_wide"]
    assert job.returncode == 0 and job.result["status"] == "check_error"
    fit = job.check("fits_build_volume", "plank")
    assert fit["status"] == "fail" and fit["data"]["margin"] == 2.0
    assert "too large for the main nozzle whichever way up it is printed (by 2.0 mm in X" in (
        fit["message"])
    result = plates["prepare_plank_too_wide"].result
    assert result["status"] == "check_error" and result["plates"] == []
    (left_out,) = result["unplaced"]
    assert "reach of 252.0 x 252.0 x 260.0 mm (with a 2 mm margin)" in left_out["reason"]


@pytest.mark.parametrize(("name", "fragment"), [
    ("prepare_unknown_part", "copies names unknown part(s) handle"),
    ("prepare_bad_orientation", "orientations is not valid: part.up"),
    ("prepare_without_printer", "context.printer"),
])
def test_bad_prepare_requests_are_refused(plates, name, fragment):
    job = plates[name]
    assert job.returncode == 2
    result = job.result
    assert result["error"]["type"] == "bad_request"
    assert fragment in result["error"]["message"]


@pytest.mark.skipif(landlock_abi() == 0, reason="needs Linux Landlock")
@pytest.mark.parametrize("name", ["prepare_cable_box", "gauge_of_model"])
def test_the_plate_ops_succeed_when_only_the_job_folder_is_writable(plates, name):
    """Plates, views and scratch files all stay in the job folder: the kernel denied the rest."""
    free, confined = plates[name], plates[f"{name}_confined"]
    result = ok(confined)
    assert result["warnings"] == free.result["warnings"]
    assert result["plates"] == free.result["plates"]
    assert confined.files() == free.files()
    assert ".engine-scratch" not in {p.name for p in confined.dir.iterdir()}
