"""small_features: geometry finer than the nozzle can lay down.

Two exact B-rep measures against the nozzle diameter from the request:
- a face narrower than one nozzle width, judged by its hydraulic diameter 4 * area / perimeter
  (the width of a long strip is half of that, so this errs on the side of not warning);
- a full cylinder (a round hole or pin) thinner than two nozzle widths.
Holes modelled as two half cylinders are not recognised by the second measure.
"""

from __future__ import annotations

import math

from ..parts import NamedPart
from .base import PASS, SKIPPED, WARN, WARNING, CheckEnv, CheckResult, round_floats


def _full_cylinder(face) -> tuple[float, bool] | None:
    """(diameter, is_hole) when the face is a complete cylinder, else None."""
    from build123d import GeomType

    if face.geom_type != GeomType.CYLINDER:
        return None
    radius, axis = face.radius, face.axis_of_rotation
    if not radius or axis is None:
        return None
    along = [(v.center() - axis.position).dot(axis.direction) for v in face.vertices()]
    length = max(along) - min(along) if along else 0.0
    if length <= 1e-9 or face.area < 0.95 * 2 * math.pi * radius * length:
        return None
    point = face.position_at(0.5, 0.5)
    offset = point - axis.position
    radial = offset - axis.direction * offset.dot(axis.direction)
    return 2 * radius, face.normal_at(point).dot(radial) < 0


def small_features(env: CheckEnv, part: NamedPart) -> CheckResult:
    nozzle = env.ctx.printer.main_nozzle if env.ctx.printer else None
    if nozzle is None:
        return CheckResult(
            "small_features", SKIPPED, WARNING,
            "Skipped: the request names no nozzle diameter to compare against.", part.name,
        )
    width = nozzle.diameter
    narrow, round_features = [], []
    for index, face in enumerate(part.shape.faces()):
        cylinder = _full_cylinder(face)
        if cylinder is not None and cylinder[0] < 2 * width - 1e-9:
            round_features.append(
                {"face": index, "diameter": cylinder[0], "kind": "hole" if cylinder[1] else "pin"}
            )
            continue
        perimeter = sum(edge.length for edge in face.edges())
        if perimeter > 0 and 4 * face.area / perimeter < width - 1e-9:
            narrow.append({"face": index, "width": 4 * face.area / perimeter})
    data = round_floats({
        "nozzle_diameter": width, "narrow_faces": narrow[:20], "round_features": round_features[:20],
        "narrow_face_count": len(narrow), "round_feature_count": len(round_features),
    })
    if not narrow and not round_features:
        return CheckResult(
            "small_features", PASS, WARNING,
            f"No feature is finer than the {width:g} mm nozzle can print.", part.name, data,
        )
    notes = []
    if round_features:
        smallest = min(round_features, key=lambda f: f["diameter"])
        notes.append(
            f"{len(round_features)} round hole(s) or pin(s) thinner than {2 * width:g} mm "
            f"(smallest: a {smallest['diameter']:.2f} mm {smallest['kind']})"
        )
    if narrow:
        notes.append(
            f"{len(narrow)} face(s) narrower than the {width:g} mm nozzle "
            f"(smallest about {min(f['width'] for f in narrow):.2f} mm)"
        )
    return CheckResult(
        "small_features", WARN, WARNING,
        "Too fine to print as modelled: " + "; ".join(notes) + ".", part.name, data,
    )
