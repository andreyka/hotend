"""The second acceptance scenario: a printable case for a phone.

The phone is the invented `sample_phone` component; a case for a real one needs a local
component file (hotend_fit/components). The model script is
tests/models/scenarios/sample-phone-case.
It is built through the worker in TPU on the Bambu Lab X2D, the way the product drives it,
at its defaults, at both ends of its sliders and with every "Check" value moved, and the
result is read the way a user would rely on it: the phone's room is proven, the rim's
stand-off and the back's thickness are measured, the openings are where the phone's
buttons and ports are, and it prints back down without support.

The second half measures the same things without the engine's checks (scenario_harness):
rays through the exact solid against the numbers of the phone's data file, read as plain
JSON, and the triangles of the plate file against the X2D's main nozzle."""

from __future__ import annotations

import copy
import json
import math
from pathlib import Path

import numpy as np
import pytest
from scenario_harness import (
    X2D_CONTEXT,
    Rays,
    checks,
    mesh_measures,
    ok,
    prepare_job,
    read_meshes,
    run_all,
    solids,
    step_is_exact_only,
)

import hotend_fit.components
from hotend_fit.components import load_component

MODEL = "scenarios/sample-phone-case/model.py"
TPU = {"id": "tpu-95a", "family": "TPU"}
CONTEXT = {**copy.deepcopy(X2D_CONTEXT), "material": TPU}
ERROR_CHECKS = ("valid_solid", "manifold", "single_solid_per_part", "fits_build_volume",
                "min_wall")
MEASURED = {"screen_rim", "back_thickness", "thinnest_wall", "stays_slim"}
ASKED = {"side_wall", "rim_reach", "left_buttons", "usb_c_position", "bottom_holes",
         "receiver_slot", "camera_window"}
SLIM = {"wall": 1.2, "back": 1.6, "lip_reach": 0.7}
THICK = {"wall": 1.8, "back": 2.4, "lip_height": 1.3}
# The phone's data as the file states it, not as the engine builds it.
RAW = json.loads((Path(hotend_fit.components.__file__).parent / "sample_phone.json")
                 .read_text(encoding="utf-8"))
MAIN_REACH = X2D_CONTEXT["printer"]["nozzles"][0]["reach"]
MOVED = {"button_height": 5.5, "usb_c_height": 4.2, "port_height": 4.5,
         "receiver_from_top": 2.4, "window_end": 39.0, "window_side": 2.5, "window_curl": 30.0}


def request(params: dict | None = None, **outputs) -> dict:
    return {
        "protocol": 0, "op": "build", "model_path": "model.py", "params": params or {},
        "context": copy.deepcopy(CONTEXT),
        "outputs": {"step": True, "brep": True, "mesh": True, "views": [], "stl": False,
                    "threemf": True, **outputs},
        "checks": {"enabled": True, "min_wall": 0.8, "verify_determinism": False,
                   "contract": True, "undeclared_joints": True, "orientation": "prepared"},
        "limits": {"wall_seconds": 480, "max_triangles": 2_000_000},
    }


@pytest.fixture(scope="module")
def jobs(tmp_path_factory):
    root = tmp_path_factory.mktemp("sample_phone_case")
    builds = {
        "default": request(views=["iso"], view_size=[320, 240]),
        "slim": request(SLIM),
        "thick": request(THICK),
        "moved": request(MOVED),
    }
    done = run_all({name: prepare_job(root / name, body, MODEL)
                    for name, body in builds.items()})
    after = {
        "plate": {"protocol": 0, "op": "prepare", "views": [], "threemf": True,
                  "context": copy.deepcopy(CONTEXT)},
        "gauge": {"protocol": 0, "op": "gauge", "views": [], "threemf": True, "nozzle_id": 1,
                  "context": copy.deepcopy(CONTEXT)},
        # The same plate with the case given to the Bowden-fed auxiliary nozzle.
        "plate_auxiliary": {"protocol": 0, "op": "prepare", "views": [], "threemf": False,
                            "nozzles": {"case": 0}, "context": copy.deepcopy(CONTEXT)},
    }
    done.update(run_all({name: prepare_job(root / name, body, copy_from=done["default"].dir)
                         for name, body in after.items()}))
    return done


@pytest.fixture(scope="module")
def phone():
    return load_component("sample_phone")


@pytest.fixture(scope="module")
def solid(jobs):
    from build123d import import_brep

    (case,) = import_brep(str(jobs["default"].dir / "model.brep")).solids()
    return case


@pytest.fixture(scope="module")
def rays(jobs):
    return Rays(solids(jobs["default"].dir)["case"])


def number(raw) -> float:
    return raw["value"] if isinstance(raw, dict) else raw


def stated(feature_id: str) -> dict:
    """A feature of the phone as its data file states it."""
    (found,) = [f for f in RAW["features"] if f["id"] == feature_id]
    return found


def centre(shape: dict) -> list[float]:
    return [number(value) for value in shape["center"]]


PHONE = [number(RAW["envelope"][key]) for key in ("width", "height", "depth")]


def requirement(result: dict, name: str) -> dict:
    (found,) = [c for c in result["checks"]
                if c["id"] == "contract" and c["data"].get("requirement") == name]
    return found


def pocket_check(result: dict) -> dict:
    (found,) = checks(result, "reference_fit", kind="pocket")
    return found


def sound(result: dict) -> None:
    """One valid piece, nothing fails, and what the contract can measure is met."""
    assert [part["name"] for part in result["parts"]] == ["case"]
    for check_id in ERROR_CHECKS:
        assert [c["status"] for c in result["checks"] if c["id"] == check_id] == ["pass"], check_id
    assert [c for c in result["checks"] if c["status"] == "fail"] == []
    for name in MEASURED:
        assert requirement(result, name)["status"] == "pass", name
    room = pocket_check(result)["data"]
    assert room["clearance_proven"] and room["intrusions"] == [] and room["problems"] == []
    (component,) = [c for c in result["checks"]
                    if c["id"] == "reference_fit" and c["data"]["kind"] == "component"]
    assert component["status"] == "pass"
    assert component["data"]["collisions"] == [] and component["data"]["keep_outs_not_handled"] == []


def test_the_case_holds_the_phone_in_the_room_that_follows_its_rounded_edges(jobs):
    result = ok(jobs["default"])
    sound(result)
    room = pocket_check(result)["data"]
    assert room["fit_class"] == "stretch" and room["reference"] == "sample_phone"
    assert room["room"] == "fitted to the stated edge section"
    assert room["target"] == pytest.approx(-0.05)
    # The rim reaches 1.0 mm over the phone's own edge and the band stretches over it.
    assert room["lip_depth"] == pytest.approx(1.0, abs=0.02)
    assert room["stretch_strain"] < room["stretch_limit"]
    assert room["grip_coverage"] > 0.7
    # Not proven on a printer nobody calibrated, and the phone's own tolerance is counted.
    assert pocket_check(result)["status"] == "warn"
    assert room["thing_tolerance"]["per_side"] == pytest.approx(0.20)
    assert room["thing_tolerance"]["ok"] is False
    part = result["parts"][0]
    size = [high - low for low, high in zip(part["bbox"]["min"], part["bbox"]["max"], strict=True)]
    assert size == pytest.approx([83.30, 133.45, 13.24], abs=0.01)
    assert part["bbox"]["min"][2] == pytest.approx(-2.0)


def test_the_rim_keeps_the_glass_off_the_table_with_the_phone_pushed_against_it(jobs):
    rim = requirement(ok(jobs["default"]), "screen_rim")["data"]
    # Face down the phone slides onto the rim's leaning underside; what is left counts.
    assert 0.15 < rim["travel"] < 0.30
    assert rim["standoff"] == pytest.approx(1.0, abs=0.005)
    assert rim["part_face"] - rim["thing_face"] == pytest.approx(
        rim["standoff"] + rim["travel"], abs=1e-3)
    taller = requirement(ok(jobs["thick"]), "screen_rim")["data"]
    assert taller["standoff"] == pytest.approx(1.3, abs=0.005)


def test_the_back_is_of_one_thickness_over_the_magnet_ring(jobs):
    for name, thickness in (("default", 2.0), ("slim", 1.6), ("thick", 2.4)):
        back = requirement(ok(jobs[name]), "back_thickness")["data"]
        assert back["open"] == 0 and back["rays"] > 3000, name
        assert back["standoff"] == pytest.approx(thickness, abs=1e-6), name
        assert back["standoff_max"] == pytest.approx(thickness, abs=1e-6), name
        assert back["travel"] == pytest.approx(0.0, abs=1e-6), name


def test_what_cannot_be_measured_is_asked_not_claimed(jobs):
    result = ok(jobs["default"])
    for name in ASKED:
        check = requirement(result, name)
        assert check["status"] == "warn" and check["data"]["met"] is None, name
        assert "is not checkable" in check["message"], name
        assert check["data"]["not_checkable"] is True and check["data"]["reason"], name
    asked = {question["requirement"] for question in result["questions"]}
    assert ASKED | {"phone_fit", "sample_phone_touch_strip", "sample_phone_mode_button",
                    "sample_phone_usb_c", "sample_phone_camera_plateau"} <= asked
    assert "is not protected" in result["stdout"]


def test_every_unverified_number_of_the_phone_carries_its_source_and_becomes_a_question(jobs):
    """Read against the data file, not against the engine's own list of questions: every
    number the case relies on that is not stated outright names the document it
    was read from and reaches the user as the question its feature carries."""
    result = ok(jobs["default"])
    (component,) = result["components"]
    assert component["id"] == "sample_phone" and component["proven"] is False
    assert component["size"] == pytest.approx(PHONE)
    assert all(source["url"].startswith("https://") for source in component["sources"].values())
    unverified = component["unverified"]
    assert len(unverified) == 29
    listed = {source["url"] for source in RAW["sources"].values()}
    texts = [question["text"] for question in result["questions"]]
    for datum in unverified:
        assert datum["source"] in listed, datum["path"]
        assert datum["confidence"] != "verified" or datum["provenance"] == "guessed", datum["path"]
        where, feature_id = datum["path"].split(".")[:2]
        assert where == "features", datum["path"]
        question = stated(feature_id)["question"]
        assert any(question in text for text in texts), feature_id
    # And the result does not call the model proven while they are open.
    (provenance,) = checks(result, "reference_fit", kind="provenance")
    assert provenance["status"] == "warn" and "29 number(s)" in provenance["message"]


def test_every_slider_end_and_every_moved_check_value_still_builds_sound(jobs):
    for name in ("slim", "thick", "moved"):
        sound(ok(jobs[name]))
    slim, thick = ok(jobs["slim"]), ok(jobs["thick"])
    assert pocket_check(slim)["data"]["lip_depth"] == pytest.approx(0.7, abs=0.02)
    assert requirement(thick, "stays_slim")["data"]["actual"][2] == pytest.approx(13.94, abs=0.01)
    stdout = ok(jobs["moved"])["stdout"]
    assert "check values: button_height 5.5 (component: 5.00)" in stdout
    assert "window_curl 30 (component: 32.00) differ from the component's data" in stdout
    assert "check values: all equal to the component's data" in ok(jobs["default"])["stdout"]


def test_the_openings_are_where_the_phones_buttons_ports_and_camera_are(solid, phone):
    half_w, half_h = phone.size[0] / 2, phone.size[1] / 2
    wall = half_w + 0.150 + 0.75                 # the middle of the side wall
    top = solid.bounding_box().max.Z

    def material(x: float, y: float, z: float) -> bool:
        return solid.is_inside((x, y, z))

    # The side wall is whole between the openings, from the back to the rim.
    assert material(wall, -4.0, 5.0) and material(-wall, 0.0, 5.0)
    assert material(wall, 50.0, top - 0.3) and material(-wall, -40.0, top - 0.3)
    # Buttons: free at the button and open from there to the rim, so they can be pressed.
    for name, side in (("mode_button", -1), ("volume_up", -1), ("volume_down", -1),
                       ("side_button", 1), ("touch_strip", 1)):
        _, y, z = phone.feature(name).center
        assert not material(side * wall, y, z), name
        assert not material(side * wall, y, top - 0.2), name
        assert not material(side * (half_w - 0.5), y, top - 0.2), name     # the rim above it
    # Touch strip: the opening flares from the phone's surface (40 degrees), so at the
    # wall's outside it is wider than the 22 mm at the phone.
    strip = phone.feature("touch_strip")
    _, y, z = strip.center
    outside = half_w + 0.150 + 1.4
    assert not material(outside, y + 11.0 + 1.2, z) and not material(outside, y - 11.0 - 1.2, z)
    assert material(outside, y + 11.0 + 3.0, z)
    # Bottom edge: the USB-C notch is open to the screen; the holes beside it are slots.
    end = half_h + 0.225 + 0.75
    usb = phone.feature("usb_c")
    assert not material(usb.center[0], -end, usb.center[2])
    assert not material(usb.center[0], -end, top - 0.2)
    for name in ("bottom_mic_1", "bottom_port_1", "bottom_mic_2", "bottom_port_2"):
        x, _, z = phone.feature(name).center
        assert not material(x, -end, z), name
        assert material(x, -end, top - 0.4), name          # the rim stays above the slots
    assert material(28.0, -end, 5.0) and material(0.0, end, 5.0)
    # The camera window is open over the lens, the flash and the plateau, and the back is
    # whole over the area marked just below the raised surround.
    for name in ("rear_camera", "flash", "rear_mic", "camera_plateau"):
        x, y, _ = phone.feature(name).center
        assert not material(x, y, -1.0), name
    area = phone.feature("back_area_corner")
    for sign in (1, -1):
        for dx in (-area.size[0] / 2, 0.0, area.size[0] / 2):
            for dy in (-area.size[1] / 2, 0.0, area.size[1] / 2 - 0.01):
                assert material(sign * (area.center[0] + dx) * 0.999, area.center[1] + dy, -1.0)
    # The receiver's keep-out at the top of the screen is free of the rim.
    receiver = phone.feature("receiver")
    assert not material(receiver.center[0], receiver.center[1], top - 0.3)


def test_it_prints_back_down_on_the_main_nozzle_without_support(jobs):
    built = ok(jobs["default"])
    (contact,) = [c for c in built["checks"] if c["id"] == "bed_contact"]
    (overhang,) = [c for c in built["checks"] if c["id"] == "overhangs"]
    assert contact["status"] == "pass"
    assert contact["data"]["contact_area"] == pytest.approx(8062.4, abs=2.0)
    assert contact["data"]["orientation"]["turned"] == "as modelled"
    # All that hangs over: the flat roofs of the two slots beside the USB-C notch.
    assert overhang["data"]["overhang_area"] < 25.0

    result = ok(jobs["plate"])
    (choice,) = result["orientations"]
    assert choice["turned"] == "as modelled" and choice["up"] == [0.0, 0.0, 1.0]
    assert choice["measures"]["contact"] > 8000 and choice["measures"]["overhang"] < 25.0
    assert len(result["plates"]) == 1 and result["unplaced"] == []
    (item,) = result["plates"][0]["items"]
    assert item["nozzle_id"] == 1
    assert {c["id"]: c["status"] for c in result["checks"]}["material_nozzle"] == "pass"
    plate = {name: mesh_measures(*mesh) for name, mesh in
             read_meshes(jobs["plate"].dir / "plates" / "1" / "plate.3mf").items()}
    assert set(plate) == {"case"}
    # The mesh that is printed encloses the volume the result states (a chamfer over the
    # outline's short edges once made the kernel's figure 1 % too large).
    assert plate["case"]["volume"] == pytest.approx(built["parts"][0]["volume"], rel=2e-4)
    assert built["parts"][0]["volume"] == pytest.approx(22_699.1, abs=5.0)
    assert plate["case"]["min"][2] == pytest.approx(0.0, abs=1e-6)
    assert set(read_meshes(jobs["default"].dir / "export" / "model.3mf")) == {"case"}
    step_is_exact_only(jobs["default"].dir / "model.step", {"case"})


def test_the_plate_is_valid_for_the_x2d_with_tpu_on_the_main_nozzle(jobs):
    """Measured on the triangles of the plate file, as a slicer gets it."""
    result = ok(jobs["plate"])
    (item,) = result["plates"][0]["items"]
    main = next(n for n in CONTEXT["printer"]["nozzles"] if n["role"] == "main")
    assert CONTEXT["material"]["family"] == "TPU" and item["nozzle_id"] == main["id"] == 1
    ((name, mesh),) = read_meshes(jobs["plate"].dir / "plates" / "1" / "plate.3mf").items()
    piece = mesh_measures(*mesh)
    assert name == "case"
    assert piece["min"][2] == pytest.approx(0.0, abs=1e-6)
    # Inside the main nozzle's reach, 2 mm clear of its edges on the bed.
    for axis, key in enumerate("xy"):
        assert MAIN_REACH[key][0] + 1.99 <= piece["min"][axis]
        assert piece["max"][axis] <= MAIN_REACH[key][1] - 1.99
    assert piece["max"][2] == pytest.approx(13.24, abs=0.01) and 13.24 < MAIN_REACH["z"][1]
    # A full first layer, and only the two slot roofs beside USB-C hang over (bridges).
    assert piece["contact"] == pytest.approx(8062.4, abs=5.0)
    assert piece["overhang"] < 25.0
    assert piece["volume"] == pytest.approx(ok(jobs["default"])["parts"][0]["volume"], rel=2e-4)
    # TPU is fed only by the direct-drive main nozzle: the same plate on the auxiliary
    # nozzle is refused, by an error check, with the reason.
    refused = jobs["plate_auxiliary"]
    assert refused.returncode == 0, refused.stderr
    assert refused.result["status"] == "check_error"
    (rule,) = [c for c in refused.result["checks"] if c["id"] == "material_nozzle"]
    assert rule["status"] == "fail" and rule["severity"] == "error"
    assert rule["data"]["material_family"] == "TPU" and rule["data"]["main_nozzle_id"] == 1


# ---- measured on the exact solid, against the phone's data file -------------------------


def test_the_cavity_is_the_phones_stated_size_plus_the_declared_gap(jobs, rays):
    width, height, depth = PHONE
    assert (width, height, depth) == (80.0, 130.0, 10.0)
    room = pocket_check(ok(jobs["default"]))["data"]
    mid = depth / 2
    sides = [rays.first((0, y, mid), (sign, 0, 0)) - width / 2
             for y in (-45.0, -5.0, 36.0) for sign in (1, -1)]
    ends = [rays.first((x, 0, mid), (0, sign, 0)) - height / 2
            for x in (-20.0, 20.0) for sign in (1, -1)]
    # The same gap all along each side, the one the fit resolver declared, and inside what
    # published practice for TPU cases allows as drawn (0.10 to 0.30 mm per side).
    assert max(sides) - min(sides) < 1e-3 and max(ends) - min(ends) < 1e-3
    assert sides[0] == pytest.approx(width / 2 * (room["scale"] - 1) + room["offset"], abs=1e-3)
    assert ends[0] == pytest.approx(height / 2 * (room["scale"] - 1) + room["offset"], abs=1e-3)
    assert ends[0] == pytest.approx(room["expected"], abs=1e-3)
    assert 0.10 <= sides[0] <= 0.30 and 0.10 <= ends[0] <= 0.30
    # The phone lies on a floor at its back; the back and the wall are what the
    # script says, and inside the phone's rules for the back and for docks.
    floor, outside = rays.hits((0, -30.0, 2.0), (0, 0, -1))
    assert floor == pytest.approx(2.0, abs=1e-6)
    assert outside - floor == pytest.approx(2.0, abs=1e-6)
    assert outside - floor <= number(RAW["rules"]["max_back_thickness"])
    inner, outer = rays.hits((0, 0, mid), (1, 0, 0))
    assert outer - inner == pytest.approx(1.5, abs=0.01)
    box = solids(jobs["default"].dir)["case"].bounding_box()
    assert -box.min.Y - height / 2 <= number(RAW["rules"]["dock_max_bottom"])
    # Over the magnet ring the back is one thickness, with no hole.
    magnets = stated("magnet_ring")["body"]
    cx, cy, _ = centre(magnets)
    radius = number(magnets["diameter"]) / 2
    thickness = []
    for x in np.linspace(-radius, radius, 41):
        for y in np.linspace(-radius, radius, 41):
            if math.hypot(x, y) <= radius:
                thickness.append(np.diff(rays.hits((cx + x, cy + y, 1.0), (0, 0, -1))).tolist())
    assert len(thickness) > 1200 and all(t == pytest.approx([2.0]) for t in thickness)


def test_the_rim_reaches_over_the_edge_not_the_glass_and_stands_off_the_table(jobs, rays):
    width, height, depth = PHONE
    mid = depth / 2
    top = solids(jobs["default"].dir)["case"].bounding_box().max.Z
    levels = np.arange(9.0, top, 0.01)
    side = min(rays.first((0, 0, z), (1, 0, 0)) for z in levels)
    end = min(rays.first((20.0, 0, z), (0, 1, 0)) for z in levels)
    reach_side, reach_end = width / 2 - side, height / 2 - end
    assert reach_side == pytest.approx(1.0, abs=0.02)
    assert reach_end == pytest.approx(0.925, abs=0.02)
    # Nothing may touch the cover glass, which starts 1.5 mm in from the edge, and the
    # receiver's keep-out starts 1.5 mm below the top edge.
    glass = RAW["envelope"]["cover_glass"]["value"]
    assert reach_side < (width - glass[0]) / 2 and reach_end < (height - glass[1]) / 2
    keep_out = stated("receiver")["keep_out"][0]
    receiver_starts = height / 2 - (centre(keep_out)[1] + keep_out["size"][1] / 2)
    assert reach_end < receiver_starts == pytest.approx(1.5, abs=1e-6)
    # Face down, the phone slides onto the rim's leaning underside until its rounded
    # shoulder (the edge section, straight between the stated points) touches. What is
    # left between the glass and the table must be the 0.7 mm of the phone's rule at least.
    inward = [point[0] for point in RAW["envelope"]["edge_profile"]["value"]]
    rise = [point[1] for point in RAW["envelope"]["edge_profile"]["value"]]

    def travel(start, across, half, wall) -> float:
        least = math.inf
        for t in np.arange(half - 1.4, wall - 0.001, 0.005):
            origin = [start[0] + across[0] * t, start[1] + across[1] * t, mid]
            underside = mid + rays.first(origin, (0, 0, 1))
            if half - t >= 0:
                least = min(least, underside - (mid + np.interp(half - t, inward, rise)))
        return least

    slide = min(travel((0, 0), (1, 0), width / 2, rays.first((0, 0, mid), (1, 0, 0))),
                travel((20.0, 0), (0, 1), height / 2, rays.first((20.0, 0, mid), (0, 1, 0))))
    standoff = top - (depth + slide)
    assert standoff >= number(RAW["rules"]["min_glass_standoff"]) == 0.7
    stated_by_engine = requirement(ok(jobs["default"]), "screen_rim")["data"]
    assert slide == pytest.approx(stated_by_engine["travel"], abs=0.01)
    assert standoff == pytest.approx(stated_by_engine["standoff"], abs=0.01)
    assert standoff == pytest.approx(1.0, abs=0.01)


def test_every_opening_is_as_large_as_the_phones_data_asks_measured_in_the_wall(rays):
    width, _, depth = PHONE
    side_wall = rays.first((0, 0, depth / 2), (1, 0, 0)) + 0.75       # the wall's middle
    end_wall = rays.first((20.0, 0, depth / 2), (0, -1, 0)) + 0.75
    play = 0.3                                    # at least this much air round a button
    free = {}
    for name in ("side_button", "mode_button", "volume_up", "volume_down"):
        body = stated(name)["body"]
        x, y, z = centre(body)
        _, length, tall = body["size"]
        sign = 1 if x > 0 else -1
        origin = (sign * side_wall, y, z)
        free[name] = (rays.first(origin, (0, 1, 0)), rays.first(origin, (0, -1, 0)))
        assert min(free[name]) >= length / 2 + play, name
        assert rays.first(origin, (0, 0, -1)) >= tall / 2 + play, name
        assert rays.first(origin, (0, 0, 1)) == math.inf, name            # open to the screen
        # Nothing stands in front of the button's face.
        assert rays.first((sign * (width / 2 + 0.01), y, z), (sign, 0, 0)) == math.inf, name
    for name in ("side_button", "mode_button"):                           # centred on it
        assert free[name][0] == pytest.approx(free[name][1], abs=0.01), name
    # The two volume buttons share one notch.
    between = centre(stated("volume_up")["body"])[1] - centre(stated("volume_down")["body"])[1]
    assert free["volume_up"][1] == pytest.approx(free["volume_down"][1] + between, abs=0.01)
    # Touch strip: the stated 22 mm opening at the phone's surface, flaring at 40 degrees
    # from there on, through the whole wall.
    strip = stated("touch_strip")["keep_out"][0]
    x, y, z = centre(strip)
    slope = math.tan(math.radians(number(strip["flare"])))
    for depth_in_wall in (0.05, 0.4, 0.75, 1.1, 1.45):
        at = side_wall - 0.75 + depth_in_wall
        origin = (at, y, z)
        opening = rays.first(origin, (0, 1, 0)) + rays.first(origin, (0, -1, 0))
        assert opening >= strip["size"][1] + 2 * (at - width / 2) * slope - 0.01
        assert rays.first(origin, (0, 0, 1)) == math.inf
    # USB-C: the rule's minimum is 12.5 x 9, recommended 13 x 10.6; open to the screen.
    usb = stated("usb_c")["keep_out"][0]
    x, _, z = centre(usb)
    origin = (x, -end_wall, z)
    left, right = rays.first(origin, (-1, 0, 0)), rays.first(origin, (1, 0, 0))
    least = number(RAW["rules"]["usb_c_opening_min"])
    assert left == pytest.approx(right, abs=1e-3)
    assert left + right >= usb["size"][0] - 0.005 >= least[0]
    assert rays.first(origin, (0, 0, -1)) >= usb["size"][2] / 2 - 0.005 >= least[1] / 2
    assert rays.first(origin, (0, 0, 1)) == math.inf
    # The four small holes: 1.5 mm of free wall around each 1.4 mm port.
    for name in ("bottom_mic_1", "bottom_port_1", "bottom_mic_2", "bottom_port_2"):
        port = stated(name)["keep_out"][0]
        x, _, z = centre(port)
        nearest = min(rays.first((x, -end_wall, z), (math.cos(turn), 0, math.sin(turn)))
                      for turn in np.linspace(0, 2 * math.pi, 73)[:-1])
        assert nearest >= number(port["diameter"]) / 2 - 0.005, name
        assert number(port["diameter"]) == pytest.approx(1.4 + 2 * 1.5)
    # The back is open over the whole lens (and a millimetre around it), and the rim is
    # clear of the receiver's keep-out from the glass upwards.
    lens = stated("rear_camera")["body"]
    cx, cy, _ = centre(lens)
    slant = (0.31, 0.52, 0.8)
    for r in np.linspace(0, number(lens["diameter"]) / 2 + 1.0, 10):
        for turn in np.linspace(0, 2 * math.pi, 25)[:-1]:
            assert not rays.filled((cx + r * math.cos(turn), cy + r * math.sin(turn), -1.0), slant)
    receiver = stated("receiver")["keep_out"][0]
    cx, cy, cz = centre(receiver)
    half_x, half_y = receiver["size"][0] / 2 - 0.01, receiver["size"][1] / 2 - 0.005
    for x in np.linspace(-half_x, half_x, 30):
        for y in (-half_y, 0.0, half_y):
            for z in np.linspace(cz + 0.01, cz + 1.5, 8):
                assert not rays.filled((cx + x, cy + y, z), slant)


@pytest.mark.xfail(strict=True, reason=(
    "The known failure of this scenario: the lens stands 2.5 mm proud of the case's back, "
    "against the phone's rule for exposed glass, and the build says so only in the script's "
    "stdout. It passes once the lens is protected or the result asks the user to decide."))
def test_the_rear_camera_glass_is_protected_or_the_user_is_asked(jobs, rays):
    lens = stated("rear_camera")["body"]
    x, y, _ = centre(lens)
    glass = -number(lens["length"])                          # 4.5 mm out of the back
    # The case's back beside the camera window, where the phone would lie on a table.
    back = 2.0 - rays.hits((x, y - 40.0, 2.0), (0, 0, -1))[-1]
    assert back == pytest.approx(-2.0, abs=1e-6) and glass == pytest.approx(-4.5)
    standoff = glass - back                                  # negative: the lens is proud
    asked = any("protect" in question["text"].lower() and "lens" in question["text"].lower()
                for question in ok(jobs["default"])["questions"])
    assert standoff >= number(RAW["rules"]["min_glass_standoff"]) or asked


def test_the_ring_gauge_for_the_stretch_fit_is_a_band_of_the_same_room(jobs):
    result = ok(jobs["gauge"])
    (gauge,) = result["gauges"]
    assert gauge["card"] == "pocket" and gauge["args"]["reference"] == "sample_phone"
    assert gauge["args"]["fit"] == "stretch" and gauge["parts"] == ["phone.ring"]
    room = pocket_check(ok(jobs["default"]))["data"]
    inside = {d["id"]: d["drawn"] for d in gauge["dimensions"]}
    assert inside["inside_a"] == pytest.approx(80.0 * room["scale"] + 2 * room["offset"], abs=1e-3)
    assert inside["inside_b"] == pytest.approx(130.0 * room["scale"] + 2 * room["offset"], abs=1e-3)
    assert set(read_meshes(jobs["gauge"].dir / "plates" / "1" / "plate.3mf")) == {"phone.ring"}
