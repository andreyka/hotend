"""Shared types of the checks: what a check receives and what it returns."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from ..context import BuildContext
from ..parts import NamedPart
from ..request import CheckOptions
from ..tessellate import PartMesh, Tessellation

PASS, WARN, FAIL, SKIPPED = "pass", "warn", "fail", "skipped"
ERROR, WARNING = "error", "warning"


@dataclass
class CheckResult:
    """One line of result.json `checks`. `message` is for a non-expert; numbers go in `data`."""

    id: str
    status: str
    severity: str
    message: str
    part: str | None = None
    data: dict = field(default_factory=dict)

    def to_json(self) -> dict:
        return {
            "id": self.id,
            "status": self.status,
            "severity": self.severity,
            "message": self.message,
            "part": self.part,
            "data": round_floats(self.data),
        }


class TessellationFailed(RuntimeError):
    """The mesher raised: a check that needs the mesh could not run (not: was not asked)."""


@dataclass
class CheckEnv:
    """Everything a check may look at. `tessellation` is None when meshing failed.

    `tessellation_crash` is the exception when the mesher raised. Then `mesh()` raises
    `TessellationFailed`, so every check that needs a mesh ends as "could not run" (a fail
    for an error-severity check, which holds the version back), never as "skipped".
    Without it a missing mesh is a reason to skip, as when meshing was not asked for.
    """

    parts: list[NamedPart]
    ctx: BuildContext
    options: CheckOptions
    tessellation: Tessellation | None = None
    tessellation_error: str | None = None
    # The fit verification of this build (checks.fit_pairs.fit_report), computed once:
    # None = not computed yet, False = nothing to verify.
    fit_cache: Any = field(default=None, repr=False)
    # The pocket verification of this build (checks.reference_fit.pocket_reports), once.
    pocket_cache: Any = field(default=None, repr=False)
    # The model's validated declarations (hotend_fit.contract objects) and its PARAMS.
    contract: Any = None
    print_hints: Any = None
    param_specs: dict = field(default_factory=dict)
    tessellation_crash: BaseException | None = field(default=None, repr=False)
    _prepared: dict = field(default_factory=dict, repr=False)
    _geometry: dict = field(default_factory=dict, repr=False)
    _turned: dict = field(default_factory=dict, repr=False)
    _fit_features: Any = field(default=None, repr=False)

    def prepared(self, part: NamedPart,
                 nozzle: Any = None) -> tuple[NamedPart, PartMesh | None, Any]:
        """The part as print preparation would turn it for `nozzle` (default: the main
        nozzle, which prints a part unless the user says otherwise), standing on z = 0:
        (part, mesh, the orientation `Choice`). Computed once per part and nozzle, with the
        margin preparation keeps by default."""
        return self._prepare(part, nozzle)[:3]

    def prepared_size(self, part: NamedPart, nozzle: Any = None) -> tuple[float, float, float]:
        """The size (x, y, z) of the part as `prepared` turns it for `nozzle`: what the
        plating lays out. Computed once per way up."""
        turned = self._turned[self._prepare(part, nozzle)[3]]
        if turned[2] is None:
            turned[2] = tuple(float(v) for v in tuple(turned[0].shape.bounding_box().size))
        return turned[2]

    def _prepare(self, part: NamedPart, nozzle: Any) -> tuple:
        printer = self.ctx.printer
        if nozzle is None:
            nozzle = printer.main_nozzle if printer else None
        key = (part.name, None if nozzle is None else nozzle.id)
        if key not in self._prepared:
            from .. import prepare

            if self._fit_features is None:
                self._fit_features = prepare.fit_features(self.parts, self.ctx.fit)
            material = self.ctx.material
            limit = (material.max_overhang_deg if material and material.max_overhang_deg
                     else prepare.DEFAULT_MAX_OVERHANG_DEG)
            hint = self.print_hints.parts.get(part.name) if self.print_hints else None
            mesh = self.mesh(part)
            if part.name not in self._geometry:
                self._geometry[part.name] = prepare.PartGeometry(part, mesh)
            choice = prepare.choose_orientation(
                self._geometry[part.name], limit, self._fit_features.get(part.name),
                preferred=hint.vector if hint else None,
                preferred_by="the model" if hint else None,
                reach_size=nozzle.reach.size if nozzle and nozzle.reach else None,
                margin=prepare.DEFAULT_MARGIN_MM,
            )
            # Two nozzles usually choose the same way up: turn the part once (the exact
            # bounding box of a turned solid is the slow step here).
            way = (part.name, tuple(round(float(v), 9) + 0.0 for v in choice.up))
            if way not in self._turned:
                moved, moved_mesh, _ = prepare.turned_part(part, mesh, choice.rotation)
                self._turned[way] = [moved, moved_mesh, None]
            self._prepared[key] = (*self._turned[way][:2], choice, way)
        return self._prepared[key]

    def mesh(self, part: NamedPart) -> PartMesh | None:
        if self.tessellation is None:
            if self.tessellation_crash is not None:
                raise TessellationFailed("the mesher failed") from self.tessellation_crash
            return None
        mesh = self.tessellation.meshes.get(part.name)
        return mesh if mesh is not None and len(mesh.triangles) else None

    @property
    def no_mesh_reason(self) -> str:
        return self.tessellation_error or "no triangle mesh is available for this part"


def round_floats(value: Any, digits: int = 4) -> Any:
    """Round every float in a JSON-like value, so `data` stays readable and stable."""
    if isinstance(value, float):
        return round(value, digits)
    if isinstance(value, dict):
        return {key: round_floats(item, digits) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [round_floats(item, digits) for item in value]
    return value
