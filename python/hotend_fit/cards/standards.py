"""Standard sizes of bought fasteners used by the cards.

Machine screws and heat-set inserts come from bd_warehouse's tables (the pinned version), so
they match the parts bd_warehouse itself can draw. bd_warehouse has no thread-forming screws
for plastics, so those come from the small table below.

Every number here has provenance `standard`: it describes a part that is bought, not printed.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from .base import CardError

# Thread-forming (tapping) screws. Thread size and major diameter d1 per ISO 1478, pan head
# diameter dk and height k per ISO 7049 (maximum values, mm).
_TAPPING_SCREWS = {
    "ST2.2": (2.24, 4.0, 1.6),
    "ST2.9": (2.90, 5.6, 2.4),
    "ST3.5": (3.53, 7.0, 2.6),
    "ST4.2": (4.22, 8.0, 3.1),
    "ST4.8": (4.80, 9.5, 3.7),
}
_TAPPING_SOURCE = "ISO 1478 (thread), ISO 7049 (pan head)"

# bd_warehouse fastener type used for a machine screw's head: ISO 1580 slotted pan head.
_MACHINE_HEAD_TYPE = "iso1580"
# Makers whose bd_warehouse rows give the wall needed around the insert as `material_thickness`.
INSERT_MAKERS = ("CNCKitchen", "ruthex")


@dataclass(frozen=True)
class ScrewSpec:
    designation: str
    diameter: float  # major (outside) diameter of the thread, mm
    head_diameter: float
    head_height: float
    thread_forming: bool
    source: str


@dataclass(frozen=True)
class InsertSpec:
    size: str  # bd_warehouse size key, such as "M3-0.5-5.7"
    maker: str
    outer_diameter: float  # over the knurls, mm
    length: float
    hole_diameter: float  # the hole the maker asks for, mm
    min_wall: float  # plastic needed around the hole, mm
    source: str


def _machine_rows() -> dict[str, dict[str, str]]:
    from bd_warehouse.fastener import PanHeadScrew, isolate_fastener_type

    return isolate_fastener_type(_MACHINE_HEAD_TYPE, PanHeadScrew.fastener_data)


def screw_designations() -> list[str]:
    """Every screw designation `screw()` accepts."""
    metric = sorted({key.split("-")[0] for key in _machine_rows() if key.startswith("M")},
                    key=lambda name: float(name[1:]))
    return metric + list(_TAPPING_SCREWS)


def screw(designation: str) -> ScrewSpec:
    """A screw by designation: a metric machine screw (`M3`) or a tapping screw (`ST2.9`)."""
    if designation in _TAPPING_SCREWS:
        diameter, head, height = _TAPPING_SCREWS[designation]
        return ScrewSpec(designation, diameter, head, height, True, _TAPPING_SOURCE)
    if isinstance(designation, str) and re.fullmatch(r"M\d+(\.\d+)?", designation):
        for key, row in _machine_rows().items():
            if key.split("-")[0] == designation:
                return ScrewSpec(
                    designation, float(designation[1:]), float(row["dk"]), float(row["k"]),
                    False, f"bd_warehouse pan head {_MACHINE_HEAD_TYPE} {key}",
                )
    raise CardError(
        f"unknown screw {designation!r}; known sizes: {', '.join(screw_designations())}"
    )


def heat_set_inserts(thread: str, maker: str = "CNCKitchen") -> list[InsertSpec]:
    """The heat-set inserts bd_warehouse lists for a metric thread (`M3`), shortest first."""
    from bd_warehouse.fastener import (
        HeatSetNut,
        isolate_fastener_type,
        read_drill_sizes,
    )

    if maker not in INSERT_MAKERS:
        raise CardError(f"unknown insert maker {maker!r}; choose from {', '.join(INSERT_MAKERS)}")
    drills = read_drill_sizes()
    found = []
    for key, row in isolate_fastener_type(maker, HeatSetNut.fastener_data).items():
        if key.split("-")[0] != thread:
            continue
        drill = row["drill"].strip()
        hole = drills.get(drill)
        if hole is None:
            continue  # the hole is not given as a drill size bd_warehouse can convert
        found.append(InsertSpec(
            key, maker, float(row["s"]), float(row["m"]), float(hole),
            float(row["material_thickness"]), f"bd_warehouse HeatSetNut {maker} {key}",
        ))
    return sorted(found, key=lambda spec: (spec.length, spec.outer_diameter))


def heat_set_insert(thread: str, max_length: float, maker: str = "CNCKitchen",
                    size: str | None = None) -> InsertSpec:
    """The longest insert for `thread` no longer than `max_length`, or the one named `size`."""
    options = heat_set_inserts(thread, maker)
    if not options:
        raise CardError(f"bd_warehouse lists no {maker} heat-set insert for {thread}")
    if size is not None:
        for spec in options:
            if spec.size == size:
                return spec
        raise CardError(
            f"unknown {maker} insert {size!r} for {thread}; known: "
            f"{', '.join(spec.size for spec in options)}"
        )
    fitting = [spec for spec in options if spec.length <= max_length]
    if not fitting:
        raise CardError(
            f"the shortest {maker} {thread} insert is {options[0].length:g} mm long, which "
            f"does not fit {max_length:.2f} mm of boss; make the boss taller"
        )
    return fitting[-1]
