"""Reference models on this machine that tests may measure.

They are other people's designs, so neither the files nor anything about them is in the
repository. `HOTEND_REFS_DIR` names the folder that holds them, together with the list
`reference-models.json`. Without the folder, the list or an entry a test skips with a reason
that names HOTEND_REFS_DIR (scripts/test-all.sh counts those). The list looks like this
(sizes in millimetres; everything but `file`, `measured_mm` and `engine` is optional):

    {"soft_figure": {
       "file": "some.3mf", "plate": 1, "object": "name in the file",
       "measured_mm": [x, y, z],
       "face_mm": {"features": {"name": [low, high]}, "edge_above_highest_mm": h},
       "expect": {"basket_outside_mm": d, "basket_inside_mm": d, "closed_height_mm": h,
                  "volumes_mm3": {"basket": v, "lid": v, "knob": v}},
       "engine": {
         "size_mm": [x, y, z], "objects_in_file": n,
         "parts": {"geometry": n, "ignored_roles": ["modifier_part"],
                   "ignored_wider_than_mm": w},
         "elsewhere": {"plate": 2, "count": n, "size_mm": [x, y, z]},
         "mesh": {"more_triangles_than": n, "volume_mm3": v, "most_ghost_triangles": n,
                  "more_section_points_than": n},
         "base_z_mm": 0.0}}}
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

#: The entries the tests ask for.
KEYS = ("soft_figure", "figure_b")
LIST = "reference-models.json"


def _read() -> tuple[Path | None, dict[str, dict], str]:
    """The folder, its list and, when there is none, why."""
    named = os.environ.get("HOTEND_REFS_DIR")
    if not named:
        return None, {}, "HOTEND_REFS_DIR is not set"
    folder = Path(named)
    if not (folder / LIST).is_file():
        return None, {}, f"HOTEND_REFS_DIR holds no {LIST}"
    return folder, json.loads((folder / LIST).read_text(encoding="utf-8")), ""


FOLDER, ENTRIES, WHY_NOT = _read()


def entry_of(key: str) -> dict:
    """The entry of the list, or a skip when it or its file is not there."""
    if WHY_NOT:
        pytest.skip(WHY_NOT)
    if key not in ENTRIES:
        pytest.skip(f"{LIST} in HOTEND_REFS_DIR has no entry {key}")
    if not (FOLDER / ENTRIES[key]["file"]).is_file():
        pytest.skip(f"the file of {key} is not in HOTEND_REFS_DIR")
    return ENTRIES[key]
