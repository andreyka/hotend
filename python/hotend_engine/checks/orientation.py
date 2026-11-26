"""overhangs and bed_contact: how the part sits on the bed (Z up).

By default the part is judged as modelled, because that is how a caller that does not run
print preparation prints it. With `checks.orientation` = `prepared` it is judged the way
`hotend_engine.prepare` turns it (the `prepare` op makes the plate from the same choice), so
a lid drawn in place on its box no longer warns about an overhang that turning it over
removes. `judge_overhangs` and `judge_bed_contact` take the part in the orientation to judge;
the `prepare` op calls them on the parts of a plate.

Planar faces are judged by their exact B-rep normal and area. Curved faces are judged per
triangle of the tessellation; when that is not available for a part that has curved faces, the
overhang check is skipped instead of guessing.
"""

from __future__ import annotations

import math

import numpy as np

from ..parts import NamedPart
from ..prepare import describe_up, how_printed
from .base import PASS, SKIPPED, WARN, WARNING, CheckEnv, CheckResult, round_floats

# Used when the request's material does not give max_overhang_deg.
DEFAULT_MAX_OVERHANG_DEG = 45.0
BED_TOLERANCE_MM = 0.01
OVERHANG_WARN_AREA_MM2 = 4.0
MIN_CONTACT_AREA_MM2 = 20.0


def _planar_normal(face) -> np.ndarray | None:
    from build123d import GeomType

    if face.geom_type != GeomType.PLANE:
        return None
    return np.array(tuple(face.normal_at(face.center())), dtype=float)


def _judged(env: CheckEnv, part: NamedPart, judge) -> CheckResult:
    """Run `judge` on the part as modelled or, when asked, as preparation turns it."""
    if env.options.orientation != "prepared":
        return judge(env.ctx.material, part, env.mesh(part), "as modelled", env.no_mesh_reason)
    moved, mesh, choice = env.prepared(part)
    how = how_printed(choice.up)
    result = judge(env.ctx.material, moved, mesh, how, env.no_mesh_reason)
    result.data["orientation"] = {
        "source": choice.source, "up": [float(v) for v in choice.up],
        "turned": describe_up(choice.up), "explanation": choice.sentence,
    }
    return result


def overhangs(env: CheckEnv, part: NamedPart) -> CheckResult:
    """Area facing downwards more steeply than the material prints without support."""
    return _judged(env, part, judge_overhangs)


def bed_contact(env: CheckEnv, part: NamedPart) -> CheckResult:
    """Flat area resting on the bed."""
    return _judged(env, part, judge_bed_contact)


def judge_overhangs(material, part: NamedPart, mesh, how: str = "as modelled",
                    no_mesh_reason: str = "no triangle mesh is available") -> CheckResult:
    """The overhangs of `part` in the orientation it is given in (Z up). `how` says in
    words what that orientation is."""
    limit = material.max_overhang_deg if material and material.max_overhang_deg else None
    source = "material" if limit else "default"
    limit = limit or DEFAULT_MAX_OVERHANG_DEG
    # A surface tilted `limit` degrees from vertical has a normal with z = -sin(limit).
    threshold = -math.sin(math.radians(limit)) - 1e-6
    box = part.shape.bounding_box()
    bed_z = float(box.min.Z)
    area = 0.0
    faces = []
    unmeasured = 0
    for index, face in enumerate(part.shape.faces()):
        normal = _planar_normal(face)
        if normal is not None:
            on_bed = abs(float(face.center().Z) - bed_z) <= BED_TOLERANCE_MM and normal[2] < -0.999
            face_area = float(face.area) if normal[2] < threshold and not on_bed else 0.0
        elif mesh is not None and mesh.face_map_ok:
            mask = mesh.triangle_face == index
            normals, areas = mesh.triangle_normals_and_areas()
            face_area = float(areas[mask & (normals[:, 2] < threshold)].sum())
        else:
            unmeasured += 1
            continue
        if face_area > 1e-6:
            area += face_area
            faces.append({"face": index, "area": face_area})
    data = round_floats({
        "limit_deg": limit, "limit_source": source, "overhang_area": area,
        "faces": sorted(faces, key=lambda f: -f["area"])[:20],
    })
    if unmeasured:
        return CheckResult(
            "overhangs", SKIPPED, WARNING,
            f"Skipped: {unmeasured} curved face(s) could not be measured because "
            f"{no_mesh_reason}.",
            part.name, data,
        )
    if area >= OVERHANG_WARN_AREA_MM2:
        return CheckResult(
            "overhangs", WARN, WARNING,
            f"About {area:.0f} mm2 of surface on {len(faces)} face(s) hangs over at more than "
            f"{limit:.0f} degrees and will need support when printed {how}.",
            part.name, data,
        )
    return CheckResult(
        "overhangs", PASS, WARNING,
        f"No overhang steeper than {limit:.0f} degrees; prints without support {how}.",
        part.name, data,
    )


def judge_bed_contact(material, part: NamedPart, mesh, how: str = "as modelled",
                      no_mesh_reason: str = "") -> CheckResult:
    """Flat area resting on the bed when `part` is printed in the orientation it is given
    in (Z up)."""
    box = part.shape.bounding_box()
    bed_z = float(box.min.Z)
    contact = 0.0
    for face in part.shape.faces():
        normal = _planar_normal(face)
        if (normal is not None and normal[2] < -0.9999
                and abs(float(face.center().Z) - bed_z) <= BED_TOLERANCE_MM):
            contact += float(face.area)
    footprint = float(box.size.X * box.size.Y)
    data = round_floats({
        "contact_area": contact, "footprint_area": footprint,
        "contact_ratio": contact / footprint if footprint > 0 else 0.0,
    })
    if contact <= 1e-6:
        return CheckResult(
            "bed_contact", WARN, WARNING,
            f"No flat face rests on the bed when printed {how}; the part needs a flat "
            "bottom, a different orientation, or a raft.",
            part.name, data,
        )
    if contact < MIN_CONTACT_AREA_MM2:
        return CheckResult(
            "bed_contact", WARN, WARNING,
            f"Only {contact:.1f} mm2 touches the bed when printed {how}, which may not hold "
            "the part down.",
            part.name, data,
        )
    return CheckResult(
        "bed_contact", PASS, WARNING,
        f"{contact:.0f} mm2 of flat surface rests on the bed "
        f"({100 * data['contact_ratio']:.0f}% of the footprint) when printed {how}.",
        part.name, data,
    )
