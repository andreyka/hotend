"""Plate files: a minimal 3MF writer of Hotend's own.

A plate file is what the slicer is given, so it holds the pieces of one plate and nothing a
slicer could read a setting from. Bambu Studio gives every distinct display colour of an
imported 3MF its own filament slot; a plate that carried the viewer's part colours was
split over two filaments and the print failed. Because nobody can list everything a slicer
reads from a 3MF, the file is plain by construction: this writer can only produce the shape
below, and the slicer adapter accepts only that shape.

The shape (the allowlist is the table `crates/hotend-slicer/tests/data/plate-shape.json`,
which the tests of this module and of the adapter both read):

- A zip archive with exactly three entries, in this order: `[Content_Types].xml` and
  `_rels/.rels`, which are constant bytes, and `3D/3dmodel.model`.
- The model part is UTF-8 text in the core 3MF namespace with the unit millimetre. It has
  one `object` per piece (`id`, `type="model"`, `name`) holding one `mesh` with its
  `vertices` and `triangles`, and one `build` `item` per object.
- Vertex coordinates are the final bed coordinates in millimetres, written as plain
  decimals. There is no transform, no component, no metadata, no colour, no material and
  no property, and this module has no argument that could add one.
- The only text that comes from outside is the piece names. Each must match `NAME_RE` and
  be used once; anything else is refused before a byte is written.

Equal pieces give equal bytes: the entries carry a fixed date, a fixed order and a fixed
compression level, and nothing in the file depends on a clock or a random number.

`export/model.3mf` is not written here. That file is for viewing, keeps its colours, and is
never given to a slicer.
"""

from __future__ import annotations

import io
import re
import zipfile
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

CONTENT_TYPES_NAME = "[Content_Types].xml"
RELATIONSHIPS_NAME = "_rels/.rels"
MODEL_NAME = "3D/3dmodel.model"
ENTRIES = (CONTENT_TYPES_NAME, RELATIONSHIPS_NAME, MODEL_NAME)

CORE_NAMESPACE = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02"
UNIT = "millimeter"
LANGUAGE = "en-US"
OBJECT_TYPE = "model"

# The two package parts never change, so the adapter compares them byte for byte instead
# of parsing them.
CONTENT_TYPES = (
    b'<?xml version="1.0" encoding="UTF-8"?>\n'
    b'<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    b'<Default Extension="rels" '
    b'ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    b'<Default Extension="model" '
    b'ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>'
    b"</Types>\n"
)
RELATIONSHIPS = (
    b'<?xml version="1.0" encoding="UTF-8"?>\n'
    b'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    b'<Relationship Target="/3D/3dmodel.model" Id="rel0" '
    b'Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>'
    b"</Relationships>\n"
)

# A piece name: a part (`lid`), a copy (`lid.2`) or a gauge piece (`peg_hole_1.peg`). The
# slicer copies object names into its own files, so the pattern admits no space, no line
# break, no quote and no path separator. Always matched with `fullmatch`.
NAME_PATTERN = "[a-z][a-z0-9_.]{0,63}"
NAME_RE = re.compile(NAME_PATTERN)
NAME_RULE = (
    "a piece name starts with a lower-case letter and holds at most 64 lower-case "
    "letters, digits, underscores and dots"
)

# Coordinates are written with this many decimals (a millionth of a millimetre) and must
# lie this close to the origin; no printer Hotend knows is a tenth of that.
COORDINATE_DECIMALS = 6
COORDINATE_LIMIT_MM = 1000.0
# The adapter refuses a larger model part, so a larger one is never written.
MAX_MODEL_BYTES = 64 * 1024 * 1024
# The fewest bytes a vertex and a triangle can take, line break included. A plate whose
# counts alone put it over the limit is refused before any of its text is made.
_SHORTEST_VERTEX = len('<vertex x="0" y="0" z="0"/>\n')
_SHORTEST_TRIANGLE = len('<triangle v1="0" v2="0" v3="0"/>\n')
# The text is made this many lines at a time, and its size is checked after each lot.
_LINES_PER_LOT = 20_000

# Every entry gets the same date (the earliest a zip can hold), the same level and the same
# file attributes (an ordinary file, as a Unix system would record it) on every system.
_ENTRY_DATE = (1980, 1, 1, 0, 0, 0)
_COMPRESSION_LEVEL = 6
_ENTRY_SYSTEM = 3
_ENTRY_ATTRIBUTES = 0o100644 << 16


class PlateFileError(ValueError):
    """The pieces cannot be written as a plain plate file. Nothing was written."""


@dataclass(frozen=True, eq=False)
class Piece:
    """One piece of a plate as the slicer gets it."""

    name: str
    vertices: Any  # (n, 3) numbers: final bed coordinates in millimetres
    triangles: Any  # (m, 3) integers: vertex indices, counter-clockwise seen from outside


def check_names(names: Sequence[Any]) -> None:
    """Refuse piece names that may not reach a plate file. The refusal never repeats a
    name: it is text from outside and the message ends up in result.json."""
    seen: set[str] = set()
    for position, name in enumerate(names, start=1):
        if not isinstance(name, str) or NAME_RE.fullmatch(name) is None:
            raise PlateFileError(
                f"the name of piece {position} cannot be written to a plate file: "
                f"{NAME_RULE}. Rename the part, or the feature card its gauge is made of."
            )
        if name in seen:
            raise PlateFileError(
                f"piece {position} has the same name as an earlier piece; every piece "
                "of a job needs a name of its own. Rename one of them."
            )
        seen.add(name)


def closed_and_oriented(triangles: Any) -> bool:
    """True when the triangles form closed surfaces that all face one way: every edge is
    used exactly once in each direction. A slicer cannot tell inside from outside on any
    other mesh."""
    corners = np.asarray(triangles, dtype=np.int64).reshape(-1, 3)
    if len(corners) == 0:
        return False
    start = np.concatenate([corners[:, 0], corners[:, 1], corners[:, 2]])
    end = np.concatenate([corners[:, 1], corners[:, 2], corners[:, 0]])
    span = int(corners.max()) + 1
    forward = np.sort(start * span + end)
    backward = np.sort(end * span + start)
    return bool(np.all(forward[1:] != forward[:-1])) and bool(np.array_equal(forward, backward))


def _decimal(value: float) -> str:
    """A coordinate as a plain decimal: no exponent, no trailing zeros, no minus zero."""
    text = f"{value:.{COORDINATE_DECIMALS}f}".rstrip("0").rstrip(".")
    return "0" if text == "-0" else text


def _checked(pieces: Sequence[Piece]) -> list[tuple[str, np.ndarray, np.ndarray]]:
    if len(pieces) == 0:
        raise PlateFileError("a plate file needs at least one piece")
    check_names([piece.name for piece in pieces])
    checked = []
    for position, piece in enumerate(pieces, start=1):
        try:
            vertices = np.asarray(piece.vertices, dtype=np.float64)
            triangles = np.asarray(piece.triangles)
        except (TypeError, ValueError, OverflowError) as exc:
            # OverflowError: a whole number too large to be a coordinate at all.
            raise PlateFileError(f"the mesh of piece {position} is not numbers") from exc
        if vertices.ndim != 2 or vertices.shape[1] != 3 or len(vertices) < 3:
            raise PlateFileError(
                f"the mesh of piece {position} needs at least three vertices of three "
                "coordinates each; the part has no printable surface"
            )
        if (triangles.dtype.kind not in "iu" or triangles.ndim != 2
                or triangles.shape[1] != 3 or len(triangles) == 0):
            raise PlateFileError(
                f"the mesh of piece {position} needs at least one triangle of three "
                "vertex indices; the part has no printable surface"
            )
        if not np.isfinite(vertices).all() or np.abs(vertices).max() > COORDINATE_LIMIT_MM:
            raise PlateFileError(
                f"piece {position} reaches further than {COORDINATE_LIMIT_MM:g} mm from "
                "the bed's origin, which no plate file may hold. Check the printer's reach "
                "in the request."
            )
        if triangles.min() < 0 or triangles.max() >= len(vertices):
            raise PlateFileError(f"a triangle of piece {position} names a vertex it lacks")
        if ((triangles[:, 0] == triangles[:, 1]) | (triangles[:, 1] == triangles[:, 2])
                | (triangles[:, 2] == triangles[:, 0])).any():
            raise PlateFileError(f"a triangle of piece {position} uses a vertex twice")
        checked.append((piece.name, vertices, triangles.astype(np.int64)))
    return checked


def _too_large() -> PlateFileError:
    return PlateFileError(
        f"the plate's mesh takes more than the {MAX_MODEL_BYTES // 2**20} MiB the slicer "
        "step accepts as a plate file. Put fewer copies on the plate or print the largest "
        "piece on its own."
    )


def _model(pieces: list[tuple[str, np.ndarray, np.ndarray]]) -> bytes:
    """The model part: one element per line, without indentation (it counts towards the
    size limit and no reader needs it). Raises PlateFileError as soon as the part is known
    to be over `MAX_MODEL_BYTES`: from the counts alone when they suffice, else while the
    text is made, so a plate that cannot be written costs little time and memory."""
    shortest = sum(_SHORTEST_VERTEX * len(vertices) + _SHORTEST_TRIANGLE * len(triangles)
                   for _name, vertices, triangles in pieces)
    if shortest > MAX_MODEL_BYTES:
        raise _too_large()
    lots: list[bytes] = []
    # What is written, and the least that the vertices and triangles still to come take.
    size, owed = 0, shortest

    def add(lines: list[str], least: int = 0) -> None:
        nonlocal size, owed
        lot = ("\n".join(lines) + "\n").encode("ascii")
        size, owed = size + len(lot), owed - least
        if size + owed > MAX_MODEL_BYTES:
            raise _too_large()
        lots.append(lot)

    add([
        '<?xml version="1.0" encoding="UTF-8"?>',
        f'<model unit="{UNIT}" xml:lang="{LANGUAGE}" xmlns="{CORE_NAMESPACE}">',
        "<resources>",
    ])
    for number, (name, vertices, triangles) in enumerate(pieces, start=1):
        add([f'<object id="{number}" type="{OBJECT_TYPE}" name="{name}">', "<mesh>",
             "<vertices>"])
        for first in range(0, len(vertices), _LINES_PER_LOT):
            lot = vertices[first:first + _LINES_PER_LOT].tolist()
            add([f'<vertex x="{_decimal(x)}" y="{_decimal(y)}" z="{_decimal(z)}"/>'
                 for x, y, z in lot], _SHORTEST_VERTEX * len(lot))
        add(["</vertices>", "<triangles>"])
        for first in range(0, len(triangles), _LINES_PER_LOT):
            lot = triangles[first:first + _LINES_PER_LOT].tolist()
            add([f'<triangle v1="{a}" v2="{b}" v3="{c}"/>' for a, b, c in lot],
                _SHORTEST_TRIANGLE * len(lot))
        add(["</triangles>", "</mesh>", "</object>"])
    add(["</resources>", "<build>",
         *(f'<item objectid="{number}"/>' for number in range(1, len(pieces) + 1)),
         "</build>", "</model>"])
    return b"".join(lots)


def plate_bytes(pieces: Sequence[Piece]) -> bytes:
    """The whole plate file of these pieces. Raises PlateFileError when a name or a mesh
    may not be written."""
    model = _model(_checked(pieces))
    if len(model) > MAX_MODEL_BYTES:
        raise _too_large()
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for name, data in zip(ENTRIES, (CONTENT_TYPES, RELATIONSHIPS, model), strict=True):
            info = zipfile.ZipInfo(name, date_time=_ENTRY_DATE)
            # Set here, or the zip library fills them in from the system it runs on.
            info.create_system = _ENTRY_SYSTEM
            info.external_attr = _ENTRY_ATTRIBUTES
            archive.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED,
                             compresslevel=_COMPRESSION_LEVEL)
    return buffer.getvalue()


def write_plate(pieces: Sequence[Piece], path: Path) -> None:
    """Write the plate file. Everything is checked and built in memory first, so a refusal
    leaves no file behind."""
    data = plate_bytes(pieces)
    Path(path).write_bytes(data)
