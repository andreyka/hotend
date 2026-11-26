"""fit_pairs: every declared fit is drawn as the resolver asked, can be assembled, and still
is its fit class across the printer's spread.

Feature cards (hotend_fit) register themselves and one entry per mating pair on `ctx.fit`
during the build; after a build in another process the worker rebuilds them from the
declared data (`FitResolver.restore`). The check then reports, all under the id `fit_pairs`
and told apart by `data.kind`:

- `pair`, one per mating pair: measured on the finished solids with the kernel's exact B-rep
  distance and compared with the clearance the resolver handed out. `fail`: the clearance
  differs, a face is missing or incomplete, a half was never applied, or the clearance did
  not come from this build's resolver.
- `joint`, one per card: the two parts brought together as the card's frames say do not
  collide, the room the joint needs is free, and all cards between the same two parts agree
  on where the parts sit.
- `robustness`: what the drawing becomes on the tightest and the loosest printer the
  calibration allows. A fit that leaves its class there is a `fail` when the calibration was
  measured and a `warn` when it is only guessed (nothing is proven on a guess either way).
- `declaration`: a card the model's process declared that could not be rebuilt; `fail`.
- `unverified`: clearances the model took from the resolver outside any card; one `warn`.
- `undeclared` (only with `checks.undeclared_joints`): faces of two parts that face each
  other within about 1 mm outside any declared card, that is a joint made by hand with a
  typed gap (`checks.undeclared`); one `warn` per pair of parts.

With no declared fits, no such clearance and no undeclared joint the check is `skipped`.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from ..context import MissingFit
from .base import ERROR, FAIL, PASS, SKIPPED, WARN, CheckEnv, CheckResult


@dataclass
class FitReport:
    """Everything hotend_fit found out about one build's fits."""

    measurements: list = field(default_factory=list)
    joints: list = field(default_factory=list)
    robustness: list = field(default_factory=list)
    foreign: int = 0


def fit_report(env: CheckEnv) -> FitReport | None:
    """The fit verification of this build, computed once per CheckEnv. None when the build
    declared nothing that hotend_fit can verify."""
    if env.fit_cache is not None:
        return env.fit_cache or None
    declared = env.ctx.declared_fits
    report: FitReport | None = None
    if declared:
        try:
            from hotend_fit.verify import (
                FitPair,
                check_joints,
                check_robustness,
                measure_pairs,
            )
        except ImportError:
            report = FitReport(foreign=len(declared))
        else:
            pairs = [fit for fit in declared if isinstance(fit, FitPair)]
            report = FitReport(foreign=len(declared) - len(pairs))
            if pairs:
                parts = [(part.name, part.shape) for part in env.parts]
                resolver = env.ctx.fit
                report.measurements = measure_pairs(parts, pairs, resolver)
                report.joints = check_joints(parts, resolver.cards, report.measurements)
                report.robustness = [r for r in check_robustness(resolver) if r["pairs"]]
    env.fit_cache = report or False
    return report


def _robustness_results(robustness: list[dict], measured: tuple[str, ...] = ()) -> list[CheckResult]:
    if not robustness:
        return []
    k = robustness[0]["k"]
    results, unproven = [], []
    for entry in robustness:
        if entry["ok"]:
            continue
        if entry["offsets_provenance"] == "calibrated":
            results.append(CheckResult(
                "fit_pairs", FAIL, ERROR,
                f"The {entry['fit_class']} fit of {', '.join(entry['pairs'])} is drawn right "
                f"for this printer as measured, but not across its spread: {entry['reason']}.",
                None, {"kind": "robustness", **entry},
            ))
        else:
            unproven.append(entry)
    if unproven:
        listed = "; ".join(
            f"{', '.join(entry['pairs'][:2])}{' ...' if len(entry['pairs']) > 2 else ''} "
            f"({entry['fit_class']}): {entry['reason']}"
            for entry in unproven[:4]
        )
        if measured:
            # The gauge measures hole and peg sizes only.
            from hotend_fit.verify import calibration_state

            state = calibration_state(measured)
            drawn = "partly from guesses"
        else:
            state, drawn = "is not calibrated", "for a guessed printer"
        results.append(CheckResult(
            "fit_pairs", WARN, ERROR,
            f"This printer {state}, so {len(unproven)} of {len(robustness)} "
            f"clearances are not sure to give their fit: {listed}"
            f"{' ...' if len(unproven) > 4 else ''}. The gaps are drawn {drawn}; "
            "print the card's gauge or calibrate the printer before relying on these fits.",
            None, {"kind": "robustness", "k": k, "unproven": unproven},
        ))
    if not results:
        results.append(CheckResult(
            "fit_pairs", PASS, ERROR,
            f"All {len(robustness)} clearances keep their fit class on the tightest and the "
            f"loosest printer the calibration allows ({k:g} sigma).",
            None, {"kind": "robustness", "k": k, "clearances": robustness},
        ))
    return results


def measure_declared_fits(env: CheckEnv) -> list[CheckResult]:
    """One CheckResult per pair, per joint and per robustness finding of this build.

    Only hotend_fit's own pair records can be verified. Anything else in the declared list
    (or any list at all when hotend_fit is not installed) is counted in one `skipped`
    result.
    """
    report = fit_report(env)
    if report is None:
        return []
    results = [
        CheckResult(
            "fit_pairs", PASS if measurement.ok else FAIL, ERROR, measurement.message,
            None, measurement.to_json(),
        )
        for measurement in report.measurements
    ]
    results += [
        CheckResult("fit_pairs", PASS if joint.ok else FAIL, ERROR, joint.message, None,
                    joint.to_json())
        for joint in report.joints
    ]
    results += _robustness_results(
        report.robustness, tuple(getattr(env.ctx.fit, "measured_quantities", ())))
    if report.foreign:
        results.append(CheckResult(
            "fit_pairs", SKIPPED, ERROR,
            f"Skipped: {report.foreign} fit(s) were declared in a form that is not a "
            "hotend_fit pair; measuring those is not implemented.",
            None, {"declared": report.foreign},
        ))
    return results


def _taken_without_a_card(env: CheckEnv, declared_fits: list) -> list[CheckResult]:
    """One warning for clearances the model took from the resolver outside any card."""
    resolver = env.ctx.fit
    if isinstance(resolver, MissingFit):
        return []
    if getattr(resolver, "restored", False):
        loose = list(resolver.reported_loose)
        classes = ", ".join(sorted({str(record.get("fit_class", "?")) for record in loose}))
        listed = loose
    else:
        covered = {id(getattr(fit, "clearance", None)) for fit in declared_fits}
        covered |= getattr(resolver, "covered_clearances", set)()
        records = [
            record for record in getattr(resolver, "issued", None) or []
            if id(record) not in covered
        ]
        loose = records
        classes = ", ".join(sorted({str(getattr(r, "fit_class", "?")) for r in records}))
        listed = [r.to_json() if hasattr(r, "to_json") else repr(r) for r in records]
    if not loose:
        return []
    return [CheckResult(
        "fit_pairs", WARN, ERROR,
        f"{len(loose)} clearance(s) ({classes}) were taken from the fit resolver without a "
        "feature card, so nothing verifies the faces they were used on. Build mating "
        "features with a card.",
        None, {"kind": "unverified", "unverified": listed},
    )]


def _not_rebuilt(env: CheckEnv) -> list[CheckResult]:
    """One failure per card the model's process declared that could not be rebuilt here."""
    if isinstance(env.ctx.fit, MissingFit):
        return []
    problems = getattr(env.ctx.fit, "restore_problems", None) or []
    return [
        CheckResult(
            "fit_pairs", FAIL, ERROR,
            f"A fit the model declared could not be verified: {problem}.",
            None, {"kind": "declaration", "problem": problem},
        )
        for problem in problems
    ]


def _undeclared(env: CheckEnv) -> list[CheckResult]:
    """One warning per pair of parts with a joint that no feature card made."""
    if not env.options.undeclared_joints or len(env.parts) < 2:
        return []
    from .undeclared import REACH_MM, find_undeclared_joints

    unmapped = [part.name for part in env.parts
                if env.mesh(part) is None or not env.mesh(part).face_map_ok]
    if unmapped:
        return [CheckResult(
            "fit_pairs", SKIPPED, ERROR,
            f"Skipped: joints made without a feature card could not be looked for because "
            f"part(s) {', '.join(unmapped)} have no usable triangle mesh.",
            None, {"kind": "undeclared", "parts": unmapped},
        )]
    cards = [] if isinstance(env.ctx.fit, MissingFit) else list(env.ctx.fit.cards)
    meshes = {part.name: env.mesh(part) for part in env.parts}
    results = []
    for record in find_undeclared_joints(env.parts, meshes, cards):
        a, b = record["parts"]
        if record["gap_max"] <= 0.0:
            gap = "touching"
        elif record["gap_max"] - record["gap_min"] < 0.02:
            gap = f"about {record['gap']:.2f} mm apart"
        else:
            gap = f"{record['gap_min']:.2f} to {record['gap_max']:.2f} mm apart"
        results.append(CheckResult(
            "fit_pairs", WARN, ERROR,
            f"'{a}' and '{b}' face each other {gap} over about {record['area']:.0f} mm2 where "
            "no feature card made the faces: this looks like a joint drawn by hand with a "
            "typed gap. Nothing verifies it and the gap does not follow the printer's "
            "calibration; build the joint with a feature card.",
            None, {**record, "reach": REACH_MM},
        ))
    return results


def fit_pairs(env: CheckEnv) -> list[CheckResult]:
    declared = env.ctx.declared_fits
    extra = _not_rebuilt(env) + _taken_without_a_card(env, declared) + _undeclared(env)
    if not declared:
        return extra or [
            CheckResult("fit_pairs", SKIPPED, ERROR, "Skipped: no declared fits.")
        ]
    return measure_declared_fits(env) + extra
