"""The `measure` op: exact measurements on the built solids (docs/PROTOCOL.md section 3.2).

Queries, each answered independently (a bad query gets `ok: false` and an `error`, the others
are still answered):

- `{"kind": "bbox", "part"?}`            -> `min`, `max`, `size`
- `{"kind": "volume", "part"?}`          -> `volume` (mm3), `area` (mm2), `center_of_mass`
- `{"kind": "distance", "a": S, "b": S}` -> `distance` and the closest `point_a`, `point_b`
- `{"kind": "section", "plane": "XY"|"XZ"|"YZ", "offset": mm, "part"?}`
                                         -> per part cut `area`, `bbox`, number of `regions`

Without `part` a query covers the whole model. A selector S is one of `{"part": name}`,
`{"part": name, "face": i}`, `{"part": name, "edge": i}` or `{"point": [x, y, z]}`; face and
edge indices are those of mesh.json. Edges are numbered as the tessellator numbers them
(`tessellate.mesh_edges`), which counts the degenerate edges build123d's `shape.edges()`
leaves out; a degenerate edge (the pole of a sphere, the tip of a cone) is a point, not a
curve, and selecting one is answered with an error.

A distance is measured between boundaries, so on its own it would be positive for something
buried in a part's material. When a selector is a whole part, the other side is therefore
classified first: a point, face, edge or part that lies inside that part's material gets
`distance` 0 and `"inside": true`, and two parts that share volume get `distance` 0 and their
`overlap_volume` in mm3.
"""

from __future__ import annotations

from typing import Any

from .parts import NamedPart, bbox_json
from .tessellate import mesh_edges

PLANES = ("XY", "XZ", "YZ")
# Shared volume below this is contact, not overlap (as in the parts_do_not_intersect check).
OVERLAP_TOLERANCE_MM3 = 1e-3


class QueryError(ValueError):
    """The query is malformed or names something the model does not have."""


def _vec(v: Any) -> list[float]:
    return [round(float(c), 6) for c in tuple(v)]


def _part(parts: dict[str, NamedPart], name: Any) -> NamedPart:
    if name not in parts:
        raise QueryError(f"unknown part {name!r}; the model has: {', '.join(parts)}")
    return parts[name]


def _whole(parts: dict[str, NamedPart], query: dict) -> Any:
    from build123d import Compound

    if query.get("part") is not None:
        return _part(parts, query["part"]).shape
    shapes = [part.shape for part in parts.values()]
    return shapes[0] if len(shapes) == 1 else Compound(shapes)


def _select(parts: dict[str, NamedPart], selector: Any) -> Any:
    if not isinstance(selector, dict):
        raise QueryError("a selector must be an object such as {\"part\": \"lid\"}")
    if "point" in selector:
        point = selector["point"]
        if (not isinstance(point, (list, tuple)) or len(point) != 3
                or not all(isinstance(c, (int, float)) and not isinstance(c, bool) for c in point)):
            raise QueryError("'point' must be [x, y, z]")
        return tuple(float(c) for c in point)
    shape = _part(parts, selector.get("part")).shape
    for key, items in (("face", shape.faces), ("edge", lambda: mesh_edges(shape))):
        if key in selector:
            index, found = selector[key], items()
            if isinstance(index, bool) or not isinstance(index, int) or not 0 <= index < len(found):
                raise QueryError(f"'{key}' must be an index from 0 to {len(found) - 1}")
            if key == "edge":
                return _edge(found[index], index)
            return found[index]
    return shape


def _edge(edge: Any, index: int) -> Any:
    from build123d import Edge
    from OCP.BRep import BRep_Tool

    if BRep_Tool.Degenerated_s(edge):
        raise QueryError(
            f"edge {index} is a degenerate edge: a single point such as the pole of a sphere "
            "or the tip of a cone, not a curve. Measure to a face or to a point instead"
        )
    return Edge(edge)


def _is_whole_part(selector: Any) -> bool:
    return isinstance(selector, dict) and "point" not in selector and not (
        "face" in selector or "edge" in selector
    )


def _inside(solid_shape: Any, point: Any) -> bool:
    """True when `point` lies strictly inside the material of a part (not on its surface)."""
    from OCP.BRepClass3d import BRepClass3d_SolidClassifier
    from OCP.gp import gp_Pnt
    from OCP.TopAbs import TopAbs_IN

    x, y, z = (float(c) for c in tuple(point))
    return any(
        BRepClass3d_SolidClassifier(solid.wrapped, gp_Pnt(x, y, z), 1e-6).State() == TopAbs_IN
        for solid in solid_shape.solids()
    )


def _shared_volume(a: Any, b: Any) -> float:
    common = a.intersect(b)
    return sum(float(solid.volume) for item in common for solid in item.solids()) if common else 0.0


def _bbox(parts: dict[str, NamedPart], query: dict) -> dict:
    box = bbox_json(_whole(parts, query))
    return {**box, "size": [round(hi - lo, 6) for lo, hi in zip(box["min"], box["max"], strict=True)]}


def _volume(parts: dict[str, NamedPart], query: dict) -> dict:
    from build123d import CenterOf

    shape = _whole(parts, query)
    return {
        "volume": round(float(shape.volume), 6),
        "area": round(float(shape.area), 6),
        "center_of_mass": _vec(shape.center(CenterOf.MASS)),
    }


def _distance(parts: dict[str, NamedPart], query: dict) -> dict:
    from build123d import Shape, Vertex

    a, b = _select(parts, query.get("a")), _select(parts, query.get("b"))
    whole_a, whole_b = _is_whole_part(query.get("a")), _is_whole_part(query.get("b"))
    if not isinstance(a, Shape):
        a = Vertex(*a)
    if not isinstance(b, Shape):
        b = Vertex(*b)
    distance, point_a, point_b = a.distance_to_with_closest_points(b)
    result = {"distance": round(float(distance), 6), "point_a": _vec(point_a),
              "point_b": _vec(point_b)}
    # The kernel measures between boundaries. Something buried in a part's material is not
    # "10 mm away" from it: classify against every side that is a whole part.
    if whole_a and whole_b:
        shared = _shared_volume(a, b)
        if shared > OVERLAP_TOLERANCE_MM3:
            return {**result, "distance": 0.0, "inside": True,
                    "overlap_volume": round(shared, 6)}
        result["inside"] = False
    elif whole_a or whole_b:
        # Boundaries that do not touch: the other side is wholly inside or wholly outside,
        # so its closest point decides. Touching boundaries already measure 0.
        solid, probe = (a, point_b) if whole_a else (b, point_a)
        if distance > 0 and _inside(solid, probe):
            return {**result, "distance": 0.0, "inside": True}
        result["inside"] = False
    return result


def _section(parts: dict[str, NamedPart], query: dict) -> dict:
    from build123d import Plane

    name, offset = query.get("plane"), query.get("offset", 0)
    if name not in PLANES:
        raise QueryError(f"'plane' must be one of {', '.join(PLANES)}")
    if isinstance(offset, bool) or not isinstance(offset, (int, float)):
        raise QueryError("'offset' must be a number of millimetres")
    plane = getattr(Plane, name).offset(float(offset))
    chosen = [_part(parts, query["part"])] if query.get("part") is not None else parts.values()
    rows, total = [], 0.0
    for part in chosen:
        cut = part.shape.intersect(plane)
        faces = [f for item in (cut or []) for f in item.faces()]
        area = sum(float(f.area) for f in faces)
        total += area
        row = {"part": part.name, "area": round(area, 6), "regions": len(faces), "bbox": None}
        if faces:
            from build123d import Compound

            row["bbox"] = bbox_json(Compound(faces))
        rows.append(row)
    return {"plane": name, "offset": float(offset), "area": round(total, 6), "parts": rows}


_KINDS = {"bbox": _bbox, "volume": _volume, "distance": _distance, "section": _section}


def run_queries(parts: list[NamedPart], queries: tuple[dict, ...]) -> list[dict]:
    """Answer each query; results are in the order of the queries."""
    by_name = {part.name: part for part in parts}
    results = []
    for query in queries:
        kind = query.get("kind")
        try:
            if kind not in _KINDS:
                raise QueryError(f"unknown kind {kind!r}; choose from {', '.join(_KINDS)}")
            results.append({"kind": kind, "ok": True, **_KINDS[kind](by_name, query)})
        except QueryError as exc:
            results.append({"kind": kind, "ok": False, "error": str(exc)})
        except Exception as exc:  # noqa: BLE001 - OCCT raises unrelated types
            results.append(
                {"kind": kind, "ok": False, "error": f"{type(exc).__name__}: {str(exc)[:300]}"}
            )
    return results
