"""Procedural stand-ins for reference files, made by the tests themselves.

Nothing here is anybody's model: `stand_in_mesh` is a pleated dome written from a formula,
and the 3MF writer lays out a package the way the published 3MF core specification and
production extension describe, with a slicer-style settings part, so the reader is tested
on the layouts it must understand without a real file in the repository.
"""

from __future__ import annotations

import struct
import zipfile
from pathlib import Path

import numpy as np

CORE = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02"
PRODUCTION = "http://schemas.microsoft.com/3dmanufacturing/production/2015/06"
RELATIONSHIP = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"
RELS = ('<?xml version="1.0" encoding="UTF-8"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Target="/3D/3dmodel.model" Id="rel-1" Type="{RELATIONSHIP}"/>'
        '</Relationships>')
TYPES = ('<?xml version="1.0" encoding="UTF-8"?>'
         '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
         '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
         '<Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>'
         '</Types>')


def stand_in_mesh(radius: float = 20.0, height: float = 24.0, pleats: int = 7,
                  rings: int = 20, segments: int = 56) -> tuple[np.ndarray, np.ndarray]:
    """A closed pleated dome standing on z = 0: (vertices, triangles), counter-clockwise
    seen from outside. Widest at mid height, where it is `2 * radius * 1.06` across."""
    vertices = [(0.0, 0.0, 0.0)]
    for ring in range(1, rings):
        polar = np.pi * ring / rings
        for segment in range(segments):
            turn = 2 * np.pi * segment / segments
            r = radius * np.sin(polar) * (1 + 0.06 * np.cos(pleats * turn) * np.sin(polar) ** 2)
            vertices.append((r * np.cos(turn), r * np.sin(turn), height / 2 * (1 - np.cos(polar))))
    vertices.append((0.0, 0.0, height))
    top = len(vertices) - 1
    triangles = []
    for segment in range(segments):
        following = (segment + 1) % segments
        triangles.append((0, 1 + following, 1 + segment))
        last = 1 + (rings - 2) * segments
        triangles.append((top, last + segment, last + following))
        for ring in range(rings - 2):
            a, b = 1 + ring * segments + segment, 1 + ring * segments + following
            c, d = a + segments, b + segments
            triangles += [(a, b, d), (a, d, c)]
    return np.array(vertices, dtype=np.float64), np.array(triangles, dtype=np.int64)


def box_mesh(size: tuple[float, float, float], low: tuple[float, float, float] = (0, 0, 0)):
    """An axis-aligned box as a closed mesh."""
    x, y, z = size
    corners = np.array([[i * x, j * y, k * z] for k in (0, 1) for j in (0, 1) for i in (0, 1)],
                       dtype=np.float64) + np.array(low, dtype=np.float64)
    triangles = np.array([(0, 2, 1), (1, 2, 3), (4, 5, 6), (5, 7, 6), (0, 1, 4), (1, 5, 4),
                          (2, 6, 3), (3, 6, 7), (0, 4, 2), (2, 4, 6), (1, 3, 5), (3, 7, 5)])
    return corners, triangles


def write_stl(path: Path, vertices: np.ndarray, triangles: np.ndarray, ascii_: bool = False) -> Path:
    corners = vertices[triangles]
    if ascii_:
        lines = ["solid stand_in"]
        for tri in corners:
            lines.append(" facet normal 0 0 0\n  outer loop")
            lines += [f"   vertex {p[0]:.9g} {p[1]:.9g} {p[2]:.9g}" for p in tri]
            lines.append("  endloop\n endfacet")
        lines.append("endsolid stand_in")
        path.write_text("\n".join(lines) + "\n", encoding="ascii")
        return path
    with path.open("wb") as handle:
        handle.write(b"stand-in written by the Hotend tests".ljust(80, b" "))
        handle.write(struct.pack("<I", len(corners)))
        for tri in corners.astype("<f4"):
            handle.write(struct.pack("<3f", 0.0, 0.0, 0.0) + tri.tobytes() + b"\x00\x00")
    return path


def mesh_xml(object_id: int, vertices: np.ndarray, triangles: np.ndarray,
             kind: str = "model") -> str:
    points = "".join(f'<vertex x="{x:.9g}" y="{y:.9g}" z="{z:.9g}"/>' for x, y, z in vertices)
    faces = "".join(f'<triangle v1="{a}" v2="{b}" v3="{c}"/>' for a, b, c in triangles)
    return (f'<object id="{object_id}" type="{kind}"><mesh><vertices>{points}</vertices>'
            f'<triangles>{faces}</triangles></mesh></object>')


def model_xml(resources: str, build: str = "", unit: str = "millimeter") -> str:
    return (f'<?xml version="1.0" encoding="UTF-8"?><model unit="{unit}" xml:lang="en-US" '
            f'xmlns="{CORE}" xmlns:p="{PRODUCTION}" requiredextensions="p">'
            '<metadata name="Application">HotendTests-1.0</metadata>'
            f'<resources>{resources}</resources><build>{build}</build></model>')


def write_package(path: Path, entries: dict[str, str | bytes]) -> Path:
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
    return path


def write_plain_3mf(path: Path, vertices: np.ndarray, triangles: np.ndarray,
                    transform: str | None = None, unit: str = "millimeter") -> Path:
    """A minimal core-specification 3MF: one object, one build item."""
    placed = "" if transform is None else f' transform="{transform}"'
    return write_package(path, {
        "[Content_Types].xml": TYPES, "_rels/.rels": RELS,
        "3D/3dmodel.model": model_xml(mesh_xml(1, vertices, triangles),
                                      f'<item objectid="1"{placed}/>', unit),
    })


def write_project_3mf(path: Path, scale: float = 1.5) -> Path:
    """A slicer-project style 3MF: two plates. Plate 1 holds 'figure', an assembly of the
    stand-in dome and a 30 x 30 x 1 base plate, with a modifier volume and a negative
    box inside the dome, all in a separate model part and placed with a build-item transform
    that scales by `scale`. Plate 2 holds three small 'eye' boxes."""
    dome_v, dome_t = stand_in_mesh(radius=12.0, height=14.0)
    base_v, base_t = box_mesh((30.0, 30.0, 1.0), (-15.0, -15.0, -1.0))
    modifier_v, modifier_t = box_mesh((60.0, 60.0, 8.0), (-30.0, -30.0, 2.0))
    hole_v, hole_t = box_mesh((3.0, 3.0, 5.0), (4.0, -1.5, 5.0))  # wholly inside the dome
    eye_v, eye_t = box_mesh((2.0, 4.0, 1.5))
    sub = model_xml(mesh_xml(1, dome_v, dome_t) + mesh_xml(2, base_v, base_t)
                    + mesh_xml(3, modifier_v, modifier_t, "other")
                    + mesh_xml(4, hole_v, hole_t, "other"))
    eyes = model_xml(mesh_xml(1, eye_v, eye_t))
    components = "".join(
        f'<component p:path="/3D/Objects/figure_1.model" objectid="{n}" '
        'transform="1 0 0 0 1 0 0 0 1 0 0 1"/>' for n in (1, 2, 3, 4))
    resources = f'<object id="10" type="model"><components>{components}</components></object>'
    build = (f'<item objectid="10" transform="{scale} 0 0 0 {scale} 0 0 0 {scale} 100 120 0" '
             'printable="1"/>')
    for index in range(3):
        resources += (f'<object id="{20 + index}" type="model"><components><component '
                      'p:path="/3D/Objects/eyes_2.model" objectid="1"/></components></object>')
        build += f'<item objectid="{20 + index}" transform="1 0 0 0 1 0 0 0 1 {40 + 6 * index} 300 0"/>'
    settings = ['<?xml version="1.0" encoding="UTF-8"?><config>',
                '<object id="10"><metadata key="name" value="figure"/>',
                '<part id="1" subtype="normal_part"><metadata key="name" value="DOME"/></part>',
                '<part id="2" subtype="normal_part"><metadata key="name" value="BASE"/></part>',
                '<part id="3" subtype="modifier_part"><metadata key="name" value="more infill"/></part>',
                '<part id="4" subtype="negative_part"><metadata key="name" value="vent"/></part>',
                '</object>']
    for index in range(3):
        settings.append(f'<object id="{20 + index}"><metadata key="name" value="eye"/>'
                        '<part id="1" subtype="normal_part"><metadata key="name" value="eye"/></part></object>')
    settings.append('<plate><metadata key="plater_id" value="1"/><metadata key="plater_name" value="big"/>'
                    '<model_instance><metadata key="object_id" value="10"/></model_instance></plate>')
    settings.append('<plate><metadata key="plater_id" value="2"/><metadata key="plater_name" value="eyes"/>'
                    + "".join(f'<model_instance><metadata key="object_id" value="{20 + index}"/></model_instance>'
                              for index in range(3)) + '</plate></config>')
    return write_package(path, {
        "[Content_Types].xml": TYPES, "_rels/.rels": RELS,
        "3D/3dmodel.model": model_xml(resources, build),
        "3D/Objects/figure_1.model": sub, "3D/Objects/eyes_2.model": eyes,
        "Metadata/model_settings.config": "".join(settings),
    })
