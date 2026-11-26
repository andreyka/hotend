"""Tessellating many shapes in one process must mesh equal shapes equally.

ocp-tessellate caches bounding boxes on the shape's address; freed addresses are reused, so
without clearing that cache an occasional shape was meshed at another shape's deflection.
"""

from __future__ import annotations

from build123d import Box, Sphere

from hotend_engine.parts import NamedPart
from hotend_engine.tessellate import tessellate


def test_a_new_shape_never_inherits_a_freed_shapes_deflection():
    counts = set()
    for i in range(400):
        # A large shape built and dropped just before is what leaves a stale box behind.
        tessellate([NamedPart("b", Box(200 + i, 300, 400), "#4c78a8")])
        # A new radius each time, so the content-keyed mesh cache never hits.
        sphere = Sphere(10 + i * 1e-4)
        counts.add(tessellate([NamedPart("p", sphere, "#4c78a8")]).triangle_count)
    # The radius grows by 0.04 mm over the loop, so the count may step up slightly. A sphere
    # meshed with the big box's deflection has well under half the triangles.
    assert min(counts) > 0.9 * max(counts), f"a sphere was meshed too coarsely: {sorted(counts)}"
