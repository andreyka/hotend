"""Ghosts: the references a model placed, shown in the viewer mesh and in the rendered
views so the user sees the real thing sitting in the part (docs/PROTOCOL.md section 3.4).

A ghost is display only. It is made from the worker's own copy of the reference, at the
frame of each placed pocket, after every check has run. It is never a part: the B-rep,
the STEP, the STL, the 3MF and the plates are written from the parts alone, and nothing
here touches them.

In mesh.json a ghost is one more leaf of the shape tree (so the viewer draws it without
knowing about references), translucent, named `ghost.<reference>`; the `hotend.references`
list names those leaves, so a client can tell them from parts, keep them out of selection
and measuring, and hide them. A large reference mesh is simplified for display only (the
checks use the exact one); how is said per ghost.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import numpy as np

GHOST_COLOR = "#d8c9a8"
GHOST_ALPHA = 0.35
# A ghost is drawn with at most this many triangles.
GHOST_MAX_TRIANGLES = 40_000
SIMPLIFY_TOLERANCES_MM = (0.05, 0.1, 0.2, 0.4, 0.8)
# OCCT's GeomAbs_OtherSurface: a ghost has no analytic faces.
OTHER_SURFACE = 10


@dataclass
class Ghost:
    name: str  # the leaf's name in mesh.json: ghost.<reference>
    reference: str
    label: str
    pocket: str
    kind: str
    provenance: str
    how: str  # exact | simplified to within x mm | convex hull
    vertices: np.ndarray  # (n, 3) world coordinates
    triangles: np.ndarray  # (m, 3)

    @property
    def path(self) -> str:
        from .tessellate import ROOT_NAME

        return f"/{ROOT_NAME}/{self.name}"

    def normals(self) -> np.ndarray:
        """Unit vertex normals (area-weighted), for smooth shading."""
        tri = self.vertices[self.triangles]
        face = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
        normals = np.zeros_like(self.vertices)
        for corner in range(3):
            np.add.at(normals, self.triangles[:, corner], face)
        length = np.linalg.norm(normals, axis=1)
        return normals / np.where(length > 0, length, 1.0)[:, None]

    def part_mesh(self) -> Any:
        """The ghost as a mesh the view renderer draws beside the parts."""
        from .tessellate import PartMesh

        return PartMesh(
            f"{self.label} (reference)", GHOST_COLOR, self.path, self.vertices,
            self.normals(), self.triangles, np.zeros(len(self.triangles), dtype=np.int64),
            np.zeros((0, 2, 3)), True)

    def listing(self) -> dict:
        return {"name": self.reference, "label": self.label, "path": self.path,
                "pocket": self.pocket, "kind": self.kind, "provenance": self.provenance,
                "mesh": self.how, "triangles": len(self.triangles),
                "color": GHOST_COLOR, "alpha": GHOST_ALPHA}


def display_mesh(reference: Any) -> tuple[np.ndarray, np.ndarray, str]:
    """The reference's mesh for display: as it is when small, otherwise simplified within
    a stated tolerance, otherwise its convex hull."""
    if len(reference.triangles) <= GHOST_MAX_TRIANGLES:
        return reference.vertices, reference.triangles, "exact"
    solid = reference._solid()
    if solid is not None:
        for tolerance in SIMPLIFY_TOLERANCES_MM:
            mesh = solid.simplify(tolerance).to_mesh64()
            if len(mesh.tri_verts) <= GHOST_MAX_TRIANGLES:
                return (np.array(mesh.vert_properties)[:, :3].astype(np.float64),
                        np.array(mesh.tri_verts).astype(np.int64),
                        f"simplified to within {tolerance:g} mm")
    mesh = reference._hull_manifold().to_mesh64()
    return (np.array(mesh.vert_properties)[:, :3].astype(np.float64),
            np.array(mesh.tri_verts).astype(np.int64), "convex hull")


def ghosts_of(resolver: Any) -> list[Ghost]:
    """One ghost per placed pocket of the build (two pockets at one place share one)."""
    from hotend_fit.reference import _matrix_of

    ghosts: list[Ghost] = []
    seen: set[tuple] = set()
    counts: dict[str, int] = {}
    meshes: dict[str, tuple] = {}
    for pocket in getattr(resolver, "pockets", None) or []:
        if pocket.at is None:
            continue
        rotation, translation = _matrix_of(pocket.at)
        key = (pocket.reference.name, tuple(np.round(rotation, 9).ravel()),
               tuple(np.round(translation, 6)))
        if key in seen:
            continue
        seen.add(key)
        reference = pocket.reference
        if reference.name not in meshes:
            meshes[reference.name] = display_mesh(reference)
        vertices, triangles, how = meshes[reference.name]
        count = counts[reference.name] = counts.get(reference.name, 0) + 1
        name = f"ghost.{reference.name}" + ("" if count == 1 else f".{count}")
        ghosts.append(Ghost(name, reference.name, reference.label, pocket.name, reference.kind,
                            reference.provenance, how, vertices @ rotation.T + translation,
                            triangles))
    return ghosts


def add_to_viewer(viewer: dict, ghosts: list[Ghost]) -> None:
    """Add the ghosts to a mesh.json object (`Tessellation.viewer`) in place."""
    from ocp_tessellate.utils import numpy_to_buffer_json

    listing = []
    shapes = viewer["shapes"]
    template = next((dict(node) for node in shapes.get("parts", [])
                     if node.get("type") == "shapes"), {})
    for ghost in ghosts:
        instance = {
            "vertices": ghost.vertices.astype(np.float32).ravel(),
            "triangles": ghost.triangles.astype(np.int32).ravel(),
            "normals": ghost.normals().astype(np.float32).ravel(),
            "edges": np.zeros(0, dtype=np.float32),
            "obj_vertices": np.zeros(0, dtype=np.float32),
            "face_types": np.array([OTHER_SURFACE], dtype=np.int32),
            "edge_types": np.zeros(0, dtype=np.int32),
            "triangles_per_face": np.array([len(ghost.triangles)], dtype=np.int32),
            "segments_per_edge": np.zeros(0, dtype=np.int32),
        }
        viewer["instances"].append(numpy_to_buffer_json(instance))
        leaf = {
            **template, "id": ghost.path, "type": "shapes", "subtype": "solid",
            "name": ghost.name, "shape": {"ref": len(viewer["instances"]) - 1},
            "state": [1, 0], "color": GHOST_COLOR, "alpha": GHOST_ALPHA,
            "loc": [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]], "renderback": False, "bb": None,
        }
        shapes.setdefault("parts", []).append(leaf)
        box = shapes.get("bb")
        if isinstance(box, dict):
            low, high = ghost.vertices.min(axis=0), ghost.vertices.max(axis=0)
            for axis, index in (("x", 0), ("y", 1), ("z", 2)):
                box[f"{axis}min"] = min(box[f"{axis}min"], float(low[index]))
                box[f"{axis}max"] = max(box[f"{axis}max"], float(high[index]))
        listing.append(ghost.listing())
    viewer["hotend"]["references"] = listing
