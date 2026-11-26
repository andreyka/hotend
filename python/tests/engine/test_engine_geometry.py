"""Parts, B-rep storage, tessellation, rendering and the docs index, in-process."""

from __future__ import annotations

import io
import json

import numpy as np
import pytest
from build123d import (
    Align,
    Box,
    BuildPart,
    Color,
    Compound,
    Cone,
    Cylinder,
    Part,
    Pos,
    Rectangle,
    Rot,
    Sphere,
)
from PIL import Image

from hotend_engine import docs
from hotend_engine.errors import BadRequest, ModelError
from hotend_engine.export import export_formats
from hotend_engine.model import load_model
from hotend_engine.parts import (
    PALETTE,
    NamedPart,
    edge_defects,
    load_parts,
    parts_from_build,
    save_parts,
    select_parts,
    summarize,
)
from hotend_engine.render import VIEWS, render_view
from hotend_engine.request import Material
from hotend_engine.tessellate import tessellate


@pytest.fixture(scope="module")
def model(tmp_path_factory):
    path = tmp_path_factory.mktemp("model") / "model.py"
    path.write_text("PARAMS = {}\n\n\ndef build(p, ctx):\n    return None\n", encoding="utf-8")
    return load_model(path)


# --- what build() may return ---------------------------------------------------------


def test_single_part_is_named_part(model):
    (part,) = parts_from_build(model, Box(1, 2, 3))
    assert part.name == "part" and part.color == PALETTE[0]
    assert type(part.shape).__name__ == "Solid"


def test_dict_of_parts_keeps_order_and_colours(model):
    red = Box(1, 1, 1)
    red.color = Color("red")
    parts = parts_from_build(model, {"base": Box(1, 2, 3), "knob": Sphere(2), "cap": red})
    assert [p.name for p in parts] == ["base", "knob", "cap"]
    assert [p.color for p in parts] == [PALETTE[0], PALETTE[1], "#ff0000"]


def test_builder_object_is_accepted(model):
    with BuildPart() as builder:
        Box(4, 4, 4)
    (part,) = parts_from_build(model, {"cube": builder})
    assert part.shape.volume == pytest.approx(64)


def test_parts_are_flattened_to_world_coordinates(model):
    moved = Pos(10, 20, 30) * Rot(0, 0, 90) * Box(2, 4, 6)
    nested = Compound(children=[Compound(children=[moved])])
    (part,) = parts_from_build(model, {"deep": nested})
    assert len(part.solids) == 1
    box = part.shape.bounding_box()
    assert tuple(box.min) == pytest.approx((8, 19, 27))
    assert tuple(box.max) == pytest.approx((12, 21, 33))


@pytest.mark.parametrize(
    ("returned", "type_", "fragment"),
    [
        (None, "BuildResultError", "got NoneType"),
        ([Box(1, 1, 1)], "BuildResultError", "got list"),
        ({}, "BuildResultError", "no parts"),
        ({"Lid": Box(1, 1, 1)}, "BuildResultError", "part name 'Lid' is not allowed"),
        ({"my part": Box(1, 1, 1)}, "BuildResultError", "is not allowed"),
        ({7: Box(1, 1, 1)}, "BuildResultError", "is not allowed"),
        ({"a": "a box"}, "BuildResultError", "part 'a' is a str"),
        ({"a": Rectangle(3, 3)}, "NotASolid", "part 'a' is a Rectangle with no solid"),
        ({"a": Box(1, 1, 1).faces()[0]}, "NotASolid", "part 'a' is a Face"),
        ({"a": Part()}, "NotASolid", "with no solid"),
        ({f"p{i}": Box(1, 1, 1) for i in range(65)}, "BuildResultError", "more than 64 parts"),
    ],
)
def test_bad_build_results_are_model_errors(model, returned, type_, fragment):
    with pytest.raises(ModelError) as caught:
        parts_from_build(model, returned)
    assert caught.value.type == type_
    assert fragment in caught.value.message
    assert caught.value.line == 4  # the build function


def test_edge_defects_handles_degenerate_edges():
    for closed in (Box(1, 2, 3), Sphere(3), Cone(3, 0, 5), Cylinder(2, 4)):
        assert edge_defects(closed) == (0, 0)
    assert edge_defects(Box(1, 2, 3).faces()[0]) == (4, 0)
    # two boxes fused along one edge: that edge has four faces
    kissing = Part() + [Box(10, 10, 10), Pos(10, 10, 0) * Box(10, 10, 10)]
    assert edge_defects(kissing) == (0, 1)


def test_summary_and_mass_estimate():
    part = NamedPart("cube", Box(10, 10, 10), "#123456")
    summary = summarize(part, Material(id="petg", family="petg"))
    assert summary["volume"] == pytest.approx(1000) and summary["area"] == pytest.approx(600)
    assert summary["mass_estimate_g"] == pytest.approx(1.27)
    assert summary["bbox"] == {"min": [-5.0, -5.0, -5.0], "max": [5.0, 5.0, 5.0]}
    assert summary["solids"] == 1 and summary["faces"] == 6
    assert summarize(part, Material(id="x", family="PLA", density_g_cm3=2.0))["mass_estimate_g"] == 2.0
    # an unknown material gives no number rather than a guess
    assert summarize(part, Material(id="x", family="unobtainium"))["mass_estimate_g"] is None
    assert summarize(part, None)["mass_estimate_g"] is None
    assert summarize(NamedPart("ball", Sphere(5), "#123456"))["is_manifold"] is True


# --- model.brep + model.parts.json ---------------------------------------------------


def _parts() -> list[NamedPart]:
    return [
        NamedPart("box", Box(40, 30, 20) - Cylinder(5, 30), "#4c78a8"),
        NamedPart("pair", Compound([Box(5, 5, 5), Pos(20, 0, 0) * Sphere(3)]), "#f58518"),
        NamedPart("lid", (Pos(0, 0, 30) * Rot(10, 20, 30) * Box(40, 30, 2)).solids()[0], "#54a24b"),
    ]


def test_brep_round_trip_keeps_names_order_and_geometry(tmp_path):
    parts = _parts()
    save_parts(parts, tmp_path)
    loaded = load_parts(tmp_path)
    assert [(p.name, p.color) for p in loaded] == [(p.name, p.color) for p in parts]
    for before, after in zip(parts, loaded, strict=True):
        assert after.shape.volume == pytest.approx(before.shape.volume, rel=1e-9)
        assert len(after.solids) == len(before.solids)
        assert tuple(after.shape.bounding_box().max) == pytest.approx(
            tuple(before.shape.bounding_box().max))


def test_brep_holds_geometry_only_even_after_tessellation(tmp_path):
    meshed = _parts()
    assert tessellate(meshed).triangle_count > 0  # leaves a triangulation on the shapes
    save_parts(meshed, tmp_path)
    assert b"Triangulations 0" in (tmp_path / "model.brep").read_bytes()
    assert [len(p.shape.faces()) for p in load_parts(tmp_path)] == [
        len(p.shape.faces()) for p in meshed]


def test_load_parts_rejects_missing_or_mismatched_files(tmp_path):
    with pytest.raises(BadRequest, match=r"model\.brep is missing"):
        load_parts(tmp_path)
    save_parts(_parts(), tmp_path)
    index = tmp_path / "model.parts.json"
    good = index.read_text(encoding="utf-8")
    for text, fragment in (
        ("{not json", "malformed"),
        (json.dumps({"parts": [{"name": "box"}]}), "malformed"),
        (json.dumps({"parts": [{"name": "box", "color": "#000000"}]}), "holds 3 parts"),
        (good.replace('"pair"', '"box"'), "invalid or repeated"),
        (good.replace('"pair"', '"../x"'), "invalid or repeated"),
    ):
        index.write_text(text, encoding="utf-8")
        with pytest.raises(BadRequest, match=fragment):
            load_parts(tmp_path)
    index.write_text(good, encoding="utf-8")
    (tmp_path / "model.brep").write_text("not a brep", encoding="utf-8")
    with pytest.raises(BadRequest, match=r"model\.brep"):
        load_parts(tmp_path)


def test_select_parts():
    parts = _parts()
    assert select_parts(parts, None) == parts
    assert [p.name for p in select_parts(parts, ("lid", "box"))] == ["box", "lid"]
    with pytest.raises(BadRequest, match="unknown part"):
        select_parts(parts, ("handle",))


def test_exports_are_written_under_export(tmp_path):
    artifacts, notes = export_formats(_parts()[:1], ("step", "3mf", "stl"), tmp_path)
    assert artifacts == {"step": "export/model.step", "threemf": "export/model.3mf",
                         "stl": {"box": "export/box.stl"}}
    assert notes == []
    assert {p.name for p in (tmp_path / "export").iterdir()} == {"model.step", "model.3mf", "box.stl"}


def _hole_tightness(vertices: np.ndarray, radius: float) -> float:
    """How far the facets of a round hole about the Z axis cut inside its radius: the
    triangles are given as (n, 3, 3) corner points."""
    worst = 0.0
    for a, b in ((0, 1), (1, 2), (2, 0)):
        first, second = vertices[:, a], vertices[:, b]
        on_hole = (np.hypot(first[:, 0], first[:, 1]) < radius * 1.001) & (
            np.hypot(second[:, 0], second[:, 1]) < radius * 1.001)
        middle = (first[on_hole] + second[on_hole]) / 2
        worst = max(worst, radius - float(np.hypot(middle[:, 0], middle[:, 1]).min()))
    return worst


@pytest.mark.parametrize("radius", [3.0, 25.0, 100.0])
def test_exported_meshes_keep_round_holes_within_a_fixed_distance(radius, tmp_path):
    """The facets of a hole cut its corner by the chord error. That must be bounded in
    millimetres whatever the radius, not in segments per circle (which gave 0.03 mm on a
    100 mm hole: a tenth of a slide clearance)."""
    import zipfile
    from xml.etree import ElementTree

    from hotend_engine.export import MAX_CHORD_ERROR_MM, write_3mf, write_stl

    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    ring = NamedPart("ring", Cylinder(radius + 5, 10, align=bottom)
                     - Cylinder(radius, 10, align=bottom), "#4c78a8")
    write_stl(ring, tmp_path / "ring.stl")
    raw = (tmp_path / "ring.stl").read_bytes()
    record = np.dtype([("normal", "<f4", 3), ("corners", "<f4", (3, 3)), ("pad", "<u2")])
    triangles = np.frombuffer(raw, dtype=record, count=int.from_bytes(raw[80:84], "little"),
                              offset=84)["corners"].astype(float)
    assert 0 < _hole_tightness(triangles, radius) <= MAX_CHORD_ERROR_MM

    assert write_3mf([ring], tmp_path / "ring.3mf") == []
    with zipfile.ZipFile(tmp_path / "ring.3mf") as archive:
        root = ElementTree.fromstring(archive.read("3D/3dmodel.model"))
    space = {"m": "http://schemas.microsoft.com/3dmanufacturing/core/2015/02"}
    mesh = root.find("m:resources", space).find("m:object", space).find("m:mesh", space)
    points = np.array([[float(v.get(axis)) for axis in "xyz"]
                       for v in mesh.find("m:vertices", space)])
    index = np.array([[int(t.get(corner)) for corner in ("v1", "v2", "v3")]
                      for t in mesh.find("m:triangles", space)])
    assert 0 < _hole_tightness(points[index], radius) <= MAX_CHORD_ERROR_MM


# --- tessellation ------------------------------------------------------------------------


@pytest.fixture(scope="module")
def tessellation():
    parts = _parts()
    return parts, tessellate(parts)


def test_tessellation_matches_the_exact_geometry(tessellation):
    parts, tess = tessellation
    assert set(tess.meshes) == {"box", "pair", "lid"}
    for part in parts:
        mesh = tess.meshes[part.name]
        assert mesh.path == f"/model/{part.name}" and mesh.color == part.color
        assert mesh.face_map_ok
        box = part.shape.bounding_box()
        # locations and rotations are applied: the mesh sits where the solid is
        assert mesh.vertices.min(axis=0) == pytest.approx(tuple(box.min), abs=0.05)
        assert mesh.vertices.max(axis=0) == pytest.approx(tuple(box.max), abs=0.05)
        _, areas = mesh.triangle_normals_and_areas()
        assert areas.sum() == pytest.approx(part.shape.area, rel=0.01)
        tri = mesh.triangle_points
        volume = np.einsum("ij,ij->i", tri[:, 0], np.cross(tri[:, 1], tri[:, 2])).sum() / 6
        assert volume == pytest.approx(part.shape.volume, rel=0.01)  # outward-facing triangles
        assert len(mesh.triangle_face) == len(mesh.triangles)
        assert mesh.triangle_face.max() == len(part.shape.faces()) - 1
        assert np.linalg.norm(mesh.normals, axis=1) == pytest.approx(1.0, abs=1e-3)
        solid, error = mesh.manifold()
        assert error is None and solid.volume() == pytest.approx(part.shape.volume, rel=0.01)
    assert tess.triangle_count == sum(len(m.triangles) for m in tess.meshes.values())


def test_triangles_and_edges_follow_the_brep_face_and_edge_order(tessellation):
    parts, tess = tessellation
    part, mesh = parts[0], tess.meshes["box"]
    _, areas = mesh.triangle_normals_and_areas()
    for index, face in enumerate(part.shape.faces()):
        assert areas[mesh.triangle_face == index].sum() == pytest.approx(face.area, rel=0.02)
    lengths = np.linalg.norm(mesh.segments[:, 1] - mesh.segments[:, 0], axis=1)
    assert lengths.sum() == pytest.approx(sum(e.length for e in part.shape.edges()), rel=0.01)


def test_viewer_json_is_stable_and_json_clean(tessellation):
    parts, tess = tessellation
    text = tess.viewer_json()
    assert text == tessellate(parts).viewer_json()
    data = json.loads(text)
    assert data["hotend"]["parts"] == {p.name: f"/model/{p.name}" for p in parts}
    assert data["hotend"]["deviation"] == 0.1


def test_triangle_limit_coarsens_then_refuses():
    ball = [NamedPart("ball", Sphere(20), "#4c78a8")]
    fine = tessellate(ball)
    coarse = tessellate(ball, max_triangles=fine.triangle_count - 1)
    assert coarse.triangle_count < fine.triangle_count and coarse.deviation > fine.deviation
    assert coarse.viewer["hotend"]["deviation"] == coarse.deviation
    with pytest.raises(ValueError, match="the limit is 10"):
        tessellate(ball, max_triangles=10)


# --- rendering ------------------------------------------------------------------------------


def _pixels(png: bytes) -> np.ndarray:
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    return np.asarray(Image.open(io.BytesIO(png)).convert("RGB")).astype(int)


def _is_colour(pixels: np.ndarray, colour: str) -> np.ndarray:
    """Pixels that are a shade of the given part colour (same hue, any brightness)."""
    rgb = np.array([int(colour[i:i + 2], 16) for i in (1, 3, 5)], dtype=float)
    scale = pixels.max(axis=2, keepdims=True) / rgb.max()
    return (np.abs(pixels - rgb * scale).max(axis=2) < 30) & (pixels.max(axis=2) > 60) & (
        np.ptp(pixels, axis=2) > 40)


def test_every_view_renders_deterministically(tessellation):
    _, tess = tessellation
    meshes = list(tess.meshes.values())
    for view in VIEWS:
        png = render_view(meshes, view, (200, 160))
        assert png == render_view(meshes, view, (200, 160)), view
        pixels = _pixels(png)
        assert pixels.shape == (160, 200, 3)
        shown = sum(_is_colour(pixels[:110], mesh.color).sum() for mesh in meshes)
        assert shown > 500, view


def test_views_look_from_the_named_side():
    # A big box with a small box on its +X side and another on top. The colours are ones
    # the axis labels and the triad do not use.
    parts = [NamedPart("blue", Box(20, 20, 20), "#32c8c8"),
             NamedPart("orange", Pos(15, 0, 0) * Box(10, 10, 10), "#c832c8"),
             NamedPart("green", Pos(0, 0, 15) * Box(10, 10, 10), "#c8c832")]
    meshes = list(tessellate(parts).meshes.values())

    def seen(view: str) -> dict[str, np.ndarray]:
        pixels = _pixels(render_view(meshes, view, (240, 240)))[:190]  # without the caption
        return {name: _is_colour(pixels, p.color) for name, p in zip(
            ("blue", "orange", "green"), parts, strict=True)}

    def centre_x(mask: np.ndarray) -> float:
        return float(np.nonzero(mask)[1].mean())

    def centre_y(mask: np.ndarray) -> float:
        return float(np.nonzero(mask)[0].mean())

    front = seen("front")
    assert centre_x(front["orange"]) > centre_x(front["blue"])      # +X is to the right
    assert centre_y(front["green"]) < centre_y(front["blue"])       # +Z is up
    back = seen("back")
    assert centre_x(back["orange"]) < centre_x(back["blue"])        # mirrored from behind
    right = seen("right")
    assert right["orange"].sum() > 0 and right["green"].sum() > 0
    assert right["blue"].sum() < front["blue"].sum()                # orange hides its centre
    left = seen("left")
    assert left["orange"].sum() == 0                                # hidden behind the blue box
    top = seen("top")
    assert centre_x(top["orange"]) > centre_x(top["blue"])
    assert top["green"].sum() > 0
    bottom = seen("bottom")
    assert bottom["green"].sum() == 0                               # hidden from below
    iso = seen("iso")
    assert all(mask.sum() > 0 for mask in iso.values())             # iso shows all three


def test_hidden_edges_are_not_drawn_but_visible_ones_are():
    cube = [NamedPart("cube", Box(20, 20, 20), "#4c78a8")]
    pixels = _pixels(render_view(list(tessellate(cube).meshes.values()), "front", (300, 300)))
    body = pixels[:250]
    dark = body.max(axis=2) < 70
    face = _is_colour(body, "#4c78a8")
    rows = np.flatnonzero(face.sum(axis=1) > 50)
    cols = np.flatnonzero(face.sum(axis=0) > 50)
    top, bottom, left, right = rows.min(), rows.max(), cols.min(), cols.max()
    assert bottom - top == pytest.approx(right - left, abs=3)       # a square, seen head-on
    # the outline of the square is drawn just outside the face on all four sides ...
    assert dark[top - 3:top + 1, left:right].any(axis=0).all()
    assert dark[bottom:bottom + 4, left:right].any(axis=0).all()
    assert dark[top:bottom, left - 3:left + 1].any(axis=1).all()
    assert dark[top:bottom, right:right + 4].any(axis=1).all()
    # ... and its inside is clean: the back edges coincide with the outline, none cross it
    assert not dark[top + 4:bottom - 4, left + 4:right - 4].any()


def test_caption_and_unknown_view(tessellation):
    _, tess = tessellation
    meshes = list(tess.meshes.values())
    plain = _pixels(render_view(meshes, "iso", (400, 300)))
    noted = _pixels(render_view(meshes, "iso", (400, 300), note="section XZ at 0 mm"))
    caption_rows = np.unique(np.nonzero((plain != noted).any(axis=2))[0])
    assert len(caption_rows) > 0 and caption_rows.min() > 300 * 0.8   # only the caption differs
    with pytest.raises(ValueError, match="unknown view 'sideways'"):
        render_view(meshes, "sideways")
    # an empty scene still gives a valid image
    assert _pixels(render_view([], "iso", (100, 100))).shape == (100, 100, 3)


# --- docs ----------------------------------------------------------------------------------


@pytest.fixture(scope="module")
def index():
    return docs.build_index()


def test_docs_index_covers_the_installed_libraries(index):
    entries, versions = index
    assert versions["build123d"] == "0.13.0" and versions["bd_warehouse"] == "0.4.0"
    names = {entry.name for entry in entries}
    for expected in ("build123d.fillet", "build123d.Box", "build123d.ShapeList.filter_by",
                     "build123d.extrude", "bd_warehouse.fastener.HeatSetNut",
                     "bd_warehouse.thread.IsoThread"):
        assert expected in names, expected
    assert not any(".._" in name or "._" in name for name in names)


@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("fillet edges", "build123d.fillet"),
        ("extrude a sketch", "build123d.extrude"),
        ("filter faces by axis", "build123d.ShapeList.filter_by"),
        ("heat set insert", "bd_warehouse.fastener.HeatSetNut"),
        ("counter sink hole", "build123d.CounterSinkHole"),
    ],
)
def test_docs_search_finds_the_right_entry(index, query, expected):
    found = docs.search(index[0], query, 5)
    assert expected in [hit["name"] for hit in found]
    assert all(hit["signature"] or hit["doc"] for hit in found)
    assert [hit["score"] for hit in found] == sorted((hit["score"] for hit in found), reverse=True)


def test_docs_search_edge_cases(index):
    assert docs.search(index[0], "zzzqqqxxx", 5) == []
    assert docs.search(index[0], "!!!", 5) == []
    assert len(docs.search(index[0], "edge", 20)) == 20
    long = docs.search(index[0], "Box", 1)[0]
    assert len(long["doc"]) <= docs.MAX_DOC_CHARS + 10
    assert docs.tokens("CounterSinkHole filter_by edges") == [
        "counter", "sink", "hole", "filter", "by", "edg"]
