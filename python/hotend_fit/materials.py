"""Material facts the fits and checks need: is it flexible, how it prints, where it prints.

A request names a material by `id` and `family` (`{"id": "tpu-95a", "family": "TPU"}`).
`material_facts(material)` answers with what Hotend knows about that family. The numbers
are general practice collected from the sources named on each entry, not measurements of
the user's filament, so every entry is tagged `guessed`; a fit that depends on them is
proven only by its gauge.

Flexible materials (TPU). A flexible part can be stretched over the thing it holds, so it is
the only kind of material the `stretch` fit class accepts (a rigid shell asked to stretch is
a failure of the `reference_fit` check: it cracks or never goes on). On a printer with a
Bowden second nozzle (the Bambu Lab X2D) flexible filament prints only on the main,
direct-drive nozzle; print preparation fails a flexible part given to another nozzle.
"""

from __future__ import annotations

import re
from dataclasses import asdict, dataclass
from typing import Any

_SHORE = re.compile(r"(\d{2})\s*a\b", re.IGNORECASE)


@dataclass(frozen=True)
class MaterialFacts:
    """What Hotend assumes about a material family. Lengths in mm, angles in degrees."""

    family: str
    flexible: bool
    # Shore hardness when the material's id names one (`tpu-95a` -> "95A").
    shore: str | None
    # Steepest overhang from vertical that prints without support.
    max_overhang_deg: float
    # Walls a part of this material is usually given: (thinnest that holds its shape,
    # thickest that is still worth it).
    wall_mm: tuple[float, float]
    # How far a rim of this material may be stretched to put the part on, as a fraction of
    # its width. None for a rigid material: it is not stretched at all.
    stretch_strain: float | None
    # `direct_drive`: only a direct-drive nozzle feeds it (the X2D's main nozzle).
    nozzle: str
    guidance: tuple[str, ...]
    sources: tuple[str, ...]
    provenance: str = "guessed"

    def to_json(self) -> dict:
        return asdict(self)


_RIGID_SOURCE = "https://www.hubs.com/knowledge-base/how-design-parts-fdm-3d-printing/"
_FACTS: dict[str, MaterialFacts] = {
    "PLA": MaterialFacts(
        "PLA", False, None, 45.0, (0.8, 2.4), None, "any",
        ("Stiff and brittle: cracks at snap points and thin tabs that are bent repeatedly.",),
        (_RIGID_SOURCE, "https://3dstarter.com/en/howto/phone-case-diy"),
    ),
    "PETG": MaterialFacts(
        "PETG", False, None, 45.0, (0.8, 2.4), None, "any",
        (("Tougher than PLA but still rigid: a shell needs room to go on (one designer's "
         "figure: 0.20 mm per side around a phone)."),),
        (_RIGID_SOURCE, "https://raw.githubusercontent.com/peterdsharpe/phone-case/main/README.md"),
    ),
    "ABS": MaterialFacts("ABS", False, None, 45.0, (0.8, 2.4), None, "any", (), (_RIGID_SOURCE,)),
    "ASA": MaterialFacts("ASA", False, None, 45.0, (0.8, 2.4), None, "any", (), (_RIGID_SOURCE,)),
    # TPU. Walls: 1.5 to 2.5 mm in a phone case (Qidi), 1.4 mm by a designer with printed
    # results (the phone-case source below); "a 2 mm wall in 95A TPU is floppy" (uavmodel).
    # Overhang: no TPU-specific source was found, so the general FDM 45 degrees stands.
    # Stretch: lips of 1.0 to 1.4 mm per side on phones about 75 mm wide (3 to 4 % of the
    # width) are printed practice in those sources; twice that is assumed to be the most a
    # rim is asked to stretch until the gauge says otherwise. Nozzle and feeding: Bambu
    # Lab's TPU guide (95A to 80A print, may buckle in the AMS, 0.4 mm nozzle, dry at 65 to
    # 75 C for 8 h, sticks strongly to the plate) and the X2D facts in
    # docs/IMPLEMENTATION_BRIEF.md section 7 (TPU only on the main nozzle; unverified on a
    # real printer).
    "TPU": MaterialFacts(
        "TPU", True, None, 45.0, (1.2, 2.5), 0.08, "direct_drive",
        (
            ("Prints only on a direct-drive nozzle (the main nozzle of the X2D) and from "
            "outside the AMS; 0.4 mm nozzle; dry the spool first."),
            ("Design a fit around a rigid thing as a `stretch` fit and print its gauge "
            "first: published practice ranges from 0.1 to 0.3 mm of drawn clearance per "
            "side, and no source ties the number to the Shore hardness."),
            ("Print slowly with few objects per plate; TPU strings across openings and "
            "bonds strongly to the plate."),
            "Softer than 80A is not recommended on this class of printer.",
        ),
        (
            "https://wiki.bambulab.com/en/knowledge-sharing/tpu-printing-guide",
            "https://qidi3d.com/blogs/print-lab/tpu-phone-cases",
            "https://raw.githubusercontent.com/peterdsharpe/phone-case/main/README.md",
            ("https://blog.uavmodel.com/tpu-3d-printing-guide-flexible-filament-settings-"
            "extruder-mods-and-fpv-part-design-2026/"),
        ),
    ),
}
# What is assumed of a family this table does not know: rigid, general FDM practice.
_UNKNOWN = MaterialFacts("", False, None, 45.0, (0.8, 2.4), None, "any", (), (_RIGID_SOURCE,))
FLEXIBLE_FAMILIES = tuple(name for name, facts in _FACTS.items() if facts.flexible)


def _field(material: Any, name: str) -> str:
    value = material.get(name) if isinstance(material, dict) else getattr(material, name, None)
    return value.strip() if isinstance(value, str) else ""


def material_facts(material: Any = None, family: str = "") -> MaterialFacts:
    """The facts for a request's material: a dict or object with `id` and `family`, or a
    family name. A material whose family is not given is recognised by its id (`tpu-95a`,
    `pla-basic`). An unknown material is taken to be rigid."""
    identifier = _field(material, "id") if material is not None else ""
    family = (family or (_field(material, "family") if material is not None else "")).upper()
    if isinstance(material, str):
        identifier = material
    if family not in _FACTS:
        family = next((name for name in _FACTS if identifier.upper().startswith(name)), family)
    facts = _FACTS.get(family)
    if facts is None:
        return _unknown(family)
    shore = _SHORE.search(identifier)
    if facts.flexible and shore:
        return MaterialFacts(**{**asdict(facts), "shore": f"{shore.group(1)}A"})
    return facts


def _unknown(family: str) -> MaterialFacts:
    return MaterialFacts(**{**asdict(_UNKNOWN), "family": family})


def is_flexible(material: Any = None, family: str = "") -> bool:
    """True when the material can be stretched over the thing it holds (TPU)."""
    return material_facts(material, family).flexible


def list_materials() -> list[dict]:
    """Every material family Hotend has facts for, as JSON data."""
    return [facts.to_json() for facts in _FACTS.values()]
