"""Reading a reference file (STL, 3MF, STEP) as hostile input.

A reference is a real thing a printed part is fitted to, given as a file the user chose:
a model downloaded from a sharing site, a scan, a maker's STEP. Such a file is somebody
else's data and is read with the same distrust as a model script's output.

Guarantees:
- Nothing is extracted to disk and no path from inside a file is used on the file system.
  A 3MF's parts are read from the archive in memory, by entry name, and a name that is
  absolute after its leading slash, holds `..`, a backslash or a drive letter is refused.
- Everything is capped: the file's size, the number of archive entries, the uncompressed
  size of each entry that is read (counted as it is read, not as the header claims), the
  number of triangles and vertices, and the depth of component nesting.
- XML is parsed by expat with document type declarations, entities and external references
  refused outright, so there is no entity expansion to bound.
- Nothing is repaired silently. Triangles that name a missing vertex, non-finite numbers
  and transforms that are not 12 finite numbers are errors; triangles of zero area are
  dropped and counted in the notes.
- What comes out is the object as it will be printed: every component and build-item
  transform applied (non-uniform scale included), in millimetres. A file that holds several
  objects or plates is never merged: `ReferenceFile.objects` lists them and
  `ReferenceFile.select` returns the one a selection names, or raises `ReferenceFileError`
  with the list when the selection does not single one out.

3MF is read from the published specifications: the core specification (package layout,
`model`, `resources`, `object`, `mesh`, `components`, `build`, row-major 4x3 transforms that
act on row vectors) and the production extension (`p:path` on a component or a build item
names the model part that holds the object). Slicer projects add a settings file,
`Metadata/model_settings.config`; what is read from it was learned from files written by
Bambu Studio 2.x: per `object` a `name` and its `part`s
(`id` = the component's object id, `subtype`), and per `plate` its number, name and the
objects on it. A part whose subtype is `normal_part` is geometry, `negative_part` is a hole
cut from the object, and anything else (`modifier_part`, `support_blocker`,
`support_enforcer`) only changes print settings in a region and is never geometry.
"""

from __future__ import annotations

import hashlib
import math
import re
import struct
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np

FORMATS = ("stl", "3mf", "step")
EXTENSIONS = {".stl": "stl", ".3mf": "3mf", ".step": "step", ".stp": "step"}
UNITS_MM = {"micron": 0.001, "millimeter": 1.0, "centimeter": 10.0, "inch": 25.4,
            "foot": 304.8, "meter": 1000.0}
REQUEST_UNITS = {"mm": "millimeter", "cm": "centimeter", "m": "meter", "in": "inch"}

MAX_FILE_BYTES = 256 * 1024 * 1024
MAX_STEP_BYTES = 64 * 1024 * 1024
MAX_ARCHIVE_ENTRIES = 4096
MAX_ENTRY_BYTES = 512 * 1024 * 1024
MAX_XML_BYTES_TOTAL = 768 * 1024 * 1024
MAX_SETTINGS_BYTES = 8 * 1024 * 1024
MAX_MODEL_PARTS = 256
MAX_OBJECTS = 4096
MAX_COMPONENT_DEPTH = 8
MAX_INSTANCES = 4096
DEFAULT_MAX_TRIANGLES = 2_000_000
# Coordinates beyond this (mm, after every transform) are not a printable thing.
MAX_COORDINATE_MM = 1.0e5
STEP_DEFLECTION_MM = 0.02
STEP_ANGULAR_RAD = 0.1

CORE_NS = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02"
PRODUCTION_NS = "http://schemas.microsoft.com/3dmanufacturing/production/2015/06"
MODEL_RELATIONSHIP = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"
SETTINGS_ENTRY = "Metadata/model_settings.config"
GEOMETRY_ROLE, NEGATIVE_ROLE = "normal_part", "negative_part"
_ENTRY_RE = re.compile(r"[^\\:\x00-\x1f]+\Z")
_IDENTITY = (1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0)


class ReferenceFileError(ValueError):
    """A reference file cannot be used as asked. The message says what to change."""


@dataclass
class MeshPart:
    """One mesh of an object, as printed (millimetres, every transform applied)."""

    name: str
    role: str  # normal_part, negative_part, or a settings-only role that is not geometry
    vertices: np.ndarray  # (n, 3) float64
    triangles: np.ndarray  # (m, 3) int64

    @property
    def is_geometry(self) -> bool:
        return self.role == GEOMETRY_ROLE

    def bbox(self) -> tuple[np.ndarray, np.ndarray]:
        return self.vertices.min(axis=0), self.vertices.max(axis=0)


@dataclass
class FileObject:
    """One printable object of a file: a build item of a 3MF, the mesh of an STL, a solid
    of a STEP file."""

    index: int  # 1-based position in the file
    name: str
    plate: int | None
    plate_name: str | None
    parts: list[MeshPart]

    def geometry(self) -> list[MeshPart]:
        return [part for part in self.parts if part.is_geometry]

    def bbox(self) -> tuple[np.ndarray, np.ndarray] | None:
        boxes = [part.bbox() for part in self.geometry() if len(part.vertices)]
        if not boxes:
            return None
        return (np.min([b[0] for b in boxes], axis=0), np.max([b[1] for b in boxes], axis=0))

    def listing(self) -> dict:
        box = self.bbox()
        return {
            "index": self.index, "name": self.name, "plate": self.plate,
            "plate_name": self.plate_name,
            "size": None if box is None else [round(float(v), 3) for v in box[1] - box[0]],
            "triangles": int(sum(len(part.triangles) for part in self.geometry())),
            "parts": [
                {"name": part.name, "role": part.role, "geometry": part.is_geometry,
                 "triangles": len(part.triangles),
                 "size": [round(float(v), 3) for v in np.ptp(part.vertices, axis=0)]
                 if len(part.vertices) else None}
                for part in self.parts
            ],
        }


@dataclass
class ReferenceFile:
    """A reference file read into memory: its objects and what was noticed on the way."""

    format: str
    unit: str
    bytes: int
    sha256: str
    objects: list[FileObject]
    notes: list[str] = field(default_factory=list)
    producer: str | None = None

    def select(self, plate: int | None = None, obj: int | str | None = None) -> FileObject:
        """The one object a selection names: by plate number, by object name or 1-based
        index, or both. Raises ReferenceFileError listing what the file holds when the
        selection matches none or more than one."""
        found = self.objects
        if plate is not None:
            found = [o for o in found if o.plate == plate]
        if isinstance(obj, int) and not isinstance(obj, bool):
            found = [o for o in found if o.index == obj]
        elif obj is not None:
            found = [o for o in found if o.name == obj]
        found = [o for o in found if o.geometry()]
        if len(found) == 1:
            return found[0]
        asked = ", ".join(f"{key} {value!r}" for key, value in (("plate", plate), ("object", obj))
                          if value is not None)
        if not found:
            raise ReferenceFileError(
                f"the file holds no object for {asked or 'this reference'}; it holds: "
                f"{self.describe()}"
            )
        raise ReferenceFileError(
            f"the file holds {len(found)} objects{' for ' + asked if asked else ''} and they "
            "are never merged into one reference; name the one to use with select "
            '({"plate": n} and/or {"object": name or index}). It holds: '
            f"{self.describe()}"
        )

    def describe(self) -> str:
        said = []
        for o in self.objects[:12]:
            box = o.bbox()
            size = "no geometry" if box is None else " x ".join(
                f"{v:.1f}" for v in box[1] - box[0]) + " mm"
            where = "" if o.plate is None else (
                f" on plate {o.plate}" + (f" '{o.plate_name}'" if o.plate_name else ""))
            said.append(f"{o.index}: '{o.name}'{where}, {size}")
        return "; ".join(said) + ("; ..." if len(self.objects) > 12 else "")


def detect_format(path: Path, declared: str | None = None) -> str:
    if declared is not None:
        if declared not in FORMATS:
            raise ReferenceFileError(f"format must be one of {', '.join(FORMATS)}, got {declared!r}")
        return declared
    try:
        return EXTENSIONS[path.suffix.lower()]
    except KeyError:
        raise ReferenceFileError(
            f"cannot tell the format of {path.name!r} from its extension; a reference is an "
            ".stl, .3mf, .step or .stp file"
        ) from None


def read_reference_file(path: Path, format_: str | None = None, *,
                        max_triangles: int = DEFAULT_MAX_TRIANGLES) -> ReferenceFile:
    """Read `path` (already resolved inside the job folder by the caller). Raises
    ReferenceFileError when the file is not a usable STL, 3MF or STEP file within the caps."""
    kind = detect_format(path, format_)
    try:
        if path.is_symlink() or not path.is_file():
            raise ReferenceFileError(f"{path.name!r} is not a plain file")
        size = path.stat().st_size
    except OSError as exc:
        raise ReferenceFileError(f"cannot read {path.name!r}: {exc}") from exc
    limit = MAX_STEP_BYTES if kind == "step" else MAX_FILE_BYTES
    if size > limit:
        raise ReferenceFileError(
            f"{path.name!r} is {size / 1e6:.0f} MB; a {kind} reference may be at most "
            f"{limit / 1e6:.0f} MB"
        )
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1 << 20):
            digest.update(chunk)
    reader = {"stl": _read_stl, "3mf": _read_3mf, "step": _read_step}[kind]
    try:
        loaded: ReferenceFile = reader(path, max_triangles)
    except ReferenceFileError:
        raise
    except (OSError, zipfile.BadZipFile, struct.error, MemoryError, RecursionError,
            UnicodeDecodeError, OverflowError) as exc:
        raise ReferenceFileError(
            f"{path.name!r} is not a readable {kind} file ({type(exc).__name__}: "
            f"{str(exc)[:160]})"
        ) from exc
    loaded.bytes, loaded.sha256 = size, digest.hexdigest()
    if not any(o.geometry() for o in loaded.objects):
        raise ReferenceFileError(f"{path.name!r} holds no geometry")
    return loaded


def _checked(vertices: np.ndarray, triangles: np.ndarray, what: str,
             notes: list[str]) -> tuple[np.ndarray, np.ndarray]:
    """Validate one mesh: finite, plausible coordinates and triangles that name existing
    vertices. Triangles of zero area are dropped and counted."""
    vertices = np.ascontiguousarray(vertices, dtype=np.float64).reshape(-1, 3)
    triangles = np.ascontiguousarray(triangles, dtype=np.int64).reshape(-1, 3)
    if not np.isfinite(vertices).all():
        raise ReferenceFileError(f"{what} has coordinates that are not finite numbers")
    if len(vertices) and float(np.abs(vertices).max()) > MAX_COORDINATE_MM:
        raise ReferenceFileError(
            f"{what} reaches {float(np.abs(vertices).max()):.3g} mm from the origin, which "
            "is not a printable thing; check the units"
        )
    if len(triangles) and (triangles.min() < 0 or triangles.max() >= len(vertices)):
        raise ReferenceFileError(f"{what} has triangles that name vertices it does not have")
    if len(triangles):
        tri = vertices[triangles]
        doubled = np.linalg.norm(np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)
        flat = doubled <= 0.0
        if flat.any():
            notes.append(f"{what}: {int(flat.sum())} triangle(s) of zero area were left out")
            triangles = triangles[~flat]
    return vertices, triangles


# --------------------------------------------------------------------------------------
# STL
# --------------------------------------------------------------------------------------


def _read_stl(path: Path, max_triangles: int) -> ReferenceFile:
    data = path.read_bytes()
    notes: list[str] = []
    count = struct.unpack_from("<I", data, 80)[0] if len(data) >= 84 else -1
    if count >= 0 and len(data) == 84 + 50 * count:
        if count > max_triangles:
            raise ReferenceFileError(
                f"the STL has {count} triangles; a reference may have at most {max_triangles}"
            )
        records = np.frombuffer(data, dtype=np.dtype([
            ("normal", "<f4", 3), ("points", "<f4", (3, 3)), ("attribute", "<u2")]),
            count=count, offset=84)
        corners = records["points"].reshape(-1, 3).astype(np.float64)
    else:
        corners = _ascii_stl(data, max_triangles)
    if len(corners) == 0:
        raise ReferenceFileError("the STL holds no triangles")
    # An STL repeats every corner; equal coordinates are one vertex (nothing is moved).
    unique, inverse = np.unique(corners, axis=0, return_inverse=True)
    vertices, triangles = _checked(unique, inverse.reshape(-1, 3), "the STL", notes)
    part = MeshPart(path.stem, GEOMETRY_ROLE, vertices, triangles)
    return ReferenceFile("stl", "millimeter", 0, "",
                         [FileObject(1, path.stem, None, None, [part])], notes)


def _ascii_stl(data: bytes, max_triangles: int) -> np.ndarray:
    if not data[:512].lstrip().lower().startswith(b"solid"):
        raise ReferenceFileError(
            "the file is neither a binary STL (its length does not match its triangle "
            "count) nor a text STL"
        )
    numbers: list[float] = []
    try:
        for line in data.split(b"\n"):
            words = line.split()
            if len(words) == 4 and words[0].lower() == b"vertex":
                numbers += (float(words[1]), float(words[2]), float(words[3]))
                if len(numbers) > 9 * max_triangles:
                    raise ReferenceFileError(
                        f"the STL has more than {max_triangles} triangles, the most a "
                        "reference may have"
                    )
    except ValueError as exc:
        if isinstance(exc, ReferenceFileError):
            raise
        raise ReferenceFileError("the text STL has a vertex that is not three numbers") from exc
    if len(numbers) % 9:
        raise ReferenceFileError("the text STL has a facet that is not three vertices")
    return np.array(numbers, dtype=np.float64).reshape(-1, 3)


# --------------------------------------------------------------------------------------
# 3MF
# --------------------------------------------------------------------------------------


@dataclass
class _Object:
    id: str
    type: str
    name: str
    vertices: Any = None  # numpy arrays for a mesh object
    triangles: Any = None
    components: list = field(default_factory=list)  # (model path or None, object id, matrix)


@dataclass
class _Model:
    unit: str = "millimeter"
    objects: dict = field(default_factory=dict)
    items: list = field(default_factory=list)  # (model path or None, object id, matrix)
    application: str | None = None


class _Budget:
    """What may still be read from one archive."""

    def __init__(self, max_triangles: int) -> None:
        self.xml_bytes = MAX_XML_BYTES_TOTAL
        self.triangles = max_triangles
        self.max_triangles = max_triangles
        self.objects = MAX_OBJECTS

    def take_triangles(self, count: int = 1) -> None:
        self.triangles -= count
        if self.triangles < 0:
            raise ReferenceFileError(
                f"the file has more than {self.max_triangles} triangles, the most a "
                "reference may have"
            )


def _entry_name(target: str, base: str = "") -> str:
    """The archive entry a part name inside the package points at. Refuses anything that
    is not a plain path below the package root."""
    if not isinstance(target, str) or not target or len(target) > 512:
        raise ReferenceFileError("the 3MF names a part with an unusable path")
    name = target[1:] if target.startswith("/") else (base + target)
    pieces = name.split("/")
    if (not _ENTRY_RE.match(name) or name.startswith("/")
            or any(piece in ("", ".", "..") for piece in pieces)):
        raise ReferenceFileError(f"the 3MF names a part outside the package: {target[:80]!r}")
    return name


def _parse_xml(archive: zipfile.ZipFile, name: str, handler: Any, budget: _Budget,
               cap: int | None = None) -> None:
    """Feed one archive entry to expat, counting the bytes as they are inflated."""
    from xml.parsers import expat

    cap = MAX_ENTRY_BYTES if cap is None else cap

    try:
        info = archive.getinfo(name)
    except KeyError:
        raise ReferenceFileError(f"the 3MF has no part {name!r}") from None
    if info.is_dir() or info.file_size > cap:
        raise ReferenceFileError(
            f"the part {name!r} of the 3MF is {info.file_size / 1e6:.0f} MB uncompressed; "
            f"the limit is {cap / 1e6:.0f} MB"
        )
    parser = expat.ParserCreate(namespace_separator=" ")
    parser.buffer_text = True

    def refuse(*_args: Any) -> None:
        raise ReferenceFileError(
            f"the part {name!r} of the 3MF declares a document type or an entity; a "
            "reference file may not"
        )

    parser.StartDoctypeDeclHandler = refuse
    parser.EntityDeclHandler = refuse
    parser.UnparsedEntityDeclHandler = refuse
    parser.ExternalEntityRefHandler = refuse
    parser.SetParamEntityParsing(expat.XML_PARAM_ENTITY_PARSING_NEVER)
    parser.StartElementHandler = handler.start
    parser.EndElementHandler = handler.end
    if hasattr(handler, "text"):
        parser.CharacterDataHandler = handler.text
    read = 0
    try:
        with archive.open(info) as stream:
            while chunk := stream.read(1 << 20):
                read += len(chunk)
                budget.xml_bytes -= len(chunk)
                if read > cap or budget.xml_bytes < 0:
                    raise ReferenceFileError(
                        "the 3MF unpacks to more data than a reference may hold "
                        f"({MAX_XML_BYTES_TOTAL / 1e6:.0f} MB of XML)"
                    )
                parser.Parse(chunk, False)
            parser.Parse(b"", True)
    except expat.ExpatError as exc:
        raise ReferenceFileError(f"the part {name!r} of the 3MF is not well-formed XML: {exc}") from exc


def _matrix(text: str | None, what: str) -> tuple[float, ...]:
    """A 3MF transform: 12 numbers, the rows of a 4x3 matrix that acts on row vectors."""
    if text is None:
        return _IDENTITY
    try:
        values = tuple(float(word) for word in text.split())
    except ValueError:
        values = ()
    if len(values) != 12 or not all(math.isfinite(v) for v in values):
        raise ReferenceFileError(f"{what} has a transform that is not 12 finite numbers")
    return values


def _apply(matrix: tuple[float, ...], points: np.ndarray) -> np.ndarray:
    m = np.array(matrix, dtype=np.float64).reshape(4, 3)
    return points @ m[:3] + m[3]


def _compose(inner: tuple[float, ...], outer: tuple[float, ...]) -> tuple[float, ...]:
    """The transform that applies `inner` first and `outer` second."""
    a, b = (np.array(m, dtype=np.float64).reshape(4, 3) for m in (inner, outer))
    return tuple(np.vstack([a[:3] @ b[:3], a[3] @ b[:3] + b[3]]).ravel())


class _ModelHandler:
    """Collects the objects and build items of one model part."""

    def __init__(self, budget: _Budget, base: str) -> None:
        self.model = _Model()
        self.budget = budget
        self.base = base
        self.current: _Object | None = None
        self.numbers: list[float] = []
        self.indices: list[int] = []
        self.metadata: str | None = None
        self.depth = 0

    def start(self, name: str, attrs: dict) -> None:
        self.depth += 1
        if self.depth > 64:
            raise ReferenceFileError("the 3MF nests its XML deeper than a model part does")
        namespace, _, local = name.rpartition(" ")
        if namespace != CORE_NS:
            return
        if local == "vertex":
            try:
                self.numbers += (float(attrs["x"]), float(attrs["y"]), float(attrs["z"]))
            except (KeyError, ValueError):
                raise ReferenceFileError("the 3MF has a vertex without three numbers") from None
        elif local == "triangle":
            self.budget.take_triangles()
            try:
                self.indices += (int(attrs["v1"]), int(attrs["v2"]), int(attrs["v3"]))
            except (KeyError, ValueError):
                raise ReferenceFileError("the 3MF has a triangle without three vertex numbers") from None
        elif local == "object":
            self.budget.objects -= 1
            if self.budget.objects < 0:
                raise ReferenceFileError(f"the 3MF has more than {MAX_OBJECTS} objects")
            identifier = attrs.get("id")
            if not identifier or identifier in self.model.objects:
                raise ReferenceFileError("the 3MF has an object without an id of its own")
            self.current = _Object(identifier, attrs.get("type", "model"),
                                   str(attrs.get("name") or "")[:120])
            self.numbers, self.indices = [], []
        elif local == "component" and self.current is not None:
            path = attrs.get(f"{PRODUCTION_NS} path")
            self.current.components.append((
                None if path is None else _entry_name(path, self.base),
                attrs.get("objectid"), _matrix(attrs.get("transform"), "a component"),
            ))
            if len(self.current.components) > MAX_INSTANCES:
                raise ReferenceFileError("an object of the 3MF has too many components")
        elif local == "item":
            path = attrs.get(f"{PRODUCTION_NS} path")
            if attrs.get("printable", "1") not in ("0", "false"):
                self.model.items.append((
                    None if path is None else _entry_name(path, self.base),
                    attrs.get("objectid"), _matrix(attrs.get("transform"), "a build item"),
                ))
            if len(self.model.items) > MAX_INSTANCES:
                raise ReferenceFileError("the 3MF has too many build items")
        elif local == "model":
            unit = attrs.get("unit", "millimeter")
            if unit not in UNITS_MM:
                raise ReferenceFileError(f"the 3MF states an unknown unit {unit[:40]!r}")
            self.model.unit = unit
        elif local == "metadata" and attrs.get("name") == "Application":
            self.metadata = ""

    def text(self, data: str) -> None:
        if self.metadata is not None and len(self.metadata) < 200:
            self.metadata += data

    def end(self, name: str) -> None:
        self.depth -= 1
        namespace, _, local = name.rpartition(" ")
        if namespace != CORE_NS:
            return
        if local == "mesh" and self.current is not None:
            self.current.vertices = np.array(self.numbers, dtype=np.float64).reshape(-1, 3)
            self.current.triangles = np.array(self.indices, dtype=np.int64).reshape(-1, 3)
            self.numbers, self.indices = [], []
        elif local == "object" and self.current is not None:
            self.model.objects[self.current.id] = self.current
            self.current = None
        elif local == "metadata" and self.metadata is not None:
            self.model.application = self.metadata.strip()[:120] or None
            self.metadata = None


class _RelationshipHandler:
    def __init__(self) -> None:
        self.target: str | None = None

    def start(self, name: str, attrs: dict) -> None:
        if (name.rpartition(" ")[2] == "Relationship" and self.target is None
                and attrs.get("Type") == MODEL_RELATIONSHIP):
            self.target = attrs.get("Target")

    def end(self, name: str) -> None:
        pass


class _SettingsHandler:
    """`Metadata/model_settings.config` of a slicer project: names, part roles, plates."""

    def __init__(self) -> None:
        self.objects: dict[str, dict] = {}
        self.plates: list[dict] = []
        self.stack: list[tuple[str, dict]] = []

    def start(self, name: str, attrs: dict) -> None:
        local = name.rpartition(" ")[2]
        if local == "object":
            entry = {"name": "", "parts": {}}
            self.objects[str(attrs.get("id"))] = entry
            self.stack.append(("object", entry))
        elif local == "part" and self.stack and self.stack[-1][0] == "object":
            entry = {"name": "", "role": str(attrs.get("subtype") or GEOMETRY_ROLE)[:40]}
            self.stack[-1][1]["parts"][str(attrs.get("id"))] = entry
            self.stack.append(("part", entry))
        elif local == "plate":
            entry = {"index": None, "name": "", "objects": []}
            self.plates.append(entry)
            self.stack.append(("plate", entry))
        elif local == "model_instance" and self.stack and self.stack[-1][0] == "plate":
            self.stack.append(("instance", self.stack[-1][1]))
        elif local == "metadata" and self.stack:
            kind, entry = self.stack[-1]
            key, value = attrs.get("key"), str(attrs.get("value") or "")[:200]
            if kind in ("object", "part") and key == "name":
                entry["name"] = value
            elif kind == "plate" and key == "plater_id" and value.isdigit():
                entry["index"] = int(value)
            elif kind == "plate" and key == "plater_name":
                entry["name"] = value
            elif kind == "instance" and key == "object_id":
                entry["objects"].append(value)
            self.stack.append(("metadata", entry))
        else:
            self.stack.append(("other", {}))
        if len(self.stack) > 32 or len(self.objects) > MAX_OBJECTS or len(self.plates) > 256:
            raise ReferenceFileError("the project settings of the 3MF are implausibly large")

    def end(self, name: str) -> None:
        if self.stack:
            self.stack.pop()


def _read_3mf(path: Path, max_triangles: int) -> ReferenceFile:
    budget = _Budget(max_triangles)
    notes: list[str] = []
    with zipfile.ZipFile(path) as archive:
        if len(archive.infolist()) > MAX_ARCHIVE_ENTRIES:
            raise ReferenceFileError(
                f"the 3MF archive has more than {MAX_ARCHIVE_ENTRIES} entries"
            )
        names = set(archive.namelist())
        root = "3D/3dmodel.model"
        if "_rels/.rels" in names:
            relationships = _RelationshipHandler()
            _parse_xml(archive, "_rels/.rels", relationships, budget, MAX_SETTINGS_BYTES)
            if relationships.target:
                root = _entry_name(relationships.target)
        models: dict[str, _Model] = {}

        def model(name: str) -> _Model:
            if name not in models:
                if len(models) >= MAX_MODEL_PARTS:
                    raise ReferenceFileError("the 3MF is split over too many model parts")
                handler = _ModelHandler(budget, name.rpartition("/")[0] + "/")
                _parse_xml(archive, name, handler, budget)
                models[name] = handler.model
            return models[name]

        main = model(root)
        settings = _SettingsHandler()
        if SETTINGS_ENTRY in names:
            _parse_xml(archive, SETTINGS_ENTRY, settings, budget, MAX_SETTINGS_BYTES)
        plate_of = {object_id: plate for plate in settings.plates for object_id in plate["objects"]}
        scale = UNITS_MM[main.unit]
        placed = [0, 0]  # meshes placed, triangles placed

        def meshes(part: str, object_id: str | None, matrix: tuple[float, ...], roles: dict,
                   depth: int, role: str, name: str) -> list[MeshPart]:
            """Every mesh below one object, with the transforms on the way applied."""
            if depth > MAX_COMPONENT_DEPTH:
                raise ReferenceFileError("the 3MF nests components deeper than a model does")
            found = model(part).objects.get(str(object_id))
            if found is None:
                raise ReferenceFileError(
                    f"the 3MF refers to an object {object_id!r} it does not hold")
            if found.vertices is not None:
                # The core specification: only `model` and `solidsupport` objects are the
                # thing itself. A slicer writes its modifiers and negative parts as `other`.
                if found.type not in ("model", "solidsupport") and role == GEOMETRY_ROLE:
                    role = found.type
                placed[0] += 1
                placed[1] += len(found.triangles)
                if placed[0] > MAX_INSTANCES or placed[1] > max_triangles:
                    raise ReferenceFileError(
                        f"the 3MF places more than {max_triangles} triangles, the most a "
                        "reference may have"
                    )
                what = f"the mesh '{name or found.name or found.id}'"
                vertices, triangles = _checked(
                    _apply(matrix, found.vertices) * scale, found.triangles, what, notes)
                det = float(np.linalg.det(np.array(matrix).reshape(4, 3)[:3]))
                if det == 0.0:
                    raise ReferenceFileError(
                        f"{what} is placed with a transform that flattens it")
                if det < 0:  # a mirroring placement turns the triangles inside out
                    triangles = triangles[:, ::-1]
                return [MeshPart(name or found.name or f"object {found.id}", role, vertices,
                                 triangles)]
            out: list[MeshPart] = []
            for sub_part, sub_id, sub_matrix in found.components:
                # The settings file describes the components of a build item's own object.
                described = roles.get(str(sub_id), {}) if depth == 0 else {}
                out += meshes(
                    sub_part or part, sub_id, _compose(sub_matrix, matrix), roles, depth + 1,
                    described.get("role", role), described.get("name") or name,
                )
            return out

        objects = []
        for index, (item_part, object_id, matrix) in enumerate(main.items, start=1):
            described = settings.objects.get(str(object_id), {})
            plate = plate_of.get(str(object_id))
            parts = meshes(item_part or root, object_id, matrix, described.get("parts", {}), 0,
                           GEOMETRY_ROLE, "")
            name = described.get("name") or next(
                (p.name for p in parts if p.is_geometry), f"object {object_id}")
            objects.append(FileObject(
                index, name, None if plate is None else plate["index"],
                None if plate is None else (plate["name"] or None), parts,
            ))
    for obj in objects:
        left_out = [part for part in obj.parts if part.role not in (GEOMETRY_ROLE, NEGATIVE_ROLE)]
        if left_out:
            notes.append(
                f"'{obj.name}': {len(left_out)} part(s) only change print settings in a "
                f"region ({', '.join(sorted({p.role for p in left_out}))}: "
                f"{', '.join(repr(p.name) for p in left_out[:4])}) and are not geometry"
            )
    return ReferenceFile("3mf", main.unit, 0, "", objects, notes, main.application)


# --------------------------------------------------------------------------------------
# STEP
# --------------------------------------------------------------------------------------


def _read_step(path: Path, max_triangles: int) -> ReferenceFile:
    """A STEP file through the CAD kernel's own reader. Every solid is one object; its
    surface is meshed finely (0.02 mm) for the measures a reference offers."""
    from build123d import import_step
    from OCP.BRep import BRep_Tool
    from OCP.BRepMesh import BRepMesh_IncrementalMesh
    from OCP.TopAbs import TopAbs_REVERSED
    from OCP.TopLoc import TopLoc_Location

    try:
        shape = import_step(path)
    except Exception as exc:
        raise ReferenceFileError(
            f"{path.name!r} cannot be read as a STEP file ({str(exc)[:160]})"
        ) from exc
    solids = list(shape.solids())
    if not solids:
        raise ReferenceFileError(f"{path.name!r} holds no solid; a STEP reference must be a solid")
    if len(solids) > MAX_OBJECTS:
        raise ReferenceFileError(f"{path.name!r} holds more than {MAX_OBJECTS} solids")
    notes: list[str] = []
    budget = _Budget(max_triangles)
    objects = []
    for index, solid in enumerate(solids, start=1):
        BRepMesh_IncrementalMesh(solid.wrapped, STEP_DEFLECTION_MM, False, STEP_ANGULAR_RAD, True)
        vertices, triangles, offset = [], [], 0
        for face in solid.faces():
            location = TopLoc_Location()
            mesh = BRep_Tool.Triangulation_s(face.wrapped, location)
            if mesh is None:
                continue
            transformation = location.Transformation()
            count = mesh.NbNodes()
            points = np.array([
                (p.X(), p.Y(), p.Z())
                for p in (mesh.Node(i).Transformed(transformation) for i in range(1, count + 1))
            ], dtype=np.float64).reshape(-1, 3)
            tri = np.array([mesh.Triangle(i).Get() for i in range(1, mesh.NbTriangles() + 1)],
                           dtype=np.int64).reshape(-1, 3) - 1
            budget.take_triangles(len(tri))
            if face.wrapped.Orientation() == TopAbs_REVERSED:
                tri = tri[:, ::-1]
            vertices.append(points)
            triangles.append(tri + offset)
            offset += count
        if not triangles:
            continue
        merged = np.concatenate(vertices)
        # Faces share their boundary points; equal coordinates are one vertex.
        keys = np.round(merged / 1e-7).astype(np.int64)
        _, first, inverse = np.unique(keys, axis=0, return_index=True, return_inverse=True)
        checked = _checked(merged[first], inverse.ravel()[np.concatenate(triangles)],
                           f"solid {index}", notes)
        name = getattr(solid, "label", "") or f"solid {index}"
        objects.append(FileObject(index, name, None, None,
                                  [MeshPart(name, GEOMETRY_ROLE, *checked)]))
    return ReferenceFile("step", "millimeter", 0, "", objects, notes)
