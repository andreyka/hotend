"""BuildContext: what a model script's `build(p, ctx)` receives as `ctx`.

`ctx.printer` and `ctx.material` are the request's printer and material (attribute access,
unknown extra fields kept). `ctx.fit` is the Proven Fit resolver.

Contract with hotend_fit (the engine never imports anything else from it):

    hotend_fit.fit.resolver_for(context: dict) -> resolver

`context` is the request's `context` object as a plain dict, with `calibration_path` replaced
by an absolute path inside the job folder, or None when the request names none. The resolver
has `clearance(fit_class, nominal)` and a list attribute `declared_fits` that feature cards
append to; the `fit_pairs` check reads that list after the build.

`ctx.references` are the request's reference objects by name (`context.references`), each a
`hotend_fit.reference.Reference` read from its file by `load_references` before any script
code runs, and `ctx.component(id)` is a bought device from `hotend_fit.components`. The
model process and the worker each read the files themselves; the worker verifies pockets
against its own copy.

When hotend_fit is not installed, `ctx.fit` is a placeholder that raises FitUnavailable on any
use, so a model that needs fits fails with a clear model_error instead of guessing a clearance.
"""

from __future__ import annotations

import atexit
import importlib.util
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .errors import BadRequest
from .request import Context, Material, Printer

# Reference files are read once per process and job folder: a second context of the same
# build (the determinism rebuild) must not read a file after script code has run. Emptied
# at exit, so the mesh kernel's objects are released before the interpreter shuts down.
_LOADED: dict[tuple, Any] = {}
atexit.register(_LOADED.clear)


class FitUnavailable(RuntimeError):
    """Raised when a model uses ctx.fit but the hotend_fit package is not installed."""


class MissingFit:
    """Stands in for the fit resolver when hotend_fit is not installed."""

    declared_fits: tuple = ()

    def __getattr__(self, name: str) -> Any:
        raise FitUnavailable(
            f"ctx.fit.{name} needs the hotend_fit package, which is not installed in this "
            "engine; clearances cannot be resolved"
        )


@dataclass(frozen=True)
class BuildContext:
    printer: Printer | None
    material: Material | None
    fit: Any
    # The request's reference objects by name (hotend_fit.reference.Reference).
    references: dict = field(default_factory=dict)

    def component(self, component_id: str) -> Any:
        """A bought device by id (`ctx.component("sample_phone")`): its body as a reference,
        its features with keep-out volumes, every number with provenance. See
        `hotend_fit.components`."""
        if isinstance(self.fit, MissingFit):
            raise FitUnavailable(
                "ctx.component needs the hotend_fit package, which is not installed in "
                "this engine")
        from hotend_fit.components import use_component

        return use_component(self.fit, component_id)

    @property
    def declared_fits(self) -> list:
        """The mating pairs the feature cards declared during the build (may be empty)."""
        if isinstance(self.fit, MissingFit):
            return []
        return list(getattr(self.fit, "declared_fits", None) or [])


def resolve_inside(job_dir: Path, relative: str, what: str) -> Path:
    """Resolve a path from the request and refuse anything outside the job folder."""
    candidate = Path(relative)
    if candidate.is_absolute():
        raise BadRequest(f"{what} must be relative to the job folder, got {relative!r}")
    root = job_dir.resolve()
    resolved = (root / candidate).resolve()
    if not resolved.is_relative_to(root):
        raise BadRequest(f"{what} points outside the job folder: {relative!r}")
    return resolved


def load_reference(spec: Any, job_dir: Path, max_triangles: int | None = None) -> tuple:
    """Read one reference file of the request: (Reference, ReferenceFile). The file is
    hostile input: it must lie inside the job folder and is read by
    `hotend_engine.reference_io` within its size and triangle caps. Nothing is repaired
    silently: what was left out or could not be closed is said in `reference.notes`.

    The reference is the object as printed: every placement the file holds is applied,
    modifier volumes are ignored and negative parts are cut away when the meshes allow.
    A file with several objects needs `plate` and/or `object`; they are never merged.
    Raises BadRequest with a sentence that lists what the file holds."""
    import numpy as np

    from hotend_fit.reference import Reference, ReferenceProblem

    from . import reference_io

    path = resolve_inside(job_dir, spec.path, f"context.references[{spec.name!r}].path")
    key = (str(path), spec.model_dump_json(), max_triangles)
    if key in _LOADED:
        return _LOADED[key]
    try:
        loaded = reference_io.read_reference_file(
            path, spec.format,
            max_triangles=max_triangles or reference_io.DEFAULT_MAX_TRIANGLES)
        chosen = loaded.select(spec.plate, spec.object)
    except reference_io.ReferenceFileError as exc:
        raise BadRequest(f"reference '{spec.name}': {exc}") from exc
    notes = list(loaded.notes)
    factor = float(spec.scale)
    if spec.units is not None:
        factor *= (reference_io.UNITS_MM[reference_io.REQUEST_UNITS[spec.units]]
                   / reference_io.UNITS_MM[loaded.unit])
    vertices, triangles = _one_mesh(chosen, notes)
    vertices = vertices * factor
    low, high = vertices.min(axis=0), vertices.max(axis=0)
    if not np.all(np.isfinite(vertices)) or float((high - low).max()) > reference_io.MAX_COORDINATE_MM:
        raise BadRequest(f"reference '{spec.name}': with units and scale applied it is not "
                         "a printable size")
    # The reference's own frame: standing on z = 0, the centre of its footprint on the Z axis.
    shift = np.array([(low[0] + high[0]) / 2, (low[1] + high[1]) / 2, low[2]])
    source = {
        "file": spec.path, "format": loaded.format, "bytes": loaded.bytes,
        "sha256": loaded.sha256, "producer": loaded.producer, "unit": loaded.unit,
        "scale": factor, "object": chosen.name, "object_index": chosen.index,
        "plate": chosen.plate, "plate_name": chosen.plate_name,
        "parts": chosen.listing()["parts"],
        "file_bbox": {"min": [round(float(v), 4) for v in low],
                      "max": [round(float(v), 4) for v in high]},
        "objects_in_file": len(loaded.objects),
    }
    try:
        reference = Reference(spec.name, vertices - shift, triangles, kind=spec.kind,
                              provenance="measured", source=source, notes=notes,
                              label=chosen.name or spec.name)
    except ReferenceProblem as exc:
        raise BadRequest(f"reference '{spec.name}': {exc}") from exc
    if not reference.closed:
        reference.notes.append(
            "The mesh is not a closed surface: its volume is an estimate and its outlines "
            "are those of its convex hull. Nothing was repaired.")
    _LOADED[key] = (reference, loaded)
    return _LOADED[key]


def _one_mesh(chosen: Any, notes: list) -> tuple:
    """The geometry parts of one object as one mesh: united when every part is a closed
    surface (and negative parts cut away), otherwise put side by side with a note."""
    import manifold3d
    import numpy as np

    def solid(part: Any) -> Any:
        candidate = manifold3d.Manifold(manifold3d.Mesh64(
            vert_properties=np.array(part.vertices, dtype=np.float64, order="C"),
            tri_verts=np.array(part.triangles, dtype=np.uint64, order="C")))
        ok = candidate.status() == manifold3d.Error.NoError and not candidate.is_empty()
        return candidate if ok else None

    geometry = chosen.geometry()
    negative = [part for part in chosen.parts if part.role == "negative_part"]
    if len(geometry) == 1 and not negative:
        return geometry[0].vertices, geometry[0].triangles
    solids = [solid(part) for part in geometry]
    if all(s is not None for s in solids):
        united = manifold3d.Manifold.batch_boolean(solids, manifold3d.OpType.Add)
        for part in negative:
            cutter = solid(part)
            if cutter is None:
                notes.append(f"The negative part '{part.name}' is not a closed surface and "
                             "was not cut away.")
            else:
                united = united - cutter
        mesh = united.to_mesh64()
        if len(geometry) > 1:
            notes.append(f"{len(geometry)} parts of the object "
                         f"({', '.join(repr(part.name) for part in geometry[:6])}) were "
                         "united into one shape, as they print.")
        if negative:
            notes.append(f"{len(negative)} negative part(s) were cut away, as they print.")
        return (np.array(mesh.vert_properties)[:, :3].astype(np.float64),
                np.array(mesh.tri_verts).astype(np.int64))
    open_parts = [part.name for part, s in zip(geometry, solids, strict=True) if s is None]
    notes.append(f"The part(s) {', '.join(repr(n) for n in open_parts[:6])} are not closed "
                 "surfaces, so the object's parts were put side by side, not united"
                 + (", and its negative parts were not cut away." if negative else "."))
    offsets = np.cumsum([0] + [len(part.vertices) for part in geometry[:-1]])
    return (np.concatenate([part.vertices for part in geometry]),
            np.concatenate([part.triangles + offset
                            for part, offset in zip(geometry, offsets, strict=True)]))


def make_context(context: Context, job_dir: Path, *, references: bool = True,
                 max_reference_triangles: int | None = None) -> BuildContext:
    """Build the ctx for one request. A fresh resolver per call, so declared fits never
    leak. `references=False` leaves the request's reference files unread (for an op that
    does not fit anything to them)."""
    loaded: dict = {}
    if importlib.util.find_spec("hotend_fit") is None:
        fit: Any = MissingFit()
        if references and context.references:
            raise BadRequest("context.references needs the hotend_fit package, which is "
                             "not installed in this engine")
    else:
        from hotend_fit.fit import resolver_for

        raw = context.model_dump(mode="json")
        raw["calibration_path"] = (
            str(resolve_inside(job_dir, context.calibration_path, "context.calibration_path"))
            if context.calibration_path
            else None
        )
        raw.pop("references", None)
        fit = resolver_for(raw)
        if references and context.references:
            for spec in context.references:
                loaded[spec.name] = load_reference(spec, job_dir, max_reference_triangles)[0]
            fit.references.update(loaded)
    return BuildContext(printer=context.printer, material=context.material, fit=fit,
                        references=loaded)
