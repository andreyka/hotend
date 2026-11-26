"""The `reference_fit` check: a printed part really fits the real thing it was built around
(docs/PROTOCOL.md sections 3.3 and 4.6).

It reports only when the model made a pocket for a reference, used a bought component or
took a reference fit class by hand; a model without any of these gets no line at all.

Per pocket (`hotend_fit.reference.verify_pocket`, on the solids, with this process's own
copy of the reference):
- no part reaches into the room the thing needs: the clearance holds all round;
- the thing can be put in and taken out along the pocket's direction: nothing but a part
  named `removable` stands in its way (a trapped thing fails);
- a `stretch` fit is in a flexible material, holds at least half of the thing's rim, and
  the lip it is pushed past is not deeper than the material stretches. A stretch fit in a
  rigid material fails with the reason in plain words.

Per component the model used (`hotend_fit.components`), placed where its pocket is:
- no part collides with a feature that stands proud of the body (a button, the camera);
- the maker's keep-out of every feature the model asked for is free of material (fail),
  and a keep-out of a feature it never asked for that is covered is a warning;
- when the model relied on a number that is not verified, a warning names it: such a
  model is not fully proven, and the questions in the result say what to check.
"""

from __future__ import annotations

from typing import Any

from ..context import MissingFit
from .base import ERROR, FAIL, PASS, WARN, CheckEnv, CheckResult

ID = "reference_fit"
REFERENCE_CLASSES = ("stretch", "cradle")
MAX_REACH_MM = 1000.0


def _loose_reference_fits(resolver: Any) -> list[str]:
    """The reference fit classes of clearances the model took without a pocket."""
    if getattr(resolver, "restored", False):
        classes = [str(record.get("fit_class")) for record in resolver.reported_loose]
    else:
        covered = resolver.covered_clearances()
        classes = [record.fit_class for record in resolver.issued if id(record) not in covered]
    return [name for name in classes if name in REFERENCE_CLASSES]


def _component_results(component: Any, pockets: list, parts: list, reach: float) -> list[CheckResult]:
    from hotend_fit.components import PROOF_TOLERANCE_MM
    from hotend_fit.reference import OVERLAP_TOLERANCE_MM3, _shared

    results: list[CheckResult] = []
    used = sorted(component.used_features)
    placed = [pocket for pocket in pockets
              if pocket.reference is component.reference and pocket.at is not None]
    data: dict = {"kind": "component", "component": component.id, "used_features": used}
    if not placed:
        results.append(CheckResult(
            ID, WARN, ERROR,
            f"The {component.name} was used but never placed with a pocket, so nothing "
            "checks the parts against its buttons, ports and camera. Make a pocket for "
            "`component.reference` and cut or place it.",
            None, {**data, "placed": False}))
    for pocket in placed:
        collisions, covered_used, covered_other = [], [], []
        for feature in component.features.values():
            solid = feature.solid
            if solid is not None:
                # The feature itself, a hair smaller, so a wall that touches it is contact.
                body = pocket.at * feature._build(feature.body, 1.0, 0.0, -PROOF_TOLERANCE_MM)
                for name, shape in parts:
                    volume, _ = _shared(shape, body)
                    if volume > OVERLAP_TOLERANCE_MM3:
                        collisions.append({"feature": feature.id, "part": name, "volume": volume})
            if feature.keep_outs:
                room = None
                for shape_data in feature.keep_outs:
                    piece = pocket.at * feature._build(shape_data, reach, 0.0, -PROOF_TOLERANCE_MM)
                    room = piece if room is None else room + piece
                for name, shape in parts:
                    volume, _ = _shared(shape, room)
                    if volume > OVERLAP_TOLERANCE_MM3:
                        (covered_used if feature.id in used else covered_other).append(
                            {"feature": feature.id, "part": name, "volume": volume})
        entry = {**data, "placed": True, "pocket": pocket.name, "collisions": collisions,
                 "keep_outs_covered": covered_used, "keep_outs_not_handled": covered_other}
        problems, cautions = [], []
        if collisions:
            worst = max(collisions, key=lambda c: c["volume"])
            label = component.features[worst["feature"]].label
            problems.append(
                f"'{worst['part']}' collides with the {label} ({worst['volume']:.1f} mm3), "
                "which stands out of the body; cut `feature.opening(ctx)` or "
                "`feature.keep_out()` for it"
                + (f" (and {len(collisions) - 1} more collision(s))" if len(collisions) > 1 else ""))
        if covered_used:
            names = sorted({component.features[c["feature"]].label for c in covered_used})
            problems.append(
                f"material is left inside the maker's keep-out of: {', '.join(names)}. "
                "Subtract `feature.keep_out()` from the part there")
        if covered_other:
            names = sorted({component.features[c["feature"]].label for c in covered_other})
            cautions.append(
                f"the part covers the maker's keep-out of: {', '.join(names)}, which the "
                "model never asked for; open them or decide that they may be covered")
        if problems:
            status, message = FAIL, f"The parts do not clear the {component.name}: " + "; ".join(
                problems + cautions) + "."
        elif cautions:
            status, message = WARN, f"Around the {component.name} " + "; ".join(cautions) + "."
        else:
            status = PASS
            message = (f"No part collides with a button, port or camera of the "
                       f"{component.name}, and the keep-outs of the features the model "
                       "asked for are free.")
        results.append(CheckResult(ID, status, ERROR, message, None, entry))
    unverified = component.unverified(used)
    if unverified:
        questions = component.questions(used)
        results.append(CheckResult(
            ID, WARN, ERROR,
            f"Not fully proven: the model relies on {len(unverified)} number(s) of the "
            f"{component.name} that are not verified ("
            + ", ".join(sorted({datum.path.split('.')[1] for datum in unverified})[:8])
            + f"). {len(questions)} question(s) for the user say what to check.",
            None, {**data, "kind": "provenance", "proven": False,
                   "unverified": [datum.to_json() for datum in unverified[:64]],
                   "questions": questions}))
    else:
        results.append(CheckResult(
            ID, PASS, ERROR,
            f"Every number of the {component.name} the model relies on is the maker's own "
            "and verified.",
            None, {**data, "kind": "provenance", "proven": True}))
    return results


def pocket_reports(env: CheckEnv) -> list:
    """The verification of every pocket of this build (`hotend_fit.reference.PocketReport`),
    computed once: the `contract` check reads it for its `reference_fit` requirements."""
    if env.pocket_cache is None:
        resolver = env.ctx.fit
        if isinstance(resolver, MissingFit) or not getattr(resolver, "pockets", None):
            env.pocket_cache = []
        else:
            from hotend_fit.reference import verify_pockets

            env.pocket_cache = verify_pockets(
                [(part.name, part.shape) for part in env.parts], resolver)
    return env.pocket_cache


def reference_questions(ctx: Any, contract: Any = None) -> list[dict]:
    """What a build that was fitted to a reference object must ask its user, whatever the
    contract says (result `questions`):

    - per reference file a pocket was made for: is this the thing, at the size it has in
      the file? A file says what was drawn, not what was printed or bought;
    - per reference whose pockets take a fit class nobody has tried on this printer (a
      guessed clearance): print the ring gauge. Left out when the contract has a
      `reference_fit` requirement for that reference, which asks the same.
    """
    resolver = ctx.fit
    if isinstance(resolver, MissingFit):
        return []
    pockets = list(getattr(resolver, "pockets", None) or [])
    stated = {requirement.reference for requirement in getattr(contract, "requirements", ())
              if getattr(requirement, "kind", "") == "reference_fit"}
    questions: list[dict] = []
    asked: set[str] = set()
    for pocket in pockets:
        reference = pocket.reference
        if reference.name in asked:
            continue
        asked.add(reference.name)
        source = reference.source if isinstance(reference.source, dict) else {}
        if source.get("file"):
            size = " x ".join(f"{value:.1f}" for value in reference.size)
            where = f"the file '{str(source['file']).rsplit('/', 1)[-1]}'"
            if source.get("object"):
                where += f", object '{source['object']}'"
            if source.get("plate") is not None:
                where += f" on plate {source['plate']}"
            scale = source.get("scale")
            scaled = (f" at {100 * float(scale):g} % of the file's size"
                      if isinstance(scale, (int, float)) else "")
            others = int(source.get("objects_in_file") or 1) - 1
            left_out = (f" The file holds {others} more object(s) that are not part of it."
                        if others > 0 else "")
            questions.append({
                "requirement": f"reference_{reference.name}"[:64],
                "text": (f"'{reference.name}' was measured from {where}{scaled}: {size} mm."
                         f"{left_out} Is that the thing you have, at the size you print or "
                         "bought it? Measure its widest width and its height with a caliper "
                         "to confirm."),
                "guessed_params": [],
            })
        guessed = [p for p in pockets if p.reference is reference
                   and "guessed" in (p.clearance.provenance, p.clearance.target_provenance)]
        if guessed and reference.name not in stated:
            first = guessed[0]
            how = ("a gap of" if first.clearance.target >= 0 else "an interference of")
            questions.append({
                "requirement": first.name,
                "text": (f"The {first.fit} fit around '{reference.name}' aims for {how} "
                         f"{abs(first.clearance.target):.2f} mm per side once printed. That "
                         "figure is a guess until it is tried: print the ring gauge and say "
                         "whether the thing goes in the way you want, is too tight or too "
                         "loose."),
                "guessed_params": [],
            })
    return questions


def reference_fit(env: CheckEnv) -> list[CheckResult]:
    resolver = env.ctx.fit
    if isinstance(resolver, MissingFit):
        return []
    pockets = list(getattr(resolver, "pockets", None) or [])
    components = dict(getattr(resolver, "components", None) or {})
    loose = _loose_reference_fits(resolver)
    # When fit_pairs runs it already reports what could not be rebuilt.
    only = env.options.only
    lost = [problem for problem in getattr(resolver, "restore_problems", [])
            if "pocket" in problem or "component" in problem]
    if only is None or "fit_pairs" in only:
        lost = []
    if not (pockets or components or loose or lost):
        return []
    results: list[CheckResult] = []
    parts = [(part.name, part.shape) for part in env.parts]
    for report in pocket_reports(env):
        results.append(CheckResult(ID, report.status, ERROR, report.message, None,
                                   report.to_json()))
    reach = 1.0
    for _, shape in parts:
        reach = max(reach, 2.0 * float(shape.bounding_box().diagonal))
    for component in components.values():
        results += _component_results(component, pockets, parts, min(reach, MAX_REACH_MM))
    for problem in lost:
        results.append(CheckResult(ID, FAIL, ERROR, problem[:1].upper() + problem[1:] + ".",
                                   None, {"kind": "not_rebuilt"}))
    if loose:
        facts = resolver.material
        flexible_needed = "stretch" in loose and not facts.flexible
        named = resolver.material_family or resolver.material_id or "this material"
        results.append(CheckResult(
            ID, FAIL if flexible_needed else WARN, ERROR,
            (f"A stretch fit was taken for a part printed in {named}, which is rigid: it "
             "would crack or never go over the thing. Print it in TPU or use a clearance "
             "class." if flexible_needed else
             f"{len(loose)} reference fit clearance(s) ({', '.join(sorted(set(loose)))}) "
             "were taken from the fit resolver without a pocket, so nothing proves the "
             "thing fits. Use `reference.pocket(...)`."),
            None, {"kind": "unverified", "fit_classes": loose,
                   "material_flexible": facts.flexible}))
    return results
