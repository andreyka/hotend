"""Tessellation: one triangulation per build, shared by the viewer, the renders and the checks.

`tessellate()` runs ocp-tessellate (the library three-cad-viewer's own tooling uses) and
returns

- `viewer`: the mesh.json object. It is ocp-tessellate's *instanced* form,
  `{"instances": [...], "shapes": {...}}`, in which every geometry buffer is
  `{"shape", "dtype", "buffer", "codec": "b64"}` (little-endian float32 / int32, base64).
  three-cad-viewer 5.x detects and decodes this form itself in `Viewer.render()`.
  The engine adds the top-level `"hotend"` key described in docs/PROTOCOL.md section 3.4.
- `meshes`: per part, the same triangles and edge polylines as numpy arrays in world
  coordinates, with the B-rep face index of every triangle.

Face order in the buffers is the order of build123d's `shape.faces()`. Edge order is the
tessellator's own: every edge sub-shape of the part in the kernel's map order, including the
degenerate edges (the poles of a sphere, the tip of a cone, the corner patches of a filleted
box) that `shape.edges()` leaves out. `mesh_edges()` returns the B-rep edges in that
numbering; it is what the `measure` op resolves an edge index with. `PartMesh.face_map_ok`
and `PartMesh.edge_map_ok` record that the counts in the buffers agree with those orders.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from . import PROTOCOL_VERSION
from .parts import NamedPart

VIEWER_PACKAGE = "three-cad-viewer"
VIEWER_VERSION = "5.1.3"
MESH_FORMAT = "ocp-tessellate-instanced-b64"
ROOT_NAME = "model"

# (linear deviation, angular tolerance in radians): the viewer's defaults first, then two
# coarser settings that are tried only when the triangle limit is exceeded.
QUALITIES = ((0.1, 0.2), (0.4, 0.5), (1.6, 1.0))


@dataclass
class PartMesh:
    name: str
    color: str
    path: str
    vertices: np.ndarray  # (n, 3) float64, world coordinates
    normals: np.ndarray  # (n, 3) float64, unit vertex normals
    triangles: np.ndarray  # (m, 3) int64, counter-clockwise seen from outside
    triangle_face: np.ndarray  # (m,) index of the B-rep face each triangle belongs to
    segments: np.ndarray  # (k, 2, 3) float64, B-rep edges as line segments
    face_map_ok: bool
    edge_map_ok: bool = True
    _manifold: Any = field(default=None, repr=False)
    _manifold_error: str | None = field(default=None, repr=False)

    @property
    def triangle_points(self) -> np.ndarray:
        return self.vertices[self.triangles]

    def triangle_normals_and_areas(self) -> tuple[np.ndarray, np.ndarray]:
        tri = self.triangle_points
        cross = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
        length = np.linalg.norm(cross, axis=1)
        normals = cross / np.where(length > 0, length, 1.0)[:, None]
        return normals, 0.5 * length

    def manifold(self) -> tuple[Any, str | None]:
        """The mesh as a manifold3d.Manifold, or (None, reason) when it is not a closed
        2-manifold. Vertices that OCCT duplicated along face boundaries are welded first."""
        if self._manifold is None and self._manifold_error is None:
            import manifold3d

            if len(self.triangles) == 0:
                self._manifold_error = "the part has no triangles"
                return None, self._manifold_error
            span = float(np.ptp(self.vertices, axis=0).max()) or 1.0
            keys = np.round(self.vertices / (span * 1e-7)).astype(np.int64)
            unique, inverse = np.unique(keys, axis=0, return_inverse=True)
            welded = np.zeros((len(unique), 3))
            welded[inverse.ravel()] = self.vertices
            tri = inverse.ravel()[self.triangles]
            keep = (tri[:, 0] != tri[:, 1]) & (tri[:, 1] != tri[:, 2]) & (tri[:, 0] != tri[:, 2])
            mesh = manifold3d.Mesh(
                vert_properties=np.ascontiguousarray(welded, dtype=np.float32),
                tri_verts=np.ascontiguousarray(tri[keep], dtype=np.uint32),
            )
            candidate = manifold3d.Manifold(mesh)
            status = candidate.status()
            if status != manifold3d.Error.NoError or candidate.is_empty():
                self._manifold_error = (
                    f"the triangle mesh is not a closed manifold ({status.name})"
                )
            else:
                self._manifold = candidate
        return self._manifold, self._manifold_error


@dataclass
class Tessellation:
    viewer: dict
    meshes: dict[str, PartMesh]
    triangle_count: int
    deviation: float

    def viewer_json(self) -> str:
        """mesh.json text. Key order and float formatting are fixed, so equal geometry gives
        equal bytes."""
        return json.dumps(self.viewer, separators=(",", ":"), default=_json_default)


def _json_default(value: Any) -> Any:
    if isinstance(value, np.integer):
        return int(value)
    if isinstance(value, np.floating):
        return float(value)
    if isinstance(value, np.ndarray):
        return value.tolist()
    raise TypeError(f"cannot serialise {type(value).__name__}")


def _transform(loc: Any) -> tuple[np.ndarray, np.ndarray]:
    """(rotation matrix, translation) of an ocp-tessellate `loc` = (position, quaternion xyzw)."""
    if loc is None:
        return np.eye(3), np.zeros(3)
    (tx, ty, tz), (x, y, z, w) = loc
    rotation = np.array([
        [1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
        [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
        [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)],
    ])
    return rotation, np.array([tx, ty, tz], dtype=float)


def _leaves(node: dict, rotation: np.ndarray, translation: np.ndarray):
    """Yield (leaf, rotation, translation) for every shape leaf, accumulating locations."""
    r, t = _transform(node.get("loc"))
    rotation, translation = rotation @ r, rotation @ t + translation
    if node.get("parts") is not None:
        for child in node["parts"]:
            yield from _leaves(child, rotation, translation)
    elif node.get("type") == "shapes" and isinstance(node.get("shape"), dict):
        yield node, rotation, translation


def mesh_edges(shape: Any) -> list:
    """The edges of a part (as TopoDS_Edge) in mesh.json's numbering.

    This is ocp-tessellate's own enumeration (`get_edges(shape, True)`: the indexed map of
    the shape's edge sub-shapes, without edges that belong to no face), so index i here is
    edge i of the part's `segments_per_edge` in mesh.json. Degenerate edges are included.
    """
    from ocp_tessellate.ocp_utils import get_edges

    return [edge for edge, _face in get_edges(shape.wrapped, True)]


def _part_mesh(part: NamedPart, path: str, node: dict, instances: list[dict]) -> PartMesh:
    vertices, normals, triangles, faces, segments = [], [], [], [], []
    offset = face_offset = edge_count = 0
    for leaf, rotation, translation in _leaves(node, np.eye(3), np.zeros(3)):
        inst = instances[leaf["shape"]["ref"]]
        v = np.asarray(inst["vertices"], dtype=np.float64).reshape(-1, 3) @ rotation.T + translation
        n = np.asarray(inst["normals"], dtype=np.float64).reshape(-1, 3) @ rotation.T
        t = np.asarray(inst["triangles"], dtype=np.int64).reshape(-1, 3)
        per_face = np.asarray(inst["triangles_per_face"], dtype=np.int64)
        e = np.asarray(inst["edges"], dtype=np.float64).reshape(-1, 2, 3) @ rotation.T + translation
        vertices.append(v)
        normals.append(n)
        triangles.append(t + offset)
        faces.append(np.repeat(np.arange(len(per_face)), per_face) + face_offset)
        segments.append(e)
        offset += len(v)
        face_offset += len(per_face)
        edge_count += len(np.asarray(inst["segments_per_edge"]).ravel())

    def cat(chunks: list, shape: tuple, dtype: Any) -> np.ndarray:
        return np.concatenate(chunks) if chunks else np.zeros(shape, dtype=dtype)

    return PartMesh(
        name=part.name,
        color=part.color,
        path=path,
        vertices=cat(vertices, (0, 3), np.float64),
        normals=cat(normals, (0, 3), np.float64),
        triangles=cat(triangles, (0, 3), np.int64),
        triangle_face=cat(faces, (0,), np.int64),
        segments=cat(segments, (0, 2, 3), np.float64),
        face_map_ok=face_offset == len(part.shape.faces()),
        edge_map_ok=edge_count == len(mesh_edges(part.shape)),
    )


def clear_library_caches(meshes: bool = False) -> None:
    """Empty what ocp-tessellate keeps between calls in one process.

    It caches bounding boxes keyed on the shape's address. A new shape that lands on a freed
    shape's address would get the old box and be meshed at the wrong deflection, so that cache
    is emptied before every tessellation (it matters for many builds in one process).

    With `meshes` its mesh cache goes too. That one is keyed on the shape's content, so a hit
    is the mesh a fresh run computes; it is emptied by a caller that builds many models in one
    process (the range sampler) so that every build starts from the same state and the cache
    does not grow to its 256 MB limit.
    """
    from ocp_tessellate import tessellator
    from ocp_tessellate.ocp_utils import bbox as _ocp_bbox

    _ocp_bbox.cache.clear()
    if meshes:
        tessellator.cache.clear()


def tessellate(parts: list[NamedPart], max_triangles: int | None = None) -> Tessellation:
    """Tessellate the parts. When the result would exceed `max_triangles` the tolerance is
    coarsened (twice at most); if it is still too large a ValueError says so."""
    from OCP.BRepTools import BRepTools
    from ocp_tessellate.convert import tessellate_group, to_ocpgroup
    from ocp_tessellate.utils import numpy_to_buffer_json

    clear_library_caches()

    for attempt, (deviation, angular_tolerance) in enumerate(QUALITIES):
        if attempt:
            # OCCT keeps a finer triangulation it already has; drop it before coarsening.
            for part in parts:
                BRepTools.Clean_s(part.shape.wrapped)
        group, instances = to_ocpgroup(
            *[part.shape for part in parts],
            names=[part.name for part in parts],
            colors=[part.color for part in parts],
        )
        group.name = ROOT_NAME
        meshed, shapes, _mapping = tessellate_group(
            group,
            instances,
            {"deviation": deviation, "angular_tolerance": angular_tolerance},
        )
        count = sum(len(inst["triangles"]) // 3 for inst in meshed)
        if max_triangles is None or count <= max_triangles:
            break
    else:
        raise ValueError(
            f"the model needs {count} triangles even at the coarsest setting; the limit is "
            f"{max_triangles}"
        )

    nodes = {node["name"]: node for node in shapes.get("parts", [])}
    paths = {part.name: f"/{ROOT_NAME}/{part.name}" for part in parts}
    meshes = {
        part.name: _part_mesh(part, paths[part.name], nodes[part.name], meshed)
        if part.name in nodes
        else PartMesh(part.name, part.color, paths[part.name], np.zeros((0, 3)),
                      np.zeros((0, 3)), np.zeros((0, 3), np.int64), np.zeros((0,), np.int64),
                      np.zeros((0, 2, 3)), False)
        for part in parts
    }
    viewer = numpy_to_buffer_json({"instances": meshed, "shapes": shapes})
    viewer["hotend"] = {
        "protocol": PROTOCOL_VERSION,
        "format": MESH_FORMAT,
        "viewer": {"package": VIEWER_PACKAGE, "version": VIEWER_VERSION},
        "deviation": deviation,
        "parts": paths,
    }
    return Tessellation(viewer, meshes, count, deviation)
