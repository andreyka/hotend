"""A bought component whose maker states the section of its edge: the body has rounded
edges, the room the pocket check proves free follows them, a lip may lean into the corner
they leave, and the maker's tolerance of the thing itself widens the printed spread. Also
the outline helpers behind it, the flared keep-out and the areas a drawing marks."""

from __future__ import annotations

import copy
import json
import math
import sys
from pathlib import Path

import numpy as np
import pytest
from fit_helpers import context

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from build123d import GeomType
from reference_stand_in import box_mesh

from hotend_fit import fit
from hotend_fit.components import DATA_DIR, Component, ComponentError, use_component
from hotend_fit.reference import (
    Reference,
    ReferenceProblem,
    inset_polygon,
    solid_from_mesh,
    stacked_rings,
    tidy_polygon,
    verify_pocket,
)

TPU = {"id": "tpu-95a", "family": "TPU"}
SQUARE = np.array([(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)])


def raw_phone() -> dict:
    return json.loads((DATA_DIR / "sample_phone.json").read_text(encoding="utf-8"))


def fresh():
    """The phone and a stretch pocket for it that has not been placed yet."""
    resolver = fit.resolver_for(context(material=TPU))
    phone = use_component(resolver, "sample_phone")
    return phone, phone.reference.pocket("stretch", ctx=resolver, name="phone")


@pytest.fixture(scope="module")
def phone_and_pocket():
    return fresh()


def shell(pocket, wall=1.5, back=2.0, above=1.0):
    """A plain case around the pocket, open to the front: the outline grown by the wall,
    cut by the room all the way up. The tests add their own lip."""
    from build123d import Pos

    depth = pocket.depth * pocket.scale
    outer = Pos(0, 0, -back) * pocket.room(offset=pocket.offset + wall, top=back + above)
    return pocket.cut(outer, at=Pos(0, 0, 0)) - pocket.room(top=above + 1.0), depth


# --- outline helpers ---------------------------------------------------------------------------


def test_an_outline_is_inset_corner_for_corner_or_not_at_all():
    assert inset_polygon(SQUARE, 1.0).tolist() == [[1, 1], [9, 1], [9, 9], [1, 9]]
    assert inset_polygon(SQUARE, -1.0).tolist() == [[-1, -1], [11, -1], [11, 11], [-1, 11]]
    assert inset_polygon(SQUARE, 0.0).tolist() == SQUARE.tolist()
    # An inset that would make an edge vanish is refused, not drawn inside out.
    assert inset_polygon(SQUARE, 5.0) is None
    assert inset_polygon(SQUARE, 6.0) is None
    # A corner cut off by a short edge: the edge vanishes at a small inset already.
    cut = np.array([(0.0, 0.0), (10.0, 0.0), (10.0, 9.9), (9.9, 10.0), (0.0, 10.0)])
    assert inset_polygon(cut, 0.1) is not None
    assert inset_polygon(cut, 0.5) is None
    # Two corners at the same place cannot be inset.
    assert inset_polygon(np.array([(0.0, 0.0), (0.0, 0.0), (10.0, 0.0), (5.0, 8.0)]), 0.1) is None


def test_corners_that_do_not_shape_an_outline_are_dropped():
    noisy = np.array([(0.0, 0.0), (5.0, 0.0002), (10.0, 0.0), (10.0, 5.0), (10.0, 10.0),
                      (0.0, 10.0)])
    assert tidy_polygon(noisy).tolist() == SQUARE.tolist()
    # A corner that shapes the outline stays, however close its neighbours are.
    kept = np.array([(0.0, 0.0), (10.0, 0.0), (10.0, 9.9), (9.9, 10.0), (0.0, 10.0)])
    assert len(tidy_polygon(kept)) == 5
    assert len(tidy_polygon(kept, tolerance=0.2)) == 4
    # Never fewer than a triangle.
    assert len(tidy_polygon(np.array([(0.0, 0.0), (1e-5, 0.0), (0.0, 1e-5)]))) == 3


def test_stacked_rings_make_a_closed_solid_of_the_right_volume():
    rings = [SQUARE, inset_polygon(SQUARE, 2.0), inset_polygon(SQUARE, 2.0)]
    vertices, triangles = stacked_rings(rings, [0.0, 3.0, 5.0])
    assert vertices.shape == (12, 3) and triangles.shape == (2 * 2 + 2 * 4 * 2, 3)
    solid = solid_from_mesh(vertices, triangles)
    # A frustum from 10 x 10 to 6 x 6 over 3 mm and a 6 x 6 prism of 2 mm.
    assert solid.volume == pytest.approx(3.0 / 3 * (100 + 36 + 60) + 72.0)
    assert solid.is_valid


# --- the rounded body --------------------------------------------------------------------------


def test_the_edge_section_runs_from_the_back_to_the_front_through_the_stated_points(
        phone_and_pocket):
    phone, _ = phone_and_pocket
    section = phone.edge_section()
    depth = phone.size[2]
    stated = phone.envelope["edge_profile"].value
    assert section[0] == (pytest.approx(stated[-1][0]), 0.0)
    assert section[-1] == (pytest.approx(stated[-1][0]), depth)
    # Symmetric about the middle of the thickness, rising all the way.
    assert [z for _, z in section] == sorted(z for _, z in section)
    for (d, z), (mirror_d, mirror_z) in zip(section, reversed(section), strict=True):
        assert (d, z) == (pytest.approx(mirror_d), pytest.approx(depth - mirror_z))
    # Every stated point is on it (height from the mid-plane), on both halves.
    points = {(round(d, 6), round(z, 6)) for d, z in section}
    for d, h in stated:
        assert (round(d, 6), round(depth / 2 + h, 6)) in points
        assert (round(d, 6), round(depth / 2 - h, 6)) in points
    # Convex: seen from the back to the front the section only ever turns one way.
    for a, b, c in zip(section[:-2], section[1:-1], section[2:], strict=True):
        turn = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0])
        assert turn <= 1e-9
    # Changing the section returns a copy, not the component's own list.
    section.clear()
    assert len(phone.edge_section()) == 26


def test_a_component_without_an_edge_profile_has_square_edges():
    raw = raw_phone()
    del raw["envelope"]["edge_profile"]
    plain = Component(raw)
    assert plain.edge_section() == [(0.0, 0.0), (0.0, 10.0)]
    assert not plain.reference.has_profile("+z")
    # The upright prism over the outline, which the rounding makes 1600 mm3 smaller.
    assert plain.reference.volume == pytest.approx(102_764.5, abs=1.0)


@pytest.mark.parametrize("points", [
    [[0.5, 2.0], [3.0, 5.0]],                  # does not start on the edge's flat band
    [[0.0, 2.0], [0.5, 1.0], [3.0, 5.0]],      # goes back down
    [[0.0, 2.0], [3.0, 4.0]],                  # does not reach the flat face
    [[0.0, 2.0], [30.0, 5.0]],                 # wider than a quarter of the body
])
def test_an_edge_profile_that_is_not_an_edge_is_refused(points):
    raw = raw_phone()
    raw["envelope"]["edge_profile"]["value"] = points
    with pytest.raises(ComponentError, match="edge_profile"):
        Component(raw).edge_section()


def test_the_body_of_the_sample_phone_has_rounded_edges(phone_and_pocket):
    phone, _ = phone_and_pocket
    body = phone.reference
    assert body.closed and body.size == pytest.approx((80.0, 130.0, 10.0))
    assert body.has_profile("+z") and body.has_profile("-z") and not body.has_profile("+x")
    # Less than the prism over the outline (102,765 mm3) by what the rounding takes off.
    assert body.volume == pytest.approx(101_138.2, abs=1.0)
    # Halfway up the body reaches the outline; at the glass it is 3 mm narrower a side.
    vertices = body.vertices
    middle = vertices[np.abs(vertices[:, 2] - 5.0) < 2.1]
    front = vertices[np.abs(vertices[:, 2] - 10.0) < 1e-9]
    assert np.ptp(middle[:, 0]) == pytest.approx(80.0, abs=1e-6)
    assert np.ptp(front[:, 0]) == pytest.approx(80.0 - 2 * 3.0, abs=0.01)


# --- the fitted room and the taper -------------------------------------------------------------


def test_the_fitted_room_follows_the_rounded_edges_inside_the_prism(phone_and_pocket):
    phone, pocket = phone_and_pocket
    assert pocket.fitted
    prism, fitted = pocket.room(), pocket.room(fitted=True)
    assert tuple(fitted.bounding_box().size) == pytest.approx(
        tuple(prism.bounding_box().size), abs=1e-6)
    assert fitted.volume < prism.volume - 400
    leftover = fitted - prism
    assert (leftover.volume if hasattr(leftover, "volume") else 0.0) == pytest.approx(0, abs=1e-3)
    # The phone itself lies inside the fitted room, with the fit's gap all round.
    solid = phone.reference._solid()
    assert solid.volume() < fitted.volume
    # A pocket of another shape, or along a direction with no stated section, is not fitted.
    resolver = fit.resolver_for(context(material=TPU))
    other = use_component(resolver, "sample_phone").reference
    assert not other.pocket("stretch", ctx=resolver, shape="hull").fitted
    vertices, triangles = box_mesh((60.0, 30.0, 8.0), (-30.0, -15.0, 0.0))
    brick = Reference("brick", vertices, triangles)
    plain = brick.pocket("cradle", ctx=fit.resolver_for(context()))
    assert not plain.fitted
    assert plain.room(fitted=True).volume == pytest.approx(plain.room().volume)


def test_a_taper_is_the_outline_between_two_heights_moved_in_corner_for_corner(phone_and_pocket):
    _, pocket = phone_and_pocket
    taper = pocket.taper(1.0, 2.0, 1.0)
    box = taper.bounding_box()
    assert (box.min.Z, box.max.Z) == pytest.approx((1.0, 2.0))
    assert box.size.X == pytest.approx(pocket.size[0], abs=0.01)
    assert taper.is_valid
    # Every face is flat: the two ends and one band per edge of the outline.
    assert all(face.geom_type == GeomType.PLANE for face in taper.faces())
    # Its top is the outline 1 mm in: a slab between the two levels there is inside it.
    wide = pocket.taper(1.0, 2.0, -1.0)
    assert wide.volume > taper.volume
    assert wide.bounding_box().size.X == pytest.approx(pocket.size[0] + 2.0, abs=0.02)
    # With another offset the same shape around a wider outline.
    assert pocket.taper(0.0, 1.0, 0.5, offset=pocket.offset + 1.5).bounding_box().size.X \
        == pytest.approx(pocket.size[0] + 3.0, abs=0.02)
    with pytest.raises(ReferenceProblem, match="two heights"):
        pocket.taper(2.0, 2.0, 1.0)
    with pytest.raises(ReferenceProblem, match="smaller inset"):
        pocket.taper(0.0, 1.0, 30.0)
    resolver = fit.resolver_for(context(material=TPU))
    hull = use_component(resolver, "sample_phone").reference.pocket(
        "stretch", ctx=resolver, shape="hull")
    with pytest.raises(ReferenceProblem, match="silhouette"):
        hull.taper(0.0, 1.0, 0.5)


def test_a_lip_may_lean_into_the_corner_the_rounded_edge_leaves():
    """The check proves the fitted room free: material in the corner between the prism
    and the rounded shoulder passes, material inside the fitted room does not."""
    phone, pocket = fresh()
    case, depth = shell(pocket)
    section = [(d, z) for d, z in phone.edge_section() if z > phone.size[2] / 2]
    slope = 1.05
    # The lowest slope of that steepness that clears the shoulder, in the room the fit keeps.
    seat = max(z * pocket.scale + pocket.top_gap - slope * d * pocket.scale
               for d, z in section) + 0.03
    leaning = case + (pocket.room(top=1.0) - pocket.room(top=seat - depth)
                      - pocket.taper(seat, seat + slope * 3.0, 3.0)
                      - pocket.room(offset=pocket.offset - 1.2, top=2.0))
    report = verify_pocket(pocket, [("case", leaning)])
    assert report.data["room"] == "fitted to the stated edge section"
    assert not report.problems, report.problems
    assert report.data["intrusions"] == []
    assert report.data["lip_depth"] > 0.9

    # The same lip 0.3 mm lower sits on the phone's shoulder: it reaches into the room.
    low = seat - 0.3
    _, pocket = fresh()
    case, _ = shell(pocket)
    pressing = case + (pocket.room(top=1.0) - pocket.room(top=low - depth)
                       - pocket.taper(low, low + slope * 3.0, 3.0)
                       - pocket.room(offset=pocket.offset - 1.2, top=2.0))
    refused = verify_pocket(pocket, [("case", pressing)])
    assert any("tighter around the thing" in problem for problem in refused.problems)
    assert refused.data["intrusions"][0]["part"] == "case"


# --- the thing's own tolerance -----------------------------------------------------------------


def test_the_makers_tolerance_of_the_thing_widens_the_printed_spread():
    from build123d import Pos

    _, pocket = fresh()
    case, _ = shell(pocket)
    lipped = case + (pocket.room(top=1.0) - pocket.room()
                     - pocket.room(offset=pocket.offset - 1.2, top=2.0))
    report = verify_pocket(pocket, [("case", lipped)])
    spread = report.data["thing_tolerance"]
    # Width +-0.25, height +-0.40; across +Z the larger counts, half of it per side.
    assert spread["per_side"] == pytest.approx(0.20)
    printed = report.data["robustness"]["printed"].values()
    assert spread["printed"] == pytest.approx([min(printed) - 0.20, max(printed) + 0.20])
    assert spread["ok"] is False
    assert any("the maker allows" in caution and "0.20 mm either way" in caution
               for caution in report.cautions)

    # A reference object states no tolerance: nothing is added.
    vertices, triangles = box_mesh((60.0, 30.0, 8.0), (-30.0, -15.0, 0.0))
    resolver = fit.resolver_for(context())
    brick = Reference("brick", vertices, triangles).pocket("cradle", ctx=resolver)
    tray = brick.cut(Pos(0, 0, -2) * brick.room(offset=brick.offset + 2, top=0), at=Pos(0, 0, 0))
    plain = verify_pocket(brick, [("tray", tray)])
    assert "thing_tolerance" not in plain.data
    assert plain.data["room"] == "outline prism"


# --- the flared keep-out and the areas a drawing marks -----------------------------------------


def test_the_touch_strip_keep_out_flares_from_the_phones_surface(phone_and_pocket):
    phone, _ = phone_and_pocket
    strip = phone.feature("touch_strip")
    stated = strip.keep_outs[0]
    assert stated.flare.value == 40.0 and stated.flare.unit == "deg"
    assert stated.flare.path == "features.touch_strip.keep_out.0.flare"
    assert stated.flare in phone.numbers()
    length, tall = stated.size[1].value, stated.size[2].value
    half_width = phone.size[0] / 2
    spread = math.tan(math.radians(40.0))
    for reach in (0.5, 3.0):
        room = strip.keep_out(reach=reach, inward=1.0)
        box = room.bounding_box()
        assert (box.min.X, box.max.X) == pytest.approx((half_width - 1.0, half_width + reach))
        # At the phone's surface it is the stated rectangle; it widens by tan(40) per mm.
        assert box.size.Y == pytest.approx(length + 2 * reach * spread, abs=1e-3)
        assert box.size.Z == pytest.approx(tall + 2 * reach * spread, abs=1e-3)
        assert room.is_valid
    # Straight inside the phone, flared outside it.
    room = strip.keep_out(reach=3.0, inward=1.0)
    cy, cz = strip.center[1], strip.center[2]
    inside_wall = room.is_inside((half_width + 2.9, cy + length / 2 + 2.0, cz))
    at_surface = room.is_inside((half_width + 0.05, cy + length / 2 + 2.0, cz))
    in_phone = room.is_inside((half_width - 0.5, cy + length / 2 - 0.5, cz))
    assert inside_wall and not at_surface and in_phone


@pytest.mark.parametrize(("flare", "fragment"), [
    ({"value": 95.0, "unit": "deg"}, "flare"),
    ({"value": 0.0, "unit": "deg"}, "flare"),
    ({"value": 40.0, "unit": "mm"}, "flare"),
])
def test_a_flare_that_is_not_an_angle_is_refused(flare, fragment):
    raw = raw_phone()
    (strip,) = [f for f in raw["features"] if f["id"] == "touch_strip"]
    shape = strip["keep_out"][0]
    shape["flare"] = {**copy.deepcopy(shape["flare"]), **flare}
    with pytest.raises(ComponentError, match=fragment):
        Component(raw)


def test_the_areas_marked_on_the_back_are_features(phone_and_pocket):
    phone, _ = phone_and_pocket
    areas = {name: feature for name, feature in phone.features.items() if feature.kind == "area"}
    assert set(areas) == {"back_area_plateau", "back_area_corner"}
    corner = areas["back_area_corner"]
    assert corner.face == "-z" and corner.size[:2] == (6.0, 14.0) and corner.proven
    assert corner.keep_outs == [] or corner.keep_outs == ()
    assert all(datum.url == "https://example.com/hotend/sample-phone"
               for datum in phone.numbers() if "back_area" in datum.path)
