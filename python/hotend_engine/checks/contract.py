"""contract: the model's CONTRACT measured on the solids it built.

`CONTRACT` (docs/PROTOCOL.md section 4.1) is validated before the script runs; a contract
that does not validate never reaches this check. With `checks.contract` set, every
requirement becomes one result with the id `contract` and `data.kind` = `requirement`:

- `cavity_min`: a cuboid of the required size, axis-aligned as the part is modelled, fits in
  the free space inside the part's bounding box without touching the part. Decided on the
  tessellation with manifold3d: the free space eroded by the cuboid is not empty. A boss in
  a corner therefore counts; "inside the bounding box" also counts space under a part that
  stands on feet, so the requirement is for containers.
- `outer_max`: the part's bounding box is no larger than the size, axis by axis.
- `wall_min`: the `min_wall` measurement of the part with the requirement's value.
- `fit`: a feature card of the named kind joins the two named parts, one of its pairs has
  the named fit class, and that card's pairs and joint all passed `fit_pairs`.
- `standoff`: how far the named part stands beyond a real thing (a reference or a bought
  component the model placed with a pocket) on one side, with the thing pushed towards
  that side as far as the parts let it go. Measured on the tessellation: rays from the
  thing's surface along the side find how far it can travel before it meets a part, and
  the part's outermost point on that side is compared with the thing's face (or with the
  face of a proud feature of a component) after that travel.
- `dimension`: cannot be measured (a dimension named in words has no location on the solid);
  it is reported as not checkable, so nobody reads it as checked.

Outcome: `pass` when the requirement is met; `fail` when it is not. A requirement Hotend
cannot evaluate (a `dimension`, a kind this check has no measurement for, a part without a
usable mesh, too many triangles) is never left out and never passed: it is reported as not
checkable, status `skipped` with `data.not_checkable` true and the reason in `data.reason`
and in the message, which starts with "Not checkable:". A requirement that rests on a
guessed number (its own tag, or a parameter it names: `hotend_fit.contract.open_questions`)
is never more than a `warn`, met or not: a guess proves nothing either way, so it carries the
question for the user in `data.question` instead, and the message says whether the geometry
meets the guess (or that it is not checkable, with the reason).
"""

from __future__ import annotations

import dataclasses

import numpy as np

from .base import ERROR, FAIL, PASS, SKIPPED, WARN, CheckEnv, CheckResult, round_floats

# The cuboid is tried this much smaller than asked (mm), so a cavity drawn exactly to size
# counts as holding it.
CAVITY_TOLERANCE_MM = 0.01
OUTER_TOLERANCE_MM = 1e-6
# manifold3d's erosion costs time in proportion to the triangle count; above this the
# requirement is reported as not measured instead of stalling the build.
MAX_CAVITY_TRIANGLES = 60_000
# A `standoff` over a region is measured on a grid of rays this far apart (mm).
REGION_SPACING_MM = 1.0
# A surface this close to where a ray starts is the one the ray starts on.
TOUCH_MM = 1e-4
# Rays that test how far a thing can move start this far inside its outline (mm), so a
# corner that lies on a part's surface does not send its ray along that surface.
GRAZE_MM = 1e-3


def _part(env: CheckEnv, name: str):
    return next(part for part in env.parts if part.name == name)


def _cavity_min(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    import manifold3d

    part = _part(env, requirement.part)
    mesh = env.mesh(part)
    if mesh is None:
        return None, f"it could not be measured because {env.no_mesh_reason}", {}
    solid, error = mesh.manifold()
    if solid is None:
        return None, f"it could not be measured because {error}", {}
    if solid.num_tri() > MAX_CAVITY_TRIANGLES:
        return None, "the part has too many triangles for the free space to be measured", {}
    low = np.array(solid.bounding_box()[:3])
    high = np.array(solid.bounding_box()[3:])
    free = manifold3d.Manifold.cube(tuple(high - low)).translate(tuple(low)) - solid
    size = np.array(requirement.size, dtype=float)
    wanted = "{:g} x {:g} x {:g} mm".format(*size)

    def room(probe: np.ndarray):
        return free.minkowski_difference(manifold3d.Manifold.cube(tuple(probe), True))

    data = {"size": size.tolist(), "bounding_box": (high - low).tolist()}
    if free.is_empty():
        # (manifold3d erodes nothing to the probe itself, so this is asked first.)
        return False, (f"no {wanted} cuboid fits inside '{part.name}': the part fills its "
                       "bounding box, there is no free space inside it"), data
    if not room(size - CAVITY_TOLERANCE_MM).is_empty():
        return True, f"a {wanted} cuboid fits inside '{part.name}'", data
    # What does fit: each size in turn let go, the other two kept.
    at_most = {}
    for axis, letter in enumerate("xyz"):
        probe = size - CAVITY_TOLERANCE_MM
        probe[axis] = CAVITY_TOLERANCE_MM
        places = room(probe)
        if places.is_empty():
            at_most[letter] = 0.0
            continue
        spans = [piece.bounding_box() for piece in places.decompose()]
        at_most[letter] = max(box[axis + 3] - box[axis] for box in spans) + CAVITY_TOLERANCE_MM
    data["fits_at_most"] = at_most
    hints = ", ".join(
        f"{value:.1f} mm along {letter.upper()}"
        for letter, value, asked in zip("xyz", at_most.values(), size, strict=True)
        if 0.0 < value < asked
    )
    return False, (
        f"no {wanted} cuboid fits inside '{part.name}' (something stands in the free space)"
        + (f"; with the other two sizes kept, at most about {hints} fits" if hints else "")
    ), data


def _outer_max(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    part = _part(env, requirement.part)
    actual = [float(v) for v in tuple(part.shape.bounding_box().size)]
    over = [f"{value - limit:.2f} mm too large along {letter.upper()}"
            for letter, value, limit in zip("xyz", actual, requirement.size, strict=True)
            if value > limit + OUTER_TOLERANCE_MM]
    data = {"size": list(requirement.size), "actual": actual}
    outside = "{:.1f} x {:.1f} x {:.1f} mm".format(*actual)
    if over:
        return False, f"'{part.name}' is {outside} outside: {', '.join(over)}", data
    return True, f"'{part.name}' is {outside} outside, within the limit", data


def _wall_min(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    from .min_wall import min_wall

    options = env.options.model_copy(update={"min_wall": requirement.value})
    result = min_wall(dataclasses.replace(env, options=options), _part(env, requirement.part))
    data = {"value": requirement.value, **result.data}
    text = result.message[0].lower() + result.message[1:].rstrip(".")
    if result.status == SKIPPED:
        return None, text.removeprefix("skipped: "), data
    return result.status != FAIL, text, data


def _fit(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    from .fit_pairs import fit_report

    a, b = requirement.between
    what = (f"{requirement.card.replace('_', ' ')} card with a "
            f"{requirement.fit_class.replace('_', ' ')} fit between '{a}' and '{b}'")
    report = fit_report(env)
    joints = [] if report is None else [
        joint for joint in report.joints
        if joint.card.kind == requirement.card
        and {joint.hosts["a"], joint.hosts["b"]} == {a, b}
    ]
    matching = []
    for joint in joints:
        pairs = [m for m in report.measurements if m.pair.owner is joint.card]
        if any(m.pair.clearance.fit_class == requirement.fit_class for m in pairs):
            matching.append((joint, pairs))
    if not matching:
        return False, f"the model has no {what}", {"cards": []}
    data = {"cards": [
        {"instance": joint.card.name, "joint_ok": joint.ok,
         "pairs": len(pairs), "pairs_failed": sum(not m.ok for m in pairs),
         "clearance_provenance": sorted({m.pair.clearance.provenance for m in pairs})}
        for joint, pairs in matching
    ]}
    good = [joint for joint, pairs in matching if joint.ok and all(m.ok for m in pairs)]
    if not good:
        return False, f"the {what} is there but fails `fit_pairs` (see those results)", data
    return True, (f"the {what} is built by '{good[0].card.name}' and all its clearances "
                  "measure as resolved"), data


def _reference_fit(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    from .reference_fit import pocket_reports

    what = f"pocket for '{requirement.reference}'"
    reports = [report for report in pocket_reports(env)
               if report.pocket.reference.name == requirement.reference]
    if requirement.fit_class:
        what = f"{requirement.fit_class} {what}"
        reports = [r for r in reports if r.pocket.fit == requirement.fit_class]
    data = {"pockets": [
        {"pocket": r.pocket.name, "fit_class": r.pocket.fit, "status": r.status,
         "target": r.pocket.clearance.target,
         "clearance_provenance": r.pocket.clearance.provenance}
        for r in reports
    ]}
    if not reports:
        return False, f"the model has no {what}", data
    failed = [r for r in reports if not r.ok]
    if failed:
        return False, (f"the {what} ('{failed[0].pocket.name}') is there but fails "
                       "`reference_fit` (see that result)"), data
    first = reports[0].pocket
    gap = (f"{first.clearance.target:.2f} mm free per side once printed"
           if first.clearance.target >= 0 else
           f"{-first.clearance.target:.2f} mm of grip per side once printed")
    return True, (f"{len(reports)} {first.fit} pocket(s) for '{requirement.reference}' keep "
                  f"{gap}, as the fit resolver asked, and the way in and out is clear"), data


def _first_hit(solid, origin: np.ndarray, direction: np.ndarray, reach: float) -> float | None:
    """How far from `origin` along `direction` a ray first runs into the material of
    `solid`: the nearest place where it enters. A ray that starts on a surface and leaves
    the material there (a thing resting on the floor, seen from the other side) is not
    stopped by it; one that starts inside the material is stopped at once (0)."""
    hits = solid.ray_cast(tuple(float(c) for c in origin),
                          tuple(float(c) for c in origin + direction * reach))
    found = sorted(
        (max(0.0, float(np.dot(np.asarray(hit.position) - origin, direction))),
         float(np.dot(np.asarray(hit.normal), direction)))
        for hit in hits)
    for distance, facing in found:
        if facing < -1e-6:
            return distance
        if facing > 1e-6 and distance > TOUCH_MM:
            return 0.0
    return None


def thing_travel(body, vertices: np.ndarray, solids: list, part_vertices: np.ndarray,
                 direction: np.ndarray) -> float | None:
    """How far a thing (`body`: its closed mesh as a manifold3d.Manifold, `vertices`: its
    corners) can move along `direction` before it meets one of `solids` (the parts as
    manifolds, in the same frame; `part_vertices`: their corners): the shortest of the
    rays from the thing's corners to the parts and from the parts' corners back to the
    thing. None when nothing is in its way. Two polyhedra first meet corner to face or
    edge to edge; the rays find the first exactly and the second to within the size of
    the facets, which on a rounded edge drawn with short pieces is a few hundredths.
    A thing that only slides along a surface it lies on is not stopped by it: the rays
    start GRAZE_MM inside the thing's outline."""
    span = float(np.linalg.norm(np.ptp(np.concatenate([vertices, part_vertices]), axis=0)))
    reach = span + 1.0
    best: float | None = None
    low, high = vertices.min(axis=0), vertices.max(axis=0)
    across = np.abs(direction) < 0.5
    inwards = np.where(across, np.sign((low + high) / 2.0 - vertices) * GRAZE_MM, 0.0)
    for vertex in vertices + inwards:
        for solid in solids:
            found = _first_hit(solid, vertex, direction, reach)
            if found is not None and (best is None or found < best):
                best = found
    # A corner of a part that lies ahead of the thing, inside its shadow.
    along = part_vertices @ direction
    near = (along >= float((vertices @ direction).min()) - 1e-9)
    near &= ((part_vertices >= low + GRAZE_MM)
             & (part_vertices <= high - GRAZE_MM))[:, across].all(axis=1)
    for vertex in part_vertices[near]:
        found = _first_hit(body, vertex, -direction, reach)
        if found is not None and (best is None or found < best):
            best = found
    return best


def _standoff(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    from hotend_fit.contract import AXES
    from hotend_fit.reference import _matrix_of

    from ..context import MissingFit

    resolver = env.ctx.fit
    pockets = [] if isinstance(resolver, MissingFit) else [
        pocket for pocket in getattr(resolver, "pockets", None) or []
        if pocket.reference.name == requirement.reference and pocket.at is not None]
    if not pockets:
        return False, (f"the model placed no pocket for '{requirement.reference}', so "
                       "nothing says where it sits"), {}
    pocket = pockets[0]
    reference = pocket.reference
    direction = np.array(AXES[requirement.side])
    thing = f"'{reference.label}'"
    face_level = float((reference.vertices @ direction).max())
    if requirement.feature:
        component = (getattr(resolver, "components", None) or {}).get(requirement.reference)
        feature = component.features.get(requirement.feature) if component else None
        proud = feature.solid if feature is not None else None
        if proud is None:
            return False, (f"'{requirement.reference}' has no feature "
                           f"'{requirement.feature}' that stands proud"), {}
        box = proud.bounding_box()
        corners = np.array([[x, y, z] for x in (box.min.X, box.max.X)
                            for y in (box.min.Y, box.max.Y) for z in (box.min.Z, box.max.Z)])
        face_level = float((corners @ direction).max())
        thing = f"the {feature.label} of {thing}"
    body = reference._solid()
    if body is None:
        return None, f"the mesh of '{requirement.reference}' is not closed", {}
    rotation, translation = _matrix_of(pocket.at)
    solids, corners, own, own_solid = [], [], None, None
    for part in env.parts:
        mesh = env.mesh(part)
        if mesh is None:
            return None, f"it could not be measured because {env.no_mesh_reason}", {}
        solid, error = mesh.manifold()
        if solid is None:
            return None, f"it could not be measured because {error}", {}
        # The parts in the reference's own frame.
        local = (np.asarray(mesh.vertices, dtype=np.float64) - translation) @ rotation
        matrix = np.column_stack([rotation.T, -rotation.T @ translation])
        solids.append(solid.transform(np.ascontiguousarray(matrix, dtype=np.float64)))
        corners.append(local)
        if part.name == requirement.part:
            own, own_solid = local, solids[-1]
    travel = thing_travel(body, reference.vertices, solids, np.concatenate(corners), direction)
    side = requirement.side.upper()
    data = {"reference": requirement.reference, "side": requirement.side,
            "feature": requirement.feature, "value": requirement.value,
            "at_most": requirement.at_most, "travel": travel,
            "thing_face": face_level, "part_face": float((own @ direction).max())}
    if travel is None:
        return False, (f"nothing holds '{reference.label}' on its {side} side, so "
                       f"'{requirement.part}' gives it no stand-off there"), data
    if requirement.region is not None:
        return _standoff_in_region(requirement, own_solid, direction, face_level, travel,
                                   float((own @ direction).max()), thing, data)
    standoff = data["part_face"] - face_level - travel
    data["standoff"] = standoff
    moved = (f"pushed {travel:.2f} mm towards {side}, as far as the parts let it go"
             if travel > 0.005 else f"resting against the parts on its {side} side")
    text = (f"with '{reference.label}' {moved}, '{requirement.part}' stands "
            f"{standoff:.2f} mm beyond {thing}")
    if standoff < requirement.value - 1e-6:
        return False, f"{text}; at least {requirement.value:g} mm is asked", data
    if requirement.at_most is not None and standoff > requirement.at_most + 1e-6:
        return False, f"{text}; at most {requirement.at_most:g} mm is allowed", data
    upper = f" and at most {requirement.at_most:g} mm" if requirement.at_most is not None else ""
    return True, f"{text} (at least {requirement.value:g} mm{upper} asked)", data


def _standoff_in_region(requirement, solid, direction: np.ndarray, face_level: float,
                        travel: float, outermost: float, thing: str,
                        data: dict) -> tuple[bool | None, str, dict]:
    """The stand-off over a rectangle across the side: rays come in from outside on a
    grid (REGION_SPACING_MM apart, the rectangle's edges included) and the first surface
    of the part each one meets is the part's face there."""
    axis = int(np.argmax(np.abs(direction)))
    others = [index for index in range(3) if index != axis]
    (a0, b0), (a1, b1) = requirement.region
    levels, missed, total = [], 0, 0
    start = outermost + 1.0
    for a in np.linspace(a0, a1, max(2, round((a1 - a0) / REGION_SPACING_MM) + 1)):
        for b in np.linspace(b0, b1, max(2, round((b1 - b0) / REGION_SPACING_MM) + 1)):
            origin = np.zeros(3)
            origin[others[0]], origin[others[1]] = a, b
            origin += direction * start
            total += 1
            found = _first_hit(solid, origin, -direction, start + 1000.0)
            if found is None:
                missed += 1
            else:
                levels.append(start - found)
    data.update({"region": [list(corner) for corner in requirement.region], "rays": total,
                 "open": missed})
    where = f"'{requirement.part}' over the stated region"
    if not levels:
        return False, f"{where} has no material beyond {thing}", data
    thinnest = min(levels) - face_level - travel
    thickest = max(levels) - face_level - travel
    data.update({"standoff": thinnest, "standoff_max": thickest})
    text = (f"{where} stands {thinnest:.2f} mm beyond {thing}"
            + (f" at its thinnest and {thickest:.2f} mm at its thickest"
               if thickest - thinnest > 0.005 else " everywhere"))
    if missed:
        return False, (f"{text}, but it is open at {missed} of {total} places measured, "
                       "so the region is not covered"), data
    if thinnest < requirement.value - 1e-6:
        return False, f"{text}; at least {requirement.value:g} mm is asked", data
    if requirement.at_most is not None and thickest > requirement.at_most + 1e-6:
        return False, f"{text}; at most {requirement.at_most:g} mm is allowed", data
    upper = f" and at most {requirement.at_most:g} mm" if requirement.at_most is not None else ""
    return True, f"{text} (at least {requirement.value:g} mm{upper} asked)", data


def _dimension(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    return None, (f"a dimension named in words ('{requirement.name}') has no place on the "
                  "solid that Hotend could find, so it cannot be measured; to have it "
                  "checked, state what must hold as `cavity_min`, `outer_max`, `wall_min`, "
                  "`standoff` or `fit`, or measure it yourself with the measure tool and say "
                  "so"), {}


def _no_measure(env: CheckEnv, requirement) -> tuple[bool | None, str, dict]:
    return None, (f"Hotend has no measurement for a requirement of kind "
                  f"`{requirement.kind}`"), {}


_MEASURES = {"cavity_min": _cavity_min, "outer_max": _outer_max, "wall_min": _wall_min,
             "fit": _fit, "dimension": _dimension, "reference_fit": _reference_fit,
             "standoff": _standoff}


def contract_questions(env: CheckEnv) -> list[dict]:
    """The questions for the user that the contract leaves open (result `questions`)."""
    if env.contract is None:
        return []
    from hotend_fit.contract import open_questions

    questions = [question.model_dump(mode="json")
                 for question in open_questions(env.contract, env.param_specs)]
    asked = {question["requirement"] for question in questions}
    # A fit is only as sure as the clearances it was drawn with. On a printer that was
    # never calibrated the resolver hands out guessed ones, so the requirement rests on a
    # guess whatever tag the script gave it ("calibrated" says where the number should
    # come from, not that this printer has been measured).
    for requirement in env.contract.requirements:
        if requirement.id in asked or requirement.kind not in ("fit", "reference_fit"):
            continue
        # A fit to a bought part (`standard`: a screw, an insert) keeps its own verdict.
        if requirement.kind == "fit" and requirement.provenance != "calibrated":
            continue
        if "guessed" not in _clearance_provenances(env, requirement):
            continue
        if requirement.kind == "fit":
            text = (f"This printer has not been calibrated, so {requirement.subject()} is "
                    "drawn with guessed clearances. Print the fit gauge and say whether it "
                    "works, is too tight or too loose.")
        else:
            text = (f"Nobody has tried {requirement.subject()} with the real thing yet. "
                    "Print the ring gauge and say whether the thing goes in the way you "
                    "want, is too tight or too loose.")
        questions.append({"requirement": requirement.id, "text": text, "guessed_params": []})
    return questions


def _clearance_provenances(env: CheckEnv, requirement) -> set[str]:
    """Where the clearances behind a `fit` or `reference_fit` requirement come from."""
    from ..context import MissingFit

    found: set[str] = set()
    try:
        if requirement.kind == "reference_fit":
            resolver = env.ctx.fit
            if isinstance(resolver, MissingFit):
                return found
            for pocket in getattr(resolver, "pockets", None) or []:
                if pocket.reference.name == requirement.reference and (
                        requirement.fit_class in (None, pocket.fit)):
                    found |= {pocket.clearance.provenance, pocket.clearance.target_provenance}
            return found
        from .fit_pairs import fit_report

        report = fit_report(env)
        if report is None:
            return found
        for joint in report.joints:
            if joint.card.kind != requirement.card or (
                    {joint.hosts["a"], joint.hosts["b"]} != set(requirement.between)):
                continue
            pairs = [m.pair for m in report.measurements if m.pair.owner is joint.card]
            if any(pair.clearance.fit_class == requirement.fit_class for pair in pairs):
                found |= {pair.clearance.provenance for pair in pairs}
    except Exception:  # noqa: BLE001 - a fit that cannot be read is reported by its own check
        return found
    return found


def contract(env: CheckEnv) -> list[CheckResult]:
    if not env.options.contract and "contract" not in (env.options.only or ()):
        return []  # not asked for: no result at all, so older callers see no new line
    if env.contract is None:
        return [CheckResult("contract", SKIPPED, ERROR,
                            "Skipped: the model declares no CONTRACT.")]
    questions = {question["requirement"]: question for question in contract_questions(env)}
    results = []
    for requirement in env.contract.requirements:
        crashed: BaseException | None = None
        try:
            met, text, data = _MEASURES.get(requirement.kind, _no_measure)(env, requirement)
        except Exception as exc:  # noqa: BLE001 - OCCT and manifold3d raise unrelated types
            met, text, data = None, "it could not be measured", {}
            crashed = exc
        question = questions.get(requirement.id)
        parts = requirement.parts_named()
        data = round_floats({
            "kind": "requirement", "requirement": requirement.id,
            "requirement_kind": requirement.kind, "provenance": requirement.provenance,
            "params": list(requirement.params), "parts": list(parts), "met": met,
            "question": question, **data,
        })
        label = f"Requirement '{requirement.id}' ({requirement.provenance})"
        if met is None and crashed is None:
            # Not checkable: said, with the reason, and never counted as checked.
            data = {**data, "not_checkable": True, "reason": text}
        if question is not None:
            verdict = {True: "is met", False: "is NOT met", None: "is not checkable"}[met]
            if crashed is not None:
                verdict = "could not be measured (a fault in Hotend)"
            status = WARN
            message = f"{label} {verdict}: {text}. It rests on a guess: {question['text']}"
            if met is None and crashed is None:
                message = f"Not checkable: {message}"
        elif crashed is not None:
            # Not measured because Hotend failed is not proven.
            status = FAIL
            message = (f"Hotend could not measure {label.lower()}, so it is not proven. "
                       "The version is held back until the check runs. This is a fault in Hotend, not a verdict on the model: changing the model and building again may get past it.")
            data = {**data, "could_not_run": True,
                    "exception": f"{type(crashed).__name__}: {str(crashed)[:200]}"}
        elif met is None:
            status = SKIPPED
            message = f"Not checkable: {label.lower()} cannot be checked: {text}."
        elif met:
            status, message = PASS, f"{label} is met: {text}."
        else:
            status, message = FAIL, f"{label} is not met: {text}."
        results.append(CheckResult(
            "contract", status, ERROR, message, parts[0] if len(parts) == 1 else None, data,
        ))
    if not results:
        results.append(CheckResult("contract", SKIPPED, ERROR,
                                   "Skipped: the CONTRACT lists no requirements."))
    return results
