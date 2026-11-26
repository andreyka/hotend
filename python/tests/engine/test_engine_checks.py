"""Unit tests of the checks on small solids built in-process.

Every check must be honest: `pass` only when the property was established, `skipped` with the
reason when it could not be.
"""

from __future__ import annotations

import math

import pytest
from build123d import (
    Align,
    Axis,
    Box,
    BuildPart,
    BuildSketch,
    Compound,
    Cone,
    Cylinder,
    Plane,
    Polygon,
    Pos,
    Shell,
    Solid,
    Sphere,
    chamfer,
    extrude,
    fillet,
)
from conftest import X2D_CONTEXT
from OCP.TopoDS import TopoDS

from hotend_engine import checks, prepare
from hotend_engine.checks import CHECK_IDS, CheckEnv, has_blocking_failure, run_checks
from hotend_engine.checks.build_volume import fits_build_volume
from hotend_engine.checks.fit_pairs import fit_pairs
from hotend_engine.checks.intersect import parts_do_not_intersect
from hotend_engine.checks.min_wall import min_wall
from hotend_engine.checks.orientation import bed_contact, overhangs
from hotend_engine.checks.small_features import small_features
from hotend_engine.checks.solid import manifold, single_solid_per_part, valid_solid
from hotend_engine.context import BuildContext, MissingFit
from hotend_engine.parts import NamedPart
from hotend_engine.request import CheckOptions, Context
from hotend_engine.tessellate import tessellate

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)
MAIN_NOZZLE_ID, AUXILIARY_NOZZLE_ID = 1, 0


def make_env(shapes: dict, *, context: dict | None = None, mesh: bool = True, fit=None,
             **options) -> CheckEnv:
    parts = [NamedPart(name, shape, "#4c78a8") for name, shape in shapes.items()]
    parsed = Context.model_validate(X2D_CONTEXT if context is None else context)
    ctx = BuildContext(parsed.printer, parsed.material, fit or MissingFit())
    tessellation = tessellate(parts) if mesh else None
    return CheckEnv(parts, ctx, CheckOptions(**options), tessellation,
                    None if mesh else "tessellation was switched off for this test")


def one(check, shape, **kwargs):
    env = make_env({"p": shape}, **kwargs)
    result = check(env, env.parts[0])
    assert result.part == "p"
    assert result.message
    return result


def open_box():
    """A box with one face missing, forced into a Solid: invalid and not watertight."""
    return Solid(Shell(Box(10, 10, 10).faces()[:5]))


# --- valid_solid ---------------------------------------------------------------------


def test_valid_solid_passes_a_box():
    result = one(valid_solid, Box(10, 20, 30))
    assert result.status == "pass" and result.severity == "error"
    assert result.data["volume"] == pytest.approx(6000)


def test_valid_solid_fails_a_defective_shape():
    result = one(valid_solid, open_box(), mesh=False)
    assert result.status == "fail"
    assert result.data["is_valid"] is False


def test_valid_solid_fails_an_inside_out_solid():
    inside_out = Solid(TopoDS.Solid(Box(10, 10, 10).solids()[0].wrapped.Reversed()))
    result = one(valid_solid, inside_out, mesh=False)
    assert result.status == "fail"
    assert result.data["is_valid"] is True and result.data["volume"] < 0
    assert "no volume" in result.message


# --- manifold ------------------------------------------------------------------------


@pytest.mark.parametrize("shape", [Box(10, 20, 30), Sphere(8), Cone(6, 0, 12), Cylinder(4, 9)],
                         ids=["box", "sphere", "cone", "cylinder"])
def test_manifold_passes_closed_solids(shape):
    # Spheres and cones have degenerate edges at their poles and tip; they are still closed.
    result = one(manifold, shape)
    assert result.status == "pass", result.message
    assert result.data == {"open_edges": 0, "non_manifold_edges": 0, "mesh_closed": True}


def test_manifold_fails_an_open_surface():
    result = one(manifold, open_box(), mesh=False)
    assert result.status == "fail" and result.severity == "error"
    assert result.data["open_edges"] == 4
    assert "4 edge(s) where the surface is open" in result.message


def test_manifold_without_a_mesh_says_what_was_not_examined():
    result = one(manifold, Box(5, 5, 5), mesh=False)
    assert result.status == "pass"
    assert result.data["mesh_closed"] is None
    assert "was not examined" in result.message


# --- single_solid_per_part -------------------------------------------------------------


def test_single_solid_per_part():
    assert one(single_solid_per_part, Box(5, 5, 5)).status == "pass"
    two = Compound([Box(5, 5, 5), Pos(20, 0, 0) * Box(5, 5, 5)])
    result = one(single_solid_per_part, two)
    assert result.status == "fail" and result.data == {"solids": 2}
    assert "2 separate pieces" in result.message


# --- fits_build_volume -----------------------------------------------------------------


def test_build_volume_pass_reports_the_tightest_axis():
    result = one(fits_build_volume, Box(100, 250, 30))
    assert result.status == "pass"
    assert result.message == "Fits the main nozzle's reach with 6.0 mm to spare in Y."
    assert result.data["size"] == [100.0, 250.0, 30.0]


def test_build_volume_allows_a_quarter_turn():
    # 250 mm along X exceeds the auxiliary nozzle's 235.5 mm X travel but fits along its Y.
    result = one(fits_build_volume, Box(250, 20, 20))
    assert result.status == "pass"
    aux = next(n for n in result.data["nozzles"] if n["role"] == "auxiliary")
    assert aux["fits"] and aux["needs_quarter_turn"]
    main = next(n for n in result.data["nozzles"] if n["role"] == "main")
    assert main["fits"] and not main["needs_quarter_turn"]


def test_build_volume_warns_when_only_some_nozzles_reach():
    result = one(fits_build_volume, Box(250, 250, 20))
    assert result.status == "warn"
    assert "Fits the main nozzle's reach" in result.message
    assert "too large for the auxiliary nozzle by 14.5 mm in X" in result.message


def test_build_volume_fails_when_no_nozzle_reaches():
    result = one(fits_build_volume, Box(20, 20, 270))
    assert result.status == "fail" and result.severity == "error"
    assert "by 10.0 mm in Z" in result.message and "by 14.0 mm in Z" in result.message


def test_build_volume_reach_comes_from_the_request():
    small = {"printer": {"id": "mini", "nozzles": [
        {"id": 0, "diameter": 0.4, "reach": {"x": [0, 100], "y": [0, 100], "z": [0, 100]}}]}}
    result = one(fits_build_volume, Box(120, 20, 20), context=small)
    assert result.status == "fail"
    assert "too large for the nozzle 0 by 20.0 mm in X (reach 100 x 100 x 100 mm)" in result.message
    assert one(fits_build_volume, Box(90, 20, 20), context=small).status == "pass"


@pytest.mark.parametrize("context", [{}, {"printer": {"id": "p"}},
                                     {"printer": {"id": "p", "nozzles": [{"id": 0, "diameter": 0.4}]}}])
def test_build_volume_is_skipped_without_a_reach(context):
    result = one(fits_build_volume, Box(500, 500, 500), context=context)
    assert result.status == "skipped"
    assert "no printer nozzle reach" in result.message


def prepared_volume(shape, **kwargs):
    return one(fits_build_volume, shape, orientation="prepared", **kwargs)


def test_build_volume_of_the_prepared_plate_keeps_the_margin_of_the_plate():
    # 250 mm fits the bare 256 mm with 6 to spare, the plate (2 mm free at each edge) with 2.
    result = prepared_volume(Box(100, 250, 30))
    assert result.status == "pass" and result.data["margin"] == 2.0
    assert result.message == ("Fits the main nozzle's reach with 2.0 mm to spare in Y beyond "
                              "the 2 mm kept free at each edge of the bed.")
    # 254 x 254 mm is inside the bare reach and on no plate, whichever way up.
    result = prepared_volume(Box(254, 254, 5))
    assert result.status == "fail" and result.severity == "error"
    assert "too large for the main nozzle whichever way up it is printed (by 2.0 mm in X as " \
           "modelled; reach 256 x 256 x 260 mm less the 2 mm kept free at each edge" in (
               result.message)
    assert not any(n["fits"] for n in result.data["nozzles"])
    # Judged as modelled, the same part is compared with the bare reach, as it always was.
    assert one(fits_build_volume, Box(254, 254, 5)).status == "warn"


def test_build_volume_of_the_prepared_plate_passes_a_part_that_fits_another_way_up():
    # 258 mm is longer than the bed but stands inside the main nozzle's 260 mm of height;
    # the auxiliary nozzle has 256 mm of height, so it cannot print it at all.
    result = prepared_volume(Box(258, 20, 20, align=BOTTOM))
    assert result.status == "warn"
    assert result.message.startswith("Fits the main nozzle's reach when printed turned onto "
                                     "its +X side with 2.0 mm to spare in Z. It is too large "
                                     "for the auxiliary nozzle whichever way up")
    main = next(n for n in result.data["nozzles"] if n["role"] == "main")
    assert main["fits"] and main["turned"] == "onto its +X side" and main["source"] == "face"
    assert main["size"][2] == pytest.approx(258.0)
    assert result.data["orientation"]["turned"] == "onto its +X side"
    assert "as modelled it does not fit" in result.data["orientation"]["explanation"]
    # Judged as modelled it is simply too long.
    assert one(fits_build_volume, Box(258, 20, 20, align=BOTTOM)).status == "fail"


@pytest.mark.parametrize("size", [
    (100, 250, 30), (252, 252, 5), (252.1, 100, 5), (254, 254, 5), (254, 20, 5), (258, 20, 20),
    (261, 20, 20), (240, 255, 10), (250, 20, 20), (253, 253, 253), (30, 30, 259),
], ids=str)
@pytest.mark.parametrize("nozzle", [MAIN_NOZZLE_ID, AUXILIARY_NOZZLE_ID], ids=["main", "aux"])
def test_build_volume_of_the_prepared_plate_says_what_the_plating_does(size, nozzle):
    """The check passes a nozzle exactly when preparation, given the part for that nozzle
    with its default margin, finds it a place on an empty plate."""
    env = make_env({"p": Box(*size, align=BOTTOM)}, orientation="prepared")
    part = env.parts[0]
    row = next(n for n in fits_build_volume(env, part).data["nozzles"]
               if n["nozzle_id"] == nozzle)
    # What the `prepare` op does with the part, step by step.
    printer_nozzle = next(n for n in env.ctx.printer.nozzles if n.id == nozzle)
    mesh = env.mesh(part)
    choice = prepare.choose_orientation(
        prepare.PartGeometry(part, mesh), prepare.DEFAULT_MAX_OVERHANG_DEG,
        reach_size=printer_nozzle.reach.size, margin=prepare.DEFAULT_MARGIN_MM)
    moved, moved_mesh, translation = prepare.turned_part(part, mesh, choice.rotation)
    item = prepare.Item(
        name="p", part="p", copy=1, shape=moved.shape, mesh=moved_mesh, color=part.color,
        rotation=choice.rotation, translation=translation,
        size=tuple(float(v) for v in tuple(moved.shape.bounding_box().size)),
        nozzle=printer_nozzle)
    placed, unplaced = prepare.lay_out([item], prepare.DEFAULT_GAP_MM,
                                       prepare.DEFAULT_MARGIN_MM)
    assert row["fits"] == (len(placed) == 1) == (not unplaced), (row, unplaced)
    assert row["turned"] == prepare.describe_up(choice.up) or not row["fits"]


# --- min_wall ----------------------------------------------------------------------------


def cup(wall: float):
    return Box(40, 30, 20, align=BOTTOM) - Pos(0, 0, 2) * Box(
        40 - 2 * wall, 30 - 2 * wall, 20, align=BOTTOM)


def test_min_wall_measures_the_thinnest_wall():
    result = one(min_wall, cup(1.5))
    assert result.status == "pass" and result.severity == "error"
    assert result.data["thinnest"] == pytest.approx(1.5, abs=0.01)
    assert result.data["thin_area"] == 0
    assert result.data["measured"] > 100


def test_min_wall_fails_a_thin_wall_and_says_where():
    result = one(min_wall, cup(0.4))
    assert result.status == "fail"
    assert result.data["thinnest"] == pytest.approx(0.4, abs=0.01)
    assert result.data["thin_area"] > 1000
    x, y, _z = result.data["thinnest_at"]
    assert abs(abs(x) - 20) < 0.5 or abs(abs(y) - 15) < 0.5
    assert "0.40 mm" in result.message


@pytest.mark.parametrize("skin", [0.00002, 0.0005, 0.0009, 0.002, 0.05])
def test_min_wall_sees_a_skin_however_thin(skin):
    """A cut that was meant to go through and stops short leaves a skin the slicer drops.
    The ray must not start behind it and measure the next wall (1 mm here) instead."""
    hollow = Box(20, 20, 20, align=BOTTOM) - Pos(0, 0, skin) * Box(18, 18, 18, align=BOTTOM)
    result = one(min_wall, hollow)
    assert result.status == "fail", result.message
    assert result.data["thinnest"] == pytest.approx(skin, abs=2e-5)
    assert result.data["thin_area"] > 300  # the 18 x 18 mm floor, from both sides
    # The same cavity over a 1 mm floor passes and still measures every sample.
    sound = one(min_wall, Box(20, 20, 20, align=BOTTOM)
                - Pos(0, 0, 1.0) * Box(18, 18, 18, align=BOTTOM))
    assert sound.status == "pass" and sound.data["thinnest"] == pytest.approx(1.0, abs=1e-3)
    assert sound.data["measured"] == sound.data["samples"] == result.data["samples"]


def test_min_wall_limit_comes_from_the_request():
    assert one(min_wall, cup(1.5), min_wall=2.0).status == "fail"
    assert one(min_wall, cup(1.5), min_wall=1.5).status == "pass"


def test_min_wall_finds_a_thin_floor_under_a_pocket():
    block = Box(30, 30, 10, align=BOTTOM) - Pos(0, 0, 0.5) * Cylinder(5, 10, align=BOTTOM)
    result = one(min_wall, block)
    assert result.status == "fail"
    assert result.data["thinnest"] == pytest.approx(0.5, abs=0.01)
    assert result.data["thinnest_at"][2] in (pytest.approx(0.0, abs=0.01), pytest.approx(0.5, abs=0.01))


def test_min_wall_finds_the_web_between_a_hole_and_the_outside():
    block = Box(30, 30, 10, align=BOTTOM) - Pos(14.5 - 3, 0, 0) * Cylinder(3, 10, align=BOTTOM)
    result = one(min_wall, block)
    assert result.status == "fail"
    assert result.data["thinnest"] == pytest.approx(0.5, abs=0.05)


def test_min_wall_does_not_mistake_chamfers_or_wedges_for_walls():
    chamfered = chamfer(Box(30, 30, 10).edges(), 2)
    assert one(min_wall, chamfered).status == "pass"
    with BuildPart() as wedge:
        with BuildSketch(Plane.XZ):
            Polygon((0, 0), (40, 0), (0, 30), align=None)
        extrude(amount=20)
    result = one(min_wall, wedge.part)
    assert result.status == "pass", result.message
    assert result.data["thinnest"] == pytest.approx(20, abs=0.01)


def test_min_wall_on_a_solid_block_and_a_sphere():
    assert one(min_wall, Box(20, 30, 40)).data["thinnest"] == pytest.approx(20, abs=0.01)
    ball = one(min_wall, Sphere(10))
    assert ball.status == "pass" and ball.data["thinnest"] == pytest.approx(20, abs=0.2)


def test_min_wall_is_skipped_when_it_cannot_sample():
    result = one(min_wall, cup(0.4), mesh=False)
    assert result.status == "skipped"
    assert "tessellation was switched off" in result.message


def test_min_wall_is_skipped_for_an_open_mesh():
    result = one(min_wall, open_box())
    assert result.status == "skipped"
    assert "not a closed manifold" in result.message


# --- overhangs and bed_contact -----------------------------------------------------------


def mushroom():
    return Cylinder(3, 10, align=BOTTOM) + Pos(0, 0, 10) * Box(30, 20, 4, align=BOTTOM)


def test_overhangs_pass_for_a_box_on_the_bed():
    result = one(overhangs, Box(20, 20, 20, align=BOTTOM))
    assert result.status == "pass" and result.severity == "warning"
    assert result.data["overhang_area"] == 0
    assert result.data["limit_deg"] == 45 and result.data["limit_source"] == "default"


def test_overhangs_measure_a_flat_ceiling_exactly():
    result = one(overhangs, mushroom())
    assert result.status == "warn" and result.severity == "warning"
    assert result.data["overhang_area"] == pytest.approx(30 * 20 - math.pi * 9, abs=1e-3)
    assert len(result.data["faces"]) == 1
    assert "need support" in result.message


def test_overhangs_ignore_the_face_on_the_bed_wherever_the_part_sits():
    assert one(overhangs, Pos(5, 5, 37) * Box(20, 20, 20)).status == "pass"


def test_overhang_limit_comes_from_the_material():
    # A funnel-like solid whose side leans out 60 degrees from vertical.
    leaning = Cone(5, 5 + 10 * math.tan(math.radians(60)), 10, align=BOTTOM)
    default = one(overhangs, leaning)
    assert default.status == "warn"
    tolerant = {**X2D_CONTEXT, "material": {"id": "x", "family": "PLA", "max_overhang_deg": 70}}
    result = one(overhangs, leaning, context=tolerant)
    assert result.status == "pass"
    assert result.data["limit_deg"] == 70 and result.data["limit_source"] == "material"


def test_overhangs_measure_curved_faces_on_the_mesh():
    result = one(overhangs, Sphere(10))
    assert result.status == "warn"
    # the cap of the sphere below 45 degrees: 2 pi r^2 (1 - cos 45)
    assert result.data["overhang_area"] == pytest.approx(
        2 * math.pi * 100 * (1 - math.cos(math.radians(45))), rel=0.05)


def test_overhangs_are_skipped_when_curved_faces_cannot_be_measured():
    result = one(overhangs, Sphere(10), mesh=False)
    assert result.status == "skipped"
    assert "1 curved face(s) could not be measured" in result.message
    # a part with only flat faces needs no mesh
    table = Box(5, 5, 10, align=BOTTOM) + Pos(0, 0, 10) * Box(30, 20, 4, align=BOTTOM)
    flat = one(overhangs, table, mesh=False)
    assert flat.status == "warn"
    assert flat.data["overhang_area"] == pytest.approx(30 * 20 - 25, abs=1e-6)


def test_bed_contact():
    flat = one(bed_contact, Pos(0, 0, 12) * Box(20, 30, 5), mesh=False)
    assert flat.status == "pass" and flat.severity == "warning"
    assert flat.data["contact_area"] == pytest.approx(600)
    assert flat.data["contact_ratio"] == pytest.approx(1.0)
    ball = one(bed_contact, Sphere(10), mesh=False)
    assert ball.status == "warn" and ball.data["contact_area"] == 0
    assert "No flat face rests on the bed" in ball.message
    pin = one(bed_contact, Box(3, 3, 40), mesh=False)
    assert pin.status == "warn" and pin.data["contact_area"] == pytest.approx(9)
    upside_down = one(bed_contact, mushroom().mirror(Plane.XY), mesh=False)
    assert upside_down.data["contact_area"] == pytest.approx(600)


# --- small_features ------------------------------------------------------------------------


def test_small_features_pass_for_ordinary_geometry():
    plate = Box(30, 30, 5) - Cylinder(2.5, 5)
    result = one(small_features, fillet(plate.edges().filter_by(Axis.Z), 2), mesh=False)
    assert result.status == "pass" and result.severity == "warning"
    assert result.data["nozzle_diameter"] == 0.4


def test_small_features_flag_a_hole_and_a_pin_thinner_than_two_nozzle_widths():
    plate = Box(30, 30, 5) - Cylinder(0.3, 5) + Pos(8, 0, 2.5) * Cylinder(0.35, 4, align=BOTTOM)
    result = one(small_features, plate, mesh=False)
    assert result.status == "warn"
    kinds = {f["kind"]: f["diameter"] for f in result.data["round_features"]}
    assert kinds == {"hole": pytest.approx(0.6), "pin": pytest.approx(0.7)}
    assert "thinner than 0.8 mm" in result.message


def test_small_features_flag_a_step_narrower_than_the_nozzle():
    stepped = Box(20, 20, 5, align=BOTTOM) + Pos(0, 0, 5) * Box(19.7, 19.7, 3, align=BOTTOM)
    result = one(small_features, stepped, mesh=False)
    assert result.status == "warn"
    assert result.data["narrow_face_count"] == 1
    assert result.data["narrow_faces"][0]["width"] < 0.4


def test_small_features_depend_on_the_nozzle_in_the_request():
    plate = Box(30, 30, 5) - Cylinder(0.5, 5)
    assert one(small_features, plate, mesh=False).status == "pass"
    wide = {"printer": {"id": "p", "nozzles": [{"id": 0, "diameter": 0.8}]}}
    assert one(small_features, plate, mesh=False, context=wide).status == "warn"
    assert one(small_features, plate, mesh=False, context={}).status == "skipped"


# --- model-level checks ----------------------------------------------------------------------


def test_parts_do_not_intersect():
    apart = make_env({"a": Box(10, 10, 10), "b": Pos(30, 0, 0) * Box(10, 10, 10)}, mesh=False)
    assert parts_do_not_intersect(apart).status == "pass"
    touching = make_env({"a": Box(10, 10, 10), "b": Pos(10, 0, 0) * Box(10, 10, 10)}, mesh=False)
    assert parts_do_not_intersect(touching).status == "pass"
    overlapping = make_env(
        {"a": Box(10, 10, 10), "b": Pos(8, 0, 0) * Box(10, 10, 10), "c": Pos(0, 0, 40) * Box(5, 5, 5)},
        mesh=False)
    result = parts_do_not_intersect(overlapping)
    assert result.status == "fail" and result.severity == "error" and result.part is None
    assert result.data["overlaps"] == [
        {"parts": ["a", "b"], "volume": pytest.approx(200), "intended": 0.0}]
    assert "'a' and 'b' share 200.0 mm3" in result.message
    single = make_env({"a": Box(10, 10, 10)}, mesh=False)
    assert parts_do_not_intersect(single).status == "skipped"


def test_fit_pairs_is_skipped_never_passed():
    env = make_env({"a": Box(10, 10, 10)}, mesh=False)
    (result,) = fit_pairs(env)
    assert result.status == "skipped" and result.message == "Skipped: no declared fits."

    class Resolver:
        declared_fits = [{"between": ["a", "b"], "fit_class": "slide"}]  # noqa: RUF012

    (declared,) = fit_pairs(make_env({"a": Box(10, 10, 10)}, mesh=False, fit=Resolver()))
    assert declared.status == "skipped" and declared.severity == "error"
    assert "1 fit(s) were declared" in declared.message and "not implemented" in declared.message
    assert declared.data == {"declared": 1}


def test_run_checks_reports_every_check_in_order():
    env = make_env({"a": cup(1.5), "b": Pos(60, 0, 0) * Box(10, 10, 10)})
    results = run_checks(env)
    assert [r.id for r in results if r.part in ("a", None)] == list(CHECK_IDS)
    assert {r.part for r in results} == {"a", "b", None}
    assert not has_blocking_failure(results)
    ranges = next(r for r in results if r.id == "parameter_ranges")
    assert ranges.status == "skipped" and "not requested" in ranges.message
    asked = make_env({"a": Box(5, 5, 5)}, mesh=False, sample_ranges=True, only=("parameter_ranges",))
    (ranges,) = run_checks(asked)
    assert ranges.status == "skipped" and "not part of the engine" in ranges.message


def test_only_selects_checks_and_failures_block_by_severity():
    env = make_env({"a": cup(0.4)}, only=("min_wall", "overhangs"))
    results = run_checks(env)
    assert [r.id for r in results] == ["min_wall", "overhangs"]
    assert has_blocking_failure(results)
    warned = run_checks(make_env({"a": mushroom()}, only=("overhangs", "bed_contact")))
    assert [r.status for r in warned] == ["warn", "pass"]
    assert not has_blocking_failure(warned)


def test_a_check_that_crashes_is_not_proven_and_the_rest_still_run(monkeypatch):
    def broken(env, part):
        raise ZeroDivisionError("boom")

    # One check of each severity crashes: `manifold` blocks, `overhangs` warns.
    patched = tuple(
        (cid, severity, broken if cid in ("manifold", "overhangs") else function, per_part)
        for cid, severity, function, per_part in checks._CHECKS
    )
    monkeypatch.setattr(checks, "_CHECKS", patched)
    results = run_checks(make_env({"a": Box(5, 5, 5)}, mesh=False))
    assert [r.id for r in results] == list(CHECK_IDS)
    assert next(r for r in results if r.id == "valid_solid").status == "pass"

    crashed = next(r for r in results if r.id == "manifold")
    assert crashed.status == "fail" and crashed.severity == "error" and crashed.part == "a"
    # An error-severity check that could not run holds the version back.
    assert has_blocking_failure(results)
    warned = next(r for r in results if r.id == "overhangs")
    assert warned.status == "warn" and warned.severity == "warning"
    assert not has_blocking_failure([r for r in results if r.id != "manifold"])
    for result in (crashed, warned):
        # A plain sentence for the user; the exception is in the data only.
        assert result.message.startswith("Hotend could not run this check on 'a', so what "
                                         "it tests is not proven.")
        assert "ZeroDivisionError" not in result.message and "boom" not in result.message
        assert result.data == {"could_not_run": True, "exception": "ZeroDivisionError: boom"}
        assert result.to_json()["data"]["could_not_run"] is True
    assert "held back" in crashed.message and "held back" not in warned.message


def test_check_results_serialise_with_rounded_numbers():
    result = one(valid_solid, Box(1 / 3, 1, 1), mesh=False).to_json()
    assert set(result) == {"id", "status", "severity", "message", "part", "data"}
    assert result["data"]["volume"] == 0.3333


def test_two_parts_whose_overlap_cannot_be_computed_hold_the_version_back(monkeypatch):
    """An OCCT boolean that raises must not leave the collision check `skipped` and the
    version stored as ok."""
    from hotend_engine.checks.intersect import parts_do_not_intersect

    env = make_env({"a": Box(10, 10, 10), "b": Pos(5, 0, 0) * Box(10, 10, 10)}, mesh=False)

    def broken(self, *others):
        raise RuntimeError("Standard_Failure")

    monkeypatch.setattr(type(env.parts[0].shape), "intersect", broken)
    result = parts_do_not_intersect(env)
    assert result.status == "fail" and result.severity == "error"
    assert has_blocking_failure([result])
    assert result.data["could_not_run"] is True
    assert result.data["undetermined"][0]["parts"] == ["a", "b"]
    assert "not proven" in result.message and "held back" in result.message
    assert "Standard_Failure" not in result.message


def test_a_mesher_that_crashed_holds_the_version_back(monkeypatch):
    """When tessellation raises, every check that needs the mesh could not run, and an
    error-severity one holds the version back; none of them is `skipped`."""
    from hotend_engine.checks.base import CheckEnv as Env

    shapes = {"a": Box(5, 5, 5)}
    plain = make_env(shapes, mesh=False)
    crashed = Env(plain.parts, plain.ctx, plain.options, None,
                  "the model could not be tessellated (boom)",
                  tessellation_crash=ValueError("boom"))
    results = run_checks(crashed)
    assert has_blocking_failure(results)
    by_id = {r.id: r for r in results}
    wall = by_id["min_wall"]
    assert wall.status == "fail" and wall.severity == "error"
    assert wall.data["could_not_run"] is True and "boom" not in wall.message
    # No error-severity check is "skipped" for want of the mesh that crashed.
    assert not [r.id for r in results if r.status == "skipped" and r.severity == "error"
                and "tessellated" in r.message]
    # Meshing not asked for is still a reason to skip, not a fault.
    assert not has_blocking_failure([r for r in run_checks(plain) if r.id == "min_wall"])
