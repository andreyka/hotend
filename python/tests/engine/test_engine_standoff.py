"""The contract's `standoff` requirement: how far a part stands beyond a real thing on one
side, with the thing pushed that way as far as the parts let it go. Measured on the
solids, never read off the drawing."""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import pytest
from build123d import Align, Box, Cylinder, Pos, Rot
from conftest import X2D_CONTEXT
from pydantic import ValidationError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reference_stand_in import box_mesh

from hotend_engine.checks import CheckEnv, run_checks
from hotend_engine.checks.contract import _first_hit, thing_travel
from hotend_engine.context import BuildContext
from hotend_engine.parts import NamedPart
from hotend_engine.request import CheckOptions
from hotend_engine.tessellate import tessellate
from hotend_fit import fit
from hotend_fit.components import use_component
from hotend_fit.contract import parse_contract
from hotend_fit.reference import Reference

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)
TPU = {"id": "tpu-95a", "family": "TPU"}


def brick_resolver(material: dict | None = None):
    """A resolver that knows a rigid 60 x 30 x 8 brick standing on z = 0."""
    resolver = fit.resolver_for({**X2D_CONTEXT, **({"material": material} if material else {})})
    vertices, triangles = box_mesh((60.0, 30.0, 8.0), (-30.0, -15.0, 0.0))
    return resolver, Reference("brick", vertices, triangles)


def frame(rim: float = 2.0, floor: float = 2.0, play: float = 0.5, lip: bool = True):
    """A frame around the brick: a floor under it, walls, and a lip over its top edge that
    leaves `play` mm above the brick and ends `rim` mm above the brick's top once the
    brick is pushed against it."""
    top = 8.0 + play + rim
    part = Pos(0, 0, -floor) * Box(66, 36, floor + top, align=BOTTOM)
    part -= Box(61, 31, 8.0 + play, align=BOTTOM)              # the room for the brick
    part -= Box(50 if lip else 61, 20 if lip else 31, top + 1, align=BOTTOM)   # the opening
    return part


def measure(part, requirements: list[dict], at=None, resolver_and_thing=None):
    resolver, thing = resolver_and_thing or brick_resolver()
    pocket = thing.pocket("cradle", ctx=resolver)
    if at is not None:
        part = at * part
    pocket.place(at=at if at is not None else Pos(0, 0, 0))
    parts = [NamedPart("frame", part, "#4c78a8")]
    contract = parse_contract({"protocol": 0, "name": "Test", "parts": ["frame"],
                               "requirements": requirements})
    env = CheckEnv(parts, BuildContext(None, None, resolver),
                   CheckOptions(contract=True, only=("contract",)), tessellate(parts), None,
                   contract=contract)
    return run_checks(env)


def standoff(side: str, value: float, **more) -> dict:
    return {"id": "off", "kind": "standoff", "part": "frame", "reference": "brick",
            "side": side, "value": value, "provenance": "standard", **more}


def test_the_stand_off_counts_from_where_the_thing_ends_up_not_where_it_was_drawn():
    (result,) = measure(frame(rim=2.0, play=0.5), [standoff("+z", 1.5)])
    assert result.status == "pass"
    # The frame's top is 2.5 mm above the brick as drawn; face down the brick slides the
    # 0.5 mm of play onto the lip, and 2.0 mm is what keeps its face off the table.
    assert result.data["travel"] == pytest.approx(0.5, abs=1e-6)
    assert result.data["standoff"] == pytest.approx(2.0, abs=1e-6)
    assert result.data["part_face"] == pytest.approx(10.5) and result.data["thing_face"] == 8.0
    assert "pushed 0.50 mm towards +Z, as far as the parts let it go" in result.message
    assert "stands 2.00 mm beyond 'brick'" in result.message

    # Asked for more than there is: a failure that says what was measured.
    (short,) = measure(frame(rim=2.0, play=0.5), [standoff("+z", 2.2)])
    assert short.status == "fail" and "at least 2.2 mm is asked" in short.message
    # The drawing's 2.5 mm does not count.
    (drawn,) = measure(frame(rim=2.0, play=0.5), [standoff("+z", 2.4)])
    assert drawn.status == "fail"
    # An upper limit as well.
    (thick,) = measure(frame(rim=2.0, play=0.5), [standoff("+z", 1.0, at_most=1.8)])
    assert thick.status == "fail" and "at most 1.8 mm is allowed" in thick.message
    (within,) = measure(frame(rim=2.0, play=0.5), [standoff("+z", 1.0, at_most=2.0)])
    assert within.status == "pass" and "at least 1 mm and at most 2 mm asked" in within.message


def test_a_thing_resting_on_the_part_does_not_travel():
    (result,) = measure(frame(floor=2.0), [standoff("-z", 2.0)])
    assert result.status == "pass"
    assert result.data["travel"] == pytest.approx(0.0, abs=1e-9)
    assert result.data["standoff"] == pytest.approx(2.0)
    assert "resting against the parts on its -Z side" in result.message
    # Sideways the walls are half a millimetre away on each side.
    (sideways,) = measure(frame(), [standoff("+x", 2.0)])
    assert sideways.data["travel"] == pytest.approx(0.5, abs=1e-6)
    assert sideways.data["standoff"] == pytest.approx(33.0 - 30.0 - 0.5, abs=1e-6)


def test_nothing_in_the_way_is_no_stand_off():
    (result,) = measure(frame(lip=False), [standoff("+z", 0.5)])
    assert result.status == "fail"
    assert result.data["travel"] is None
    assert "nothing holds 'brick' on its +Z side" in result.message


def test_the_measurement_follows_the_pocket_wherever_the_model_put_it():
    at = Pos(40, -25, 12) * Rot(0, 0, 30) * Rot(90, 0, 0)
    (result,) = measure(frame(rim=2.0, play=0.5), [standoff("+z", 1.5)], at=at)
    assert result.status == "pass"
    assert result.data["travel"] == pytest.approx(0.5, abs=1e-5)
    assert result.data["standoff"] == pytest.approx(2.0, abs=1e-5)


def test_a_region_must_be_covered_and_within_the_limits_everywhere():
    region = [[-20.0, -10.0], [20.0, 10.0]]
    (whole,) = measure(frame(floor=2.0), [standoff("-z", 1.6, at_most=2.1, region=region)])
    assert whole.status == "pass"
    assert whole.data["standoff"] == pytest.approx(2.0) and whole.data["open"] == 0
    assert whole.data["rays"] == 41 * 21
    assert "stands 2.00 mm beyond 'brick' everywhere" in whole.message

    # A hole through the floor inside the region.
    holed = frame(floor=2.0) - Pos(5, 3, -3) * Cylinder(2.0, 4.0, align=BOTTOM)
    (opened,) = measure(holed, [standoff("-z", 1.6, at_most=2.1, region=region)])
    assert opened.status == "fail" and opened.data["open"] > 5
    assert "so the region is not covered" in opened.message
    # Outside the region the same hole does not matter.
    far = frame(floor=2.0) - Pos(27, 0, -3) * Cylinder(2.0, 4.0, align=BOTTOM)
    (kept,) = measure(far, [standoff("-z", 1.6, at_most=2.1, region=region)])
    assert kept.status == "pass"

    # A dimple in the outer face makes it too thin there; a boss makes it too thick.
    dimpled = frame(floor=2.0) - Pos(-8, 2, -2.0) * Box(6, 6, 0.7, align=BOTTOM)
    (thin,) = measure(dimpled, [standoff("-z", 1.6, at_most=2.1, region=region)])
    assert thin.status == "fail"
    assert thin.data["standoff"] == pytest.approx(1.3) and thin.data["standoff_max"] == pytest.approx(2.0)
    assert "1.30 mm beyond 'brick' at its thinnest and 2.00 mm at its thickest" in thin.message
    bossed = frame(floor=2.0) + Pos(-8, 2, -2.5) * Box(6, 6, 0.5, align=BOTTOM)
    (thick,) = measure(bossed, [standoff("-z", 1.6, at_most=2.1, region=region)])
    assert thick.status == "fail" and thick.data["standoff_max"] == pytest.approx(2.5)
    assert "at most 2.1 mm is allowed" in thick.message
    # Without the region only the outermost face counts, and the boss is it.
    (outermost,) = measure(bossed, [standoff("-z", 1.6)])
    assert outermost.data["standoff"] == pytest.approx(2.5)


def test_a_feature_that_stands_proud_is_what_the_stand_off_counts_from():
    resolver = fit.resolver_for({**X2D_CONTEXT, "material": TPU})
    phone = use_component(resolver, "sample_phone")
    pocket = phone.reference.pocket("stretch", ctx=resolver)
    lens = phone.feature("rear_camera")
    # A slab under the phone, thicker than the lens stands proud, with a hole for it.
    slab = Pos(0, 0, -7.0) * Box(90, 170, 7.0, align=BOTTOM)
    slab -= lens.keep_out(reach=8.0)
    pocket.place(at=Pos(0, 0, 0))
    parts = [NamedPart("frame", slab, "#4c78a8")]

    def run(requirement: dict):
        contract = parse_contract({"protocol": 0, "name": "Test", "parts": ["frame"],
                                   "requirements": [requirement]})
        env = CheckEnv(parts, BuildContext(None, None, resolver),
                       CheckOptions(contract=True, only=("contract",)), tessellate(parts),
                       None, contract=contract)
        (result,) = run_checks(env)
        return result

    base = {"id": "off", "kind": "standoff", "part": "frame", "reference": "sample_phone",
            "side": "-z", "value": 0.7, "provenance": "standard"}
    body = run(base)
    assert body.status == "pass" and body.data["standoff"] == pytest.approx(7.0)
    guarded = run({**base, "feature": "rear_camera"})
    assert guarded.status == "pass"
    assert guarded.data["standoff"] == pytest.approx(7.0 - lens.size[2], abs=1e-6)
    assert f"beyond the {lens.label} of" in guarded.message
    # A feature the component does not have, or one with no proud shape.
    missing = run({**base, "feature": "headphone_jack"})
    assert missing.status == "fail" and "has no feature 'headphone_jack'" in missing.message


def test_a_reference_the_model_never_placed_cannot_be_measured():
    resolver, thing = brick_resolver()
    thing.pocket("cradle", ctx=resolver)        # made, never placed
    parts = [NamedPart("frame", frame(), "#4c78a8")]
    contract = parse_contract({"protocol": 0, "name": "Test", "parts": ["frame"],
                               "requirements": [standoff("+z", 1.0)]})
    env = CheckEnv(parts, BuildContext(None, None, resolver),
                   CheckOptions(contract=True, only=("contract",)), tessellate(parts), None,
                   contract=contract)
    (result,) = run_checks(env)
    assert result.status == "fail" and "placed no pocket for 'brick'" in result.message


@pytest.mark.parametrize(("change", "fragment"), [
    ({"side": "up"}, "side"),
    ({"value": -1.0}, "value"),
    ({"at_most": 0.5}, "at_most is smaller than value"),
    ({"region": [[0.0, 0.0], [0.5, 10.0]]}, "at least 1 mm apart"),
    ({"region": [[10.0, 0.0], [0.0, 10.0]]}, "the lower first"),
    ({"thickness": 2.0}, "Extra inputs are not permitted"),
])
def test_a_stand_off_that_says_nothing_measurable_is_refused(change, fragment):
    with pytest.raises(ValidationError, match=fragment):
        parse_contract({"protocol": 0, "name": "Test", "parts": ["frame"],
                        "requirements": [{**standoff("+z", 1.0), **change}]})


def test_a_ray_is_stopped_where_it_enters_material_not_where_it_leaves_a_surface():
    import manifold3d

    cube = manifold3d.Manifold.cube((10.0, 10.0, 10.0))
    up = np.array([0.0, 0.0, 1.0])
    # From below: stopped at the bottom face. From inside: at once.
    assert _first_hit(cube, np.array([5.0, 5.0, -3.0]), up, 50.0) == pytest.approx(3.0)
    assert _first_hit(cube, np.array([5.0, 5.0, 4.0]), up, 50.0) == 0.0
    # From on the top face, leaving it: nothing ahead.
    assert _first_hit(cube, np.array([5.0, 5.0, 10.0]), up, 50.0) is None
    assert _first_hit(cube, np.array([5.0, 5.0, 12.0]), up, 50.0) is None

    # A thing above the cube falls 2 mm onto it; a thing beside it falls past.
    thing = manifold3d.Manifold.cube((4.0, 4.0, 4.0)).translate((3.0, 3.0, 12.0))
    corners = np.array([[x, y, z] for x in (3.0, 7.0) for y in (3.0, 7.0) for z in (12.0, 16.0)])
    cube_corners = np.array([[x, y, z] for x in (0.0, 10.0) for y in (0.0, 10.0)
                             for z in (0.0, 10.0)])
    assert thing_travel(thing, corners, [cube], cube_corners, -up) == pytest.approx(2.0)
    aside = corners + np.array([20.0, 0.0, 0.0])
    assert thing_travel(thing.translate((20.0, 0.0, 0.0)), aside, [cube], cube_corners, -up) is None
    # A part's corner under the middle of a wide thing stops it, though no corner of the
    # thing is above the part.
    wide = manifold3d.Manifold.cube((30.0, 30.0, 4.0)).translate((-10.0, -10.0, 13.0))
    wide_corners = np.array([[x, y, z] for x in (-10.0, 20.0) for y in (-10.0, 20.0)
                             for z in (13.0, 17.0)])
    assert thing_travel(wide, wide_corners, [cube], cube_corners, -up) == pytest.approx(3.0)


def test_a_solid_whose_flat_faces_the_kernel_computes_wrongly_is_not_passed_as_exact():
    """A chamfer over the many short edges of a reference's outline leaves the end face
    with a stale outline: the kernel still calls the solid valid, but its area, and with
    it the volume, the mass and the bed contact, come out wrong."""
    from build123d import Axis, Plane, chamfer

    resolver = fit.resolver_for({**X2D_CONTEXT, "material": TPU})
    pocket = use_component(resolver, "sample_phone").reference.pocket("stretch", ctx=resolver)
    outer = pocket.offset + 1.5
    prism = pocket.taper(-2.0, 6.0, 0.0, offset=outer)
    chamfered = chamfer(prism.edges().group_by(Axis.Z)[0], 0.5)
    # The same bevel from flat pieces over the same corners.
    flat = pocket.taper(-1.5, 6.0, 0.0, offset=outer) + pocket.taper(
        -2.0, -1.5, 0.5, offset=outer).mirror(Plane.XY.offset(-1.75))

    def check(shape):
        parts = [NamedPart("slab", shape, "#4c78a8")]
        env = CheckEnv(parts, BuildContext(None, None, None),
                       CheckOptions(only=("valid_solid",)), tessellate(parts), None)
        (result,) = run_checks(env)
        return result

    stale = check(chamfered)
    assert stale.status == "warn" and stale.data["is_valid"] is True
    assert stale.data["inconsistent_face_count"] >= 1
    worst = stale.data["inconsistent_faces"][0]
    assert worst["kernel_area"] - worst["drawn_area"] > 50.0
    # The mesh, which is what gets exported and printed, has the true volume: the prism
    # less the bevel's ring.
    assert stale.data["mesh_volume"] < stale.data["volume"] - 50.0
    assert stale.data["mesh_volume"] == pytest.approx(float(flat.volume), rel=2e-4)
    assert "flat face(s) wrongly" in stale.message and "build that bevel" in stale.message
    # The same bevel built from flat pieces, and ordinary solids with round holes and
    # fillets, are exact.
    for shape in (prism, flat,
                  Box(40, 30, 10) - Cylinder(8, 20),
                  Pos(0, 0, 0) * Box(40, 30, 10, align=BOTTOM) - Pos(5, 5, 0) * Cylinder(1.2, 30)):
        passed = check(shape)
        assert passed.status == "pass", passed.message
        assert "inconsistent_faces" not in passed.data
