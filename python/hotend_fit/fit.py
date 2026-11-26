"""The fit resolver: the only source of clearances in a model (docs/PROTOCOL.md section 4.2).

A model never types a gap. It names a fit class and the nominal size of the mating feature,
and the resolver answers with the gap to draw for this printer, material and nozzle:

    drawn gap = printed gap aimed for (the fit class) + hole_offset + peg_offset

so that after the printer has pulled the hole in and pushed the peg out, the printed pair has
the gap the class asks for. All gaps are per side (on a radius, or between two faces).

Guarantees:
- Every clearance handed out is recorded in `resolver.issued`; a declared fit pair whose
  clearance is not one of those records fails the `fit_pairs` check.
- The same context gives the same numbers; nothing here reads the clock or the environment.
- Provenance is never overstated. A clearance carries `offsets_provenance` (`calibrated` only
  when every quantity of the calibration entry was measured) and `target_provenance` (where
  the printed gap it aims for comes from: `guessed` for the class numbers below, `standard`
  for a maker's datasheet). `provenance` is the weaker of the two, so a class fit is
  `guessed` until its gauge has been printed, however well the printer is measured.
- `robustness(clearance)` says what the one drawing the user prints becomes on the tightest
  and on the loosest printer the calibration allows, and whether that still is the fit the
  class promises (the windows below). It never redraws the model.

Fit classes. The printed gap aimed for is `base + per_mm * nominal`, per side, in mm. The
numbers are general FDM practice for a 0.4 mm nozzle, not measurements, so treat them as
guessed until the gauge of the feature has been printed:

| class       | base  | per_mm | floor | where the number comes from                               |
| ----------- | ----- | ------ | ----- | --------------------------------------------------------- |
| press       | -0.05 | 0      | no    | 0.1 mm interference on a diameter holds a printed pin     |
| snug        | +0.05 | 0      | yes   | locating fit, pushed together by hand, no rattle          |
| slide       | +0.15 | 0.001  | yes   | 0.3 mm on a diameter is the usual free sliding fit        |
| loose       | +0.30 | 0.002  | yes   | 0.6 mm on a diameter drops in and has visible play        |
| snap        | -0.30 | 0      | no    | undercut of a snap bead; a few tenths is what a wall of   |
|             |       |        |       | 1.6 to 2 mm deflects without whitening                    |
| thread_tap  |  0    | -0.10  | no    | pilot hole of 0.8 x the screw's major diameter, the usual |
|             |       |        |       | rule for thread-forming screws in stiff thermoplastics    |
| thread_free | +0.15 | 0.02   | yes   | ISO 273 medium clearance holes (M3 3.4, M4 4.5, M5 5.5)   |

`floor` means the printed gap is never aimed below the printer's `min_clearance`.

Acceptance windows. A printed pair still is its class while its printed gap, per side, stays
inside the window; outside it the fit binds, rattles out or overstresses the part. Like the
targets these are general FDM practice, tagged `guessed`:

| class       | printed gap from      | to                    | outside the window                  |
| ----------- | --------------------- | --------------------- | ----------------------------------- |
| press       | -0.15                 | -0.01                 | splits the hole / falls out         |
| snug        |  0                    | (no limit)            | cannot be pushed together by hand   |
| slide       | +0.05                 | (no limit)            | binds                               |
| loose       | +0.15                 | (no limit)            | no longer drops in                  |
| snap        | -0.45                 | -0.15                 | bead overstressed / does not hold   |
| thread_tap  | -0.15 x nominal       | -0.05 x nominal       | boss splits / thread strips         |
| thread_free | +0.05                 | (no limit)            | the screw does not pass             |
| (datasheet) | datasheet gap - 0.10  | datasheet gap + 0.10  | outside the maker's hole tolerance  |

Reference fit classes. Two more classes are for a printed part that holds a real thing (a
reference object or a bought device, `hotend_fit.reference`), not for two printed halves,
so they live in `REFERENCE_FIT_CLASSES` and the feature cards do not offer them. The thing
is not printed by this printer, so only the printed side is compensated
(`printed_peg=False`). Both are guesses until the pocket's gauge has been printed:

| class   | base  | per_mm | floor | window (printed gap) | where the number comes from              |
| ------- | ----- | ------ | ----- | -------------------- | ---------------------------------------- |
| stretch | -0.05 | 0      | no    | -0.40 to +0.10       | a flexible shell that grips a rigid      |
|         |       |        |       |                      | thing: see STRETCH_SOURCE below          |
| cradle  | +1.50 | 0.01   | yes   | +0.50 and up         | a soft thing dropped into a rigid        |
|         |       |        |       |                      | holder: see CRADLE_SOURCE below          |

`stretch` is for flexible materials only (`FitClassSpec.flexible_only`): the resolver hands
the clearance out for any material, and the `reference_fit` check fails a stretch fit in a
rigid one with a plain explanation, so the result says what is wrong instead of the build
dying on an exception.
"""

from __future__ import annotations

import math
from dataclasses import asdict, dataclass
from typing import Any, Literal

from .calibration import (
    DEFAULT_K,
    QUANTITIES,
    CalibrationEntry,
    Extreme,
    entry_for,
)

FitClass = Literal["press", "snug", "slide", "loose", "snap", "thread_tap", "thread_free",
                   "stretch", "cradle"]
Axis = Literal["xy", "z"]


@dataclass(frozen=True)
class FitClassSpec:
    """The printed gap a fit class aims for: `base + per_mm * nominal`, per side, in mm."""

    base: float
    per_mm: float
    floor: bool
    meaning: str
    # The printed gap must stay inside [low, high]: each bound is (base, per_mm of nominal),
    # or None for no limit.
    low: tuple[float, float] | None = None
    high: tuple[float, float] | None = None
    too_tight: str = "binds"
    too_loose: str = "is too loose"
    provenance: str = "guessed"
    # Only a flexible material (TPU) can be this fit: the printed part is stretched on.
    flexible_only: bool = False

    def target(self, nominal: float, min_clearance: float) -> float:
        gap = self.base + self.per_mm * nominal
        return max(gap, min_clearance) if self.floor else gap

    def window(self, nominal: float) -> tuple[float | None, float | None]:
        """(lowest, highest) printed gap per side that still is this class."""
        low = None if self.low is None else self.low[0] + self.low[1] * nominal
        high = None if self.high is None else self.high[0] + self.high[1] * nominal
        return low, high


FIT_CLASSES: dict[str, FitClassSpec] = {
    "press": FitClassSpec(-0.05, 0.0, False, "held by interference, pressed in once",
                          (-0.15, 0.0), (-0.01, 0.0),
                          "would split the hole or not go in", "would not hold"),
    "snug": FitClassSpec(0.05, 0.0, True, "locates without rattle, assembled by hand",
                         (0.0, 0.0), None, "could not be pushed together by hand"),
    "slide": FitClassSpec(0.15, 0.001, True, "slides freely, little play",
                          (0.05, 0.0), None, "would bind"),
    "loose": FitClassSpec(0.30, 0.002, True, "drops in, visible play",
                          (0.15, 0.0), None, "would no longer drop in"),
    "snap": FitClassSpec(-0.30, 0.0, False, "undercut that a snap bead rides over",
                         (-0.45, 0.0), (-0.15, 0.0),
                         "would overstress the snap bead", "would not hold the snap"),
    "thread_tap": FitClassSpec(0.0, -0.10, False, "pilot hole a screw cuts its own thread in",
                               (0.0, -0.15), (0.0, -0.05),
                               "could split the boss", "would let the thread strip"),
    "thread_free": FitClassSpec(0.15, 0.02, True, "hole a screw passes through freely",
                                (0.05, 0.0), None, "would not let the screw pass"),
}
# STRETCH_SOURCE. A flexible (TPU) shell around a rigid thing, such as a phone case. No source
# gives a printed interference: published cases are drawn with +0.10 mm per side (a designer
# with printed results, https://raw.githubusercontent.com/peterdsharpe/phone-case/main/README.md)
# to +0.2..0.3 mm for a fit that wraps past the mid-line
# (https://qidi3d.com/blogs/print-lab/tpu-phone-cases), and the one interference figure is
# generic, 0.2 to 0.3 mm for TPU press fits (https://blog.uavmodel.com/tpu-3d-printing-guide-
# flexible-filament-settings-extruder-mods-and-fpv-part-design-2026/). A drawn +0.10 mm less
# the default hole offset of 0.08 mm prints at about +0.02 mm, line to line, and "TPU loosens
# a little with use", so the class aims slightly under: 0.05 mm of printed interference per
# side. The window runs from the generic press figure plus margin (-0.40: cannot be put on,
# buttons are squeezed) to the loosest drawn clearance the sources call a friction fit less
# the offset (+0.10: the thing slides out; Qidi: "0.6 mm and up falls off").
#
# CRADLE_SOURCE. A soft thing (a squishy TPU figure) dropped into a rigid holder. It is not a
# calibrated mating part: its size changes with walls, infill and a squeeze, so it gets a
# visible gap, not a fit clearance. No source gives a number: 1.5 mm plus 1 % of the thing's
# size (2.3 mm at 80 mm) per side is Hotend's choice, and anything under 0.5 mm counts as
# squeezing it.
REFERENCE_FIT_CLASSES: dict[str, FitClassSpec] = {
    "stretch": FitClassSpec(-0.05, 0.0, False,
                            "a flexible shell stretched over a rigid thing and gripping it",
                            (-0.40, 0.0), (0.10, 0.0),
                            "could not be put on without overstretching",
                            "would let the thing slide out", flexible_only=True),
    "cradle": FitClassSpec(1.50, 0.01, True,
                           "a soft thing dropped into a rigid holder with visible room",
                           (0.50, 0.0), None, "would squeeze the thing"),
}
# Every class the resolver answers: the printed-pair classes and the reference classes.
ALL_FIT_CLASSES: dict[str, FitClassSpec] = {**FIT_CLASSES, **REFERENCE_FIT_CLASSES}
# A hole drawn to a maker's datasheet must print within this of the datasheet gap, per side.
DATASHEET_WINDOW_MM = 0.10
ROBUSTNESS_TOLERANCE_MM = 1e-9
EXTREMES = ("tight", "loose")
_SUPERLATIVE = {"tight": "tightest", "loose": "loosest"}

MAX_NOMINAL_MM = 1000.0
MAX_REPORTED_LOOSE = 256
# A datasheet may ask for at most this much interference or play per side.
MAX_DATASHEET_GAP_MM = 2.0


class FitError(ValueError):
    """A clearance was asked for in a way the resolver cannot answer."""


@dataclass(frozen=True, eq=False)
class Clearance:
    """One resolved clearance. Compared by identity: each request is its own record.

    `gap` is what to draw between the two mating surfaces, per side; negative is interference.
    `hole_delta` is added to the hole's radius (or moves an inside face outwards) and
    `peg_delta` is taken off the peg's radius; `gap == hole_delta + peg_delta`.
    """

    fit_class: str
    nominal: float
    axis: str
    gap: float
    hole_delta: float
    peg_delta: float
    target: float
    target_source: str
    printed_hole: bool
    printed_peg: bool
    provenance: str
    extreme: str
    index: int
    # `provenance` is the weaker of these two: the calibration entry's and the target's.
    offsets_provenance: str = "guessed"
    target_provenance: str = "guessed"
    # The quantities of the calibration entry that were measured on this printer, in the
    # order of `calibration.QUANTITIES`; empty for the guessed defaults.
    measured: tuple[str, ...] = ()

    @property
    def diametral(self) -> float:
        """The gap on a diameter (both sides)."""
        return 2.0 * self.gap

    def to_json(self) -> dict:
        return asdict(self)


class FitResolver:
    """`ctx.fit`: resolves fit classes against one calibration entry and keeps the record."""

    def __init__(
        self,
        calibration: CalibrationEntry,
        *,
        extreme: Extreme = "nominal",
        k: float = DEFAULT_K,
        source: str = "",
        material_family: str = "",
        material_id: str = "",
    ) -> None:
        self.base_calibration = calibration
        self.calibration = calibration.at_extreme(extreme, k)
        self.extreme = extreme
        self.k = k
        self.source = source
        # The request's material family (`PLA`, `PETG`, ...; empty when unknown); cards that
        # depend on what the material survives read it.
        self.material_family = material_family.strip().upper()
        self.material_id = material_id.strip()
        # Real things the model may fit a part to, by name (`hotend_fit.reference`): the
        # engine puts the request's reference objects here, bought devices are added when
        # the model asks for them. Pockets register themselves like cards do.
        self.references: dict = {}
        self.components: dict = {}
        self.pockets: list = []
        self.issued: list[Clearance] = []
        # Feature cards register themselves and their mating pairs here during the build.
        self.cards: list = []
        self.declared_fits: list = []
        # Set by `restore`: what another process's build declared that could not be rebuilt,
        # and the clearances it reported taking without a card.
        self.restored = False
        self.restore_problems: list[str] = []
        self.reported_loose: list[dict] = []

    @property
    def provenance(self) -> str:
        return self.base_calibration.provenance

    @property
    def measured_quantities(self) -> tuple[str, ...]:
        """The quantities of the calibration entry measured on this printer."""
        return tuple(
            name for name in QUANTITIES
            if getattr(self.base_calibration, name).provenance in ("measured", "calibrated")
        )

    @property
    def material(self) -> Any:
        """What is known about the request's material (`hotend_fit.materials`)."""
        from .materials import material_facts

        return material_facts(self.material_id, self.material_family)

    def clearance(
        self,
        fit_class: str,
        nominal: float,
        *,
        axis: Axis = "xy",
        printed_hole: bool = True,
        printed_peg: bool = True,
        datasheet_gap: float | None = None,
    ) -> Clearance:
        """The clearance to draw for a mating pair of the given class.

        `nominal` is the nominal size of the mating feature in mm (a diameter, the width of an
        opening, the length of an engagement). `axis="z"` is for gaps along the build
        direction, where the hole and peg offsets do not apply. `printed_hole=False` or
        `printed_peg=False` says that side is a bought part (a screw, an insert, a magnet)
        that needs no compensation. `datasheet_gap` replaces the class's own target with the
        per-side gap a part's maker specifies (a heat-set insert's hole); it is for feature
        cards that read it from a standards table, never for a number typed into a model.
        """
        if fit_class not in ALL_FIT_CLASSES:
            raise FitError(
                f"unknown fit class {fit_class!r}; choose from {', '.join(ALL_FIT_CLASSES)}"
            )
        if axis not in ("xy", "z"):
            raise FitError(f"axis must be 'xy' or 'z', got {axis!r}")
        if not _is_number(nominal) or not 0 < nominal <= MAX_NOMINAL_MM:
            raise FitError(
                f"nominal must be a size in mm between 0 and {MAX_NOMINAL_MM:g}, got {nominal!r}"
            )
        if not printed_hole and not printed_peg:
            raise FitError("a fit between two bought parts has nothing to resolve")
        calibration = self.calibration
        spec = ALL_FIT_CLASSES[fit_class]
        if datasheet_gap is None:
            target = spec.target(float(nominal), calibration.min_clearance.value)
            target_source = "class"
        else:
            if not _is_number(datasheet_gap) or abs(datasheet_gap) > MAX_DATASHEET_GAP_MM:
                raise FitError(
                    f"datasheet_gap must be within +/- {MAX_DATASHEET_GAP_MM:g} mm per side, "
                    f"got {datasheet_gap!r}"
                )
            target = float(datasheet_gap)
            target_source = "datasheet"
        # Shrinkage moves both sides of a printed pair alike, so it cancels in their gap; it
        # only shows against a bought part, whose size does not shrink.
        if axis == "xy":
            shrink = calibration.xy_shrink.value * nominal / 2.0
            hole_offset = calibration.hole_offset.value
            peg_offset = calibration.peg_offset.value
        else:
            shrink = calibration.z_shrink.value * nominal / 2.0
            hole_offset = peg_offset = 0.0
        hole_delta = target + (hole_offset + shrink if printed_hole else 0.0)
        peg_delta = peg_offset - shrink if printed_peg else 0.0
        target_provenance = spec.provenance if datasheet_gap is None else "standard"
        record = Clearance(
            fit_class=fit_class,
            nominal=float(nominal),
            axis=axis,
            gap=hole_delta + peg_delta,
            hole_delta=hole_delta,
            peg_delta=peg_delta,
            target=target,
            target_source=target_source,
            printed_hole=printed_hole,
            printed_peg=printed_peg,
            provenance=(
                "guessed" if "guessed" in (self.provenance, target_provenance) else "calibrated"
            ),
            extreme=self.extreme,
            index=len(self.issued),
            offsets_provenance=self.provenance,
            target_provenance=target_provenance,
            measured=self.measured_quantities,
        )
        self.issued.append(record)
        return record

    def printed_gap(self, clearance: Clearance, extreme: Extreme) -> float:
        """The gap per side that `clearance`, drawn as it is, prints with on the tightest or
        loosest printer the calibration allows (`nominal`: on the printer as measured).

        A printed hole wall moves inwards by the hole offset and a printed peg surface
        outwards by the peg offset. Shrinkage moves both sides of a printed pair alike and so
        only shows against a bought part. Along Z there are no offsets, only shrinkage.
        """
        entry = self.base_calibration.at_extreme(extreme, self.k)
        if clearance.axis == "xy":
            shrink = entry.xy_shrink.value * clearance.nominal / 2.0
            hole_offset, peg_offset = entry.hole_offset.value, entry.peg_offset.value
        else:
            shrink = entry.z_shrink.value * clearance.nominal / 2.0
            hole_offset = peg_offset = 0.0
        gap = clearance.gap
        if clearance.printed_hole:
            gap -= hole_offset + shrink
        if clearance.printed_peg:
            gap -= peg_offset - shrink
        return gap

    def robustness(self, clearance: Clearance) -> dict:
        """Whether the pair drawn with `clearance` still is its fit class on the tightest and
        on the loosest printer: `ok`, the printed gap per extreme, the acceptance `window`
        and, when not ok, a `reason`. The drawing is never changed; see the module doc."""
        spec = ALL_FIT_CLASSES[clearance.fit_class]
        if clearance.target_source == "datasheet":
            low = clearance.target - DATASHEET_WINDOW_MM
            high = clearance.target + DATASHEET_WINDOW_MM
            too_tight, too_loose = "is under the maker's hole size", "is over the maker's hole size"
        else:
            low, high = spec.window(clearance.nominal)
            too_tight, too_loose = spec.too_tight, spec.too_loose
        printed = {extreme: self.printed_gap(clearance, extreme) for extreme in EXTREMES}
        problems = []
        for extreme, gap in printed.items():
            if low is not None and gap < low - ROBUSTNESS_TOLERANCE_MM:
                problems.append(
                    f"on the {_SUPERLATIVE[extreme]} printer the calibration allows it "
                    f"prints with {gap:+.3f} mm per side, below {low:+.3f} mm: it {too_tight}"
                )
            elif high is not None and gap > high + ROBUSTNESS_TOLERANCE_MM:
                problems.append(
                    f"on the {_SUPERLATIVE[extreme]} printer the calibration allows it "
                    f"prints with {gap:+.3f} mm per side, above {high:+.3f} mm: it {too_loose}"
                )
        return {
            "ok": not problems,
            "fit_class": clearance.fit_class,
            "nominal": clearance.nominal,
            "axis": clearance.axis,
            "drawn": clearance.gap,
            "target": clearance.target,
            "printed": printed,
            "window": [low, high],
            "k": self.k,
            "offsets_provenance": self.provenance,
            "target_provenance": clearance.target_provenance,
            "reason": "; ".join(problems),
        }

    def declarations(self) -> dict:
        """What this build declared, as plain JSON data for a process that verifies it
        without having run the model: the cards (`Card.declaration()`) and the clearances
        that were taken without a card."""
        covered = self.covered_clearances()
        declared = {
            "cards": [card.declaration() for card in self.cards],
            "loose": [
                {"fit_class": record.fit_class, "nominal": record.nominal, "axis": record.axis}
                for record in self.issued if id(record) not in covered
            ][:MAX_REPORTED_LOOSE],
        }
        # Present only when the model used them, so a model without references declares
        # exactly what it always did.
        if self.pockets:
            declared["pockets"] = [pocket.declaration() for pocket in self.pockets]
        if self.components:
            declared["components"] = [
                {"id": name, "features": sorted(component.used_features)}
                for name, component in self.components.items()
            ]
        return declared

    def covered_clearances(self) -> set[int]:
        """The ids of the clearance records a feature card or a pocket was built with: what
        is left over was taken from the resolver by hand and nothing verifies it."""
        covered = {id(getattr(pair, "clearance", None)) for pair in self.declared_fits}
        covered |= {id(pocket.clearance) for pocket in self.pockets}
        covered |= {id(record) for component in self.components.values()
                    for record in component.clearances}
        return covered

    def restore(self, declarations: Any) -> None:
        """Fill a fresh resolver from `declarations()` data of another process, as if the
        cards had been applied here. The data is untrusted: cards are rebuilt from their
        definitions (`cards.restore_cards`), and whatever cannot be rebuilt is kept in
        `restore_problems` for the `fit_pairs` check to fail on."""
        from .cards import restore_cards

        self.restored = True
        if (not isinstance(declarations, dict)
                or not {"cards", "loose"} <= set(declarations)
                <= {"cards", "loose", "pockets", "components"}):
            self.restore_problems.append("the declared fits are malformed")
            return
        self.restore_problems += restore_cards(self, declarations["cards"])
        if "components" in declarations or "pockets" in declarations:
            from .reference import restore_components, restore_pockets

            # Components first: a pocket may be made for one.
            self.restore_problems += restore_components(self, declarations.get("components", []))
            self.restore_problems += restore_pockets(self, declarations.get("pockets", []))
        loose = declarations["loose"]
        if not isinstance(loose, list):
            self.restore_problems.append("the list of clearances taken without a card is malformed")
            return
        for raw in loose[:MAX_REPORTED_LOOSE]:
            known = (isinstance(raw, dict) and set(raw) == {"fit_class", "nominal", "axis"}
                     and raw["fit_class"] in ALL_FIT_CLASSES and raw["axis"] in ("xy", "z")
                     and _is_number(raw["nominal"]))
            self.reported_loose.append(
                {"fit_class": raw["fit_class"], "nominal": float(raw["nominal"]),
                 "axis": raw["axis"]} if known else {"fit_class": "unknown"}
            )

    def was_issued(self, clearance: Any) -> bool:
        """True when `clearance` is a record this resolver handed out (by identity)."""
        return any(record is clearance for record in self.issued)

    def summary(self) -> dict:
        """What the resolver used and handed out, for reports."""
        return {
            "source": self.source,
            "provenance": self.provenance,
            "extreme": self.extreme,
            "k": self.k,
            "material": self.calibration.material,
            "nozzle_id": self.calibration.nozzle_id,
            "issued": [record.to_json() for record in self.issued],
        }


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _main_nozzle(printer: dict) -> dict:
    nozzles = [n for n in printer.get("nozzles") or () if isinstance(n, dict)]
    for nozzle in nozzles:
        if nozzle.get("role") == "main":
            return nozzle
    return nozzles[0] if nozzles else {}


def resolver_for(context: dict | None = None) -> FitResolver:
    """The resolver for one build, from the request's `context` object as a dict.

    Reads `printer` (id and main nozzle), `material` (id, family) and `calibration_path`
    (absolute, or None). Two optional keys: `fit_k` (how many sigmas the material extremes
    sit from the measured value, default 2) and `fit_extreme` (`nominal`, `tight` or `loose`:
    draw the model compensated for that extreme printer instead of the measured one, for
    experiments; the checks and the range sampler never do). A fresh resolver per call.
    """
    context = context or {}
    printer = context.get("printer") or {}
    material = context.get("material") or {}
    nozzle = _main_nozzle(printer)
    extreme = context.get("fit_extreme") or "nominal"
    if extreme not in ("nominal", "tight", "loose"):
        raise FitError(f"context.fit_extreme must be nominal, tight or loose, got {extreme!r}")
    k = context.get("fit_k", DEFAULT_K)
    if not _is_number(k) or k < 0:
        raise FitError(f"context.fit_k must be a non-negative number, got {k!r}")
    entry, source = entry_for(
        context.get("calibration_path"),
        printer_id=printer.get("id"),
        material=material.get("id"),
        nozzle_id=nozzle.get("id"),
        nozzle_diameter=nozzle.get("diameter"),
    )
    family, identifier = material.get("family"), material.get("id")
    return FitResolver(
        entry, extreme=extreme, k=float(k), source=source,
        material_family=family if isinstance(family, str) else "",
        material_id=identifier if isinstance(identifier, str) else "",
    )


def resolver_of(ctx: Any) -> FitResolver:
    """The resolver behind a BuildContext (`ctx.fit`), or `ctx` itself when it is one."""
    resolver = ctx if isinstance(ctx, FitResolver) else getattr(ctx, "fit", None)
    if not isinstance(resolver, FitResolver):
        raise FitError(
            "a fit needs the build context: pass ctx (the second argument of build) so the "
            "clearance can come from the printer's calibration"
        )
    return resolver


def clearance(fit_class: str, nominal: float, ctx: Any, **options: Any) -> Clearance:
    """`fit.clearance(fit_class, nominal, ctx)`: the same as `ctx.fit.clearance(...)`."""
    return resolver_of(ctx).clearance(fit_class, nominal, **options)
