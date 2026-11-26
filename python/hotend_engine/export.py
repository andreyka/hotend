"""Exports: STEP (exact), 3MF and STL (meshes), all written inside the job folder.

- `model.step` / `export/model.step`: one STEP assembly whose children carry the part names
  and colours. The header timestamp is fixed, so equal geometry gives equal bytes.
- `export/model.3mf`: one 3MF with one named mesh object per part (lib3mf through build123d's
  Mesher), with the parts' colours: it is for viewing. lib3mf stamps random UUIDs, so its
  bytes differ from run to run.
- `export/<part>.stl`: one binary STL per part, because STL cannot hold several named bodies.
- `plates/<n>/plate.3mf`: the pieces of one plate for the slicer, written by Hotend's own
  minimal writer (`plate3mf`) from the same triangles, without colours or anything else a
  slicer could read a setting from. Equal pieces give equal bytes.
"""

from __future__ import annotations

import copy
import io
import warnings
from collections.abc import Sequence
from pathlib import Path
from typing import Any

import numpy as np

from . import plate3mf
from .parts import NamedPart

STEP_TIMESTAMP = "1970-01-01T00:00:00"
EXPORT_DIR = "export"
# Mesh tolerances for printing, finer than the viewer's.
#
# build123d's mesh writers (`Mesher.add_shape`, `export_stl`) take a linear deflection that is
# relative to the size of each edge and an angular deflection in radians; the tighter of the
# two decides. With their defaults (0.001 relative, 0.1 rad) the angle decides for every
# circle: it is cut into about 126 segments whatever its radius, so the facets cut the corner
# by radius / 3200: 0.001 mm on a 3 mm hole, 0.03 mm on a 100 mm one, a visible share of a
# slide clearance. The error must be bounded in millimetres, not in segments, so the relative
# deflection is lowered per part until no edge of it may deviate by more than
# MAX_CHORD_ERROR_MM (OCCT allows an edge at most half the part's largest dimension times
# the relative deflection). Small features of a large part come out finer than they need to
# be; that costs triangles, not accuracy.
MAX_CHORD_ERROR_MM = 0.01
LINEAR_DEFLECTION = 0.001
ANGULAR_DEFLECTION = 0.1
# The kernel repeats a vertex on every face that meets at it. Vertices that agree to this
# many decimals of a millimetre are one vertex of a plate mesh (the same rounding the
# viewing 3MF gets from build123d's Mesher), which makes the mesh one closed surface.
WELD_DECIMALS = plate3mf.COORDINATE_DECIMALS


def linear_deflection_for(shape) -> float:
    """The relative linear deflection that keeps a part's mesh within MAX_CHORD_ERROR_MM of
    its exact surface, never coarser than build123d's default."""
    box = shape.bounding_box()
    largest = max(float(c) for c in tuple(box.size))
    if largest <= 0:
        return LINEAR_DEFLECTION
    return min(LINEAR_DEFLECTION, 2.0 * MAX_CHORD_ERROR_MM / largest)


def _labelled(part: NamedPart):
    from build123d import Color

    shape = copy.copy(part.shape)
    shape.label = part.name
    shape.color = Color(part.color)
    return shape


def write_step(parts: list[NamedPart], path: Path) -> None:
    from build123d import Compound, export_step

    assembly = Compound(label="model", children=[_labelled(part) for part in parts])
    buffer = io.BytesIO()
    export_step(assembly, buffer, timestamp=STEP_TIMESTAMP)
    path.write_bytes(buffer.getvalue())


def write_3mf(parts: list[NamedPart], path: Path) -> list[str]:
    """Write one 3MF holding every part with its colour, for viewing. Returns the mesher's
    warnings as text. A file for the slicer is written by `write_plate_3mf` instead."""
    from build123d import Mesher

    mesher = Mesher()
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        for part in parts:
            mesher.add_shape(
                _labelled(part), linear_deflection_for(part.shape), ANGULAR_DEFLECTION
            )
    mesher.write(path)
    return [f"3mf: {w.message}" for w in caught]


def print_mesh(shape: Any) -> tuple[np.ndarray, np.ndarray]:
    """The triangles a shape is printed from, at the printing tolerances: (vertices, an
    (n, 3) array in millimetres; triangles, an (m, 3) array of vertex indices,
    counter-clockwise seen from outside).

    These are the triangles `write_3mf` gives the same shape: the same mesher settings, the
    same merging of the vertices the faces share, and triangles that collapse to a line are
    dropped. A face the kernel could not mesh is left out, which `write_plate_3mf` reports
    as a surface that is not closed.
    """
    from OCP.BRep import BRep_Tool
    from OCP.BRepMesh import BRepMesh_IncrementalMesh
    from OCP.TopAbs import TopAbs_REVERSED
    from OCP.TopLoc import TopLoc_Location

    # The mesher keeps its triangles on the shape. A deep copy starts without any, so a
    # coarser mesh the viewer left behind is not reused and the caller's shape stays as it was.
    meshed = copy.deepcopy(shape)
    BRepMesh_IncrementalMesh(meshed.wrapped, linear_deflection_for(shape), True,
                             ANGULAR_DEFLECTION, True)
    location = TopLoc_Location()
    points: list[tuple[float, float, float]] = []
    corners: list[tuple[int, int, int]] = []
    for face in meshed.faces():
        triangulation = BRep_Tool.Triangulation_s(face.wrapped, location)
        if triangulation is None:
            continue
        placement = location.Transformation()
        offset = len(points) - 1  # the kernel counts nodes from 1
        for index in range(1, triangulation.NbNodes() + 1):
            point = triangulation.Node(index).Transformed(placement)
            points.append((point.X(), point.Y(), point.Z()))
        # A reversed face lists its triangles clockwise seen from outside.
        a, b, c = (1, 3, 2) if face.wrapped.Orientation() == TopAbs_REVERSED else (1, 2, 3)
        for index in range(1, triangulation.NbTriangles() + 1):
            triangle = triangulation.Triangle(index)
            corners.append((triangle.Value(a) + offset, triangle.Value(b) + offset,
                            triangle.Value(c) + offset))
    if not points or not corners:
        return np.zeros((0, 3)), np.zeros((0, 3), dtype=np.int64)
    # Adding 0.0 turns a minus zero into zero, so both are one vertex.
    rounded = np.round(np.asarray(points, dtype=np.float64), WELD_DECIMALS) + 0.0
    unique, first, inverse = np.unique(rounded, axis=0, return_index=True,
                                       return_inverse=True)
    # Vertices in the order they were first seen, not sorted by coordinate.
    order = np.argsort(first, kind="stable")
    rank = np.empty(len(order), dtype=np.int64)
    rank[order] = np.arange(len(order))
    triangles = rank[inverse.ravel()][np.asarray(corners, dtype=np.int64)]
    distinct = ((triangles[:, 0] != triangles[:, 1]) & (triangles[:, 1] != triangles[:, 2])
                & (triangles[:, 2] != triangles[:, 0]))
    return unique[order], triangles[distinct]


def write_plate_3mf(pieces: Sequence[tuple[str, Any]], path: Path) -> list[str]:
    """Write one plate file for the slicer: `pieces` are (name, shape) pairs, the shapes
    standing where they are printed. Returns notes about the meshes as text.

    A plate file takes a name and a shape and nothing else. In particular it takes no
    colour: a slicer may give each colour of a file a filament of its own. Raises
    `plate3mf.PlateFileError`, before anything is written, when a name is not a plain
    piece name or a mesh cannot be written.
    """
    plate3mf.check_names([name for name, _shape in pieces])
    meshes = [plate3mf.Piece(name, *print_mesh(shape)) for name, shape in pieces]
    plate3mf.write_plate(meshes, path)
    return [
        f"3mf: the mesh of '{piece.name}' is not a closed surface; a slicer may fill it "
        "wrongly"
        for piece in meshes if not plate3mf.closed_and_oriented(piece.triangles)
    ]


def write_stl(part: NamedPart, path: Path) -> None:
    from build123d import export_stl

    if not export_stl(copy.copy(part.shape), path, linear_deflection_for(part.shape),
                      ANGULAR_DEFLECTION):
        raise RuntimeError(f"the STL writer failed for part {part.name!r}")


def export_formats(
    parts: list[NamedPart], formats: tuple[str, ...], job_dir: Path
) -> tuple[dict, list[str]]:
    """Write the requested formats under export/. Returns (artifacts, warnings); artifact
    paths are relative to the job folder, with keys `step`, `threemf` and `stl`."""
    out = job_dir / EXPORT_DIR
    out.mkdir(exist_ok=True)
    artifacts: dict = {}
    notes: list[str] = []
    if "step" in formats:
        write_step(parts, out / "model.step")
        artifacts["step"] = f"{EXPORT_DIR}/model.step"
    if "3mf" in formats:
        notes += write_3mf(parts, out / "model.3mf")
        artifacts["threemf"] = f"{EXPORT_DIR}/model.3mf"
    if "stl" in formats:
        artifacts["stl"] = {}
        for part in parts:
            write_stl(part, out / f"{part.name}.stl")
            artifacts["stl"][part.name] = f"{EXPORT_DIR}/{part.name}.stl"
    return artifacts, notes
