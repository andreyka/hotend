"""The calibration table: what this printer does to a designed dimension.

One file per printer (docs/PROTOCOL.md section 4.2), one entry per material and nozzle:

    {"protocol": 0, "printer_id": "bambu-x2d", "entries": [
      {"material": "pla-basic", "nozzle_id": 1, "nozzle_diameter": 0.4,
       "hole_offset":   {"value": 0.07, "sigma": 0.02, "provenance": "calibrated",
                         "measured_at": "2026-11-02T10:00:00Z"},
       "peg_offset": {...}, "xy_shrink": {...}, "z_shrink": {...}, "min_clearance": {...}}]}

Quantities (lengths in mm per side, that is on a radius or on one face, never on a diameter):
- `hole_offset`: how far a printed inside surface (a hole wall, a slot wall) sits inwards of
  where it was drawn; positive when holes print small. (drawn diameter - measured) / 2.
- `peg_offset`: how far a printed outside surface sits outwards of where it was drawn; positive
  when pegs print fat. (measured diameter - drawn) / 2.
- `xy_shrink`, `z_shrink`: linear shrinkage as a ratio; measured = drawn * (1 - shrink).
- `min_clearance`: the smallest printed gap between two separately printed surfaces that still
  assembles on this printer; clearance fits are never aimed below it.

Guarantees:
- Loading is strict: unknown keys, numbers written as strings, non-finite or implausible
  numbers, a measured value without `measured_at`, duplicate entries and a table for another
  printer all raise CalibrationError. Nothing is repaired or guessed from a malformed file.
- A missing table, a table without an entry for this material and nozzle, or an entry that
  was measured with another nozzle diameter than the one fitted now, gives the documented
  defaults below with provenance `guessed`, and the reason is kept as a sentence.
- `calibrate_from_gauge` turns caliper readings of a printed gauge into an updated table. It
  reads no clock: the caller passes the date of the measurement. The values it writes are
  tagged `measured`; quantities the gauge does not measure keep what they had.
- `extremes` returns the tightest and the loosest printer the table allows (every quantity at
  value + k sigma and at value - k sigma); the fit resolver evaluates every clearance it
  hands out against both (`FitResolver.robustness`).
"""

from __future__ import annotations

import math
from datetime import datetime
from pathlib import Path
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError, model_validator

from . import PROTOCOL_VERSION

Provenance = Literal["measured", "standard", "calibrated", "guessed", "style"]
Extreme = Literal["nominal", "tight", "loose"]

QUANTITIES = ("hole_offset", "peg_offset", "xy_shrink", "z_shrink", "min_clearance")
MAX_TABLE_BYTES = 256 * 1024
# How many sigmas the material extremes sit from the measured value.
DEFAULT_K = 2.0
# The defaults below are written for this nozzle and scaled linearly for others.
REFERENCE_NOZZLE_MM = 0.4
# An entry applies to the fitted nozzle only when their diameters agree to this (mm).
NOZZLE_DIAMETER_TOLERANCE_MM = 1e-6

# Plausibility limits (low, high) per quantity; a value outside is a broken table, not a printer.
_LIMITS = {
    "hole_offset": (-1.0, 1.0),
    "peg_offset": (-1.0, 1.0),
    "xy_shrink": (-0.05, 0.05),
    "z_shrink": (-0.05, 0.05),
    "min_clearance": (0.0, 1.0),
}

# Conservative defaults for an uncalibrated printer with a 0.4 mm nozzle: (value, sigma,
# scales with the nozzle). They are general FDM practice, not measurements of any machine, so
# every one is tagged `guessed`:
# - hole_offset 0.08 +/- 0.04: small printed holes usually come out 0.1 to 0.2 mm under their
#   drawn diameter (perimeters pull inwards on a tight curve).
# - peg_offset 0.03 +/- 0.04: outside dimensions on a tuned printer are within about
#   +/- 0.1 mm on a diameter, biased slightly fat.
# - xy_shrink, z_shrink 0.003 +/- 0.002: PLA and PETG shrink roughly 0.2 to 0.5 % linearly.
# - min_clearance 0.10 +/- 0.03: about a quarter of the nozzle width is the smallest gap that
#   separately printed parts reliably assemble with.
_DEFAULTS = {
    "hole_offset": (0.08, 0.04, True),
    "peg_offset": (0.03, 0.04, True),
    "xy_shrink": (0.003, 0.002, False),
    "z_shrink": (0.003, 0.002, False),
    "min_clearance": (0.10, 0.03, True),
}


class CalibrationError(ValueError):
    """The calibration table cannot be used. The message says what to fix in the file."""


class _Strict(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True, allow_inf_nan=False)


class Quantity(_Strict):
    """One calibrated number with its spread and where it came from."""

    value: float
    sigma: float = Field(ge=0)
    provenance: Provenance
    measured_at: datetime | None = None

    @model_validator(mode="after")
    def _measured_values_are_dated(self) -> Quantity:
        if self.provenance in ("measured", "calibrated") and self.measured_at is None:
            raise ValueError(f"a {self.provenance} value needs measured_at")
        return self


class CalibrationEntry(_Strict):
    """The calibration of one material on one nozzle."""

    material: str = Field(min_length=1, max_length=64)
    nozzle_id: int
    nozzle_diameter: float = Field(gt=0, le=2.0)
    hole_offset: Quantity
    peg_offset: Quantity
    xy_shrink: Quantity
    z_shrink: Quantity
    min_clearance: Quantity

    @model_validator(mode="after")
    def _plausible(self) -> CalibrationEntry:
        for name in QUANTITIES:
            quantity: Quantity = getattr(self, name)
            low, high = _LIMITS[name]
            if not low <= quantity.value <= high:
                raise ValueError(f"{name}.value {quantity.value} is outside [{low}, {high}]")
            if quantity.sigma > high - low:
                raise ValueError(f"{name}.sigma {quantity.sigma} is implausibly large")
        return self

    @property
    def provenance(self) -> str:
        """`calibrated` only when every quantity was measured, otherwise `guessed`."""
        measured = all(
            getattr(self, name).provenance in ("measured", "calibrated") for name in QUANTITIES
        )
        return "calibrated" if measured else "guessed"

    def at_extreme(self, extreme: Extreme, k: float = DEFAULT_K) -> CalibrationEntry:
        """This entry as the tightest (`value + k sigma`) or loosest (`value - k sigma`)
        printer it allows, each value kept inside its plausible range. `nominal` returns the
        entry unchanged."""
        if extreme == "nominal":
            return self
        if extreme not in ("tight", "loose"):
            raise ValueError(f"extreme must be nominal, tight or loose, got {extreme!r}")
        if isinstance(k, bool) or not isinstance(k, (int, float)) or not math.isfinite(k) or k < 0:
            raise ValueError(f"k must be a non-negative number, got {k!r}")
        sign = 1.0 if extreme == "tight" else -1.0
        shifted: dict[str, Any] = {}
        for name in QUANTITIES:
            quantity: Quantity = getattr(self, name)
            low, high = _LIMITS[name]
            value = min(high, max(low, quantity.value + sign * k * quantity.sigma))
            shifted[name] = quantity.model_copy(update={"value": value})
        return self.model_copy(update=shifted)


class CalibrationTable(_Strict):
    protocol: Literal[0] = PROTOCOL_VERSION
    printer_id: str = Field(min_length=1, max_length=64)
    entries: tuple[CalibrationEntry, ...] = ()

    @model_validator(mode="after")
    def _no_duplicates(self) -> CalibrationTable:
        seen = set()
        for entry in self.entries:
            key = (entry.material, entry.nozzle_id)
            if key in seen:
                raise ValueError(
                    f"two entries for material {entry.material!r} on nozzle {entry.nozzle_id}"
                )
            seen.add(key)
        return self

    def find(self, material: str | None, nozzle_id: int | None) -> CalibrationEntry | None:
        """The entry for a material on a nozzle position, whatever nozzle was fitted when it
        was measured; `entry_for` checks the diameter."""
        for entry in self.entries:
            if entry.material == material and entry.nozzle_id == nozzle_id:
                return entry
        return None


def default_entry(
    material: str | None = None, nozzle_id: int | None = None, nozzle_diameter: float | None = None
) -> CalibrationEntry:
    """The guessed calibration used until the printer has been measured.

    Offsets, their spreads and the minimum clearance scale linearly with the nozzle diameter
    (the defaults are written for 0.4 mm); shrinkage does not depend on the nozzle.
    """
    diameter = nozzle_diameter or REFERENCE_NOZZLE_MM
    scale = diameter / REFERENCE_NOZZLE_MM
    quantities = {
        name: Quantity(
            value=value * (scale if scaled else 1.0),
            sigma=sigma * (scale if scaled else 1.0),
            provenance="guessed",
        )
        for name, (value, sigma, scaled) in _DEFAULTS.items()
    }
    return CalibrationEntry(
        material=material or "unknown",
        nozzle_id=0 if nozzle_id is None else nozzle_id,
        nozzle_diameter=diameter,
        **quantities,
    )


def load_table(path: str | Path) -> CalibrationTable:
    """Read and validate a calibration file. Raises CalibrationError; never returns a
    partly valid table."""
    path = Path(path)
    try:
        if path.stat().st_size > MAX_TABLE_BYTES:
            raise CalibrationError(
                f"calibration table {path.name} is larger than {MAX_TABLE_BYTES} bytes"
            )
        raw = path.read_bytes()
    except OSError as exc:
        raise CalibrationError(f"cannot read calibration table {path.name}: {exc}") from exc
    try:
        # Strict JSON mode: a number written as a string, or true for 1, is rejected.
        return CalibrationTable.model_validate_json(raw, strict=True)
    except ValidationError as exc:
        problems = "; ".join(
            f"{'.'.join(str(p) for p in e['loc']) or 'table'}: {e['msg']}"
            for e in exc.errors()[:8]
        )
        raise CalibrationError(
            f"calibration table {path.name} is not valid: {problems}. Re-run the calibration "
            "or remove the file to fall back to the guessed defaults."
        ) from exc


def entry_for(
    path: str | Path | None,
    *,
    printer_id: str | None = None,
    material: str | None = None,
    nozzle_id: int | None = None,
    nozzle_diameter: float | None = None,
) -> tuple[CalibrationEntry, str]:
    """The calibration to use for one build, and a sentence saying where it came from.

    Falls back to the guessed defaults when there is no table or no entry for this material
    and nozzle. A table that exists but is malformed, or belongs to another printer, raises.
    """
    fallback = default_entry(material, nozzle_id, nozzle_diameter)
    if path is None or not Path(path).exists():
        return fallback, "no calibration table for this printer: guessed defaults"
    table = load_table(path)
    if printer_id is not None and table.printer_id != printer_id:
        raise CalibrationError(
            f"calibration table {Path(path).name} is for printer {table.printer_id!r}, "
            f"but this build is for {printer_id!r}"
        )
    entry = table.find(material, nozzle_id)
    if entry is None:
        return fallback, (
            f"the calibration table has no entry for material {material!r} on nozzle "
            f"{nozzle_id}: guessed defaults"
        )
    # Offsets measured with one nozzle size say nothing about another: after a nozzle swap
    # the old entry must not be handed out as this printer's calibration.
    if (nozzle_diameter is not None
            and abs(entry.nozzle_diameter - nozzle_diameter) > NOZZLE_DIAMETER_TOLERANCE_MM):
        return fallback, (
            f"the table's entry for material {material!r} on nozzle {nozzle_id} was measured "
            f"with a {entry.nozzle_diameter:g} mm nozzle, but a {nozzle_diameter:g} mm nozzle "
            "is fitted: guessed defaults"
        )
    return entry, f"calibration table of printer {table.printer_id}"


# The spread written for an offset measured on a gauge (mm per side). One reading says
# where this print landed, not how much the printer varies, so it keeps the spread of the
# guessed default; two or more readings give their own standard deviation, never less than
# what a caliper resolves on a printed surface.
SINGLE_READING_SIGMA_MM = 0.04
MIN_READING_SIGMA_MM = 0.02
MAX_READINGS = 64
_FEATURE_QUANTITY = {"hole": "hole_offset", "peg": "peg_offset"}


def gauge_offset(feature: str, drawn: float, measured: float) -> float:
    """The offset per side one caliper reading shows: for a `hole` (an inside size)
    (drawn - measured) / 2, for a `peg` (an outside size) (measured - drawn) / 2."""
    if feature not in _FEATURE_QUANTITY:
        raise CalibrationError(f"a gauge reading is of a 'hole' or a 'peg', got {feature!r}")
    for name, value in (("drawn", drawn), ("measured", measured)):
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or value <= 0):
            raise CalibrationError(f"a gauge reading's {name} size must be a positive number "
                                   f"of millimetres, got {value!r}")
    return (drawn - measured) / 2.0 if feature == "hole" else (measured - drawn) / 2.0


def calibrate_from_gauge(
    path: str | Path | None,
    readings: Any,
    *,
    measured_at: datetime | str,
    printer_id: str,
    material: str,
    nozzle_id: int,
    nozzle_diameter: float,
) -> tuple[CalibrationTable, list[dict]]:
    """The calibration table after a printed gauge was measured, and what changed.

    `readings` are caliper readings of the gauge's dimensions (`Card.gauge_dimensions`):
    dicts with `feature` (`hole` or `peg`), `drawn` and `measured` in mm. Holes give the
    table's `hole_offset`, pegs its `peg_offset`: the mean of the readings, tagged `measured`
    at `measured_at`. The entry for this material and nozzle is replaced (or added); its
    other quantities, and every other entry of the table at `path`, are kept. An entry that
    was measured with another nozzle diameter is started again from the guessed defaults.

    Raises CalibrationError when the table at `path` is unusable, when there are no
    readings, or when a reading is implausible (an offset beyond 1 mm). Returns the new
    table and one record per changed quantity (`quantity`, `before`, `after`, `sigma`,
    `readings`).
    """
    readings = list(readings)
    if not 1 <= len(readings) <= MAX_READINGS:
        raise CalibrationError(f"a calibration needs 1 to {MAX_READINGS} gauge readings")
    try:
        when = Quantity(value=0.0, sigma=0.0, provenance="measured",
                        measured_at=measured_at).measured_at
    except ValidationError as exc:
        raise CalibrationError(
            "measured_at must be the date and time of the measurement (ISO 8601), given by "
            f"the caller: {exc.errors()[0]['msg']}"
        ) from exc
    offsets: dict[str, list[float]] = {}
    for reading in readings:
        if not isinstance(reading, dict) or set(reading) != {"feature", "drawn", "measured"}:
            raise CalibrationError(
                "a gauge reading is {feature, drawn, measured}, got " + repr(reading)[:120])
        offset = gauge_offset(reading["feature"], reading["drawn"], reading["measured"])
        offsets.setdefault(_FEATURE_QUANTITY[reading["feature"]], []).append(offset)

    if path is not None and Path(path).exists():
        table = load_table(path)
        if table.printer_id != printer_id:
            raise CalibrationError(
                f"calibration table {Path(path).name} is for printer {table.printer_id!r}, "
                f"but the gauge was printed on {printer_id!r}"
            )
    else:
        table = CalibrationTable(printer_id=printer_id)
    entry = table.find(material, nozzle_id)
    if entry is None or abs(entry.nozzle_diameter - nozzle_diameter) > NOZZLE_DIAMETER_TOLERANCE_MM:
        entry = default_entry(material, nozzle_id, nozzle_diameter)
    updated: dict[str, Quantity] = {}
    changes = []
    for name, values in offsets.items():
        # Rounded to a nanometre: the table holds no binary-fraction noise.
        mean = round(sum(values) / len(values), 6)
        if len(values) > 1:
            spread = math.sqrt(sum((v - mean) ** 2 for v in values) / (len(values) - 1))
            sigma = round(max(spread, MIN_READING_SIGMA_MM), 6)
        else:
            sigma = SINGLE_READING_SIGMA_MM
        updated[name] = Quantity(value=mean, sigma=sigma, provenance="measured",
                                 measured_at=when)
        changes.append({"quantity": name, "before": getattr(entry, name).value,
                        "after": mean, "sigma": sigma, "readings": len(values)})
    try:
        # Validated again as a whole: an offset beyond the plausible range is a misreading.
        new_entry = CalibrationEntry.model_validate(
            {**{name: getattr(entry, name) for name in CalibrationEntry.model_fields},
             **updated})
        entries = tuple(e for e in table.entries
                        if (e.material, e.nozzle_id) != (material, nozzle_id)) + (new_entry,)
        return CalibrationTable(printer_id=printer_id, entries=entries), changes
    except ValidationError as exc:
        raise CalibrationError(
            "the gauge readings give an implausible calibration "
            f"({exc.errors()[0]['msg']}); measure again, in millimetres"
        ) from exc


def table_json(table: CalibrationTable) -> dict:
    """A table as the JSON object `load_table` reads back."""
    return table.model_dump(mode="json")


def extremes(
    entry: CalibrationEntry, k: float = DEFAULT_K
) -> tuple[CalibrationEntry, CalibrationEntry]:
    """(tight, loose): the two material extremes every clearance is evaluated at."""
    return entry.at_extreme("tight", k), entry.at_extreme("loose", k)
