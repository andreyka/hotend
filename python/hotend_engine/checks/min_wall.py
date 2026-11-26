"""min_wall: sampled wall thickness.

Thickness is sampled, not proven: from points spread over the surface a ray is cast straight
into the material (manifold3d's ray cast on the tessellation) and the distance to where it
leaves the solid is the local thickness. Only exits through a surface that roughly faces the
opposite way count as a wall, so the tapering tip of a wedge or a chamfer is not reported as a
thin wall. A part whose mesh is not closed cannot be sampled and is reported as skipped.

The ray starts a hair outside the surface (a millionth of the part's size), not inside it, so
no skin is too thin to be seen: the first surface it meets is the one the sample lies on, and
the next one is where it leaves the material, however close that is. (A ray started a
micrometre inside would begin behind a thinner skin and measure the next wall instead; such
skins come from a cut that was meant to go through and stops short by rounding.)
"""

from __future__ import annotations

import numpy as np

from ..parts import NamedPart
from .base import ERROR, FAIL, PASS, SKIPPED, WARN, CheckEnv, CheckResult, round_floats

TARGET_SAMPLES = 6000
MAX_SAMPLES = 120_000
MAX_PER_TRIANGLE = 48
# The two sides of a wall face away from each other within this angle.
OPPOSING_COS = -0.866
# A thin region smaller than this is reported as a warning, not a failure.
FAIL_AREA_MM2 = 1.0
# The ray starts this fraction of the part's size outside the surface. The mesh is held in
# single precision (about 1e-7 of the size), so this is the nearest start that is reliably
# outside.
START_OUTSIDE = 1e-6


def _sample_points(tri: np.ndarray, areas: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Deterministic points on the triangles: (points, triangle index, area each stands for).

    Every triangle gets its centroid; large triangles get extra points in proportion to
    their area so a big flat face is not judged by one ray.
    """
    total = float(areas.sum())
    per_sample = total / TARGET_SAMPLES if total > 0 else 1.0
    counts = np.clip(np.ceil(areas / per_sample).astype(np.int64), 1, MAX_PER_TRIANGLE)
    index = np.repeat(np.arange(len(tri)), counts)
    rng = np.random.default_rng(20261117)
    u, v = rng.random(len(index)), rng.random(len(index))
    flip = u + v > 1
    u, v = np.where(flip, 1 - u, u), np.where(flip, 1 - v, v)
    first = np.concatenate(([0], np.cumsum(counts)[:-1]))
    u[first], v[first] = 1 / 3, 1 / 3
    t = tri[index]
    points = t[:, 0] + (t[:, 1] - t[:, 0]) * u[:, None] + (t[:, 2] - t[:, 0]) * v[:, None]
    weights = (areas / counts)[index]
    if len(points) > MAX_SAMPLES:
        step = -(-len(points) // MAX_SAMPLES)
        points, index, weights = points[::step], index[::step], weights[::step] * step
    return points, index, weights


def min_wall(env: CheckEnv, part: NamedPart) -> CheckResult:
    limit = env.options.min_wall
    mesh = env.mesh(part)
    if mesh is None:
        return CheckResult(
            "min_wall", SKIPPED, ERROR,
            f"Skipped: wall thickness could not be sampled because {env.no_mesh_reason}.",
            part.name,
        )
    solid, error = mesh.manifold()
    if solid is None:
        return CheckResult(
            "min_wall", SKIPPED, ERROR,
            f"Skipped: wall thickness could not be sampled because {error}.", part.name,
        )
    normals, areas = mesh.triangle_normals_and_areas()
    usable = areas > 1e-12
    tri = mesh.triangle_points[usable]
    normals, areas = normals[usable], areas[usable]
    points, index, weights = _sample_points(tri, areas)
    span = float(np.linalg.norm(np.ptp(mesh.vertices, axis=0)))
    reach = span * 1.05 + 1.0
    outside = span * START_OUTSIDE
    thickness = np.full(len(points), np.nan)
    for i in range(len(points)):
        inward = -normals[index[i]]
        origin = points[i] - inward * outside
        for hit in solid.ray_cast(tuple(origin), tuple(origin + inward * reach)):
            position = np.asarray(hit.position)
            if float(np.dot(normals[index[i]], hit.normal)) <= OPPOSING_COS:
                # Where the ray leaves the material: the other side of the wall.
                thickness[i] = max(0.0, float(np.dot(position - points[i], inward)))
                break
            if float(np.linalg.norm(position - origin)) <= 2.0 * outside:
                continue  # the surface the sample lies on
            break
    measured = ~np.isnan(thickness)
    if not measured.any():
        return CheckResult(
            "min_wall", SKIPPED, ERROR,
            "Skipped: no opposing surfaces were found to measure a wall between.", part.name,
        )
    thin = measured & (thickness < limit - 1e-3)
    thinnest = int(np.nanargmin(thickness))
    data = round_floats({
        "min_wall": limit,
        "thinnest": float(thickness[thinnest]),
        "thinnest_at": points[thinnest].tolist(),
        "thin_area": float(weights[thin].sum()),
        "samples": len(points),
        "measured": int(measured.sum()),
    })
    where = "x={:.1f}, y={:.1f}, z={:.1f}".format(*points[thinnest])
    if not thin.any():
        return CheckResult(
            "min_wall", PASS, ERROR,
            f"Thinnest wall found is {thickness[thinnest]:.2f} mm; the minimum is "
            f"{limit:.2f} mm ({int(measured.sum())} points sampled).",
            part.name, data,
        )
    status = FAIL if data["thin_area"] >= FAIL_AREA_MM2 else WARN
    return CheckResult(
        "min_wall", status, ERROR,
        f"A wall is only {thickness[thinnest]:.2f} mm thick near {where}; the minimum is "
        f"{limit:.2f} mm. About {data['thin_area']:.1f} mm2 of surface is too thin"
        + ("." if status == FAIL else ", a very small region."),
        part.name, data,
    )
