"""References and pockets on real geometry: a procedural stand-in mesh, the room a fit
gives it, and the proof that it fits and can be put in and taken out."""

from __future__ import annotations

import inspect
import sys
from pathlib import Path

import numpy as np
import pytest
from fit_helpers import context

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reference_stand_in import box_mesh, stand_in_mesh

from hotend_fit import fit
from hotend_fit.reference import (
    POCKET_FITS,
    SHAPES,
    Pocket,
    Reference,
    ReferenceProblem,
    _inside_any,
    verify_pocket,
    verify_pockets,
)

TPU = {"id": "tpu-95a", "family": "TPU"}
BOTTOM = None


def resolver(material: dict | None = None) -> fit.FitResolver:
    return fit.resolver_for(context(**({"material": material} if material else {})))


def figure(kind: str = "soft") -> Reference:
    vertices, triangles = stand_in_mesh()
    return Reference("figure", vertices, triangles, kind=kind)


def brick() -> Reference:
    """A rigid 60 x 30 x 8 slab standing on z = 0, centred on the Z axis."""
    vertices, triangles = box_mesh((60.0, 30.0, 8.0), (-30.0, -15.0, 0.0))
    return Reference("brick", vertices, triangles)


def cup(pocket: Pocket, wall: float = 2.0, floor: float = 2.0, height: float | None = None):
    from build123d import Align, Cylinder, Pos

    cx, cy, _ = pocket.center
    tall = floor + 0.6 * pocket.depth if height is None else height
    body = Pos(cx, cy, 0) * Cylinder(pocket.radius + wall, tall,
                                     align=(Align.CENTER, Align.CENTER, Align.MIN))
    return pocket.cut(body, at=Pos(0, 0, floor))


def ring(pocket: Pocket, inner: float, z: float, wall: float = 2.0, thick: float = 2.0):
    from build123d import Align, Cylinder, Pos

    align = (Align.CENTER, Align.CENTER, Align.MIN)
    cx, cy, _ = pocket.center
    return Pos(cx, cy, z) * (Cylinder(pocket.radius + wall, thick, align=align)
                             - Cylinder(inner, thick, align=align))


def test_a_reference_reports_exact_measures_with_provenance_measured():
    thing = figure()
    vertices, _ = stand_in_mesh()
    assert thing.size == pytest.approx(tuple(np.ptp(vertices, axis=0)))
    assert thing.size[2] == pytest.approx(24.0)
    assert thing.closed
    assert 0 < thing.volume < thing.hull_volume < np.prod(thing.size)
    report = thing.report()
    assert report["provenance"] == "measured" and report["kind"] == "soft"
    assert report["triangles"] == len(thing.triangles)
    outline = thing.silhouette_size()
    assert outline["area"] < np.pi * outline["radius"] ** 2
    assert outline["height"] == pytest.approx(24.0)


def test_sections_and_the_silhouette_are_outlines_of_the_thing():
    thing = figure()
    silhouette = thing.silhouette()
    middle, low = thing.section(12.0), thing.section(2.0)
    assert silhouette.area >= middle.area > low.area > 0
    # The silhouette of a dome is its widest section, here at mid height.
    assert silhouette.area == pytest.approx(middle.area, rel=0.02)
    assert middle.bounding_box().min.Z == pytest.approx(12.0)
    assert len(thing.silhouette_points()[0]) <= 160
    with pytest.raises(ReferenceProblem, match="outside the reference"):
        thing.section(30.0)


@pytest.mark.parametrize("shape", SHAPES)
def test_every_envelope_contains_the_thing_and_grows_by_the_resolvers_clearance(shape):
    ctx, thing = resolver(), figure()
    pocket = Pocket(thing, "cradle", ctx=ctx, shape=shape)
    envelope = pocket.envelope
    assert envelope.is_valid and envelope.volume > thing.hull_volume
    # Every corner of the mesh lies inside the room (the bottom ones on its seat).
    assert _inside_any(thing.vertices[::7], [envelope]).all()
    # The gap is the resolver's own record for this class and size, never a typed number.
    assert ctx.was_issued(pocket.clearance)
    assert pocket.clearance.fit_class == "cradle"
    assert pocket.gap == pytest.approx(pocket.clearance.gap)
    assert pocket.gap > 1.5
    assert "clearance" not in inspect.signature(thing.pocket).parameters
    assert "gap" not in inspect.signature(thing.envelope).parameters
    if shape in ("cylinder", "box"):
        assert pocket.envelope_tolerance == 0.0
    # The hull envelope of `reference.hull()` is the thing's hull without any gap.
    if shape == "hull":
        hull = thing.hull()
        assert hull.volume == pytest.approx(thing.hull_volume, rel=0.03)
        assert hull.volume >= thing.hull_volume * 0.999


def test_a_cut_pocket_is_proven_free_all_round_and_open_along_its_direction():
    ctx, thing = resolver(), figure()
    pocket = thing.pocket("cradle", ctx=ctx, shape="cylinder")
    basket = cup(pocket)
    report = verify_pocket(pocket, [("basket", basket)])
    assert report.status == "pass", report.message
    assert report.data["clearance_proven"] and report.data["path_clear"]
    assert "goes in and out along +Z" in report.message
    data = report.to_json()
    assert data["fit_class"] == "cradle" and data["reference_provenance"] == "measured"
    assert data["expected"] == pytest.approx(pocket.gap)


def test_material_inside_the_room_fails_the_clearance():
    from build123d import Box, Pos

    ctx, thing = resolver(), figure()
    pocket = thing.pocket("cradle", ctx=ctx, shape="cylinder")
    basket = cup(pocket)
    # A rib that reaches 1 mm into the room from the wall.
    rib = Pos(pocket.center[0] + pocket.radius, pocket.center[1], 6.0) * Box(2.0, 4.0, 6.0)
    report = verify_pocket(pocket, [("basket", basket + rib)])
    assert report.status == "fail"
    assert "reaches" in report.message and "into the room" in report.message
    assert report.data["intrusions"][0]["part"] == "basket"


def test_a_trapped_thing_fails_the_insertion_check_until_the_lid_is_named_removable():
    ctx, thing = resolver(), figure()
    pocket = thing.pocket("cradle", ctx=ctx, shape="cylinder")
    tall = 2.0 + pocket.size[2] + 0.5
    basket = cup(pocket, height=tall)
    lid = ring(pocket, pocket.radius - 6.0, tall)
    trapped = verify_pocket(pocket, [("basket", basket + lid)])
    assert trapped.status == "fail"
    assert "trapped" in trapped.message and "+Z" in trapped.message
    assert trapped.data["clearance_proven"] and not trapped.data["path_clear"]

    # The same rim as a separate part is still in the way ...
    two_parts = verify_pocket(pocket, [("basket", basket), ("lid", lid)])
    assert two_parts.status == "fail" and "'lid'" in two_parts.message
    # ... until the pocket says it is taken off first.
    ctx2 = resolver()
    lidded = figure().pocket("cradle", ctx=ctx2, shape="cylinder", removable="lid")
    basket2 = cup(lidded, height=tall)
    report = verify_pocket(lidded, [("basket", basket2), ("lid", ring(lidded, lidded.radius - 6.0, tall))])
    assert report.status == "pass", report.message
    assert "once 'lid' is off" in report.message


def test_an_exception_in_a_proof_fails_the_pocket_and_is_traced_only_in_a_test_run(
        monkeypatch, capsys):
    from hotend_fit.reference import MAX_TRACE_CHARS, TRACE_VARIABLE

    ctx = resolver()
    figure().pocket("cradle", ctx=ctx)

    def broken(pocket, parts):
        raise RuntimeError("stand-in " + "x" * 5000)

    monkeypatch.setattr("hotend_fit.reference.verify_pocket", broken)
    monkeypatch.delenv(TRACE_VARIABLE, raising=False)
    (report,) = verify_pockets([], ctx)
    assert report.status == "fail" and "could not be examined" in report.message
    assert capsys.readouterr().err == ""

    monkeypatch.setenv(TRACE_VARIABLE, "1")
    (report,) = verify_pockets([], ctx)
    assert report.status == "fail"
    log = capsys.readouterr().err
    assert "xxx" in log and len(log) <= MAX_TRACE_CHARS + 1

    # A stderr that cannot be written changes nothing in the verdict.
    class Broken:
        def write(self, text):
            raise OSError("no space left")

        def flush(self):
            raise OSError("no space left")

    monkeypatch.setattr("sys.stderr", Broken())
    (report,) = verify_pockets([], ctx)
    assert report.status == "fail" and "could not be examined" in report.message


def test_a_pocket_that_was_never_placed_or_has_a_foreign_clearance_fails():
    ctx, thing = resolver(), figure()
    pocket = thing.pocket("cradle", ctx=ctx)
    report = verify_pockets([], ctx)[0]
    assert report.status == "fail" and "never placed" in report.message
    other = resolver()
    foreign = Pocket(figure(), "cradle", ctx=other)
    foreign.resolver = ctx
    from build123d import Pos

    foreign.place(Pos(0, 0, 0))
    assert "did not come from this build" in verify_pocket(foreign, []).message
    with pytest.raises(ReferenceProblem, match="already placed"):
        foreign.place(Pos(0, 0, 1))
    with pytest.raises(ReferenceProblem, match="must be one of"):
        thing.pocket("press", ctx=ctx)
    assert pocket.name == "pocket_1"


def shell_for(pocket: Pocket, lip: float = 1.0, wall: float = 1.6, back: float = 1.6):
    """A case-like shell: the pocket's outline grown by the wall, a back, and a lip."""
    from build123d import Pos

    shell = Pos(0, 0, -back) * pocket.room(offset=pocket.offset + wall, top=back + 1.0)
    case = pocket.cut(shell, at=Pos(0, 0, 0))
    return case - pocket.room(offset=pocket.offset - lip, top=3.0)


def test_a_flexible_shell_gets_a_stretch_fit_that_grips_and_can_be_put_on():
    ctx = resolver(TPU)
    pocket = brick().pocket("stretch", ctx=ctx)
    assert pocket.clearance.target == pytest.approx(-0.05)
    assert pocket.clearance.provenance == "guessed"
    case = shell_for(pocket)
    report = verify_pocket(pocket, [("case", case)])
    assert report.status != "fail", report.message
    assert report.data["material_flexible"] is True
    assert report.data["clearance_proven"]
    assert report.data["grip_coverage"] == pytest.approx(1.0)
    assert 0.9 < report.data["lip_depth"] < 1.2
    assert report.data["stretch_strain"] < report.data["stretch_limit"] == pytest.approx(0.08)
    assert "prints 0.05 mm smaller per side" in report.message
    # A guessed interference is said to be a guess until its gauge is printed.
    assert report.cautions and "gauge" in " ".join(report.cautions)


def test_a_stretch_fit_in_a_rigid_material_is_refused_in_plain_words():
    ctx = resolver()  # PLA
    pocket = brick().pocket("stretch", ctx=ctx)
    report = verify_pocket(pocket, [("case", shell_for(pocket))])
    assert report.status == "fail"
    assert "needs a flexible material such as TPU" in report.message
    assert "PLA is rigid" in report.message
    assert report.data["material_flexible"] is False


def test_a_stretch_fit_fails_when_the_lip_is_deeper_than_the_material_stretches_or_grips_nothing():
    ctx = resolver(TPU)
    deep = brick().pocket("stretch", ctx=ctx)
    report = verify_pocket(deep, [("case", shell_for(deep, lip=4.0))])
    assert report.status == "fail"
    assert "Make the lip shallower" in report.message
    assert report.data["stretch_strain"] > 0.08

    from build123d import Box, Pos

    ctx2 = resolver(TPU)
    bare = brick().pocket("stretch", ctx=ctx2)
    bare.place(Pos(0, 0, 0))
    plate = Pos(0, 0, -1.0) * Box(70, 40, 2.0)  # only a back plate: nothing holds the rim
    report = verify_pocket(bare, [("plate", plate)])
    assert report.status == "fail" and "holds only 0 %" in report.message


def test_the_declarations_of_a_build_rebuild_the_same_pockets_on_another_resolver():
    ctx, thing = resolver(), figure()
    pocket = thing.pocket("cradle", ctx=ctx, shape="cylinder")
    basket = cup(pocket)
    declared = ctx.declarations()
    assert declared["pockets"][0]["reference"] == "figure"
    assert declared["loose"] == []  # a pocket's clearance is not "taken without a card"

    other = resolver()
    other.references["figure"] = figure()  # the verifier's own copy
    other.restore(declared)
    assert other.restore_problems == []
    rebuilt = other.pockets[0]
    assert rebuilt.gap == pytest.approx(pocket.gap) and rebuilt.shape == "cylinder"
    assert verify_pockets([("basket", basket)], other)[0].status == "pass"

    # A declaration that names an unknown reference, or lies about its shape, is refused.
    lying = fit.resolver_for(context())
    lying.restore({**declared, "pockets": [{**declared["pockets"][0], "reference": "nothing"}]})
    assert "no reference named 'nothing'" in lying.restore_problems[0]
    broken = fit.resolver_for(context())
    broken.references["figure"] = figure()
    broken.restore({**declared, "pockets": [{**declared["pockets"][0], "shape": "sphere"}]})
    assert "could not be rebuilt" in broken.restore_problems[0]


@pytest.mark.parametrize(("fit_class", "shape"), [("cradle", "cylinder"), ("cradle", "silhouette"),
                                                   ("stretch", "silhouette"), ("stretch", "box")])
def test_each_reference_fit_class_has_a_gauge_that_builds(fit_class, shape):
    ctx = resolver(TPU if fit_class == "stretch" else None)
    thing = figure() if fit_class == "cradle" else brick()
    pocket = thing.pocket(fit_class, ctx=ctx, shape=shape)
    gauge = pocket.gauge_set()
    piece = pocket.gauge()
    assert list(gauge.parts) == ["gauge_ring"]
    assert piece.is_valid and len(piece.solids()) == 1
    box = piece.bounding_box()
    assert box.min.Z == pytest.approx(0.0) and box.size.Z == pytest.approx(5.0)
    # A ring: its inside is the pocket's outline, its wall 2 mm.
    widths = [d["drawn"] for d in gauge.dimensions]
    assert len(widths) == 2 and all(d["feature"] == "hole" for d in gauge.dimensions)
    across = sorted(pocket.size[:2]) if shape != "cylinder" else [2 * pocket.radius] * 2
    assert sorted(widths) == pytest.approx(across, abs=0.2)
    assert sorted([box.size.X, box.size.Y]) == pytest.approx([w + 4.0 for w in across], abs=0.3)
    said = gauge.dimensions[0]["what"]
    assert ("stretch over" in said) if fit_class == "stretch" else ("drop through" in said)


def test_the_reference_classes_are_not_offered_to_the_feature_cards():
    assert set(fit.REFERENCE_FIT_CLASSES) == {"stretch", "cradle"}
    assert not set(fit.REFERENCE_FIT_CLASSES) & set(fit.FIT_CLASSES)
    assert set(POCKET_FITS) >= set(fit.REFERENCE_FIT_CLASSES)
    assert fit.REFERENCE_FIT_CLASSES["stretch"].flexible_only
    assert all(spec.provenance == "guessed" for spec in fit.REFERENCE_FIT_CLASSES.values())
    ctx = resolver()
    cradle = ctx.clearance("cradle", 80.0, printed_peg=False)
    assert cradle.target == pytest.approx(1.5 + 0.8)
    stretch = ctx.clearance("stretch", 150.0, printed_peg=False)
    assert stretch.target == pytest.approx(-0.05) and stretch.provenance == "guessed"
    # The stretch cavity prints smaller than the thing: drawn gap less than what the
    # printer takes away.
    assert ctx.printed_gap(stretch, "nominal") == pytest.approx(-0.05, abs=1e-6)


def test_the_range_sampler_builds_with_the_requests_references(tmp_path):
    import shutil

    from fit_helpers import MODELS
    from reference_stand_in import write_stl

    from hotend_fit.sampler import ERROR_CHECKS, _Runner

    shutil.copy(MODELS / "ref_basket.py", tmp_path / "model.py")
    write_stl(tmp_path / "figure.stl", *stand_in_mesh())
    request = context(references=[{"name": "figure", "path": "figure.stl", "kind": "soft"}])
    assert "reference_fit" in ERROR_CHECKS
    runner = _Runner(tmp_path / "model.py", request, 2.0, 0.8, ERROR_CHECKS)
    assert runner.run({"wall": 2.0, "floor": 2.0, "rim": 0.0}, "default").ok
    trapped = runner.run({"wall": 2.0, "floor": 2.0, "rim": 6.0}, "sweep")
    assert list(trapped.failed) == ["reference_fit"]
    assert "trapped" in trapped.failed["reference_fit"]


def test_the_circle_around_an_outline_is_the_smallest_one():
    """Every round room, and every part sized from `pocket.radius`, is as large as this
    circle. A circle that is only close to the smallest one makes the holder larger than
    the fit asks for (0.15 mm per side on a real file), so it is exact."""
    from hotend_fit.reference import _enclosing_circle

    # Three points of a known circle and many strictly inside it, far from the origin.
    centre, radius = np.array([137.5, -62.25]), 42.0
    on = centre + radius * np.array([[np.cos(a), np.sin(a)] for a in (0.3, 2.4, 4.6)])
    rng = np.random.default_rng(7)
    inside = centre + rng.uniform(-0.6, 0.6, (400, 2)) * radius
    inside = inside[np.hypot(*(inside - centre).T) < radius - 0.01]
    cx, cy, r = _enclosing_circle(np.concatenate([inside, on]))
    assert (cx, cy, r) == pytest.approx((centre[0], centre[1], radius), abs=1e-9)
    # A lopsided outline: the circle is set by its two far ends, not by its centroid.
    oblong = np.array([[0.0, 0.0], [100.0, 0.0], [10.0, 6.0], [12.0, -5.0], [8.0, 3.0]])
    assert _enclosing_circle(oblong) == pytest.approx((50.0, 0.0, 50.0), abs=1e-9)
    # Random clouds: nothing lies outside, and two or three points lie on the circle.
    for seed in range(20):
        cloud = np.random.default_rng(seed).normal(size=(160, 2)) * (1 + seed) + seed
        cx, cy, r = _enclosing_circle(cloud)
        distance = np.hypot(cloud[:, 0] - cx, cloud[:, 1] - cy)
        assert distance.max() <= r + 1e-12
        assert 2 <= int((distance > r - 1e-7).sum()) <= 3
    assert _enclosing_circle(np.array([[3.0, 4.0]])) == (3.0, 4.0, 0.0)
    # The pocket of a round thing is no wider than the thing plus its gap.
    pocket = figure().pocket("cradle", ctx=resolver(), shape="cylinder")
    vertices, _ = stand_in_mesh()
    cx, cy, _ = pocket.center
    farthest = float(np.hypot(vertices[:, 0] - cx, vertices[:, 1] - cy).max())
    assert farthest - 1e-6 <= pocket._frame.circle[2] <= farthest + pocket._frame.tolerance + 1e-6
