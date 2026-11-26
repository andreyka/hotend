"""parts_do_not_intersect: no two parts occupy the same space (exact B-rep intersection).

One overlap is meant: an interference fit (a pressed peg) that a feature card built, drawn
with its two parts in place. The volume such a joint intends is allowed between its two
parts, and only when the joint itself passed the `fit_pairs` joint check.
"""

from __future__ import annotations

from itertools import combinations

from .base import ERROR, FAIL, PASS, SKIPPED, CheckEnv, CheckResult, round_floats

# Shared volume below this is contact, not overlap.
OVERLAP_TOLERANCE_MM3 = 1e-3


def _boxes_overlap(a, b) -> bool:
    return all(
        lo_a <= hi_b + 1e-6 and lo_b <= hi_a + 1e-6
        for lo_a, hi_a, lo_b, hi_b in zip(tuple(a.min), tuple(a.max), tuple(b.min), tuple(b.max),
                                           strict=True)
    )


def parts_do_not_intersect(env: CheckEnv) -> CheckResult:
    if len(env.parts) < 2:
        return CheckResult(
            "parts_do_not_intersect", SKIPPED, ERROR, "Skipped: the model has only one part."
        )
    boxes = {part.name: part.shape.bounding_box() for part in env.parts}
    overlaps, undetermined, intended = [], [], []
    for a, b in combinations(env.parts, 2):
        if not _boxes_overlap(boxes[a.name], boxes[b.name]):
            continue
        try:
            common = a.shape.intersect(b.shape)
            volume = sum(float(s.volume) for s in common) if common else 0.0
        except Exception as exc:  # noqa: BLE001 - OCCT booleans raise unrelated types
            undetermined.append({"parts": [a.name, b.name], "reason": str(exc)[:200]})
            continue
        allowed = _intended_overlap(env, a.name, b.name) if volume > OVERLAP_TOLERANCE_MM3 else 0.0
        if volume > allowed + OVERLAP_TOLERANCE_MM3:
            overlaps.append({"parts": [a.name, b.name], "volume": volume, "intended": allowed})
        elif volume > OVERLAP_TOLERANCE_MM3:
            intended.append({"parts": [a.name, b.name], "volume": volume, "intended": allowed})
    data = round_floats({"overlaps": overlaps, "undetermined": undetermined,
                         "intended": intended})
    if overlaps:
        worst = max(overlaps, key=lambda o: o["volume"])
        return CheckResult(
            "parts_do_not_intersect", FAIL, ERROR,
            f"{len(overlaps)} pair(s) of parts overlap; '{worst['parts'][0]}' and "
            f"'{worst['parts'][1]}' share {worst['volume']:.1f} mm3. Parts that are printed "
            "separately must not occupy the same space.",
            None, data,
        )
    if undetermined:
        # Not computed is not proven: an error-severity check that
        # could not run holds the version back, as checks.could_not_run does.
        pair = undetermined[0]["parts"]
        return CheckResult(
            "parts_do_not_intersect", FAIL, ERROR,
            f"Hotend could not compute whether '{pair[0]}' and '{pair[1]}' overlap, so it "
            "is not proven that the parts do not occupy the same space. "
            "The version is held back until the check runs. This is a fault in Hotend, not a verdict on the model: changing the model and building again may get past it.",
            None, {**data, "could_not_run": True},
        )
    if intended:
        pair = intended[0]["parts"]
        return CheckResult(
            "parts_do_not_intersect", PASS, ERROR,
            f"No two parts overlap beyond the interference their joints intend ('{pair[0]}' "
            f"and '{pair[1]}' share {intended[0]['volume']:.2f} mm3 of an interference fit).",
            None, data,
        )
    return CheckResult(
        "parts_do_not_intersect", PASS, ERROR, "No two parts overlap.", None, data
    )


def _intended_overlap(env: CheckEnv, a: str, b: str) -> float:
    """The interference (mm3) verified joints intend between two parts drawn in place."""
    from .fit_pairs import fit_report

    report = fit_report(env)
    if report is None or not report.joints:
        return 0.0
    from hotend_fit.verify import allowed_overlap

    return allowed_overlap(report.joints, a, b)
