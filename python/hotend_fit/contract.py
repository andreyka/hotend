"""The product contract: the checkable promise a model is built against.

A contract (docs/PROTOCOL.md section 4.1) names the printed parts and lists requirements.
Every requirement carries a provenance tag that says where its numbers came from:

- `measured`: the user measured the real object.
- `standard`: taken from a standard or a datasheet (a screw size, a bought insert).
- `calibrated`: resolved from this printer's measured calibration.
- `guessed`: nobody measured it; the model's author picked it.
- `style`: a matter of taste that no fit depends on.

Guarantees:
- Validation is strict: unknown keys, repeated ids, a part that is not in `parts`, or a fit
  between a part and itself are rejected, so a contract that validates is internally whole.
- `open_questions` lists every requirement that rests on a guessed number, either because the
  requirement itself is tagged `guessed` or because it names a model parameter that is. Those
  are what the user is asked before the first print. Nothing else is ever asked.

This module is data and validation only. The engine's `contract` check
(`hotend_engine.checks.contract`) measures the requirements on the built solids.

Two smaller declarations of a model script are validated here as well, because they name the
same parts: `ASSEMBLY` (which part mates with which; data only, no check reads it yet) and
`PRINT` (the orientation a part prefers on the bed, read by the engine's print preparation).
"""

from __future__ import annotations

import re
from typing import Annotated, Any, Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator

from . import PROTOCOL_VERSION
from .calibration import Provenance
from .fit import FitClass

PROVENANCES = ("measured", "standard", "calibrated", "guessed", "style")
CARD_NAMES = ("snap_lid", "peg_hole", "screw_boss")

_NAME = r"^[a-z][a-z0-9_]{0,31}$"
Name = Annotated[str, Field(pattern=_NAME)]
Size3 = tuple[
    Annotated[float, Field(gt=0)], Annotated[float, Field(gt=0)], Annotated[float, Field(gt=0)]
]


class _Strict(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True, allow_inf_nan=False)


class _Requirement(_Strict):
    id: Name
    provenance: Provenance
    # Model parameters (PARAMS names) this requirement's numbers come from, if any.
    params: tuple[Name, ...] = ()
    note: str = Field(default="", max_length=400)

    def parts_named(self) -> tuple[str, ...]:
        return ()

    def subject(self) -> str:
        """What the requirement is about, for a question to the user."""
        return self.id.replace("_", " ")


class CavityMin(_Requirement):
    """The part has a free inner space at least this large (x, y, z in mm)."""

    kind: Literal["cavity_min"]
    part: Name
    size: Size3

    def parts_named(self) -> tuple[str, ...]:
        return (self.part,)

    def subject(self) -> str:
        x, y, z = self.size
        return f"the space inside '{self.part}' of at least {x:g} x {y:g} x {z:g} mm"


class OuterMax(_Requirement):
    """The part is no larger than this outside (x, y, z in mm)."""

    kind: Literal["outer_max"]
    part: Name
    size: Size3

    def parts_named(self) -> tuple[str, ...]:
        return (self.part,)

    def subject(self) -> str:
        x, y, z = self.size
        return f"the outside of '{self.part}' of at most {x:g} x {y:g} x {z:g} mm"


class WallMin(_Requirement):
    """No wall of the part is thinner than `value` mm."""

    kind: Literal["wall_min"]
    part: Name
    value: float = Field(gt=0)

    def parts_named(self) -> tuple[str, ...]:
        return (self.part,)

    def subject(self) -> str:
        return f"the wall of '{self.part}' of at least {self.value:g} mm"


class Dimension(_Requirement):
    """A named dimension of a part is `value` mm within `tolerance`."""

    kind: Literal["dimension"]
    part: Name
    name: str = Field(min_length=1, max_length=80)
    value: float
    tolerance: float = Field(default=0.1, gt=0)

    def parts_named(self) -> tuple[str, ...]:
        return (self.part,)

    def subject(self) -> str:
        return f"{self.name} of '{self.part}' ({self.value:g} mm)"


class Fit(_Requirement):
    """Two parts mate through a feature card with a named fit class."""

    kind: Literal["fit"]
    card: Literal["snap_lid", "peg_hole", "screw_boss"]
    between: tuple[Name, Name]
    fit_class: FitClass

    @model_validator(mode="after")
    def _two_parts(self) -> Fit:
        if self.between[0] == self.between[1]:
            raise ValueError("a fit is between two different parts")
        return self

    def parts_named(self) -> tuple[str, ...]:
        return self.between

    def subject(self) -> str:
        return (
            f"the {self.fit_class.replace('_', ' ')} fit ({self.card.replace('_', ' ')}) "
            f"between '{self.between[0]}' and '{self.between[1]}'"
        )


class ReferenceFit(_Requirement):
    """A real thing (a reference object of the request, or the body of a bought component)
    fits the room a printed part keeps for it: every pocket made for it is proven by the
    `reference_fit` check. `fit_class`, when given, is the class those pockets must have."""

    kind: Literal["reference_fit"]
    reference: Name
    fit_class: Literal["cradle", "stretch", "loose", "slide", "snug"] | None = None

    def subject(self) -> str:
        fit = f" with a {self.fit_class} fit" if self.fit_class else ""
        return f"the room for '{self.reference}'{fit}"


class Standoff(_Requirement):
    """A printed part stands at least `value` mm beyond a real thing on one side, so the
    thing's face there (a screen, a lens) stays off a flat surface the part lies on.

    `reference` is a reference object of the request or a bought component the model made
    a pocket for; `side` is a direction in the reference's own frame (`+z` is the front of
    a phone). The thing is taken where it ends up when it is pushed towards that side as
    far as the parts let it go, not where it was drawn. `feature` names a feature of a
    component that stands proud on that side (the camera lens): the stand-off is then
    measured from that feature's face. `at_most` bounds it from above as well (the
    thickness of a case's back). `region` limits the measurement to a rectangle across
    the side, given as two corners in the reference's frame (the two coordinates other
    than the side's axis, in axis order): the part must cover it everywhere, and its
    thinnest and thickest place there must both lie within the limits."""

    kind: Literal["standoff"]
    part: Name
    reference: Name
    side: Literal["+x", "-x", "+y", "-y", "+z", "-z"]
    value: float = Field(ge=0)
    at_most: float | None = Field(default=None, gt=0)
    feature: Name | None = None
    region: tuple[tuple[float, float], tuple[float, float]] | None = None

    @model_validator(mode="after")
    def _a_range(self) -> Standoff:
        if self.at_most is not None and self.at_most < self.value:
            raise ValueError("at_most is smaller than value")
        if self.region is not None and not all(
                high - low >= 1.0 for low, high in zip(*self.region, strict=True)):
            raise ValueError("region needs two corners at least 1 mm apart, the lower first")
        return self

    def parts_named(self) -> tuple[str, ...]:
        return (self.part,)

    def subject(self) -> str:
        thing = f"the {self.feature.replace('_', ' ')} of '{self.reference}'" if self.feature \
            else f"'{self.reference}'"
        upper = f" and at most {self.at_most:g} mm" if self.at_most is not None else ""
        return (f"the stand-off of '{self.part}' beyond {thing} on its {self.side} side "
                f"(at least {self.value:g} mm{upper})")


Requirement = Annotated[
    CavityMin | OuterMax | WallMin | Dimension | Fit | ReferenceFit | Standoff,
    Field(discriminator="kind")
]


class Contract(_Strict):
    protocol: Literal[0] = PROTOCOL_VERSION
    name: str = Field(min_length=1, max_length=120)
    parts: tuple[Name, ...] = Field(min_length=1, max_length=64)
    requirements: tuple[Requirement, ...] = Field(default=(), max_length=256)

    @model_validator(mode="after")
    def _consistent(self) -> Contract:
        if len(set(self.parts)) != len(self.parts):
            raise ValueError("a part is listed twice")
        seen: set[str] = set()
        for requirement in self.requirements:
            if requirement.id in seen:
                raise ValueError(f"requirement id {requirement.id!r} is used twice")
            seen.add(requirement.id)
            for part in requirement.parts_named():
                if part not in self.parts:
                    raise ValueError(
                        f"requirement {requirement.id!r} names part {part!r}, which is not in "
                        f"parts ({', '.join(self.parts)})"
                    )
        return self


class Question(_Strict):
    """One thing to ask the user before the first print."""

    requirement: str
    text: str
    guessed_params: tuple[str, ...] = ()


def parse_contract(raw: Any) -> Contract:
    """Validate a CONTRACT value from a model script. Raises pydantic.ValidationError."""
    return Contract.model_validate(raw)


class Mate(_Strict):
    """One entry of ASSEMBLY: the part mates with `mate`, written `<part>` or
    `<part>.<feature>` (the feature is a free label such as `rim`)."""

    mate: str = Field(pattern=r"^[a-z][a-z0-9_]{0,31}(\.[a-z][a-z0-9_]{0,31})?$")
    note: str = Field(default="", max_length=400)

    @property
    def part(self) -> str:
        return self.mate.split(".", 1)[0]


class Assembly(_Strict):
    """ASSEMBLY of a model script: part name -> what it mates with."""

    mates: dict[Name, Mate] = Field(max_length=64)

    @model_validator(mode="after")
    def _not_with_itself(self) -> Assembly:
        for name, entry in self.mates.items():
            if entry.part == name:
                raise ValueError(f"part {name!r} cannot mate with itself")
        return self

    def parts_named(self) -> tuple[str, ...]:
        named = list(self.mates)
        named += [entry.part for entry in self.mates.values() if entry.part not in named]
        return tuple(named)


def parse_assembly(raw: Any) -> Assembly:
    """Validate an ASSEMBLY value (`{"lid": {"mate": "box.rim"}}`). Raises
    pydantic.ValidationError; the error locations start with `mates`."""
    return Assembly.model_validate({"mates": raw})


AXES = {"+x": (1.0, 0.0, 0.0), "-x": (-1.0, 0.0, 0.0), "+y": (0.0, 1.0, 0.0),
        "-y": (0.0, -1.0, 0.0), "+z": (0.0, 0.0, 1.0), "-z": (0.0, 0.0, -1.0)}


class PrintHint(_Strict):
    """One entry of PRINT: `up` is the direction of the part, as modelled, that should point
    up on the bed: an axis name (`+z` is as modelled, `-z` is upside down) or a vector."""

    up: Literal["+x", "-x", "+y", "-y", "+z", "-z"] | tuple[float, float, float]
    note: str = Field(default="", max_length=400)

    @model_validator(mode="after")
    def _a_direction(self) -> PrintHint:
        if sum(c * c for c in self.vector) < 1e-12:
            raise ValueError("up must not be the zero vector")
        return self

    @property
    def vector(self) -> tuple[float, float, float]:
        """`up` as a vector (not normalised)."""
        return AXES[self.up] if isinstance(self.up, str) else self.up


class PrintHints(_Strict):
    """PRINT of a model script: part name -> how it prefers to be printed."""

    parts: dict[Name, PrintHint] = Field(max_length=64)


def parse_print(raw: Any) -> PrintHints:
    """Validate a PRINT value (`{"lid": {"up": "-z"}}`). Raises pydantic.ValidationError;
    the error locations start with `parts`."""
    return PrintHints.model_validate({"parts": raw})


def describe_errors(exc: Any, limit: int = 4) -> str:
    """A pydantic ValidationError as one sentence that names each bad field."""
    # ASSEMBLY and PRINT are written as bare dicts and validated inside a wrapper field
    # that the script never typed; it is not part of the field's name.
    wrapper = {"Assembly": "mates", "PrintHints": "parts"}.get(getattr(exc, "title", ""))
    problems: dict[str, list[str]] = {}
    for error in exc.errors():
        steps = list(error["loc"])
        if steps and steps[0] == wrapper and len(steps) > 1:
            steps = steps[1:]
        # A step such as `tuple[float, float, float]` names an alternative of a union, not
        # a field: the alternatives of one field are joined under the field's name.
        where = ".".join(str(step) for step in steps if "[" not in str(step))
        problems.setdefault(where or "value", []).append(error["msg"])
    shown = [f"{where}: {' / '.join(messages)}"
             for where, messages in list(problems.items())[:limit]]
    more = len(problems) - limit
    return "; ".join(shown) + (f" (and {more} more)" if more > 0 else "")


def open_questions(contract: Contract, param_specs: dict[str, dict] | None = None) -> list[Question]:
    """What must be asked of the user: every requirement resting on a guessed number.

    `param_specs` is the model's PARAMS (name -> spec with `provenance`); a requirement that
    names a guessed parameter is a question even when its own tag is not `guessed`. A
    requirement naming a parameter the model does not define is reported too, because its
    number cannot be traced.
    """
    specs = param_specs or {}
    questions = []
    for requirement in contract.requirements:
        guessed = tuple(
            name for name in requirement.params
            if specs.get(name, {}).get("provenance") == "guessed"
        )
        unknown = tuple(name for name in requirement.params if param_specs is not None
                        and name not in specs)
        if requirement.provenance != "guessed" and not guessed and not unknown:
            continue
        if isinstance(requirement, Fit):
            text = (
                f"Nobody has measured {requirement.subject()} on this printer yet. Print the "
                "fit gauge and say whether it works, is too tight or too loose."
            )
        elif isinstance(requirement, ReferenceFit):
            text = (
                f"Nobody has tried {requirement.subject()} with the real thing yet. Print "
                "the ring gauge and say whether the thing goes in the way you want, is too "
                "tight or too loose."
            )
        elif guessed or unknown:
            labels = ", ".join(
                str(specs.get(name, {}).get("label", name)) for name in guessed + unknown
            )
            text = (
                f"{_capital(requirement.subject())} depends on a value that was guessed "
                f"({labels}). Please measure it."
            )
        else:
            text = f"{_capital(requirement.subject())} was guessed. Please measure it."
        questions.append(
            Question(requirement=requirement.id, text=text, guessed_params=guessed + unknown)
        )
    return questions


def _capital(text: str) -> str:
    return re.sub(r"^[a-z]", lambda m: m.group(0).upper(), text)
