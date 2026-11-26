"""Helpers shared by the hotend_fit tests (imported by name; this folder has no conftest so
that `from conftest import ...` in the engine's tests keeps meaning the engine's conftest)."""

from __future__ import annotations

import copy
import json
from pathlib import Path

from hotend_fit import fit

MODELS = Path(__file__).resolve().parents[1] / "models"
EXTREMES = ("nominal", "tight", "loose")

X2D_CONTEXT = {
    "printer": {"id": "bambu-x2d", "nozzles": [
        {"id": 1, "role": "main", "diameter": 0.4,
         "reach": {"x": [0, 256], "y": [0, 256], "z": [0, 260]}},
        {"id": 0, "role": "auxiliary", "diameter": 0.4,
         "reach": {"x": [20.5, 256], "y": [0, 256], "z": [0, 256]}}]},
    "material": {"id": "pla-basic", "family": "PLA"},
}

MEASURED_AT = "2026-11-02T10:00:00Z"


def context(**extra) -> dict:
    return {**copy.deepcopy(X2D_CONTEXT), **extra}


def resolver(extreme: str = "nominal", **extra) -> fit.FitResolver:
    return fit.resolver_for(context(fit_extreme=extreme, **extra))


def quantity(value: float, sigma: float, provenance: str = "calibrated") -> dict:
    measured = provenance in ("calibrated", "measured")
    return {"value": value, "sigma": sigma, "provenance": provenance,
            "measured_at": MEASURED_AT if measured else None}


def table(**overrides) -> dict:
    """A valid calibration table for the X2D's main nozzle and PLA; `overrides` replace
    quantities of the one entry."""
    entry = {
        "material": "pla-basic", "nozzle_id": 1, "nozzle_diameter": 0.4,
        "hole_offset": quantity(0.06, 0.01),
        "peg_offset": quantity(0.02, 0.01),
        "xy_shrink": quantity(0.002, 0.0005),
        "z_shrink": quantity(0.001, 0.0005),
        "min_clearance": quantity(0.08, 0.01),
    }
    entry.update(overrides)
    return {"protocol": 0, "printer_id": "bambu-x2d", "entries": [entry]}


def write_table(folder: Path, data: dict | str | None = None, name: str = "calibration.json") -> Path:
    path = folder / name
    data = table() if data is None else data
    path.write_text(data if isinstance(data, str) else json.dumps(data), encoding="utf-8")
    return path


def check_env(shapes: dict, resolver_: fit.FitResolver, mesh: bool = True, **options):
    """A CheckEnv over in-process shapes, as the engine builds it after a build."""
    from hotend_engine.checks import CheckEnv
    from hotend_engine.context import BuildContext
    from hotend_engine.parts import NamedPart
    from hotend_engine.request import CheckOptions, Context
    from hotend_engine.tessellate import tessellate

    parts = [NamedPart(name, shape, "#4c78a8") for name, shape in shapes.items()]
    parsed = Context.model_validate(X2D_CONTEXT)
    ctx = BuildContext(parsed.printer, parsed.material, resolver_)
    tessellation = tessellate(parts) if mesh else None
    return CheckEnv(parts, ctx, CheckOptions(**options), tessellation,
                    None if mesh else "tessellation was switched off for this test")


def fit_results(shapes: dict, resolver_: fit.FitResolver, kind: str | None = "pair") -> list:
    """The engine's fit_pairs check on in-process shapes: the results of one `data.kind`
    (`pair`, `joint`, `robustness`, `unverified`, `declaration`), or all of them with None.
    Results without a kind (skipped) are always kept."""
    from hotend_engine.checks.fit_pairs import fit_pairs

    results = fit_pairs(check_env(shapes, resolver_, mesh=False))
    return [r for r in results if kind is None or r.data.get("kind", kind) == kind]
