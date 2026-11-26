"""Printability and validity checks (docs/PROTOCOL.md section 3.3).

Each check is one small function. It returns `pass`, `warn`, `fail` or `skipped`; a check that
cannot determine its answer says `skipped` with the reason and never `pass`.

A check that raises did not decide anything, and that is not the same as having nothing to
decide: what it tests is unproven. It is reported as `fail` when its severity is `error` (so
the build becomes `check_error` and the version is not offered for printing) and as `warn`
otherwise, with `data.could_not_run` true and the exception in `data.exception`, never in
the sentence the user reads. The other checks still run.

`run_checks` is the only entry point the worker uses. `CHECK_IDS` is the list, in order, of
the checks every build runs. `OPTIONAL_CHECK_IDS` are run only when the request asks:
`contract` gives no result at all unless `checks.contract` is set or `checks.only` names it.
`CONDITIONAL_CHECK_IDS` run in every build but report only when the model has what they
judge: `reference_fit` gives no result for a model without references or components.
"""

from __future__ import annotations

import sys
import traceback
from collections.abc import Callable

from .base import ERROR, FAIL, SKIPPED, WARN, WARNING, CheckEnv, CheckResult
from .build_volume import fits_build_volume
from .contract import contract
from .fit_pairs import fit_pairs
from .intersect import parts_do_not_intersect
from .min_wall import min_wall
from .orientation import bed_contact, overhangs
from .reference_fit import reference_fit
from .small_features import small_features
from .solid import manifold, single_solid_per_part, valid_solid


def parameter_ranges(env: CheckEnv) -> CheckResult:
    """The range sampler lives in hotend_fit and drives whole builds; the engine only
    reports that it did not run, so nobody reads silence as "every size was checked"."""
    reason = (
        "the range sampler is not part of the engine"
        if env.options.sample_ranges
        else "range sampling was not requested"
    )
    return CheckResult("parameter_ranges", SKIPPED, WARNING, f"Skipped: {reason}.")


# (id, severity, function, runs once per part)
_CHECKS: tuple[tuple[str, str, Callable, bool], ...] = (
    ("valid_solid", ERROR, valid_solid, True),
    ("manifold", ERROR, manifold, True),
    ("single_solid_per_part", ERROR, single_solid_per_part, True),
    ("fits_build_volume", ERROR, fits_build_volume, True),
    ("min_wall", ERROR, min_wall, True),
    ("overhangs", WARNING, overhangs, True),
    ("bed_contact", WARNING, bed_contact, True),
    ("small_features", WARNING, small_features, True),
    ("parts_do_not_intersect", ERROR, parts_do_not_intersect, False),
    ("fit_pairs", ERROR, fit_pairs, False),
    ("reference_fit", ERROR, reference_fit, False),
    ("contract", ERROR, contract, False),
    ("parameter_ranges", WARNING, parameter_ranges, False),
)
OPTIONAL_CHECK_IDS = ("contract",)
CONDITIONAL_CHECK_IDS = ("reference_fit",)
CHECK_IDS = tuple(entry[0] for entry in _CHECKS
                  if entry[0] not in OPTIONAL_CHECK_IDS + CONDITIONAL_CHECK_IDS)
ALL_CHECK_IDS = tuple(entry[0] for entry in _CHECKS)


def _guarded(check_id: str, severity: str, call: Callable, part: str | None) -> list[CheckResult]:
    try:
        result = call()
    except Exception as exc:  # noqa: BLE001 - one broken check must not hide the others
        traceback.print_exc(file=sys.stderr)
        return [could_not_run(check_id, severity, part, exc)]
    return result if isinstance(result, list) else [result]


def could_not_run(check_id: str, severity: str, part: str | None,
                  exc: BaseException) -> CheckResult:
    """The result of a check that raised (module doc)."""
    blocks = severity == ERROR
    return CheckResult(
        check_id, FAIL if blocks else WARN, severity,
        "Hotend could not run this check"
        + (f" on '{part}'" if part else "")
        + ", so what it tests is not proven. "
        + ("The version is held back until the check runs. " if blocks else "")
        + "This is a fault in Hotend, not a verdict on the model: changing the model and "
          "building again may get past it.",
        part,
        {"could_not_run": True, "exception": f"{type(exc).__name__}: {str(exc)[:200]}"},
    )


def run_checks(env: CheckEnv) -> list[CheckResult]:
    """Run the enabled checks in order and return their results."""
    only = env.options.only
    results: list[CheckResult] = []
    for check_id, severity, function, per_part in _CHECKS:
        if only is not None and check_id not in only:
            continue
        if per_part:
            for part in env.parts:
                results += _guarded(
                    check_id, severity, lambda f=function, p=part: f(env, p), part.name
                )
        else:
            results += _guarded(check_id, severity, lambda f=function: f(env), None)
    return results


def has_blocking_failure(results: list[CheckResult]) -> bool:
    """True when a check of severity `error` failed: the build status becomes check_error."""
    return any(r.status == FAIL and r.severity == ERROR for r in results)


__all__ = [
    "ALL_CHECK_IDS", "CHECK_IDS", "CONDITIONAL_CHECK_IDS", "OPTIONAL_CHECK_IDS", "CheckEnv", "CheckResult", "has_blocking_failure",
    "run_checks",
]
