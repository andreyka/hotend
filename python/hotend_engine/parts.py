"""Named parts: what a build returns, how it is summarised, and how it is stored.

A part is one printed piece: a name and a build123d solid (or a compound of solids, which the
`single_solid_per_part` check then reports). Every part is flattened to its solids in world
coordinates, so nested assemblies and locations never reach the checks or the exporters.

`model.brep` holds one compound whose children are the parts in order; B-rep files carry no
names, so `model.parts.json` stores the names and colours next to it. The other ops load both.
A build also notes there, under `print`, what print preparation needs and cannot read from
the solids: the orientation each part prefers (the script's `PRINT`) and the feature cards
the build applied (as `Card.declaration()` data, rebuilt by the worker). Both are optional
when the file is read, and the cards are treated as untrusted data again when they are.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import PROTOCOL_VERSION
from .errors import BadRequest
from .model import MAX_PARTS, NAME_RE, LoadedModel, rule_error
from .request import Material

BREP_NAME = "model.brep"
INDEX_NAME = "model.parts.json"
MAX_INDEX_BYTES = 64 * 1024
# The core copies the index with a 64 kB cap; the print notes are dropped before it grows
# past this.
MAX_NOTES_BYTES = 56 * 1024
COLOR_RE = re.compile(r"#[0-9a-f]{6}\Z")

# Distinct, colour-blind-friendly part colours, assigned by part order.
PALETTE = ("#4c78a8", "#f58518", "#54a24b", "#e45756", "#72b7b2", "#eeca3b", "#b279a2", "#9d755d")

# Solid (100 % infill) densities in g/cm3, used only when the request's material gives none.
FAMILY_DENSITY_G_CM3 = {
    "PLA": 1.24, "PETG": 1.27, "ABS": 1.04, "ASA": 1.07, "TPU": 1.21, "PA": 1.14, "PC": 1.20,
}


@dataclass
class NamedPart:
    name: str
    shape: Any  # build123d Solid, or Compound of solids, in world coordinates
    color: str

    @property
    def solids(self) -> list:
        return list(self.shape.solids())


def _color_hex(shape: Any, index: int) -> str:
    color = getattr(shape, "color", None)
    if color is not None:
        try:
            r, g, b = (round(max(0.0, min(1.0, c)) * 255) for c in tuple(color)[:3])
            return f"#{r:02x}{g:02x}{b:02x}"
        except (TypeError, ValueError):
            pass
    return PALETTE[index % len(PALETTE)]


def _flatten(shape: Any) -> Any:
    from build123d import Compound

    solids = list(shape.solids())
    return solids[0] if len(solids) == 1 else Compound(solids)


def parts_from_build(model: LoadedModel, returned: Any) -> list[NamedPart]:
    """Validate what build() returned and turn it into named parts. Raises ModelError."""
    from build123d import Shape

    def unwrap(value: Any) -> Any:
        # Returning the BuildPart builder instead of its .part is a common slip; accept it.
        if not isinstance(value, Shape) and isinstance(getattr(value, "part", None), Shape):
            return value.part
        return value

    returned = unwrap(returned)
    if isinstance(returned, Shape):
        returned = {"part": returned}
    if not isinstance(returned, dict):
        raise rule_error(
            model,
            "BuildResultError",
            "build must return one Part or a dict of named Parts, "
            f"got {type(returned).__name__}",
        )
    if not returned:
        raise rule_error(model, "BuildResultError", "build returned an empty dict: no parts")
    if len(returned) > MAX_PARTS:
        raise rule_error(model, "BuildResultError", f"build returned more than {MAX_PARTS} parts")
    parts = []
    for index, (name, value) in enumerate(returned.items()):
        if not isinstance(name, str) or not NAME_RE.match(name):
            raise rule_error(
                model,
                "BuildResultError",
                f"part name {name!r} is not allowed; use lower-case letters, digits and "
                "underscores, starting with a letter, at most 32 characters",
            )
        value = unwrap(value)
        if not isinstance(value, Shape):
            raise rule_error(
                model,
                "BuildResultError",
                f"part {name!r} is a {type(value).__name__}, not a build123d Part, Solid or "
                "Compound",
            )
        if value.is_null or not value.solids():
            kind = type(value).__name__
            raise rule_error(
                model,
                "NotASolid",
                f"part {name!r} is a {kind} with no solid in it; a part must be a 3D solid "
                "(extrude or revolve the sketch, or return the Part)",
            )
        parts.append(NamedPart(name, _flatten(value), _color_hex(value, index)))
    return parts


def density_g_cm3(material: Material | None) -> float | None:
    if material is None:
        return None
    if material.density_g_cm3:
        return material.density_g_cm3
    return FAMILY_DENSITY_G_CM3.get(material.family.upper())


def bbox_json(shape: Any) -> dict:
    box = shape.bounding_box()
    return {"min": [round(v, 6) for v in tuple(box.min)],
            "max": [round(v, 6) for v in tuple(box.max)]}


def edge_defects(shape: Any) -> tuple[int, int]:
    """(open edges, edges shared by more than two faces) of a B-rep shape.

    A closed, printable solid has neither. build123d 0.13.0's `Shape.is_manifold` is not used
    because it counts the degenerate edges at a sphere's poles or a cone's tip as defects.
    """
    from OCP.BRep import BRep_Tool
    from OCP.collections import (
        IndexedDataMap_TopoDS_Shape_List_TopoDS_Shape_TopTools_ShapeMapHasher as EdgeFaceMap,
    )
    from OCP.TopAbs import TopAbs_EDGE, TopAbs_FACE
    from OCP.TopExp import TopExp
    from OCP.TopoDS import TopoDS

    edge_faces = EdgeFaceMap()
    TopExp.MapShapesAndAncestors_s(shape.wrapped, TopAbs_EDGE, TopAbs_FACE, edge_faces)
    open_edges = shared_edges = 0
    for i in range(1, edge_faces.Extent() + 1):
        if BRep_Tool.Degenerated_s(TopoDS.Edge(edge_faces.FindKey(i))):
            continue
        count = edge_faces.FindFromIndex(i).Extent()
        open_edges += count < 2
        shared_edges += count > 2
    return open_edges, shared_edges


def summarize(part: NamedPart, material: Material | None = None) -> dict:
    """The `parts[]` entry of result.json. Volumes, areas and boxes are exact B-rep values."""
    shape = part.shape
    volume = float(shape.volume)
    density = density_g_cm3(material)
    return {
        "name": part.name,
        "volume": round(volume, 6),
        "area": round(float(shape.area), 6),
        "mass_estimate_g": None if density is None else round(volume / 1000.0 * density, 3),
        "bbox": bbox_json(shape),
        "solids": len(part.solids),
        "faces": len(shape.faces()),
        "is_valid": bool(shape.is_valid),
        "is_manifold": edge_defects(shape) == (0, 0),
        "color": part.color,
    }


def save_parts(parts: list[NamedPart], job_dir: Path, notes: dict | None = None) -> bool:
    """Write model.brep and model.parts.json. The B-rep file is written without any
    triangulation the shapes may carry, so tessellation settings never leak into it.

    `notes` is the `print` object (`orientations`, `fits`). Returns False when the declared
    cards had to be left out because the file would have grown too large."""
    from build123d import Compound
    from OCP.BRepTools import BRepTools
    from OCP.TopTools import TopTools_FormatVersion

    written = BRepTools.Write_s(
        Compound([part.shape for part in parts]).wrapped,
        str(job_dir / BREP_NAME),
        False,
        False,
        TopTools_FormatVersion.TopTools_FormatVersion_CURRENT,
    )
    if not written:
        raise OSError(f"could not write {BREP_NAME}")
    index = {
        "protocol": PROTOCOL_VERSION,
        "parts": [{"name": part.name, "color": part.color} for part in parts],
    }
    complete = True
    if notes is not None:
        index["print"] = notes
        # Measured as written: the core copies this file only up to a fixed size.
        if len(json.dumps(index, indent=1).encode("utf-8")) > MAX_NOTES_BYTES:
            index["print"] = {**notes, "fits": None, "fits_left_out": True}
            complete = False
    (job_dir / INDEX_NAME).write_text(json.dumps(index, indent=1) + "\n", encoding="utf-8")
    return complete


def load_print_notes(job_dir: Path) -> dict:
    """The `print` notes of model.parts.json: `orientations` (part -> up vector) and `fits`
    (declared cards, or None). Empty when the build left none; anything malformed is
    ignored, because the notes only ever improve a choice the engine makes itself."""
    try:
        raw = json.loads((job_dir / INDEX_NAME).read_text(encoding="utf-8")).get("print")
    except (OSError, ValueError, AttributeError):
        return {}
    if not isinstance(raw, dict):
        return {}
    notes: dict = {"orientations": {}, "fits": None,
                   "fits_left_out": raw.get("fits_left_out") is True}
    orientations = raw.get("orientations")
    if isinstance(orientations, dict):
        for name, up in orientations.items():
            if (isinstance(name, str) and NAME_RE.match(name) and isinstance(up, list)
                    and len(up) == 3
                    and all(isinstance(c, (int, float)) and not isinstance(c, bool)
                            and abs(c) < 1e6 for c in up)
                    and any(abs(c) > 1e-9 for c in up)):
                notes["orientations"][name] = tuple(float(c) for c in up)
    if isinstance(raw.get("fits"), list):
        notes["fits"] = raw["fits"]
    if isinstance(raw.get("pockets"), list):
        notes["pockets"] = raw["pockets"]
    if isinstance(raw.get("components"), list):
        notes["components"] = [c for c in raw["components"] if isinstance(c, str)][:32]
    return notes


def load_parts(job_dir: Path) -> list[NamedPart]:
    """Read model.brep and model.parts.json back. Raises BadRequest when they do not match."""
    from build123d import import_brep

    brep, index_path = job_dir / BREP_NAME, job_dir / INDEX_NAME
    for path in (brep, index_path):
        if path.is_symlink() or not path.is_file():
            raise BadRequest(
                f"this op needs {BREP_NAME} and {INDEX_NAME} from a finished build in the job "
                f"folder; {path.name} is missing"
            )
    if index_path.stat().st_size > MAX_INDEX_BYTES:
        raise BadRequest(f"{INDEX_NAME} is too large")
    try:
        index = json.loads(index_path.read_text(encoding="utf-8"))
        entries = [(str(e["name"]), str(e["color"])) for e in index["parts"]]
    except (ValueError, KeyError, TypeError) as exc:
        raise BadRequest(f"{INDEX_NAME} is malformed: {exc}") from exc
    if any(not NAME_RE.match(name) for name, _ in entries) or len(
        {name for name, _ in entries}
    ) != len(entries):
        raise BadRequest(f"{INDEX_NAME} has invalid or repeated part names")
    if not 1 <= len(entries) <= MAX_PARTS:
        raise BadRequest(f"{INDEX_NAME} must name 1 to {MAX_PARTS} parts")
    if any(not COLOR_RE.match(color) for _, color in entries):
        raise BadRequest(f"{INDEX_NAME} has a colour that is not #rrggbb")
    try:
        children = list(import_brep(brep))
    except Exception as exc:  # OCCT raises several unrelated types
        raise BadRequest(f"{BREP_NAME} cannot be read: {exc}") from exc
    if len(children) != len(entries):
        raise BadRequest(
            f"{BREP_NAME} holds {len(children)} parts but {INDEX_NAME} names {len(entries)}"
        )
    parts = []
    for (name, color), child in zip(entries, children, strict=True):
        if not child.solids():
            raise BadRequest(f"part {name!r} in {BREP_NAME} holds no solid")
        parts.append(NamedPart(name, _flatten(child), color))
    return parts


def select_parts(parts: list[NamedPart], names: Any) -> list[NamedPart]:
    """The parts with the given names, in model order. None selects all."""
    if names is None:
        return parts
    known = {part.name for part in parts}
    missing = [name for name in names if name not in known]
    if missing:
        raise BadRequest(
            f"unknown part(s) {', '.join(map(repr, missing))}; the model has: "
            f"{', '.join(sorted(known))}"
        )
    return [part for part in parts if part.name in set(names)]
