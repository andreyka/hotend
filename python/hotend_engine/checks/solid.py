"""valid_solid, manifold, single_solid_per_part: is each part one closed, valid solid?"""

from __future__ import annotations

import numpy as np

from ..parts import NamedPart, edge_defects
from .base import ERROR, FAIL, PASS, WARN, CheckEnv, CheckResult, round_floats

# A flat face is described inconsistently when the area the kernel computes for it and the
# area inside its own edges differ by more than this share of the face and by more than
# this much (mm2).
FACE_AREA_SHARE = 0.005
FACE_AREA_MM2 = 0.5
# A curved edge is followed with this many points when the area inside the edges is taken.
EDGE_POINTS = 64
# Edges of one loop are taken to join when their ends are this close (mm): on such a face
# the kernel lets them miss each other by hundredths, inside the corners' tolerances.
JOIN_MM = 0.1


def outline_area(face) -> float | None:
    """The area inside a flat face's edges, taken from the edges as curves in space (not
    from the face's own description of them): the outer loop less the holes. None when a
    loop's edges do not join up within JOIN_MM."""
    from build123d import GeomType

    centre = face.center()
    normal = np.array(tuple(face.normal_at(centre)))
    seed = np.array([1.0, 0.0, 0.0]) if abs(normal[0]) < 0.9 else np.array([0.0, 1.0, 0.0])
    u = np.cross(normal, seed)
    u /= np.linalg.norm(u)
    v = np.cross(normal, u)
    loops = []
    for wire in face.wires():
        pieces = []
        for edge in wire.edges():
            count = 1 if edge.geom_type == GeomType.LINE else EDGE_POINTS
            pieces.append(np.array([tuple(edge.position_at(step / count))
                                    for step in range(count + 1)]))
        chain = pieces.pop(0)
        while pieces:
            end = chain[-1]
            gaps = [min(np.linalg.norm(piece[0] - end), np.linalg.norm(piece[-1] - end))
                    for piece in pieces]
            nearest = int(np.argmin(gaps))
            if gaps[nearest] > JOIN_MM:
                return None
            piece = pieces.pop(nearest)
            if np.linalg.norm(piece[0] - end) > np.linalg.norm(piece[-1] - end):
                piece = piece[::-1]
            chain = np.concatenate([chain, piece[1:]])
        if np.linalg.norm(chain[0] - chain[-1]) > JOIN_MM:
            return None
        x, y = chain @ u, chain @ v
        loops.append(abs(0.5 * float(np.sum(x[:-1] * y[1:] - x[1:] * y[:-1]))))
    if not loops:
        return None
    return 2.0 * max(loops) - sum(loops)


def inconsistent_faces(env: CheckEnv, part: NamedPart) -> list[dict]:
    """Flat faces whose area as the kernel integrates it is not the area of the surface it
    draws (and exports, and the slicer prints). It happens after a chamfer or fillet over
    many short edges: the face's outline on its plane is left stale, the solid still
    passes the kernel's validity test, and its volume and areas come out wrong.

    The triangles pick the candidates cheaply; a candidate counts only when the area
    inside the face's own edges, followed finely, disagrees with the kernel as well (the
    triangles of a face with round edges are a little off by themselves)."""
    from build123d import GeomType

    mesh = env.mesh(part)
    if mesh is None or not mesh.face_map_ok or len(mesh.triangles) == 0:
        return []
    faces = part.shape.faces()
    _, areas = mesh.triangle_normals_and_areas()
    drawn = np.bincount(mesh.triangle_face, weights=areas, minlength=len(faces))
    found = []
    for index, face in enumerate(faces):
        if face.geom_type != GeomType.PLANE:
            continue
        exact = float(face.area)
        allowed = max(FACE_AREA_MM2, FACE_AREA_SHARE * exact)
        if abs(exact - drawn[index]) <= allowed:
            continue
        inside = outline_area(face)
        if inside is not None and abs(exact - inside) > allowed:
            found.append({"face": index, "kernel_area": exact, "drawn_area": inside})
    return sorted(found, key=lambda entry: -abs(entry["kernel_area"] - entry["drawn_area"]))


def valid_solid(env: CheckEnv, part: NamedPart) -> CheckResult:
    """The geometry kernel's own validity test, plus a positive volume."""
    valid = bool(part.shape.is_valid)
    volume = float(part.shape.volume)
    data = {"is_valid": valid, "volume": volume}
    if not valid:
        return CheckResult(
            "valid_solid", FAIL, ERROR,
            "The geometry kernel reports a defect in this part (for example a "
            "self-intersecting or badly joined surface). It cannot be trusted to print.",
            part.name, data,
        )
    if volume <= 1e-9:
        return CheckResult(
            "valid_solid", FAIL, ERROR,
            "This part has no volume; it is empty or turned inside out.", part.name, data,
        )
    stale = inconsistent_faces(env, part)
    if stale:
        mesh = env.mesh(part)
        solid, _ = mesh.manifold()
        enclosed = None if solid is None else float(solid.volume())
        data.update(round_floats({"inconsistent_faces": stale[:20],
                                  "inconsistent_face_count": len(stale),
                                  "mesh_volume": enclosed}))
        worst = stale[0]
        printed = "" if enclosed is None else (
            f" The surface that is exported and printed encloses {enclosed / 1000:.2f} cm3.")
        return CheckResult(
            "valid_solid", WARN, ERROR,
            f"The kernel calls this solid valid, but it computes {len(stale)} flat face(s) "
            f"wrongly (the worst as {worst['kernel_area']:.1f} mm2 where the surface drawn is "
            f"{worst['drawn_area']:.1f} mm2), so the volume of {volume / 1000:.2f} cm3, the "
            "mass and the bed contact taken from it are off." + printed + " It happens "
            "after a chamfer or fillet over many short edges: build that bevel from flat "
            "pieces instead (`pocket.taper` for a reference's outline).",
            part.name, data,
        )
    return CheckResult(
        "valid_solid", PASS, ERROR, f"A valid solid of {volume / 1000:.2f} cm3.", part.name, data
    )


def manifold(env: CheckEnv, part: NamedPart) -> CheckResult:
    """Closed surface: every edge joins exactly two faces, and the triangle mesh is closed."""
    open_edges, shared_edges = edge_defects(part.shape)
    data = {"open_edges": open_edges, "non_manifold_edges": shared_edges, "mesh_closed": None}
    if open_edges or shared_edges:
        problems = []
        if open_edges:
            problems.append(f"{open_edges} edge(s) where the surface is open")
        if shared_edges:
            problems.append(f"{shared_edges} edge(s) shared by more than two faces")
        return CheckResult(
            "manifold", FAIL, ERROR,
            "The surface is not watertight: " + " and ".join(problems) + ".", part.name, data,
        )
    mesh = env.mesh(part)
    if mesh is None:
        return CheckResult(
            "manifold", PASS, ERROR,
            "The solid is watertight. Its triangle mesh was not examined: "
            f"{env.no_mesh_reason}.",
            part.name, data,
        )
    _, error = mesh.manifold()
    data["mesh_closed"] = error is None
    if error is not None:
        return CheckResult(
            "manifold", WARN, ERROR,
            f"The solid is watertight, but {error}; the slicer may have to repair it.",
            part.name, data,
        )
    return CheckResult(
        "manifold", PASS, ERROR, "Watertight: the solid and its triangle mesh are closed.",
        part.name, data,
    )


def single_solid_per_part(env: CheckEnv, part: NamedPart) -> CheckResult:
    count = len(part.solids)
    if count == 1:
        return CheckResult(
            "single_solid_per_part", PASS, ERROR, "One solid piece.", part.name, {"solids": 1}
        )
    return CheckResult(
        "single_solid_per_part", FAIL, ERROR,
        f"This part is {count} separate pieces. A part must be one piece: join them, or "
        "return them as separately named parts.",
        part.name, {"solids": count},
    )
