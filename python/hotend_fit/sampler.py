"""The range sampler: are the slider ranges a model declares safe to offer?

`sample_ranges(model_path, context)` builds the model at many parameter settings, runs the
engine's error-severity checks on every build in this process, and reports

- which checks fail in which region of the parameter space, and
- the largest box of parameter ranges it could verify: the suggested, clamped slider ranges.

Every setting is built once, as the drawing the user would print (the printer as calibrated).
The material extremes are not redrawn: the `fit_pairs` check evaluates each clearance of that
drawing on the tightest and on the loosest printer the calibration allows (`fit_k` sigmas)
and fails a fit that leaves its class there. On a guessed calibration that evaluation proves
nothing, so it is reported as `unproven` and the report cannot be a plain `pass`.

Samples, in order: the defaults; for each parameter a sweep with everything else at its
default (every value of a count parameter with up to 16 values, `sweep` evenly spaced values
of any other); every corner of the declared ranges (all of them up to `max_corners`,
otherwise both all-low and all-high corners plus a seeded choice of the rest); then a seeded
stratified set inside the ranges (a Latin hypercube whose size grows with the number of
parameters: at least `random_samples` and four per parameter; none when `random_samples` is
0).

Clamping, when a sample fails:
1. Each parameter's own range is cut to the run of passing sweep values around its default,
   and the bound is pushed towards the first failing value by bisection (`refine_steps`).
   A band of failing values inside a range, or one bad count, ends the range there.
2. A failing sample that is still inside the box is attributed before anything is cut: each
   parameter in turn is put back to its default; the cut is made on a parameter whose
   change makes the sample pass (found one at a time, else by resetting them cumulatively),
   at the last passing value along that line. No parameter is clamped without evidence
   that it is involved.
3. The corners of the clamped box and a fresh stratified set inside it are built; the loop
   repeats until they all pass.
The result is conservative: the box may be smaller than the largest one that would pass, and
it always contains the defaults.

Guarantees:
- Repeatable, within limits that are stated here and not hidden: the settings that are
  built depend only on the model's PARAMS, the arguments and `random.Random(seed)`, and
  before every build the caches the libraries keep in a process are emptied
  (`hotend_engine.tessellate.clear_library_caches`), so a build does not depend on the
  builds before it. The same model, context and seed therefore give the same report, byte
  for byte as JSON, when the model script is itself deterministic and the run is on the
  same machine with the pinned libraries; that is what the tests check (twice in one
  process, and in two processes). It is not promised across platforms or library versions:
  a mesh-based check near its threshold can fall the other way there. And unlike a `build`
  of the engine, the model shares this process with the sampler: a script that keeps state
  in a module global, or changes a library, carries it from one setting to the next.
  (Hypothesis is not used: it is a test-time tool that owns the run loop and shrinks
  counterexamples, where this needs fixed sweeps and a stable report.)
- A sample counts as passed only when it built and no error-severity check failed. Checks
  that were skipped are listed, so silence is never read as a pass.
- `verified` is true only when the reported box holds no failing sample and its corners and
  inner points were built and passed. The report gives, per parameter, how many distinct
  values inside the box were built.
- Sampling is not proof: a fault narrower than the spacing of the samples can be missed.
  The report says what was checked, nothing more.
- The model runs in this process (as in the model process of a build), so a report is only
  as trustworthy as the script: it is advice for choosing ranges, not a verdict the core
  stores. A caller runs it inside the sandbox.
"""

from __future__ import annotations

import contextlib
import itertools
import math
import random
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import PROTOCOL_VERSION
from .calibration import DEFAULT_K
from .verify import calibration_state

EXTREMES = ("tight", "loose")
# The checks whose failure makes a build unusable (severity `error` in the engine).
ERROR_CHECKS = (
    "valid_solid", "manifold", "single_solid_per_part", "fits_build_volume", "min_wall",
    "parts_do_not_intersect", "fit_pairs", "reference_fit",
)
DEFAULT_MAX_CORNERS = 16
DEFAULT_RANDOM_SAMPLES = 8
DEFAULT_SWEEP = 9
DEFAULT_MAX_BUILDS = 300
# Bisections between the last passing and the first failing value of a parameter.
DEFAULT_REFINE_STEPS = 3
# A count parameter with at most this many values is swept in full.
MAX_COUNT_VALUES = 16
# Inner points per sampled parameter (at least).
INNER_POINTS_PER_PARAMETER = 4
MAX_ROUNDS = 8
_DIGITS = 6


@dataclass
class _Outcome:
    """One sample: a parameter setting, built once."""

    values: dict[str, float]
    kind: str
    failed: dict[str, str] = field(default_factory=dict)  # failure id -> message
    skipped: set[str] = field(default_factory=set)
    unproven: str = ""  # the fit_pairs robustness warning of a guessed calibration, if any
    # The contract's requirements as measured (only when the runner measures the contract).
    requirements: list[dict] = field(default_factory=list)
    # The size (x, y, z) of every part that was built, in mm.
    sizes: dict[str, list[float]] = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return not self.failed


class _Runner:
    """Builds and checks one parameter setting in this process, with the engine's own code.
    With `contract`, the model's CONTRACT is measured on every build too, as a build of the
    product measures it."""

    def __init__(self, model_path: Path, context: dict | None, k: float, min_wall: float,
                 checks: tuple[str, ...], contract: bool = False) -> None:
        from hotend_engine.model import load_model
        from hotend_engine.request import CheckOptions, Context

        from .fit import resolver_for

        self.model = load_model(Path(model_path))
        # The drawing that is printed: never one redrawn for an extreme printer.
        self.raw_context = {**(context or {}), "fit_extreme": "nominal", "fit_k": k}
        self.context = Context.model_validate(self.raw_context)
        # Reference objects of the request are read once, from the model's own folder (the
        # job folder), before any script code runs.
        self.references: dict = {}
        if self.context.references:
            from hotend_engine.context import load_reference

            folder = Path(model_path).resolve().parent
            self.references = {spec.name: load_reference(spec, folder)[0]
                               for spec in self.context.references}
            self.raw_context = {key: value for key, value in self.raw_context.items()
                                if key != "references"}
        self.k = k
        self.options = CheckOptions(enabled=True, min_wall=min_wall, only=checks,
                                    contract=contract)
        self.builds = 0
        nominal = resolver_for(self.raw_context)
        self.calibration = {"source": nominal.source, "provenance": nominal.provenance}
        self.measured = nominal.measured_quantities

    def run(self, values: dict[str, float], kind: str) -> _Outcome:
        from hotend_engine import checks as engine_checks
        from hotend_engine.context import BuildContext
        from hotend_engine.errors import ModelError
        from hotend_engine.model import StdoutCapture, merge_params, run_build
        from hotend_engine.parts import parts_from_build
        from hotend_engine.tessellate import clear_library_caches, tessellate

        from .fit import resolver_for

        # Every build starts as the first one of a fresh process would: nothing a library
        # kept from an earlier setting (boxes keyed on a freed shape's address, meshes) is
        # left for this one to pick up.
        clear_library_caches(meshes=True)
        self.builds += 1
        outcome = _Outcome(values, kind)
        resolver = resolver_for(self.raw_context)
        resolver.references.update(self.references)
        ctx = BuildContext(self.context.printer, self.context.material, resolver,
                           dict(self.references))
        try:
            params = merge_params(self.model.param_specs, values)
        except ValueError as exc:
            outcome.failed["build:ParameterError"] = str(exc)
            return outcome
        try:
            parts = parts_from_build(
                self.model, run_build(self.model, params, ctx, StdoutCapture())
            )
        except ModelError as exc:
            outcome.failed[f"build:{exc.type}"] = exc.message[:300]
            return outcome
        tessellation, tessellation_error, tessellation_crash = None, None, None
        try:
            tessellation = tessellate(parts, None)
        except Exception as exc:  # noqa: BLE001 - OCCT raises unrelated types
            tessellation_error = f"the model could not be tessellated ({str(exc)[:200]})"
            tessellation_crash = exc
        outcome.sizes = {
            part.name: [round(float(v), 4) for v in tuple(part.shape.bounding_box().size)]
            for part in parts
        }
        env = engine_checks.CheckEnv(parts, ctx, self.options, tessellation, tessellation_error,
                                     tessellation_crash=tessellation_crash,
                                     contract=self.model.contract,
                                     print_hints=self.model.print_hints,
                                     param_specs=self.model.param_specs)
        for result in engine_checks.run_checks(env):
            if result.id == "contract" and result.data.get("kind") == "requirement":
                outcome.requirements.append({
                    "id": result.data["requirement"], "status": result.status,
                    "met": result.data.get("met"),
                    "not_checkable": bool(result.data.get("not_checkable")),
                    "message": result.message[:300],
                })
            if result.status == "fail" and result.severity == "error":
                outcome.failed.setdefault(result.id, result.message)
            elif result.status == "skipped":
                outcome.skipped.add(result.id)
            elif result.status == "warn" and result.data.get("kind") == "robustness":
                outcome.unproven = result.message
        return outcome


class _Space:
    """The sampled parameters, their declared ranges and the defaults."""

    def __init__(self, specs: dict[str, dict], ranges: dict[str, tuple[float, float]] | None):
        self.specs = specs
        self.defaults = {name: spec["default"] for name, spec in specs.items()}
        self.low: dict[str, float] = {}
        self.high: dict[str, float] = {}
        for name, spec in specs.items():
            low, high = spec.get("min"), spec.get("max")
            if ranges and name in ranges:
                low, high = ranges[name]
            if low is None or high is None or not high > low:
                continue
            if not low <= spec["default"] <= high:
                raise ValueError(
                    f"the range given for {name!r} does not contain its default {spec['default']}"
                )
            self.low[name], self.high[name] = low, high
        unknown = [name for name in (ranges or {}) if name not in specs]
        if unknown:
            raise ValueError(f"ranges names unknown parameter(s): {', '.join(unknown)}")
        self.names = list(self.low)

    def clean(self, name: str, value: float) -> float:
        if self.specs[name]["unit"] == "count":
            return round(value)
        return round(float(value), _DIGITS)

    def point(self, **overrides: float) -> dict[str, float]:
        values = dict(self.defaults)
        values.update(overrides)
        return {name: self.clean(name, value) for name, value in values.items()}

    def corners(self, low: dict, high: dict, cap: int, rng: random.Random) -> list[dict]:
        if not self.names:
            return []
        total = 2 ** len(self.names)
        if total <= cap:
            picks = list(itertools.product((0, 1), repeat=len(self.names)))
        else:
            # Both extreme corners always; the rest drawn without building the whole list.
            chosen = {0, total - 1}
            while len(chosen) < max(2, cap):
                chosen.add(rng.randrange(total))
            picks = [
                tuple((index >> bit) & 1 for bit in range(len(self.names)))
                for index in sorted(chosen)
            ]
        return [
            self.point(**{name: (high if bit else low)[name]
                          for name, bit in zip(self.names, pick, strict=True)})
            for pick in picks
        ]

    def sweep(self, name: str, count: int) -> list[float]:
        """The values of one parameter to build with the others at their defaults."""
        low, high = self.low[name], self.high[name]
        if self.specs[name]["unit"] == "count":
            low, high = math.ceil(low), math.floor(high)
            total = high - low + 1
            if total <= MAX_COUNT_VALUES:
                return list(range(low, high + 1))
            steps = MAX_COUNT_VALUES - 1
            return sorted({low + round(i * (high - low) / steps) for i in range(steps + 1)})
        count = max(2, count)
        return [self.clean(name, low + i * (high - low) / (count - 1)) for i in range(count)]

    def stratified(self, low: dict, high: dict, count: int, rng: random.Random) -> list[dict]:
        """`count` points of a Latin hypercube in the box: every parameter's range is cut
        into `count` strata and each stratum is used once."""
        if not self.names or count <= 0:
            return []
        columns = {}
        for name in self.names:
            order = list(range(count))
            rng.shuffle(order)
            if self.specs[name]["unit"] == "count":
                first, last = math.ceil(low[name]), math.floor(high[name])
                columns[name] = [first + (slot * (last - first + 1)) // count for slot in order]
            else:
                width = (high[name] - low[name]) / count
                columns[name] = [low[name] + (slot + rng.random()) * width for slot in order]
        return [self.point(**{name: columns[name][i] for name in self.names})
                for i in range(count)]

    def midpoint(self, name: str, passing: float, failing: float) -> float | None:
        """A value between a passing and a failing one, or None when none is left."""
        middle = (passing + failing) / 2
        if self.specs[name]["unit"] == "count":
            middle = math.floor(middle) if failing > passing else math.ceil(middle)
        middle = self.clean(name, middle)
        return None if middle in (passing, failing) else middle


    def inside(self, values: dict, low: dict, high: dict) -> bool:
        return all(low[name] - 1e-9 <= values[name] <= high[name] + 1e-9 for name in self.names)


def _key(values: dict[str, float]) -> tuple:
    return tuple(sorted(values.items()))


class _Budget(Exception):
    """The build budget ran out."""


def sample_ranges(
    model_path: str | Path,
    context: dict | None = None,
    *,
    seed: int = 0,
    max_corners: int = DEFAULT_MAX_CORNERS,
    random_samples: int = DEFAULT_RANDOM_SAMPLES,
    sweep: int = DEFAULT_SWEEP,
    k: float = DEFAULT_K,
    min_wall: float = 0.8,
    checks: tuple[str, ...] = ERROR_CHECKS,
    ranges: dict[str, tuple[float, float]] | None = None,
    max_builds: int = DEFAULT_MAX_BUILDS,
    refine_steps: int = DEFAULT_REFINE_STEPS,
) -> dict:
    """Sample a model script's parameter ranges.

    `context` is the request's `context` object as a dict (printer, material and an absolute
    `calibration_path` or None), as for `fit.resolver_for`. `ranges` narrows the declared
    range of some parameters, `{name: (low, high)}`. `sweep` is the number of values each
    parameter is stepped through on its own, `k` the number of sigmas of the material
    extremes each fit is evaluated at. `max_builds` caps the work; a report cut short by it
    says `truncated`, and `verified` is then false. Returns the report as a JSON-ready dict;
    see the module doc.
    Raises ModelError (hotend_engine) when the script cannot be loaded at all.
    """
    runner = _Runner(Path(model_path), context, k, min_wall, tuple(checks))
    space = _Space(runner.model.param_specs, ranges)
    rng = random.Random(seed)
    outcomes: dict[tuple, _Outcome] = {}

    def evaluate(values: dict[str, float], kind: str) -> _Outcome:
        key = _key(values)
        if key not in outcomes:
            if runner.builds >= max_builds:
                raise _Budget
            outcomes[key] = runner.run(values, kind)
        return outcomes[key]

    default = space.point()
    inner = 0 if random_samples <= 0 else max(
        random_samples, INNER_POINTS_PER_PARAMETER * len(space.names)
    )
    sweeps = {name: space.sweep(name, sweep) for name in space.names}
    declared_corners: list[dict] = []
    box_low: dict | None = dict(space.low)
    box_high: dict | None = dict(space.high)
    verified, truncated, rounds = False, False, 0
    try:
        evaluate(default, "default")
        for name in space.names:
            for value in sweeps[name]:
                evaluate(space.point(**{name: value}), "sweep")
        declared_corners = space.corners(space.low, space.high, max_corners, rng)
        for corner in declared_corners:
            evaluate(corner, "corner")
        for point in space.stratified(space.low, space.high, inner, rng):
            evaluate(point, "inner")
        box_low, box_high, verified, rounds = _clamp(
            space, default, sweeps, outcomes, evaluate, rng, max_corners, inner, refine_steps
        )
    except _Budget:
        truncated, verified = True, False
        if _key(default) in outcomes and not outcomes[_key(default)].ok:
            box_low = box_high = None
    return _report(
        runner, space, outcomes, sweeps, box_low, box_high, verified, truncated,
        rounds, seed, k, min_wall, tuple(checks), len(declared_corners),
    )


# The most settings one sweep builds, and the checks it runs on each: the error-severity
# checks and the contract, as a build of the product judges a version.
MAX_SWEEP_SETTINGS = 16
SWEEP_CHECKS = (*ERROR_CHECKS, "contract")
# How often a setting past its time is interrupted again, seconds: a script or a check that
# swallows the first interruption gets another until the build gives up.
_STOP_REPEAT_S = 0.25


class _OutOfTime(Exception):
    """Raised into a setting that is still building at the sweep's time limit."""


class _Alarm:
    fired = False


@contextlib.contextmanager
def _stopped_at(seconds: float | None) -> Iterator[_Alarm]:
    """Interrupts the code in the block with `_OutOfTime` after `seconds` (and again every
    `_STOP_REPEAT_S` until the block ends). Does nothing without a limit, off the main
    thread, or where there is no SIGALRM."""
    import signal
    import threading

    alarm = _Alarm()
    if (seconds is None or not hasattr(signal, "setitimer")
            or threading.current_thread() is not threading.main_thread()):
        yield alarm
        return

    def interrupt(signum: int, frame: Any) -> None:
        alarm.fired = True
        raise _OutOfTime

    previous = signal.signal(signal.SIGALRM, interrupt)
    signal.setitimer(signal.ITIMER_REAL, max(seconds, 0.001), _STOP_REPEAT_S)
    try:
        yield alarm
    finally:
        # The timer may fire once more on the way out; it is stopped all the same.
        while True:
            try:
                signal.setitimer(signal.ITIMER_REAL, 0)
                break
            except _OutOfTime:
                continue
        signal.signal(signal.SIGALRM, previous)


def sweep(
    model_path: str | Path,
    context: dict | None = None,
    *,
    base: dict[str, float],
    settings: list[dict[str, float]],
    min_wall: float = 0.8,
    deadline: float | None = None,
    hard_deadline: float | None = None,
    clock: Any = None,
) -> dict:
    """Build a model script at a few chosen parameter settings and say how each one fares.

    `base` holds the values every setting starts from (the current version's parameters);
    each entry of `settings` overrides some of them. Every setting is built once, in order,
    with this module's runner (the engine's own build and checks, the caches emptied before
    each build) and judged by the error-severity checks and the model's CONTRACT: per
    setting the status (`ok`, `check_error`, `model_error`), the checks that failed with
    their messages, every requirement of the contract with its outcome, and the size of
    every part. Nothing is stored and no range is clamped: this is the sampler's runner
    pointed at the settings a caller chose, so the caller can try values without making a
    version of each.

    `deadline` is a value of `clock` (default `time.monotonic`) after which no further
    setting is started; the settings left are reported as `not_run` and the report says
    `truncated`. `hard_deadline` is the last moment by which the report must be done (the
    job's own time limit, less a margin to write it): a setting is not started when the
    slowest setting so far would not fit before it, and a setting still building then is
    stopped (`not_run`, with a `time_limit` entry in `failed`), so the settings already
    built are reported instead of the whole job being killed. Stopping uses SIGALRM and so
    works only on the main thread of a Unix process; elsewhere only the first rule holds.
    At most MAX_SWEEP_SETTINGS settings are accepted.
    Raises ValueError for too many settings; ModelError (hotend_engine) when the script
    cannot be loaded at all.
    """
    import time

    if not 1 <= len(settings) <= MAX_SWEEP_SETTINGS:
        raise ValueError(f"a sweep builds 1 to {MAX_SWEEP_SETTINGS} settings, "
                         f"got {len(settings)}")
    clock = clock or time.monotonic
    runner = _Runner(Path(model_path), context, DEFAULT_K, min_wall, SWEEP_CHECKS,
                     contract=True)
    samples = []
    slowest, stopped = 0.0, 0
    for overrides in settings:
        values = {**base, **overrides}
        entry: dict[str, Any] = {"params": dict(overrides)}
        not_run = {**entry, "status": "not_run", "failed": [], "requirements": [],
                   "parts": {}}
        now = clock()
        if (deadline is not None and now >= deadline) or stopped or (
                hard_deadline is not None and now + slowest >= hard_deadline):
            samples.append(not_run)
            continue
        started, outcome = now, None
        alarm = _Alarm()
        try:
            with _stopped_at(None if hard_deadline is None else hard_deadline - now) as alarm:
                outcome = runner.run(values, "sweep")
        except _OutOfTime:
            alarm.fired = True
        slowest = max(slowest, clock() - started)
        if alarm.fired or outcome is None:
            # Whatever the build made of the interruption (a model error, a check that could
            # not run), it is not a result of this setting.
            stopped += 1
            samples.append({**not_run, "failed": [{
                "check": "time_limit",
                "message": (f"stopped after {round(clock() - started)} s: building this "
                            "setting would have run past the time limit of the sweep"),
            }]})
            continue
        built = not any(name.startswith("build:") for name in outcome.failed)
        status = "ok" if outcome.ok else ("check_error" if built else "model_error")
        samples.append({
            **entry, "status": status,
            "failed": [{"check": name, "message": message[:300]}
                       for name, message in sorted(outcome.failed.items())],
            "requirements": outcome.requirements,
            "parts": outcome.sizes,
        })
    return {
        "protocol": PROTOCOL_VERSION,
        # A setting stopped at the time limit was started but not built.
        "builds": runner.builds - stopped,
        "truncated": any(sample["status"] == "not_run" for sample in samples),
        "checks": list(SWEEP_CHECKS),
        "samples": samples,
    }


def _refine(space: _Space, name: str, base: dict, passing: float, failing: float,
            evaluate: Any, steps: int) -> float:
    """The last value of `name` known to pass on the line through `base`, between a passing
    and a failing value, after `steps` bisections."""
    for _ in range(steps):
        middle = space.midpoint(name, passing, failing)
        if middle is None:
            break
        if evaluate({**base, name: middle}, "refine").ok:
            passing = middle
        else:
            failing = middle
    return passing


def _cut_to(space: _Space, name: str, bound: float, toward_low: bool, low: dict,
            high: dict) -> None:
    if toward_low:
        low[name] = max(low[name], bound)
    else:
        high[name] = min(high[name], bound)


def _attribute(space: _Space, default: dict, failing: dict, evaluate: Any) -> tuple[str, dict]:
    """(parameter, base): a parameter whose return to its default makes the failing setting
    pass, and the setting it was found on (the failing one, or one with other parameters
    already back at their defaults)."""
    moved = [name for name in space.names if failing[name] != default[name]]
    span = {name: space.high[name] - space.low[name] for name in moved}
    # One at a time first: the plainest evidence. When several parameters each clear the
    # failure on their own, the cut goes on the one that costs the least of its range.
    alone = [name for name in moved
             if evaluate({**failing, name: default[name]}, "attribute").ok]
    if alone:
        return min(alone, key=lambda name: (abs(failing[name] - default[name]) / span[name],
                                            name)), failing
    moved.sort(key=lambda name: (-abs(failing[name] - default[name]) / span[name], name))
    # No single parameter is enough: put them back one after another until it passes.
    base = dict(failing)
    for name in moved:
        trial = {**base, name: default[name]}
        if evaluate(trial, "attribute").ok:
            return name, base
        base = trial
    raise AssertionError("the defaults passed, so resetting every parameter must pass")


def _clamp(space: _Space, default: dict, sweeps: dict, outcomes: dict, evaluate: Any,
           rng: random.Random, max_corners: int, inner: int,
           refine_steps: int) -> tuple[dict | None, dict | None, bool, int]:
    """Shrink the declared box until no known failure is inside and its corners and inner
    points pass. Returns (low, high, verified, rounds)."""
    if not outcomes[_key(default)].ok:
        return None, None, False, 0
    low, high = dict(space.low), dict(space.high)
    span = {name: space.high[name] - space.low[name] for name in space.names}

    # 1. Each parameter on its own: the run of passing sweep values around the default.
    for name in space.names:
        centre = default[name]
        for toward_low in (True, False):
            values = sorted((v for v in sweeps[name] if (v < centre) == toward_low and v != centre),
                            reverse=toward_low)
            passing = centre
            for value in values:
                if outcomes[_key(space.point(**{name: value}))].ok:
                    passing = value
                    continue
                bound = _refine(space, name, default, passing, value, evaluate, refine_steps)
                _cut_to(space, name, bound, toward_low, low, high)
                break

    def distance(outcome: _Outcome) -> tuple:
        # Deterministic order: the failing sample nearest the default first, ties by value.
        return (sum(abs(outcome.values[n] - default[n]) / span[n] for n in space.names),
                _key(outcome.values))

    # 2. and 3. Failures that need several parameters, then confirmation of the box.
    for round_number in range(1, MAX_ROUNDS + 1):
        shrunk = round_number > 1 or low != space.low or high != space.high
        while True:
            inside = [o for o in outcomes.values()
                      if not o.ok and space.inside(o.values, low, high)]
            if not inside:
                break
            failing = min(inside, key=distance).values
            name, base = _attribute(space, default, failing, evaluate)
            bound = _refine(space, name, base, default[name], base[name], evaluate,
                            refine_steps)
            _cut_to(space, name, bound, base[name] < default[name], low, high)
            shrunk = True
        if not shrunk:
            return low, high, True, round_number  # the declared samples themselves all passed
        fresh = [evaluate(corner, "confirm_corner")
                 for corner in space.corners(low, high, max_corners, rng)]
        fresh += [evaluate(point, "confirm_inner")
                  for point in space.stratified(low, high, inner, rng)]
        if all(outcome.ok for outcome in fresh):
            return low, high, True, round_number
    return low, high, False, MAX_ROUNDS


def _region(space: _Space, failing: list[_Outcome], passing: list[_Outcome]) -> dict:
    """Where a check fails: the box around the failing samples, and every parameter whose
    value alone separates the failing samples from the passing ones."""
    box = {
        name: [min(o.values[name] for o in failing), max(o.values[name] for o in failing)]
        for name in space.names
    }
    separated = []
    for name in space.names:
        fails = [o.values[name] for o in failing]
        passes = [o.values[name] for o in passing]
        if not passes:
            continue
        if max(fails) < min(passes):
            separated.append({"param": name, "side": "below", "fails_up_to": max(fails),
                              "passes_from": min(passes)})
        elif min(fails) > max(passes):
            separated.append({"param": name, "side": "above", "fails_from": min(fails),
                              "passes_up_to": max(passes)})
    return {"box": box, "separated_by": separated}


def _report(runner: _Runner, space: _Space, outcomes: dict, sweeps: dict, box_low: dict | None,
            box_high: dict | None, verified: bool, truncated: bool, rounds: int, seed: int,
            k: float, min_wall: float, checks: tuple[str, ...], corners_checked: int) -> dict:
    ordered = list(outcomes.values())
    passing = [o for o in ordered if o.ok]
    failing = [o for o in ordered if not o.ok]
    unproven = [o for o in ordered if o.unproven]
    failures = []
    for failure in sorted({name for o in failing for name in o.failed}):
        hit = [o for o in failing if failure in o.failed]
        example = hit[0]
        failures.append({
            "check": failure,
            "count": len(hit),
            **_region(space, hit, passing),
            "example": {"params": example.values, "message": example.failed[failure]},
        })
    parameters = {}
    for name, spec in runner.model.param_specs.items():
        sampled = name in space.names
        entry = {"default": spec["default"], "unit": spec["unit"], "sampled": sampled}
        if sampled:
            entry["declared"] = [space.low[name], space.high[name]]
            entry["passed"] = None if box_low is None else [box_low[name], box_high[name]]
            entry["clamped"] = box_low is not None and (
                box_low[name] > space.low[name] or box_high[name] < space.high[name]
            )
            entry["swept"] = len(sweeps[name])
            # How densely the reported range was looked at along this parameter.
            entry["values_checked"] = 0 if box_low is None else len({
                o.values[name] for o in ordered if space.inside(o.values, box_low, box_high)
            })
        parameters[name] = entry
    clamped = [name for name in space.names if parameters[name].get("clamped")]
    default_ok = _key(space.point()) in outcomes and outcomes[_key(space.point())].ok
    fits = (
        "Each fit was also evaluated on the tightest and the loosest printer the calibration "
        f"allows ({k:g} sigma)."
    )
    # A build that declared a fit: fit_pairs ran on it and had something to check.
    declared = "fit_pairs" in checks and any(
        "fit_pairs" not in o.skipped and not any(name.startswith("build:") for name in o.failed)
        for o in ordered
    )
    if "fit_pairs" not in checks:
        fits = "The fits were not checked."
    elif not declared:
        fits = "The model declares no fits."
    if unproven:
        fits = (
            f"The printer {calibration_state(runner.measured)}, so the fits are unproven: "
            "on the spread the calibration allows "
            f"{len(unproven)} of {len(ordered)} settings have a fit that may leave its class. "
            "Print the gauge or calibrate the printer."
        )
    if not default_ok:
        status = "fail"
        message = "The model fails its checks at its default sizes, so no range was checked."
    elif truncated:
        status = "warn"
        message = (
            f"The build budget ran out after {runner.builds} builds ({len(failing)} of "
            f"{len(ordered)} settings failed); no range could be confirmed."
        )
    elif not failing:
        status = "warn" if unproven else "pass"
        message = (
            f"Every sampled size passed: {len(ordered)} settings ({runner.builds} builds) were "
            f"checked across the declared ranges. {fits}"
        )
    else:
        status = "warn"
        if verified:
            ranges = "; ".join(
                f"{name} {box_low[name]:g} to {box_high[name]:g}" for name in clamped
            )
            message = (
                f"{len(failing)} of {len(ordered)} sampled settings failed "
                f"({', '.join(f['check'] for f in failures)}). The sliders should be clamped "
                f"to the ranges that were checked and passed: {ranges}. {fits}"
            )
        else:
            message = (
                f"{len(failing)} of {len(ordered)} sampled settings failed "
                f"({', '.join(f['check'] for f in failures)}) and no clamped range could be "
                "confirmed."
            )
    return {
        "protocol": PROTOCOL_VERSION,
        "status": status,
        "message": message,
        "model": runner.model.filename,
        "seed": seed,
        "k": k,
        "extremes": list(EXTREMES),
        "min_wall": min_wall,
        "checks": list(checks),
        "skipped_checks": sorted({name for o in ordered for name in o.skipped}),
        "calibration": runner.calibration,
        "fits": {
            "declared": declared,
            "proven": declared and not unproven,
            "unproven_settings": len(unproven),
            "example": unproven[0].unproven if unproven else None,
        },
        "parameters": parameters,
        "samples": {
            "settings": len(ordered),
            "builds": runner.builds,
            "passed": len(passing),
            "failed": len(failing),
            "by_kind": {kind: sum(1 for o in ordered if o.kind == kind)
                        for kind in sorted({o.kind for o in ordered})},
            "declared_corners_checked": corners_checked,
            "declared_corners_total": 2 ** len(space.names) if space.names else 0,
        },
        "failures": failures,
        "failed_settings": [
            {"params": o.values, "kind": o.kind, "failed": sorted(o.failed)} for o in failing
        ],
        "passed_ranges": None if box_low is None else {
            name: [box_low[name], box_high[name]] for name in space.names
        },
        "clamped": clamped,
        "verified": verified,
        "truncated": truncated,
        "rounds": rounds,
    }
