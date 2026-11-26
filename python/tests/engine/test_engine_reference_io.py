"""Reference files are hostile input: what the reader accepts, what it refuses, and that
what comes out is the object as printed. Every file here is made by the test itself."""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reference_stand_in import (
    RELS,
    TYPES,
    box_mesh,
    mesh_xml,
    model_xml,
    stand_in_mesh,
    write_package,
    write_plain_3mf,
    write_project_3mf,
    write_stl,
)

from hotend_engine import reference_io
from hotend_engine.context import load_reference, make_context
from hotend_engine.errors import BadRequest
from hotend_engine.reference_io import ReferenceFileError, read_reference_file
from hotend_engine.request import Context, ReferenceSpec


def package(path: Path, model: str, extra: dict | None = None) -> Path:
    return write_package(path, {"[Content_Types].xml": TYPES, "_rels/.rels": RELS,
                                "3D/3dmodel.model": model, **(extra or {})})


@pytest.mark.parametrize("ascii_", [False, True])
def test_an_stl_comes_back_as_the_mesh_that_was_written(tmp_path, ascii_):
    vertices, triangles = stand_in_mesh()
    path = write_stl(tmp_path / "figure.stl", vertices, triangles, ascii_)
    loaded = read_reference_file(path)
    assert loaded.format == "stl" and len(loaded.objects) == 1
    part = loaded.select().geometry()[0]
    assert len(part.triangles) == len(triangles)
    assert len(part.vertices) == len(vertices)  # repeated corners are one vertex again
    assert np.ptp(part.vertices, axis=0) == pytest.approx(np.ptp(vertices, axis=0), abs=1e-4)
    assert loaded.sha256 and loaded.bytes == path.stat().st_size


def test_a_plain_3mf_applies_its_unit_and_its_build_transform(tmp_path):
    vertices, triangles = box_mesh((1.0, 2.0, 3.0))
    # Scale x by 2 and move by (10, 0, 0), in inches.
    path = write_plain_3mf(tmp_path / "box.3mf", vertices, triangles,
                           transform="2 0 0 0 1 0 0 0 1 10 0 0", unit="inch")
    loaded = read_reference_file(path)
    low, high = loaded.select().bbox()
    assert low == pytest.approx((254.0, 0.0, 0.0))
    assert high - low == pytest.approx((2 * 25.4, 2 * 25.4, 3 * 25.4))
    assert loaded.unit == "inch" and loaded.producer == "HotendTests-1.0"


def test_a_slicer_project_lists_its_objects_and_never_merges_them(tmp_path):
    path = write_project_3mf(tmp_path / "project.3mf", scale=1.5)
    loaded = read_reference_file(path)
    assert [(o.index, o.name, o.plate, o.plate_name) for o in loaded.objects] == [
        (1, "figure", 1, "big"), (2, "eye", 2, "eyes"), (3, "eye", 2, "eyes"), (4, "eye", 2, "eyes")]
    with pytest.raises(ReferenceFileError, match="holds 4 objects and they are never merged"):
        loaded.select()
    with pytest.raises(ReferenceFileError, match="holds 3 objects for plate 2"):
        loaded.select(plate=2)
    with pytest.raises(ReferenceFileError, match="holds no object for plate 7"):
        loaded.select(plate=7)
    assert loaded.select(obj=3).name == "eye"

    figure = loaded.select(plate=1)
    assert figure is loaded.select(obj="figure")
    roles = {part.name: part.role for part in figure.parts}
    assert roles == {"DOME": "normal_part", "BASE": "normal_part",
                     "more infill": "modifier_part", "vent": "negative_part"}
    assert [part.name for part in figure.geometry()] == ["DOME", "BASE"]
    # As printed: the component transform (+1 in z) and the build item's scale of 1.5
    # are applied; the modifier (60 x 60 x 8 before scaling) does not count.
    low, high = figure.bbox()
    assert high - low == pytest.approx((45.0, 45.0, 22.5), abs=1e-6)
    assert low == pytest.approx((100 - 22.5, 120 - 22.5, 0.0), abs=1e-6)
    listing = figure.listing()
    assert listing["size"] == [45.0, 45.0, 22.5]
    assert [p["geometry"] for p in listing["parts"]] == [True, True, False, False]
    assert any("only change print settings" in note for note in loaded.notes)


def test_a_reference_is_the_printed_object_in_its_own_frame(tmp_path):
    write_project_3mf(tmp_path / "project.3mf", scale=1.5)
    spec = ReferenceSpec(name="figure", path="project.3mf", plate=1, kind="soft")
    reference, loaded = load_reference(spec, tmp_path)
    low = reference.bbox[0]
    assert low == pytest.approx((-22.5, -22.5, 0.0), abs=1e-6)  # stands on z = 0, centred
    assert reference.size == pytest.approx((45.0, 45.0, 22.5), abs=1e-6)
    assert reference.closed and reference.kind == "soft" and reference.provenance == "measured"
    source = reference.source
    assert source["object"] == "figure" and source["plate"] == 1 and source["format"] == "3mf"
    assert source["sha256"] == loaded.sha256 and source["objects_in_file"] == 4
    # Dome and base are united and the negative part is cut away, as they print.
    dome, base = (reference_io.MeshPart("p", "normal_part", *mesh) for mesh in (
        stand_in_mesh(radius=12.0, height=14.0), box_mesh((30.0, 30.0, 1.0))))
    united = 1.5 ** 3 * (_volume(dome) + _volume(base))
    hole = 1.5 ** 3 * 3.0 * 3.0 * 5.0  # the vent lies wholly inside the dome
    assert reference.volume == pytest.approx(united - hole, rel=1e-6)
    assert any("united into one shape" in note for note in reference.notes)
    assert any("negative part(s) were cut away" in note for note in reference.notes)

    # The request's own scale and units come on top.
    doubled, _ = load_reference(ReferenceSpec(name="big", path="project.3mf", plate=1, scale=2.0),
                                tmp_path)
    assert doubled.size == pytest.approx((90.0, 90.0, 45.0), abs=1e-6)
    write_stl(tmp_path / "inch.stl", *box_mesh((1.0, 2.0, 3.0)))
    inch, _ = load_reference(ReferenceSpec(name="inch", path="inch.stl", units="in"), tmp_path)
    assert inch.size == pytest.approx((25.4, 50.8, 76.2))

    with pytest.raises(BadRequest, match="never merged"):
        load_reference(ReferenceSpec(name="all", path="project.3mf"), tmp_path)
    ctx = make_context(Context(references=(spec,)), tmp_path)
    assert list(ctx.references) == ["figure"] and ctx.fit.references["figure"] is ctx.references["figure"]


def _volume(part) -> float:
    tri = part.vertices[part.triangles]
    return abs(float(np.einsum("ij,ij->i", tri[:, 0], np.cross(tri[:, 1], tri[:, 2])).sum())) / 6


@pytest.mark.parametrize("path", ["../outside.stl", "/etc/passwd", "sub/../../outside.stl"])
def test_a_reference_outside_the_job_folder_is_refused(tmp_path, path):
    job = tmp_path / "job"
    job.mkdir()
    write_stl(tmp_path / "outside.stl", *box_mesh((1.0, 1.0, 1.0)))
    with pytest.raises(BadRequest, match="job folder"):
        load_reference(ReferenceSpec(name="thing", path=path), job)


def test_a_symlink_is_not_followed_out_of_the_job_folder(tmp_path):
    job = tmp_path / "job"
    job.mkdir()
    target = write_stl(tmp_path / "outside.stl", *box_mesh((1.0, 1.0, 1.0)))
    (job / "link.stl").symlink_to(target)
    with pytest.raises(BadRequest, match="job folder"):
        load_reference(ReferenceSpec(name="thing", path="link.stl"), job)
    inside = write_stl(job / "inside.stl", *box_mesh((1.0, 1.0, 1.0)))
    (job / "inner.stl").symlink_to(inside)
    with pytest.raises(ReferenceFileError, match="not a plain file"):
        read_reference_file(job / "inner.stl")


BOX = mesh_xml(1, *box_mesh((1.0, 1.0, 1.0)))
ITEM = '<item objectid="1"/>'
DOCTYPE = ('<?xml version="1.0"?><!DOCTYPE model [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;">]>'
           + model_xml(BOX, ITEM).split("?>", 1)[1])
HOSTILE = {
    "doctype_and_entities": (DOCTYPE, "declares a document type or an entity"),
    "not_xml": ("<model", "not well-formed XML"),
    "missing_vertex": (model_xml('<object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/></vertices>'
                                 '<triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>', ITEM),
                       "name vertices it does not have"),
    "not_a_number": (model_xml(BOX.replace('x="1"', 'x="nan"', 1), ITEM), "not finite"),
    "huge": (model_xml(BOX.replace('x="1"', 'x="1e9"', 1), ITEM), "check the units"),
    "short_transform": (model_xml(BOX, '<item objectid="1" transform="1 0 0 0 1 0 0 0 1 0 0"/>'),
                        "not 12 finite numbers"),
    "flattening_transform": (model_xml(BOX, '<item objectid="1" transform="1 0 0 0 1 0 0 0 0 0 0 0"/>'),
                             "flattens it"),
    "missing_object": (model_xml(BOX, '<item objectid="9"/>'), "refers to an object '9'"),
    "path_escape": (model_xml('<object id="2"><components><component p:path="/../../etc/passwd" '
                              'objectid="1"/></components></object>', '<item objectid="2"/>'),
                    "outside the package"),
    "backslash_path": (model_xml('<object id="2"><components><component p:path="/3D\\\\x.model" '
                                 'objectid="1"/></components></object>', '<item objectid="2"/>'),
                       "outside the package"),
    "endless_nesting": (model_xml('<object id="2"><components><component objectid="2"/></components>'
                                  '</object>', '<item objectid="2"/>'),
                        "nests components deeper"),
    "unknown_unit": (model_xml(BOX, ITEM, unit="parsec"), "unknown unit"),
    "no_geometry": (model_xml("", ""), "holds no geometry"),
}


@pytest.mark.parametrize("name", HOSTILE)
def test_a_hostile_or_broken_3mf_is_refused_with_the_reason(tmp_path, name):
    model, message = HOSTILE[name]
    path = package(tmp_path / "bad.3mf", model)
    with pytest.raises(ReferenceFileError, match=message):
        read_reference_file(path)


def test_size_and_triangle_caps_hold_while_reading(tmp_path, monkeypatch):
    vertices, triangles = stand_in_mesh()
    stl = write_stl(tmp_path / "figure.stl", vertices, triangles)
    with pytest.raises(ReferenceFileError, match="at most 100"):
        read_reference_file(stl, max_triangles=100)
    plain = write_plain_3mf(tmp_path / "figure.3mf", vertices, triangles)
    with pytest.raises(ReferenceFileError, match="more than 100 triangles"):
        read_reference_file(plain, max_triangles=100)
    assert len(read_reference_file(plain).select().geometry()[0].triangles) == len(triangles)

    # A part that inflates to far more than it weighs in the archive (a zip bomb) is cut
    # off while it is read, whatever the archive's header says.
    padding = "<!-- " + "x" * 4_000_000 + " -->"
    bomb = package(tmp_path / "bomb.3mf", model_xml(BOX + padding, ITEM))
    assert bomb.stat().st_size < 50_000
    monkeypatch.setattr(reference_io, "MAX_ENTRY_BYTES", 1_000_000)
    with pytest.raises(ReferenceFileError, match="uncompressed; the limit is"):
        read_reference_file(bomb)
    monkeypatch.setattr(reference_io, "MAX_ENTRY_BYTES", 512 * 1024 * 1024)
    monkeypatch.setattr(reference_io, "MAX_XML_BYTES_TOTAL", 1_000_000)
    with pytest.raises(ReferenceFileError, match="unpacks to more data"):
        read_reference_file(bomb)
    many = tmp_path / "many.3mf"
    with zipfile.ZipFile(many, "w") as archive:
        for index in range(reference_io.MAX_ARCHIVE_ENTRIES + 1):
            archive.writestr(f"junk/{index}", b"")
    with pytest.raises(ReferenceFileError, match="entries"):
        read_reference_file(many)
    monkeypatch.setattr(reference_io, "MAX_FILE_BYTES", 1000)
    with pytest.raises(ReferenceFileError, match="may be at most"):
        read_reference_file(stl)


def test_files_that_are_not_what_they_say_are_refused(tmp_path):
    (tmp_path / "text.3mf").write_text("not a zip archive")
    with pytest.raises(ReferenceFileError, match="not a readable 3mf"):
        read_reference_file(tmp_path / "text.3mf")
    (tmp_path / "cut.stl").write_bytes(b"\x00" * 80 + (5).to_bytes(4, "little") + b"\x00" * 60)
    with pytest.raises(ReferenceFileError):
        read_reference_file(tmp_path / "cut.stl")
    (tmp_path / "thing.obj").write_text("v 0 0 0")
    with pytest.raises(ReferenceFileError):
        read_reference_file(tmp_path / "thing.obj")
    (tmp_path / "empty.step").write_text("ISO-10303-21;\nEND-ISO-10303-21;\n")
    with pytest.raises(ReferenceFileError):
        read_reference_file(tmp_path / "empty.step")


def test_a_step_file_is_read_as_solids(tmp_path):
    from build123d import Box, export_step

    export_step(Box(20, 10, 5), tmp_path / "block.step")
    loaded = read_reference_file(tmp_path / "block.step")
    low, high = loaded.select().bbox()
    assert high - low == pytest.approx((20.0, 10.0, 5.0), abs=1e-6)
    reference, _ = load_reference(ReferenceSpec(name="block", path="block.step"), tmp_path)
    assert reference.closed and reference.volume == pytest.approx(1000.0, rel=1e-6)


def test_the_docs_op_finds_the_capability_summaries():
    from hotend_engine.docs import build_index, search

    entries, _ = build_index()
    wanted = {
        "reference object STL 3MF file": "hotend_fit.capabilities.reference_objects",
        "dumpling holder": "hotend_fit.capabilities.reference_objects",
        "bought device phone": "hotend_fit.capabilities.bought_components",
        "phone case": "hotend_fit.capabilities.bought_components",
        "TPU flexible filament": "hotend_fit.capabilities.flexible_materials",
        "stretch cradle fit classes": "hotend_fit.capabilities.reference_fit_classes",
        "test print of a reference fit": "hotend_fit.capabilities.reference_fit_gauges",
        "rim above the screen glass stand-off": "hotend_fit.capabilities.leaning_lips_and_stand_off",
        "lip leaning over a rounded edge": "hotend_fit.capabilities.leaning_lips_and_stand_off",
    }
    for query, name in wanted.items():
        found = search(entries, query, 5)
        assert name in [entry["name"] for entry in found], query
        entry = next(e for e in found if e["name"] == name)
        assert "[...]" not in entry["doc"]  # the whole recipe fits the excerpt

    from hotend_fit import capabilities

    assert capabilities.reference_objects()["fits"][:2] == ["cradle", "stretch"]
    # A checkout may hold a component file of its own beside the sample that ships.
    offered = capabilities.bought_components()
    assert set(offered) == {"components"} and "sample_phone" in offered["components"]
    assert set(capabilities.reference_fit_classes()) == {"stretch", "cradle"}
    assert capabilities.reference_fit_gauges() == {"height": 5.0, "wall": 2.0}
    assert capabilities.leaning_lips_and_stand_off()["contract_kind"] == "standoff"
    assert any(entry["flexible"] for entry in capabilities.flexible_materials()["materials"])
