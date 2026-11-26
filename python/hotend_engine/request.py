"""The request contract of docs/PROTOCOL.md section 3.2 as pydantic models.

Unknown fields are rejected everywhere except inside `context` (the core may describe a printer
or material in more detail than the engine uses) and `limits` (the supervisor's own limits).
A request that does not validate is a bad request: the worker exits with code 2.
"""

from __future__ import annotations

import json
from typing import Annotated, Any, Literal

from pydantic import (
    AfterValidator,
    BaseModel,
    ConfigDict,
    Field,
    TypeAdapter,
    model_validator,
)

from . import PROTOCOL_VERSION

VIEW_NAMES = ("iso", "front", "back", "left", "right", "top", "bottom")
EXPORT_FORMATS = ("3mf", "stl", "step")

ViewName = Literal["iso", "front", "back", "left", "right", "top", "bottom"]
Span = tuple[float, float]


class _Strict(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)


class _Open(BaseModel):
    model_config = ConfigDict(extra="allow", frozen=True)


class Reach(_Open):
    """The box a nozzle can reach, as [low, high] per axis."""

    x: Span
    y: Span
    z: Span

    @model_validator(mode="after")
    def _ordered(self) -> Reach:
        for axis in ("x", "y", "z"):
            low, high = getattr(self, axis)
            if not high > low:
                raise ValueError(f"reach.{axis} must be [low, high] with high > low")
        return self

    @property
    def size(self) -> tuple[float, float, float]:
        return (self.x[1] - self.x[0], self.y[1] - self.y[0], self.z[1] - self.z[0])


class Nozzle(_Open):
    id: int
    role: str = ""
    diameter: float = Field(gt=0)
    reach: Reach | None = None


class Printer(_Open):
    id: str
    nozzles: tuple[Nozzle, ...] = ()

    @property
    def main_nozzle(self) -> Nozzle | None:
        """The nozzle with role "main", otherwise the first one listed."""
        for nozzle in self.nozzles:
            if nozzle.role == "main":
                return nozzle
        return self.nozzles[0] if self.nozzles else None


class Material(_Open):
    id: str
    family: str = ""
    density_g_cm3: float | None = Field(default=None, gt=0)
    max_overhang_deg: float | None = Field(default=None, gt=0, lt=90)


class ReferenceSpec(_Strict):
    """A reference object: a real thing the model is built around, given as a file in the
    job folder (docs/PROTOCOL.md section 3.2). The file is untrusted input."""

    # The name the model asks for it by: ctx.references["name"].
    name: str = Field(pattern=r"^[a-z][a-z0-9_]{0,31}$")
    # Relative to the job folder; anything that resolves outside it is refused.
    path: str = Field(min_length=1, max_length=512)
    # Default: by the file's extension and first bytes.
    format: Literal["stl", "3mf", "step"] | None = None
    # The unit of the file's numbers. Default: what the file says (3MF, STEP), else mm.
    units: Literal["mm", "cm", "m", "in"] | None = None
    # Applied on top of the unit and of every placement the file itself holds.
    scale: float = Field(default=1.0, gt=0, le=1000)
    # `soft`: a squishy thing; fit it with the cradle class.
    kind: Literal["rigid", "soft"] = "rigid"
    # Which object of a file that holds several: the plate's number and/or the object's
    # name or 1-based index, as the `references` op lists them. Never merged silently.
    plate: int | None = Field(default=None, ge=1, le=4096)
    object: int | str | None = None


class Context(_Open):
    printer: Printer | None = None
    material: Material | None = None
    calibration_path: str | None = None
    references: tuple[ReferenceSpec, ...] = Field(default=(), max_length=8)

    @model_validator(mode="after")
    def _distinct_references(self) -> Context:
        names = [reference.name for reference in self.references]
        if len(set(names)) != len(names):
            raise ValueError("context.references names a reference twice")
        return self


class Outputs(_Strict):
    step: bool = True
    brep: bool = True
    mesh: bool = True
    views: tuple[ViewName, ...] = ()
    stl: bool = False
    threemf: bool = False
    view_size: tuple[int, int] = (768, 768)
    # Show placed references as ghosts in mesh.json and in the views. They are never part
    # of the B-rep, the STEP, the STL, the 3MF or a plate.
    ghosts: bool = True


class CheckOptions(_Strict):
    enabled: bool = True
    min_wall: float = Field(default=0.8, gt=0)
    verify_determinism: bool = False
    only: tuple[str, ...] | None = None
    sample_ranges: bool = False
    # Which orientation `overhangs` and `bed_contact` judge: the part as modelled, or as
    # print preparation turns it (for a caller that prints the prepared plate).
    orientation: Literal["as_modelled", "prepared"] = "as_modelled"
    # Measure the model's CONTRACT on the solids (the `contract` check, and `questions`).
    contract: bool = False
    # Look for faces of two parts that face each other closely outside any declared fit.
    undeclared_joints: bool = False


class Limits(_Open):
    wall_seconds: float = 120
    max_triangles: int = Field(default=2_000_000, gt=0)
    # The most triangles one reference file may hold (all its objects together).
    max_reference_triangles: int = Field(default=2_000_000, gt=0, le=8_000_000)


class Section(_Strict):
    """Cut the model with a principal plane before rendering, to show the inside."""

    plane: Literal["XY", "XZ", "YZ"]
    offset: float = 0.0


class _Request(_Strict):
    protocol: Literal[0] = PROTOCOL_VERSION


class BuildRequest(_Request):
    op: Literal["build"]
    model_path: str = "model.py"
    params: dict[str, Any] = {}
    context: Context = Context()
    outputs: Outputs = Outputs()
    checks: CheckOptions = CheckOptions()
    limits: Limits = Limits()


class MeasureRequest(_Request):
    op: Literal["measure"]
    queries: tuple[dict[str, Any], ...] = Field(min_length=1, max_length=64)
    limits: Limits = Limits()


class ExportRequest(_Request):
    op: Literal["export"]
    formats: tuple[Literal["3mf", "stl", "step"], ...] = Field(min_length=1)
    parts: tuple[str, ...] | None = None
    limits: Limits = Limits()


class ViewsRequest(_Request):
    op: Literal["views"]
    views: tuple[ViewName, ...] = Field(min_length=1)
    size: tuple[int, int] = (768, 768)
    section: Section | None = None
    limits: Limits = Limits()


class DocsRequest(_Request):
    op: Literal["docs"]
    query: str = Field(min_length=1, max_length=400)
    limit: int = Field(default=5, ge=1, le=20)
    limits: Limits = Limits()


Copies = Annotated[int, Field(ge=1, le=64)]


def _plain_zero(value: float) -> float:
    """A minus zero as zero, so that it is echoed and compared as the number it is."""
    return value + 0.0


# The lengths of a layout are numbers and nothing else: strict, so that no text ("6") and
# no flag (true) is read as a length. A whole number is a number.
Length = Annotated[float, Field(ge=0, le=50, strict=True), AfterValidator(_plain_zero)]
# Room kept free around every piece for its brim (PrepareRequest.clearance).
Clearance = Annotated[float, Field(ge=0, le=20, strict=True), AfterValidator(_plain_zero)]
BedCoordinate = Annotated[float, Field(ge=-1000, le=1000, strict=True),
                          AfterValidator(_plain_zero)]
Stage = Literal["all", "orient", "plate"]
Anchor = Literal["front_left", "centre"]
# The request fields of the print settings feature. A request that names none of them is
# answered exactly as before the feature existed (`_PlateRequest.print_settings`).
_PRINT_SETTINGS_FIELDS = frozenset({"stage", "clearance", "anchor", "keepouts"})


class Keepout(_Strict):
    """A box of the bed, in bed coordinates, that no piece and no brim may touch."""

    # A name from the printer data. It is repeated in the `keepout` check, so it is held
    # to a pattern.
    id: str = Field(pattern=r"^[a-z][a-z0-9_]{0,31}$")
    min: tuple[BedCoordinate, BedCoordinate]
    max: tuple[BedCoordinate, BedCoordinate]

    @model_validator(mode="after")
    def _ordered(self) -> Keepout:
        if not (self.max[0] > self.min[0] and self.max[1] > self.min[1]):
            raise ValueError("a keep-out zone needs max greater than min on both axes")
        return self


class _PlateRequest(_Request):
    """What the `prepare` and `gauge` requests share about laying out plates."""

    gap: Length = 5.0
    margin: Length = 2.0
    # `all`: orientations and plates in one go. `orient`: orientations, facts and checks;
    # no plate is laid out and no file is written. `plate`: the layout for orientations
    # that an `orient` pass chose (a `prepare` request must then pin every part).
    stage: Stage = "all"
    # Grown onto every footprint before the layout: the widest brim plus what is to stay
    # free around it. The print settings rules choose the brim; the engine only makes room.
    clearance: Clearance = 0.0
    # `centre`: the pieces of a plate are moved together to the middle of the reach.
    anchor: Anchor = "front_left"
    keepouts: tuple[Keepout, ...] = Field(default=(), max_length=16)

    @model_validator(mode="after")
    def _distinct_keepouts(self) -> _PlateRequest:
        ids = [zone.id for zone in self.keepouts]
        if len(set(ids)) != len(ids):
            raise ValueError("keepouts names a zone twice")
        return self

    @property
    def print_settings(self) -> bool:
        """Whether the caller speaks the print settings interface: it named one of its
        fields. Only then does the result carry the print facts and the layout record,
        which a caller from before the feature would refuse as unknown fields."""
        return bool(_PRINT_SETTINGS_FIELDS & self.model_fields_set)


class PrepareRequest(_PlateRequest):
    """Turn a built model into plates: orientation per part, then a layout per plate."""

    op: Literal["prepare"]
    context: Context = Context()
    parts: tuple[str, ...] | None = None
    copies: dict[str, Copies] = {}
    # Part name -> id of the nozzle that prints it (default: the main nozzle).
    nozzles: dict[str, int] = {}
    # Part name -> {"up": ...} as in a model's PRINT; the user's choice, used before the
    # model's own.
    orientations: dict[str, Any] = {}
    threemf: bool = True
    views: tuple[ViewName, ...] = ("iso", "top")
    view_size: tuple[int, int] = (768, 768)
    limits: Limits = Limits()


class CardRef(_Strict):
    """A feature card by kind and public arguments, as `cards.<card>(**args, ctx=ctx)`."""

    # A card kind, or "pocket" for the gauge of a reference fit, whose args are
    # {"reference": name or component id, "fit": ..., "shape": ..., "direction": ...}.
    card: str = Field(min_length=1, max_length=64)
    args: dict[str, Any] = {}
    name: str | None = Field(default=None, min_length=1, max_length=64)


class GaugeRequest(_PlateRequest):
    """Export the test gauges of feature cards as a plate of their own. Without `cards`,
    the gauges of every card the built model in the job folder uses."""

    op: Literal["gauge"]
    context: Context = Context()
    cards: tuple[CardRef, ...] | None = Field(default=None, min_length=1, max_length=32)
    nozzle_id: int | None = None
    threemf: bool = True
    views: tuple[ViewName, ...] = ("iso", "top")
    view_size: tuple[int, int] = (768, 768)
    limits: Limits = Limits()


class GaugeReading(_Strict):
    """One caliper reading of a printed gauge dimension (`gauges[].dimensions`)."""

    feature: Literal["hole", "peg"]
    drawn: float = Field(gt=0, le=1000)
    measured: float = Field(gt=0, le=1000)


class CalibrateRequest(_Request):
    """Turn readings of a printed gauge into the updated calibration table."""

    op: Literal["calibrate"]
    context: Context = Context()
    # When the gauge was measured. The engine reads no clock: the caller says.
    measured_at: str = Field(min_length=10, max_length=40)
    # The nozzle that printed the gauge (default: the main nozzle).
    nozzle_id: int | None = None
    readings: tuple[GaugeReading, ...] = Field(min_length=1, max_length=64)
    limits: Limits = Limits()


class ReferencesRequest(_Request):
    """Read the reference files of `context.references` and say what each holds: every
    object with its plate, parts and size, and the measures of the one a selection names.
    Nothing is built. Without `context.references` it lists the shipped components."""

    op: Literal["references"]
    context: Context = Context()
    # Component ids to describe in full (every number with its provenance and source).
    components: tuple[str, ...] = Field(default=(), max_length=8)
    limits: Limits = Limits()


ModelName = Annotated[str, Field(pattern=r"^[a-z][a-z0-9_]{0,31}$")]
Text = Annotated[str, Field(max_length=300)]


class SweepFailure(_Strict):
    check: str = Field(max_length=64)
    message: Text


class SweepRequirement(_Strict):
    id: str = Field(max_length=64)
    status: Literal["pass", "warn", "fail", "skipped"]
    met: bool | None = None
    not_checkable: bool = False
    message: Text


class SweepSample(_Strict):
    params: dict[ModelName, float] = Field(max_length=8)
    status: Literal["ok", "check_error", "model_error", "not_run"]
    failed: tuple[SweepFailure, ...] = Field(default=(), max_length=16)
    requirements: tuple[SweepRequirement, ...] = Field(default=(), max_length=32)
    parts: dict[ModelName, tuple[float, float, float]] = Field(default={}, max_length=64)


class SweepReport(_Strict):
    """What the range sampler's `sweep` answered, as the model process wrote it: a claim
    of the process that ran the script. The worker passes it on only in this shape and the
    core never stores it: advice for the agent, not a check outcome of a version."""

    protocol: Literal[0] = PROTOCOL_VERSION
    builds: int = Field(ge=0, le=64)
    truncated: bool
    checks: tuple[str, ...] = Field(max_length=16)
    samples: tuple[SweepSample, ...] = Field(min_length=1, max_length=16)


class SweepRequest(_Request):
    """Build the model at a few parameter settings, one after the other, and report how each
    fares (docs/PROTOCOL.md section 3.2). Nothing is stored. The settings run in the model
    process with the range sampler (`hotend_fit.sampler.sweep`)."""

    op: Literal["sweep"]
    model_path: str = "model.py"
    # The values every setting starts from: the current version's parameters.
    params: dict[str, Any] = {}
    # Each setting overrides some parameters; 1 to 16 of them.
    settings: tuple[dict[ModelName, float], ...] = Field(min_length=1, max_length=16)
    context: Context = Context()
    min_wall: float = Field(default=0.8, gt=0)
    # No further setting is started after this many seconds of building.
    budget_seconds: float = Field(default=150.0, gt=0, le=600)
    limits: Limits = Limits()

    @model_validator(mode="after")
    def _small_settings(self) -> SweepRequest:
        if any(not 1 <= len(setting) <= 8 for setting in self.settings):
            raise ValueError("every sweep setting names 1 to 8 parameters")
        return self


Request = Annotated[
    BuildRequest | MeasureRequest | ExportRequest | ViewsRequest | DocsRequest
    | PrepareRequest | GaugeRequest | CalibrateRequest | ReferencesRequest | SweepRequest,
    Field(discriminator="op"),
]

_ADAPTER: TypeAdapter[Request] = TypeAdapter(Request)


class RecordError(_Strict):
    type: str
    message: str
    line: int | None = None
    traceback: str = ""


class BuildRecord(_Strict):
    """`model.build.json`: what the model process says happened. It is written by the process
    that ran the script, so every field is a claim: the worker copies the error and the
    prints, rebuilds the fits from `fits` and takes nothing else from it. For a `sweep`
    request it holds the sampler's report instead of a build."""

    protocol: Literal[0] = PROTOCOL_VERSION
    status: Literal["ok", "model_error"]
    error: RecordError | None = None
    stdout: str = ""
    stdout_truncated: bool = False
    fits: dict[str, Any] | None = None
    timings: dict[str, Any] = {}
    sweep: SweepReport | None = None

    @model_validator(mode="after")
    def _error_goes_with_status(self) -> BuildRecord:
        if (self.status == "model_error") != (self.error is not None):
            raise ValueError("error must be given exactly when status is model_error")
        return self


def parse_build_record(raw: Any) -> BuildRecord:
    """Validate a decoded model.build.json. Raises pydantic.ValidationError."""
    return BuildRecord.model_validate(raw)

MAX_VIEW_PIXELS = 2048


def _once(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    """An object of the request text. A key given twice is refused: the JSON reader would
    keep the later one, so what one reader checked another might not use."""
    found = dict(pairs)
    if len(found) != len(pairs):
        raise ValueError("an object names a key twice; give every key once")
    return found


def _no_constant(_name: str) -> Any:
    raise ValueError("NaN and Infinity are not JSON numbers; give a finite number")


def decode_request(text: str) -> Any:
    """Decode the text of request.json for `parse_request`. Raises ValueError, with the
    reason, when the text is not JSON, names a key twice in one object, holds NaN or an
    infinity, or is nested deeper than the reader follows."""
    try:
        return json.loads(text, object_pairs_hook=_once, parse_constant=_no_constant)
    except RecursionError:
        raise ValueError("the text is nested too deeply; a request is a few levels of "
                         "objects and lists") from None


def parse_request(raw: Any) -> Request:
    """Validate a decoded request.json. Raises pydantic.ValidationError when it is malformed."""
    request = _ADAPTER.validate_python(raw)
    size = (getattr(request, "size", None) or getattr(request, "view_size", None)
            or getattr(getattr(request, "outputs", None), "view_size", None))
    if size is not None and not all(64 <= side <= MAX_VIEW_PIXELS for side in size):
        raise ValueError(f"view size must be between 64 and {MAX_VIEW_PIXELS} pixels per side")
    return request
