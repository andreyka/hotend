"""Undeclared joints: faces of two parts that face each other closely outside any declared fit.

A joint the script makes by hand (a lip in a recess, a pin in a hole) has a gap that somebody
typed, and no feature card that `fit_pairs` could verify. This finds such places so they are
at least visible: where a face of one part looks straight at a face of another part from at
most `REACH_MM` away, over at least `MIN_AREA_MM2`, and the place is not inside the solids a
declared feature card added or cut (`Card.feature_boxes`, widened by the reach).

How it is measured. On the tessellation: points are spread over the faces of the first part
that lie near the second, a ray is cast from each along the face's outward normal, and a
point counts when the ray meets a face of the second part that looks back at it (normals
opposed within about 45 degrees) within the reach. The gap of a pair of faces is the
distance along those rays, exact for planar faces and within the tessellation's deviation
(about 0.1 mm) for curved ones; areas are sums over the sample points. This is a detector,
not a measurement: it reports that a joint is there and roughly how wide its gap is.

What is not reported: faces that touch in one direction only (a lid resting on a rim is not a
fit; planar faces in contact are reported only when the same two parts touch in two opposite
directions, a tongue drawn exactly as wide as its slot); parts drawn apart from each other;
anything inside a declared card's own solids.
"""

from __future__ import annotations

from itertools import combinations

import numpy as np

from ..parts import NamedPart
from ..tessellate import PartMesh

REACH_MM = 1.0
MIN_AREA_MM2 = 10.0
# Planar faces closer than this touch; curved faces are compared on facets, so they touch
# within the tessellation's deviation.
CONTACT_MM = 0.005
CURVED_CONTACT_MM = 0.12
# Normals of two faces that face each other: opposed within about 45 degrees.
FACING_COS = -0.7
SPACING_MM = 0.5
MAX_POINTS_PER_PAIR = 4000
MAX_FACE_PAIRS = 4000
MAX_REPORTED_FACES = 12


def _face_boxes(mesh: PartMesh) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """(face index of each group, low corners, high corners) of the triangles of each face."""
    points = mesh.triangle_points
    order = np.argsort(mesh.triangle_face, kind="stable")
    faces, starts = np.unique(mesh.triangle_face[order], return_index=True)
    low = np.minimum.reduceat(points[order].min(axis=1), starts)
    high = np.maximum.reduceat(points[order].max(axis=1), starts)
    return faces, low, high


def _samples(triangles: np.ndarray, low: np.ndarray, high: np.ndarray,
             spacing: float) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Points spread over the triangles (each split into n x n smaller ones), kept where
    they lie in the box [low, high]: (points, triangle index, area each stands for)."""
    edges = np.linalg.norm(triangles - np.roll(triangles, 1, axis=1), axis=2).max(axis=1)
    points, owners, weights = [], [], []
    areas = 0.5 * np.linalg.norm(
        np.cross(triangles[:, 1] - triangles[:, 0], triangles[:, 2] - triangles[:, 0]), axis=1)
    for index, (tri, edge, area) in enumerate(zip(triangles, edges, areas, strict=True)):
        n = int(min(200, max(1, np.ceil(edge / spacing))))
        i, j = np.meshgrid(np.arange(n), np.arange(n), indexing="ij")
        keep = i + j < n
        i, j = i[keep], j[keep]
        # Centroids of the upright and of the inverted small triangles.
        up = np.stack([(i + 1 / 3) / n, (j + 1 / 3) / n], axis=1)
        down_keep = i + j < n - 1
        down = np.stack([(i[down_keep] + 2 / 3) / n, (j[down_keep] + 2 / 3) / n], axis=1)
        uv = np.concatenate([up, down])
        found = tri[0] + uv[:, :1] * (tri[1] - tri[0]) + uv[:, 1:] * (tri[2] - tri[0])
        inside = np.all((found >= low) & (found <= high), axis=1)
        points.append(found[inside])
        owners.append(np.full(int(inside.sum()), index))
        weights.append(np.full(int(inside.sum()), area / (n * n)))
    if not points:
        return np.zeros((0, 3)), np.zeros(0, dtype=int), np.zeros(0)
    return np.concatenate(points), np.concatenate(owners), np.concatenate(weights)


def _ray_hits(origins: np.ndarray, directions: np.ndarray, triangles: np.ndarray,
              normals: np.ndarray, start: float) -> np.ndarray:
    """For each ray the nearest distance in [start, REACH_MM] at which it meets one of the
    triangles that faces back at it, NaN when it meets none (Moeller-Trumbore)."""
    edge1 = triangles[:, 1] - triangles[:, 0]
    edge2 = triangles[:, 2] - triangles[:, 0]
    best = np.full(len(origins), np.nan)
    for tri0, e1, e2, normal in zip(triangles[:, 0], edge1, edge2, normals, strict=True):
        facing = directions @ normal <= FACING_COS
        if not facing.any():
            continue
        d, o = directions[facing], origins[facing]
        p = np.cross(d, e2)
        det = p @ e1
        ok = np.abs(det) > 1e-12
        inv = np.where(ok, 1.0 / np.where(ok, det, 1.0), 0.0)
        s = o - tri0
        u = np.einsum("ij,ij->i", s, p) * inv
        q = np.cross(s, e1)
        v = np.einsum("ij,ij->i", d, q) * inv
        t = (q @ e2) * inv
        hit = ok & (u >= -1e-9) & (v >= -1e-9) & (u + v <= 1 + 1e-9) & (t >= start) & (
            t <= REACH_MM)
        index = np.flatnonzero(facing)[hit]
        best[index] = np.fmin(best[index], t[hit])
    return best


def _in_boxes(points: np.ndarray, boxes: list) -> np.ndarray:
    inside = np.zeros(len(points), dtype=bool)
    for low, high in boxes:
        inside |= np.all((points >= np.asarray(low) - REACH_MM)
                         & (points <= np.asarray(high) + REACH_MM), axis=1)
    return inside


def _pair(a: NamedPart, mesh_a: PartMesh, b: NamedPart, mesh_b: PartMesh, planar_a: set,
          planar_b: set, boxes: list) -> dict | None:
    """What part `a` and part `b` show of an undeclared joint, or None."""
    faces_a, low_a, high_a = _face_boxes(mesh_a)
    faces_b, low_b, high_b = _face_boxes(mesh_b)
    normals_a, _ = mesh_a.triangle_normals_and_areas()
    normals_b, _ = mesh_b.triangle_normals_and_areas()
    tri_a, tri_b = mesh_a.triangle_points, mesh_b.triangle_points
    found = []
    pairs = 0
    for ia, (face_a, la, ha) in enumerate(zip(faces_a, low_a, high_a, strict=True)):
        near = np.all((low_b <= ha + REACH_MM) & (high_b >= la - REACH_MM), axis=1)
        if not near.any():
            continue
        mask_a = mesh_a.triangle_face == face_a
        for ib in np.flatnonzero(near):
            pairs += 1
            if pairs > MAX_FACE_PAIRS:
                break
            face_b = faces_b[ib]
            mask_b = mesh_b.triangle_face == face_b
            if (normals_a[mask_a] @ normals_b[mask_b].T).min() > FACING_COS:
                continue
            low = np.maximum(la, low_b[ib]) - REACH_MM
            high = np.minimum(ha, high_b[ib]) + REACH_MM
            extent = np.sort(np.maximum(high - low, 0.0))
            spacing = max(SPACING_MM, float(np.sqrt(extent[1] * extent[2] / MAX_POINTS_PER_PAIR)))
            points, owner, weights = _samples(tri_a[mask_a], low, high, spacing)
            if not len(points):
                continue
            planar = int(face_a) in planar_a and int(face_b) in planar_b
            touch = CONTACT_MM if planar else CURVED_CONTACT_MM
            gaps = _ray_hits(points, normals_a[mask_a][owner], tri_b[mask_b],
                             normals_b[mask_b], -touch)
            seen = ~np.isnan(gaps) & ~_in_boxes(points, boxes)
            if not seen.any():
                continue
            gaps, weights = gaps[seen], weights[seen]
            direction = normals_a[mask_a][owner][seen]
            for contact in (False, True):
                part = (gaps <= touch) == contact
                if part.any():
                    found.append({
                        "a": int(face_a), "b": int(face_b), "contact": contact,
                        "planar": planar, "area": float(weights[part].sum()),
                        "gap": 0.0 if contact else float(np.median(gaps[part])),
                        "gap_min": 0.0 if contact else float(gaps[part].min()),
                        "gap_max": 0.0 if contact else float(gaps[part].max()),
                        "normal": direction[part].mean(axis=0),
                    })
    # Planar faces in contact are a fit only when the parts hold each other from two
    # opposite sides; curved faces in contact always are (a pin drawn as large as its hole).
    resting = [f for f in found if f["contact"] and f["planar"]]
    held = [
        f for f in resting
        if any(float(f["normal"] @ other["normal"]) < FACING_COS for other in resting)
    ]
    joint = [f for f in found if not f["contact"] or not f["planar"]] + held
    area = sum(f["area"] for f in joint)
    if area < MIN_AREA_MM2:
        return None
    joint.sort(key=lambda f: -f["area"])
    widest = max(f["gap_max"] for f in joint)
    narrowest = min(f["gap_min"] for f in joint)
    return {
        "kind": "undeclared", "parts": [a.name, b.name], "area": area,
        "gap_min": narrowest, "gap_max": widest,
        "gap": float(np.average([f["gap"] for f in joint], weights=[f["area"] for f in joint])),
        "faces": [{key: f[key] for key in ("a", "b", "area", "gap", "contact")}
                  for f in joint[:MAX_REPORTED_FACES]],
    }


def find_undeclared_joints(parts: list[NamedPart], meshes: dict[str, PartMesh],
                           cards: list) -> list[dict]:
    """One record per pair of parts that shows an undeclared joint (module doc). `meshes`
    are the parts' tessellations with their face maps; `cards` the declared feature cards."""
    from build123d import GeomType

    boxes = [box for card in cards for box in card.feature_boxes()]
    planar = {
        part.name: {index for index, face in enumerate(part.shape.faces())
                    if face.geom_type == GeomType.PLANE}
        for part in parts
    }
    records = []
    for a, b in combinations(parts, 2):
        mesh_a, mesh_b = meshes[a.name], meshes[b.name]
        low_a, high_a = mesh_a.vertices.min(axis=0), mesh_a.vertices.max(axis=0)
        low_b, high_b = mesh_b.vertices.min(axis=0), mesh_b.vertices.max(axis=0)
        if np.any(low_a > high_b + REACH_MM) or np.any(low_b > high_a + REACH_MM):
            continue
        record = _pair(a, mesh_a, b, mesh_b, planar[a.name], planar[b.name], boxes)
        if record is not None:
            records.append(record)
    return records
