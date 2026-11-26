"""The `measure` op's selectors and distances, in process (the op through the worker is in
test_engine_worker_ops.py)."""

from __future__ import annotations

import base64

import numpy as np
import pytest
from build123d import Align, Box, Cone, Pos, Sphere, fillet

from hotend_engine.measure import run_queries
from hotend_engine.parts import NamedPart
from hotend_engine.tessellate import _transform, mesh_edges, tessellate

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)


def part(shape, name: str = "p") -> NamedPart:
    return NamedPart(name, shape, "#4c78a8")


def mesh_json_edges(shape) -> list[np.ndarray]:
    """The polyline of every edge of mesh.json, in mesh.json's order, in world coordinates."""
    tessellation = tessellate([part(shape)])
    assert tessellation.meshes["p"].edge_map_ok
    node = tessellation.viewer["shapes"]["parts"][0]
    rotation, translation = _transform(node.get("loc"))
    instance = tessellation.viewer["instances"][node["shape"]["ref"]]

    def decode(key: str, dtype: str) -> np.ndarray:
        return np.frombuffer(base64.b64decode(instance[key]["buffer"]), dtype=dtype)

    segments = decode("edges", "<f4").reshape(-1, 2, 3).astype(float) @ rotation.T + translation
    polylines, start = [], 0
    for count in decode("segments_per_edge", "<i4"):
        polylines.append(segments[start:start + count].reshape(-1, 3))
        start += count
    return polylines


def distance(parts: list[NamedPart], a: dict, b: dict) -> dict:
    (answer,) = run_queries(parts, ({"kind": "distance", "a": a, "b": b},))
    return answer


# name -> (shape, edges in mesh.json, edges build123d's shape.edges() lists)
SHAPES = {
    "box": (lambda: Box(10, 10, 10), 12, 12),
    "sphere": (lambda: Sphere(5), 3, 1),
    "cone": (lambda: Cone(5, 0, 8), 3, 2),
    "filleted box": (lambda: fillet(Box(30, 20, 10, align=BOTTOM).edges(), 2), 56, 48),
    "box with ball": (lambda: Box(30, 20, 10, align=BOTTOM) + Pos(0, 0, 10) * Sphere(4), 15, 14),
}


@pytest.mark.parametrize("name", SHAPES)
def test_an_edge_index_selects_the_edge_mesh_json_shows_under_that_index(name):
    """mesh.json numbers the degenerate edges build123d's `shape.edges()` drops (sphere
    poles, cone tips, the corner patches of a filleted box). An index picked in the viewer
    must measure that very edge, whatever comes before it in the list."""
    make, in_mesh, in_build123d = SHAPES[name]
    shape = make()
    polylines = mesh_json_edges(shape)
    assert len(polylines) == len(mesh_edges(shape)) == in_mesh
    assert len(shape.edges()) == in_build123d
    degenerate = 0
    for index, polyline in enumerate(polylines):
        middle = [float(c) for c in polyline[len(polyline) // 2]]
        answer = distance([part(shape)], {"part": "p", "edge": index}, {"point": middle})
        if answer["ok"]:
            # a point of mesh.json's edge i lies on the edge that index i selects
            assert answer["distance"] == pytest.approx(0.0, abs=1e-3), (index, answer)
        else:
            assert "degenerate edge" in answer["error"], answer
            degenerate += 1
    # every index of mesh.json is answered: none is refused as out of range
    assert degenerate == in_mesh - in_build123d
    beyond = distance([part(shape)], {"part": "p", "edge": in_mesh}, {"point": [0, 0, 0]})
    assert not beyond["ok"] and f"0 to {in_mesh - 1}" in beyond["error"]


def test_a_part_buried_in_another_part_is_not_reported_as_apart():
    block, core = part(Box(30, 30, 30), "block"), part(Box(5, 5, 5), "core")
    answer = distance([block, core], {"part": "block"}, {"part": "core"})
    # The boundaries are 12.5 mm apart; the parts occupy the same space.
    assert answer["ok"] and answer["distance"] == 0.0 and answer["inside"] is True
    assert answer["overlap_volume"] == pytest.approx(125.0)
    swapped = distance([block, core], {"part": "core"}, {"part": "block"})
    assert swapped["distance"] == 0.0 and swapped["overlap_volume"] == pytest.approx(125.0)
    # Partly overlapping parts: the boundaries cross (distance 0 anyway), the overlap is given.
    shifted = part(Pos(14, 0, 0) * Box(5, 5, 5), "core")
    crossing = distance([block, shifted], {"part": "block"}, {"part": "core"})
    assert crossing["distance"] == 0.0 and crossing["overlap_volume"] == pytest.approx(3.5 * 25)


def test_a_point_face_or_edge_inside_a_parts_material_is_at_distance_zero():
    block, core = part(Box(30, 30, 30), "block"), part(Box(5, 5, 5), "core")
    inside = distance([block], {"part": "block"}, {"point": [0, 0, 5]})
    assert inside["ok"] and inside["distance"] == 0.0 and inside["inside"] is True
    assert distance([block], {"point": [0, 0, 5]}, {"part": "block"})["inside"] is True
    face = distance([block, core], {"part": "block"}, {"part": "core", "face": 0})
    assert face["distance"] == 0.0 and face["inside"] is True
    edge = distance([block, core], {"part": "core", "edge": 0}, {"part": "block"})
    assert edge["distance"] == 0.0 and edge["inside"] is True


def test_distances_outside_the_material_are_unchanged():
    block = part(Box(30, 30, 30), "block")
    far = part(Pos(40, 0, 0) * Box(10, 10, 10), "far")
    apart = distance([block, far], {"part": "block"}, {"part": "far"})
    assert apart["distance"] == pytest.approx(20.0) and apart["inside"] is False
    assert "overlap_volume" not in apart
    above = distance([block], {"part": "block"}, {"point": [0, 0, 40]})
    assert above["distance"] == pytest.approx(25.0) and above["inside"] is False
    on_surface = distance([block], {"part": "block"}, {"point": [0, 0, 15]})
    assert on_surface["distance"] == pytest.approx(0.0, abs=1e-9)
    assert on_surface["inside"] is False
    # Touching parts share no volume: contact, not overlap.
    resting = part(Pos(0, 0, 20) * Box(10, 10, 10), "far")
    touching = distance([block, resting], {"part": "block"}, {"part": "far"})
    assert touching["distance"] == pytest.approx(0.0, abs=1e-9) and touching["inside"] is False
    # Between a face and a point nothing is classified: the kernel's distance stands.
    to_face = distance([block], {"part": "block", "face": 0}, {"point": [0, 0, 0]})
    assert to_face["distance"] == pytest.approx(15.0) and "inside" not in to_face
    two_points = distance([block], {"point": [0, 0, 0]}, {"point": [3, 4, 0]})
    assert two_points["distance"] == pytest.approx(5.0) and "inside" not in two_points
