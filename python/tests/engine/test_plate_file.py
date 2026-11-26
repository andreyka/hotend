"""Plate files: the plain 3MF the slicer is given (hotend_engine.plate3mf).

A plate file once carried the viewer's part colours, Bambu Studio gave each colour a
filament of its own, and the print failed. These tests hold the fix in place:

- a plate file has the allowlisted shape and nothing else. The allowlist is the table
  `crates/hotend-slicer/tests/data/plate-shape.json`, which the slicer adapter's reader is
  tested against too, so the writer and the reader cannot drift apart;
- the only outside text in it is the piece names, which follow a fixed pattern and are the
  job's piece names, and any other name is refused before a file exists;
- the meshes are the triangles the earlier writer (lib3mf through build123d's Mesher) made,
  where the layout put them;
- equal input gives equal bytes;
- `export/model.3mf`, the file for viewing, still has its colours.
"""

from __future__ import annotations

import copy
import io
import json
import re
import zipfile
from pathlib import Path
from xml.etree import ElementTree

import lib3mf
import numpy as np
import pytest
from conftest import (
    PLATE_OPS,
    X2D_CONTEXT,
    ok,
    prepare_job,
    read_3mf,
    run_all,
    run_worker,
)

from hotend_engine import plate3mf

TABLE_PATH = (Path(__file__).resolve().parents[3] / "crates" / "hotend-slicer" / "tests"
              / "data" / "plate-shape.json")
TABLE = json.loads(TABLE_PATH.read_text(encoding="utf-8"))
ARCHIVE, MODEL = TABLE["archive"], TABLE["model"]

# Words of things a slicer could read a setting from. The allowlist walk below already
# excludes them; this list is a second, cruder look at the raw text.
FORBIDDEN_WORDS = (
    b"color", b"colour", b"material", b"metadata", b"property", b"pid", b"pindex", b"uuid",
    b"component", b"transform", b"texture", b"thumbnail", b"partnumber", b"slice",
)

# One tag of the model part as the writer may spell it: a name, then attributes in double
# quotes. Whatever does not match is a failure, so nothing is skipped over.
TAG = re.compile(r'<(/?)([^\s<>/="]+)((?: [^\s<>/="]+="[^"<>&]*")*)(/?)>')
ATTRIBUTE = re.compile(r' ([^\s<>/="]+)="([^"<>&]*)"')
DECLARATION = '<?xml version="1.0" encoding="UTF-8"?>\n'


class Element:
    def __init__(self, name: str, attributes: dict[str, str]) -> None:
        self.name, self.attributes, self.children = name, attributes, []


def parse_model(data: bytes) -> Element:
    """The model part as a tree, read tag by tag. It fails on anything the table's syntax
    rules forbid: a byte order mark, another encoding, a DOCTYPE, an entity, a processing
    instruction, CDATA, a comment, a reference, a prefixed element, text between tags."""
    assert set(MODEL["syntax"].values()) == {False}
    assert MODEL["encoding"] == "UTF-8"
    assert len(data) <= ARCHIVE["max_model_bytes"]
    text = data.decode("ascii")  # plain ASCII is UTF-8 without a byte order mark
    assert text.startswith(DECLARATION)
    body = text[len(DECLARATION):]
    assert "<?" not in body and "<!" not in body and "&" not in body
    root, stack, position = None, [], 0
    for match in TAG.finditer(body):
        assert body[position:match.start()].strip() == "", body[position:match.start()][:80]
        position = match.end()
        closing, name, raw_attributes, empty = match.groups()
        assert ":" not in name, f"prefixed element {name}"
        if closing:
            assert not raw_attributes and not empty and stack.pop().name == name
            continue
        pairs = ATTRIBUTE.findall(raw_attributes)
        assert "".join(f' {key}="{value}"' for key, value in pairs) == raw_attributes
        assert len({key for key, _ in pairs}) == len(pairs), f"{name} repeats an attribute"
        element = Element(name, dict(pairs))
        if stack:
            stack[-1].children.append(element)
        else:
            assert root is None, "two root elements"
            root = element
        if not empty:
            stack.append(element)
    assert body[position:].strip() == "" and not stack and root is not None
    # A second opinion on the namespace: a real XML parser sees every element in the core
    # 3MF namespace and no other.
    for node in ElementTree.fromstring(data).iter():
        assert node.tag.startswith("{" + MODEL["namespace"] + "}"), node.tag
    return root


def walk(element: Element):
    yield element
    for child in element.children:
        yield from walk(child)


def check_shape(root: Element) -> dict[str, tuple[np.ndarray, np.ndarray]]:
    """Hold a parsed model part against the table. Returns its objects as they are built:
    name -> (vertices, triangles), in file order."""
    structure, values = MODEL["structure"], MODEL["values"]
    assert root.name == MODEL["root"]
    assert root.attributes["xmlns"] == MODEL["namespace"]
    for element in walk(root):
        assert element.name in MODEL["elements"], element.name
        rule = structure[element.name]
        assert set(element.attributes) <= set(rule["attributes"]), element.name
        assert set(element.attributes) <= set(MODEL["attributes"]), element.name
        assert set(rule["required"]) <= set(element.attributes), element.name
        names = [child.name for child in element.children]
        expected = []
        for child in rule["children"]:
            count = names.count(child["element"])
            assert count == 1 if child["count"] == "one" else count >= 1, (element.name, names)
            expected += [child["element"]] * count
        assert names == expected, f"{element.name} holds {names[:6]}"
        for key in ("unit", "xml:lang", "type"):
            if key in element.attributes:
                assert element.attributes[key] in values[key], (element.name, key)

    resources, build = root.children
    objects, ids = {}, {}
    for entry in resources.children:
        attributes = entry.attributes
        assert re.fullmatch(values["id"]["pattern"], attributes["id"])
        assert re.fullmatch(values["name"]["pattern"], attributes["name"]), attributes["name"]
        assert attributes["id"] not in ids and attributes["name"] not in objects
        vertices_node, triangles_node = entry.children[0].children
        for vertex in vertices_node.children:
            for axis in values["coordinate"]["attributes"]:
                assert re.fullmatch(values["coordinate"]["pattern"], vertex.attributes[axis]), \
                    vertex.attributes[axis]
        for triangle in triangles_node.children:
            for corner in values["index"]["attributes"]:
                assert re.fullmatch(values["index"]["pattern"], triangle.attributes[corner])
        vertices = np.array([[float(v.attributes[axis]) for axis in "xyz"]
                             for v in vertices_node.children])
        triangles = np.array([[int(t.attributes[corner]) for corner in ("v1", "v2", "v3")]
                              for t in triangles_node.children])
        assert np.abs(vertices).max() <= values["coordinate"]["limit_mm"]
        assert triangles.min() >= 0 and triangles.max() < len(vertices)
        assert (triangles[:, 0] != triangles[:, 1]).all()
        assert (triangles[:, 1] != triangles[:, 2]).all()
        assert (triangles[:, 2] != triangles[:, 0]).all()
        ids[attributes["id"]] = attributes["name"]
        objects[attributes["name"]] = (vertices, triangles)
    # Every object is built exactly once, in the order of the objects.
    assert [item.attributes["objectid"] for item in build.children] == list(ids)
    return objects


def read_plate(path: Path | io.BytesIO) -> dict[str, tuple[np.ndarray, np.ndarray]]:
    """Open a plate file the strict way: the three entries, the two constant parts, and a
    model part of the allowlisted shape. Returns name -> (vertices, triangles)."""
    with zipfile.ZipFile(path) as archive:
        assert archive.namelist() == ARCHIVE["entries"]
        for name, text in ARCHIVE["constant_parts"].items():
            assert archive.read(name) == text.encode("ascii"), name
        model = archive.read(ARCHIVE["model_part"])
    lowered = model.lower()
    for word in FORBIDDEN_WORDS:
        assert word not in lowered, word
    assert model.count(b"xmlns") == 1
    return check_shape(parse_model(model))


def read_lib3mf(path: Path) -> dict[str, tuple[np.ndarray, np.ndarray]]:
    """The named mesh objects of a 3MF as lib3mf reads them: name -> (vertices, triangles),
    in the file's own coordinates."""
    model = lib3mf.get_wrapper().CreateModel()
    reader = model.QueryReader("3mf")
    reader.ReadFromFile(str(path))
    assert reader.GetWarningCount() == 0
    meshes = {}
    iterator = model.GetMeshObjects()
    while iterator.MoveNext():
        mesh = iterator.GetCurrentMeshObject()
        meshes[mesh.GetName()] = (
            np.array([v.Coordinates[:] for v in mesh.GetVertices()], dtype=float),
            np.array([t.Indices[:] for t in mesh.GetTriangleIndices()], dtype=int))
    return meshes


def volume(vertices: np.ndarray, triangles: np.ndarray) -> float:
    a, b, c = (vertices[triangles[:, i]] for i in range(3))
    return float(np.einsum("ij,ij->i", a, np.cross(b, c)).sum() / 6.0)


def cube(low=(0.0, 0.0, 0.0), size: float = 10.0) -> tuple[np.ndarray, np.ndarray]:
    """A closed cube, counter-clockwise seen from outside."""
    corners = np.array([[x, y, z] for z in (0, 1) for y in (0, 1) for x in (0, 1)], dtype=float)
    triangles = np.array([
        [0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4],
        [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]])
    return corners * size + np.asarray(low, dtype=float), triangles


def piece(name: str, low=(0.0, 0.0, 0.0), size: float = 10.0) -> plate3mf.Piece:
    return plate3mf.Piece(name, *cube(low, size))


PLATE_JOBS = ("prepare_cable_box", "prepare_twelve_lids", "gauge_of_model", "gauge_of_cards")
# A card may be named with capitals, spaces and dashes; its gauge pieces may not.
HOSTILE_CARD = {"op": "gauge", "views": [], "cards": [
    {"card": "peg_hole", "name": "Lid Hinge-1",
     "args": {"diameter": 6.0, "length": 6.0, "fit": "slide"}}]}


UNNAMEABLE_CARD = {"card": "snap_lid", "name": "Lid; M104 S300",
                   "args": {"opening": [40, 30], "wall": 2.0}}


@pytest.fixture(scope="module")
def more(tmp_path_factory, builds, plates) -> dict:
    """Worker runs of this file's own: a gauge whose card name is no piece name, with and
    without a plate file, and the cable box prepared a second time."""
    root = tmp_path_factory.mktemp("plate_file")
    base = {"protocol": 0, "context": copy.deepcopy(X2D_CONTEXT)}
    built, request = PLATE_OPS["prepare_cable_box"]
    specs = {
        "hostile": (prepare_job(root / "hostile", {**base, **HOSTILE_CARD}), False),
        "again": (prepare_job(root / "again", {**base, **request},
                              copy_from=builds[built].dir), False),
        # Text that no card name and no card kind may be.
        "unnameable": (prepare_job(root / "unnameable", {**base, "op": "gauge", "views": [],
                                                         "cards": [UNNAMEABLE_CARD]}), False),
        "unnameable_no_file": (
            prepare_job(root / "unnameable_no_file", {
                **base, "op": "gauge", "views": [], "threemf": False,
                "cards": [UNNAMEABLE_CARD]}), False),
        "unknown_kind": (prepare_job(root / "unknown_kind", {
            **base, "op": "gauge", "views": [], "cards": [
                {"card": "peg_hole", "args": {"diameter": 6.0, "length": 6.0, "fit": "slide"}},
                {"card": "Lid; M104 S300", "args": {}}]}), False),
    }
    jobs = run_all(specs)
    # The core always asks for the plate file (`GaugeRequest::threemf`), so it never gets this
    # answer; it would refuse the piece names in it (`is_piece_name`).
    folder = prepare_job(root / "hostile_no_file", {**base, **HOSTILE_CARD, "threemf": False})
    jobs["hostile_no_file"] = run_worker(folder, core_reads=False)
    return jobs


def plate_files(job):
    result = ok(job)
    assert result["plates"]
    for plate in result["plates"]:
        assert plate["threemf"] == f"plates/{plate['index']}/plate.3mf"
        yield plate, job.dir / plate["threemf"]


# --- the table and the writer ------------------------------------------------------------


def test_the_table_is_the_documented_allowlist():
    """Pinned here on purpose: the allowlist grows only by a change to this test too."""
    assert TABLE["schema"] == 1
    assert ARCHIVE["entries"] == ["[Content_Types].xml", "_rels/.rels", "3D/3dmodel.model"]
    assert set(ARCHIVE["constant_parts"]) == set(ARCHIVE["entries"]) - {ARCHIVE["model_part"]}
    assert ARCHIVE["max_model_bytes"] == 64 * 1024 * 1024
    assert MODEL["elements"] == ["model", "resources", "object", "mesh", "vertices", "vertex",
                                 "triangles", "triangle", "build", "item"]
    assert MODEL["attributes"] == ["unit", "xml:lang", "xmlns", "id", "type", "name", "x", "y",
                                   "z", "v1", "v2", "v3", "objectid"]
    assert set(MODEL["structure"]) == set(MODEL["elements"])
    used = {name for rule in MODEL["structure"].values() for name in rule["attributes"]}
    assert used == set(MODEL["attributes"])
    values = MODEL["values"]
    assert values["unit"] == ["millimeter"] and values["type"] == ["model"]
    assert values["name"]["pattern"] == "^[a-z][a-z0-9_.]{0,63}$"
    assert values["coordinate"]["limit_mm"] == 1000.0 and values["box_tolerance_mm"] == 0.01


def test_the_writer_and_the_table_agree():
    assert list(plate3mf.ENTRIES) == ARCHIVE["entries"]
    assert plate3mf.MODEL_NAME == ARCHIVE["model_part"]
    parts = ARCHIVE["constant_parts"]
    assert plate3mf.CONTENT_TYPES == parts[plate3mf.CONTENT_TYPES_NAME].encode("ascii")
    assert plate3mf.RELATIONSHIPS == parts[plate3mf.RELATIONSHIPS_NAME].encode("ascii")
    assert plate3mf.MAX_MODEL_BYTES == ARCHIVE["max_model_bytes"]
    assert plate3mf.CORE_NAMESPACE == MODEL["namespace"]
    values = MODEL["values"]
    assert [plate3mf.UNIT] == values["unit"] and [plate3mf.OBJECT_TYPE] == values["type"]
    assert [plate3mf.LANGUAGE] == values["xml:lang"]
    assert "^" + plate3mf.NAME_PATTERN + "$" == values["name"]["pattern"]
    assert plate3mf.COORDINATE_LIMIT_MM == values["coordinate"]["limit_mm"]
    # The package parts are well-formed and name the model part.
    for data in (plate3mf.CONTENT_TYPES, plate3mf.RELATIONSHIPS):
        ElementTree.fromstring(data)
    assert b'Target="/' + plate3mf.MODEL_NAME.encode() + b'"' in plate3mf.RELATIONSHIPS


def test_a_written_file_has_the_allowlisted_shape_and_the_meshes_it_was_given(tmp_path):
    pieces = [piece("lid"), piece("lid.2", (20.0, 0.0, 0.0)),
              piece("peg_hole_1.peg", (0.123456, 987.654321, 0.0), 2.5)]
    path = tmp_path / "plate.3mf"
    plate3mf.write_plate(pieces, path)
    objects = read_plate(path)
    assert list(objects) == ["lid", "lid.2", "peg_hole_1.peg"]
    for given in pieces:
        vertices, triangles = objects[given.name]
        assert np.array_equal(triangles, given.triangles)
        assert vertices == pytest.approx(given.vertices, abs=5e-7)
        assert plate3mf.closed_and_oriented(triangles)
    # lib3mf, the reference reader of the format, reads it without a warning.
    for name, found in read_3mf(path).items():
        size = 2.5 if name.startswith("peg") else 10.0
        assert found["volume"] == pytest.approx(size ** 3, rel=1e-5)


@pytest.mark.parametrize(("value", "text"), [
    (0.0, "0"), (-0.0, "0"), (-1e-9, "0"), (4e-7, "0"), (1.5, "1.5"), (100.0, "100"),
    (0.1234564, "0.123456"), (-12.3400001, "-12.34"), (255.9999996, "256"), (-1000.0, "-1000"),
    (1e-6, "0.000001"),
])
def test_coordinates_are_plain_decimals(value, text):
    assert plate3mf._decimal(value) == text
    assert re.fullmatch(MODEL["values"]["coordinate"]["pattern"], text)


def test_a_surface_is_closed_when_every_edge_is_used_once_each_way():
    _, triangles = cube()
    assert plate3mf.closed_and_oriented(triangles)
    assert not plate3mf.closed_and_oriented(triangles[:-1])          # a hole
    flipped = triangles.copy()
    flipped[0] = flipped[0][::-1]
    assert not plate3mf.closed_and_oriented(flipped)                 # one face inside out
    assert not plate3mf.closed_and_oriented(np.vstack([triangles, triangles[:1]]))
    assert not plate3mf.closed_and_oriented(np.zeros((0, 3), dtype=int))


# --- what the worker writes ----------------------------------------------------------------


@pytest.mark.parametrize("name", PLATE_JOBS)
def test_every_plate_file_holds_only_the_allowlisted_shape(plates, name):
    """Every zip entry, element, attribute and value of every plate the worker writes, for
    a prepared model, copies split over plates, and the gauges of a model and of cards."""
    box = MODEL["values"]["box_tolerance_mm"]
    for plate, path in plate_files(plates[name]):
        objects = read_plate(path)
        # The names are the job's piece names, in the job's order, and nothing else.
        assert list(objects) == [item["name"] for item in plate["items"]]
        for item in plate["items"]:
            assert re.fullmatch(MODEL["values"]["name"]["pattern"], item["name"])
            vertices, triangles = objects[item["name"]]
            # No transform: the vertices are where the piece lies on the bed.
            assert vertices.min(axis=0) == pytest.approx(item["bbox"]["min"], abs=box)
            assert vertices.max(axis=0) == pytest.approx(item["bbox"]["max"], abs=box)
            assert vertices[:, 2].min() == pytest.approx(0.0, abs=1e-6)
            assert plate3mf.closed_and_oriented(triangles)
        assert not [w for w in ok(plates[name])["warnings"] if "3mf" in w]


def test_a_plate_file_has_no_colour_material_property_or_metadata(plates):
    """The failure this file exists for, looked at on its own: two parts with two viewer
    colours give a plate without any resource but the two meshes."""
    job = plates["prepare_cable_box"]
    (plate, path), = plate_files(job)
    assert len(plate["items"]) == 2
    with zipfile.ZipFile(path) as archive:
        assert len(archive.infolist()) == 3
        model = ElementTree.fromstring(archive.read("3D/3dmodel.model"))
    space = "{" + MODEL["namespace"] + "}"
    resources = model.find(space + "resources")
    assert [child.tag for child in resources] == [space + "object"] * 2
    for entry in resources:
        assert set(entry.attrib) == {"id", "type", "name"}
    assert model.find(space + "metadata") is None
    assert set(model.attrib) == {"unit", "{http://www.w3.org/XML/1998/namespace}lang"}
    assert [set(item.attrib) for item in model.find(space + "build")] == [{"objectid"}] * 2


def test_the_plate_meshes_are_the_built_meshes_moved_to_the_bed(builds, plates):
    """Against the build's own 3MF, which lib3mf still writes: every piece of the plate
    has the built mesh's vertices, one for one, under the item's transform, as many
    triangles, and the same volume. (The mesher cuts a flat face into triangles a little
    differently from one placement to the next, so the triangles themselves are compared
    with the earlier writer further down, on shapes in the same place. lib3mf stores single
    precision, hence the tolerance.)"""
    built = ok(builds["cable_box_asked"])
    as_built = read_lib3mf(builds["cable_box_asked"].dir / built["artifacts"]["threemf"])
    volumes = {part["name"]: part["volume"] for part in built["parts"]}
    for name in ("prepare_cable_box", "prepare_twelve_lids"):
        for plate, path in plate_files(plates[name]):
            objects = read_plate(path)
            for item in plate["items"]:
                vertices, triangles = objects[item["name"]]
                old_vertices, old_triangles = as_built[item["part"]]
                assert len(triangles) == len(old_triangles)
                matrix = np.array(item["transform"]).reshape(3, 4)
                moved = old_vertices @ matrix[:, :3].T + matrix[:, 3]
                assert vertices == pytest.approx(moved, abs=1e-4)
                assert volume(vertices, triangles) == pytest.approx(
                    volume(old_vertices, old_triangles), rel=1e-6)
                assert volume(vertices, triangles) == pytest.approx(
                    volumes[item["part"]], rel=2e-3)


def test_equal_requests_give_equal_bytes(plates, more):
    first = plates["prepare_cable_box"].dir / "plates/1/plate.3mf"
    again = more["again"]
    ok(again)
    assert (again.dir / "plates/1/plate.3mf").read_bytes() == first.read_bytes()
    confined = plates.get("prepare_cable_box_confined")
    if confined is not None:
        assert (confined.dir / "plates/1/plate.3mf").read_bytes() == first.read_bytes()
    # Nothing in the archive tells when or where it was written.
    with zipfile.ZipFile(first) as archive:
        for info in archive.infolist():
            assert info.date_time == (1980, 1, 1, 0, 0, 0)
            assert info.create_system == 3 and info.external_attr == 0o100644 << 16
            assert info.extra == b"" and info.comment == b""
        assert archive.comment == b""


def test_the_viewing_3mf_keeps_its_colours(builds):
    job = builds["cable_box_asked"]
    result = ok(job)
    colours = {part["name"]: part["color"] for part in result["parts"]}
    assert len(set(colours.values())) == 2
    with zipfile.ZipFile(job.dir / result["artifacts"]["threemf"]) as archive:
        model = ElementTree.fromstring(archive.read("3D/3dmodel.model"))
    space = "{" + MODEL["namespace"] + "}"
    shown = {}
    for group in model.iter(space + "basematerials"):
        (base,) = group
        shown[group.get("id")] = base.get("displaycolor")
    named = [entry for entry in model.iter(space + "object") if entry.get("name")]
    assert {entry.get("name") for entry in named} == set(colours)
    for entry in named:
        # #rrggbb in the result, #RRGGBBAA in the file.
        assert shown[entry.get("pid")].lower() == colours[entry.get("name")] + "ff"


# --- the earlier writer, side by side ----------------------------------------------------


def test_the_meshes_are_the_triangles_the_earlier_writer_made(tmp_path):
    """The same placed shapes through the earlier path (lib3mf, with a colour each) and
    through the plate writer: equal triangle counts, the same triangles index for index,
    equal volumes and boxes, and the same place on the bed."""
    from build123d import Align, Axis, Box, Cylinder, Location, Sphere, fillet

    from hotend_engine.export import write_3mf, write_plate_3mf
    from hotend_engine.parts import NamedPart

    bottom = (Align.CENTER, Align.CENTER, Align.MIN)
    ring = Cylinder(30, 10, align=bottom) - Cylinder(28, 10, align=bottom)
    block = Box(40, 30, 20, align=bottom) - Cylinder(5, 20, align=bottom)
    block = fillet(block.edges().filter_by(Axis.Z), 2)
    placed = {
        "ring": Location((100, 128, 0)) * ring,
        "block.2": Location((45, 20, 0), (0, 0, 90)) * block,   # a quarter turn, as plated
        "ball": Location((200, 50, 10)) * Sphere(10),
    }
    old_path, new_path = tmp_path / "old.3mf", tmp_path / "new.3mf"
    palette = ("#4c78a8", "#f58518", "#54a24b")
    assert write_3mf([NamedPart(name, shape, colour)
                      for (name, shape), colour in zip(placed.items(), palette, strict=True)],
                     old_path) == []
    assert write_plate_3mf(list(placed.items()), new_path) == []

    old, new = read_lib3mf(old_path), read_plate(new_path)
    assert list(new) == list(placed) and set(old) == set(placed)
    summary_old, summary_new = read_3mf(old_path), read_3mf(new_path)
    for name, shape in placed.items():
        (old_vertices, old_triangles), (vertices, triangles) = old[name], new[name]
        assert len(triangles) == len(old_triangles) > 100
        assert np.array_equal(triangles, old_triangles)
        assert vertices == pytest.approx(old_vertices, abs=5e-5)
        assert summary_new[name]["volume"] == pytest.approx(summary_old[name]["volume"], rel=1e-6)
        box = shape.bounding_box()
        for found in (summary_old[name], summary_new[name]):
            assert found["min"] == pytest.approx(tuple(box.min), abs=0.02)
            assert found["max"] == pytest.approx(tuple(box.max), abs=0.02)
        assert vertices[:, 2].min() == pytest.approx(0.0, abs=1e-6)
    # The earlier file is what must not reach a slicer: one colour per part.
    with zipfile.ZipFile(old_path) as archive:
        assert archive.read("3D/3dmodel.model").count(b"displaycolor") == 3
    # Equal shapes, equal bytes; lib3mf's file differs from run to run.
    again = tmp_path / "again.3mf"
    write_plate_3mf(list(placed.items()), again)
    assert again.read_bytes() == new_path.read_bytes()


def test_an_open_surface_is_written_with_a_note(tmp_path):
    from build123d import Box

    from hotend_engine.export import write_plate_3mf

    face = Box(10, 10, 10).faces().sort_by()[0]
    (note,) = write_plate_3mf([("sheet", face)], tmp_path / "sheet.3mf")
    assert note.startswith("3mf: ") and "'sheet'" in note and "not a closed surface" in note
    assert list(read_plate(tmp_path / "sheet.3mf")) == ["sheet"]


# --- refusals ------------------------------------------------------------------------------

HOSTILE_NAMES = [
    "", "Lid", "lid 2", "lid-2", "2lid", "_lid", ".lid", "lid\n", "lid\r\nM104 S300",
    "lid\x00", "../lid", "lid/2", "lid\\2", 'lid" pid="1', "lid<object>", "lid&amp;",
    "lid;G28", "l\u0456d", "lid\u200b", "l" * 65, "lid.2\t", " lid", None, 7, b"lid",
    ["lid"],
]


@pytest.mark.parametrize("name", HOSTILE_NAMES, ids=repr)
def test_hostile_piece_names_are_refused_before_anything_is_written(tmp_path, name):
    from hotend_engine.export import write_plate_3mf

    path = tmp_path / "plate.3mf"
    good = cube()
    for pieces in ([plate3mf.Piece(name, *good)],
                   [piece("lid"), plate3mf.Piece(name, *good)]):
        with pytest.raises(plate3mf.PlateFileError) as refused:
            plate3mf.write_plate(pieces, path)
        # The refusal names the rule and the position, never the text it refused.
        message = str(refused.value)
        assert "lower-case" in message and f"piece {len(pieces)}" in message
        if isinstance(name, str) and len(name) > 3:
            assert name not in message
        with pytest.raises(plate3mf.PlateFileError):
            plate3mf.plate_bytes(pieces)
    # The export function refuses before it looks at a shape (None would crash a mesher).
    with pytest.raises(plate3mf.PlateFileError):
        write_plate_3mf([("lid", None), (name, None)], path)
    assert list(tmp_path.iterdir()) == []


def test_names_at_the_limits_of_the_pattern_are_written(tmp_path):
    names = ["a", "l" * 64, "peg_hole_1.peg", "lid.12", "a.b.c", "x9__.."]
    path = tmp_path / "plate.3mf"
    plate3mf.write_plate([piece(name, (15.0 * index, 0.0, 0.0))
                          for index, name in enumerate(names)], path)
    assert list(read_plate(path)) == names


def test_two_pieces_of_one_name_are_refused(tmp_path):
    path = tmp_path / "plate.3mf"
    with pytest.raises(plate3mf.PlateFileError, match="same name"):
        plate3mf.write_plate([piece("lid"), piece("lid", (20.0, 0.0, 0.0))], path)
    with pytest.raises(plate3mf.PlateFileError, match="at least one piece"):
        plate3mf.write_plate([], path)
    assert not path.exists()


def _broken_meshes():
    vertices, triangles = cube()
    far = vertices.copy()
    far[0, 0] = 1000.5
    not_a_number = vertices.copy()
    not_a_number[3, 2] = np.nan
    endless = vertices.copy()
    endless[3, 2] = np.inf
    beyond = triangles.copy()
    beyond[0, 0] = 8
    negative = triangles.copy()
    negative[0, 0] = -1
    collapsed = triangles.copy()
    collapsed[0] = [1, 1, 2]
    return {
        "too far from the origin": (far, triangles),
        "not a number": (not_a_number, triangles),
        "infinite": (endless, triangles),
        "an index past the vertices": (vertices, beyond),
        "a negative index": (vertices, negative),
        "a triangle on one vertex": (vertices, collapsed),
        "indices that are not whole numbers": (vertices, triangles.astype(float)),
        "no triangles": (vertices, np.zeros((0, 3), dtype=int)),
        "no vertices": (np.zeros((0, 3)), np.zeros((0, 3), dtype=int)),
        "flat vertices": (vertices[:, :2], triangles),
        "text for coordinates": ([["1", "2", "x"]] * 3, [[0, 1, 2]]),
        "a whole number too large for a coordinate": (
            [[10 ** 400, 0, 0], [1, 0, 0], [0, 1, 0]], [[0, 1, 2]]),
        "an index too large for any mesh": (vertices.tolist(), [[0, 1, 2 ** 70]]),
    }


@pytest.mark.parametrize("case", list(_broken_meshes()))
def test_a_mesh_that_is_not_the_allowlisted_shape_is_refused(tmp_path, case):
    vertices, triangles = _broken_meshes()[case]
    path = tmp_path / "plate.3mf"
    with pytest.raises(plate3mf.PlateFileError, match="piece 2"):
        plate3mf.write_plate([piece("lid"), plate3mf.Piece("box", vertices, triangles)], path)
    assert not path.exists()


def test_a_model_part_over_the_cap_is_refused(tmp_path, monkeypatch):
    path = tmp_path / "plate.3mf"
    size = len(zipfile.ZipFile(io.BytesIO(plate3mf.plate_bytes([piece("lid")])))
               .read(plate3mf.MODEL_NAME))
    monkeypatch.setattr(plate3mf, "MAX_MODEL_BYTES", size)
    plate3mf.write_plate([piece("lid")], path)                       # exactly at the cap
    path.unlink()
    monkeypatch.setattr(plate3mf, "MAX_MODEL_BYTES", size - 1)
    with pytest.raises(plate3mf.PlateFileError, match="fewer"):
        plate3mf.write_plate([piece("lid")], path)
    assert not path.exists()


def test_a_plate_far_over_the_cap_is_refused_before_its_text_is_made(tmp_path, monkeypatch):
    # A mesh whose shortest possible text is already over the cap: the refusal must not
    # cost the time and the memory of writing the text out first.
    count = 200_000
    vertices = np.zeros((count, 3))
    vertices[:, 0] = np.arange(count) / 1000.0
    triangles = np.stack([np.arange(count - 2), np.arange(1, count - 1),
                          np.arange(2, count)], axis=1)
    monkeypatch.setattr(plate3mf, "MAX_MODEL_BYTES", 1024 * 1024)

    def never(*_args, **_kwargs):
        raise AssertionError("the text of a plate that cannot fit was being made")

    monkeypatch.setattr(plate3mf, "_decimal", never)
    path = tmp_path / "plate.3mf"
    with pytest.raises(plate3mf.PlateFileError, match="fewer"):
        plate3mf.write_plate([piece("lid"), plate3mf.Piece("box", vertices, triangles)], path)
    assert not path.exists()


def test_the_text_of_a_plate_stops_being_made_once_it_is_over_the_cap(monkeypatch):
    # Under the cap by the count of its lines, over it by the digits of its coordinates:
    # the writer notices while it writes, not after it has made the whole text.
    count = 30_000
    vertices = np.full((count, 3), -999.123456)
    triangles = np.stack([np.zeros(count - 2, dtype=int), np.arange(1, count - 1),
                          np.arange(2, count)], axis=1)
    shortest = 28 * count + 33 * (count - 2)
    monkeypatch.setattr(plate3mf, "MAX_MODEL_BYTES", shortest + 200_000)
    made = []
    decimal = plate3mf._decimal

    def counted(value):
        made.append(1)
        return decimal(value)

    monkeypatch.setattr(plate3mf, "_decimal", counted)
    with pytest.raises(plate3mf.PlateFileError, match="fewer"):
        plate3mf.plate_bytes([plate3mf.Piece("box", vertices, triangles)])
    # About 200 kB of extra digits are allowed, 33 bytes per vertex: it stops soon after.
    assert 3 * 6_000 < len(made) < 3 * count


def test_the_worker_refuses_a_gauge_whose_pieces_cannot_be_named(more):
    """A card named `Lid Hinge-1` would give the pieces `Lid Hinge-1.peg` and `.hole`. The
    job stops as a bad request before a plate folder exists, and says what to do without
    repeating the name."""
    job = more["hostile"]
    assert job.returncode == 2
    error = job.result["error"]
    assert job.result["status"] == "internal_error" and error["type"] == "bad_request"
    assert "lower-case" in error["message"] and "feature card" in error["message"]
    assert "Lid" not in error["message"] and "Hinge" not in error["message"]
    assert "Lid Hinge" not in job.stderr
    assert not (job.dir / "plates").exists()
    assert job.files() == {"request.json", "result.json"}


def test_without_a_plate_file_the_same_gauge_answers_as_before(more):
    job = more["hostile_no_file"]
    result = ok(job)
    (plate,) = result["plates"]
    assert "threemf" not in plate
    names = sorted(item["name"] for item in plate["items"])
    assert names == ["Lid Hinge-1.hole", "Lid Hinge-1.peg"]
    assert not list(job.dir.rglob("*.3mf"))


@pytest.mark.parametrize(("name", "rule"), [
    ("unnameable", "name is not allowed"),
    ("unnameable_no_file", "name is not allowed"),
    ("unknown_kind", "card 2 is not a known card"),
])
def test_a_refused_card_is_named_by_its_place_not_by_its_text(more, name, rule):
    """The words of a request that is refused are not repeated in the refusal: a card name
    or kind may hold anything, and the message is shown and stored."""
    job = more[name]
    assert job.returncode == 2
    error = job.result["error"]
    assert job.result["status"] == "internal_error" and error["type"] == "bad_request"
    assert rule in error["message"]
    for text in (error["message"], job.stderr):
        assert "M104" not in text and "Lid" not in text
    assert job.files() == {"request.json", "result.json"}
