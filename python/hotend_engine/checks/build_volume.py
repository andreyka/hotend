"""fits_build_volume: does each part fit the reach of the printer's nozzles?

The reach of every nozzle comes from the request's printer context; nothing about a printer is
hard-coded here. Sizes are compared, not positions: a part is one printed piece that may lie
anywhere on the bed and be turned about Z.

What is compared depends on who prints the part (`checks.orientation`):

- `as_modelled` (the default): the part's bounding box as drawn, or a quarter turn about Z
  further, against the full reach. That is all a caller that does not run print preparation
  can be told.
- `prepared`: the part the way print preparation prints it. For every nozzle the orientation
  is chosen exactly as the `prepare` op chooses it for a part given to that nozzle, the part
  is turned, and its size is held against the reach less the margin preparation keeps at the
  edges of the bed. So a part passes here when, and only when, preparation can lay it out
  with its default margin: one that is too long as drawn but fits standing another way up
  passes, and the message says how it is printed; one that fits the bare reach but not the
  margin fails.

Both use `prepare.reach_spare`, the one rule for "fits the reach".
"""

from __future__ import annotations

from .. import prepare
from ..parts import NamedPart
from .base import ERROR, FAIL, PASS, SKIPPED, WARN, CheckEnv, CheckResult, round_floats


def _nozzle_name(nozzle) -> str:
    return f"{nozzle.role} nozzle" if nozzle.role else f"nozzle {nozzle.id}"


def _row(nozzle, spare: tuple[float, float, float], quarter: bool) -> dict:
    return {
        "nozzle_id": nozzle.id,
        "role": nozzle.role,
        "name": _nozzle_name(nozzle),
        "reach": list(nozzle.reach.size),
        "fits": prepare.spare_fits(spare),
        "needs_quarter_turn": quarter,
        "spare": list(spare),
    }


def fits_build_volume(env: CheckEnv, part: NamedPart) -> CheckResult:
    printer = env.ctx.printer
    nozzles = [n for n in (printer.nozzles if printer else ()) if n.reach is not None]
    if not nozzles:
        return CheckResult(
            "fits_build_volume", SKIPPED, ERROR,
            "Skipped: the request names no printer nozzle reach to compare against.", part.name,
        )
    box = part.shape.bounding_box()
    drawn = tuple(float(v) for v in tuple(box.size))
    as_prepared = env.options.orientation == "prepared"
    margin = prepare.DEFAULT_MARGIN_MM if as_prepared else 0.0
    rows = []
    for nozzle in nozzles:
        room = prepare.reach_room(nozzle.reach.size, margin)
        if not as_prepared:
            rows.append(_row(nozzle, *prepare.reach_spare(drawn, room)))
            continue
        choice = env.prepared(part, nozzle)[2]
        # Not turned, it has the size it was drawn with (and no second bounding box).
        size = (drawn if prepare.describe_up(choice.up) == "as modelled"
                else env.prepared_size(part, nozzle))
        row = _row(nozzle, *prepare.reach_spare(size, room))
        row.update({"source": choice.source, "up": [float(v) for v in choice.up],
                    "turned": prepare.describe_up(choice.up), "size": list(size)})
        as_drawn = prepare.reach_spare(drawn, room)[0]
        if not row["fits"] and not prepare.spare_fits(as_drawn):
            # No way up fits (the choice takes one that fits whenever there is one). Say
            # by how much the part as drawn is too large: that is the size the user knows.
            row.update({"spare": list(as_drawn), "needs_quarter_turn": False,
                        "turned": "as modelled"})
        rows.append(row)
    data = {"size": list(drawn), "margin": margin, "nozzles": rows}
    main = printer.main_nozzle
    first = next((r for r in rows if main is not None and r["nozzle_id"] == main.id), rows[0])
    if as_prepared:
        choice = env.prepared(part)[2]
        data["orientation"] = {
            "source": choice.source, "up": [float(v) for v in choice.up],
            "turned": prepare.describe_up(choice.up), "explanation": choice.sentence,
        }
    data = round_floats(data)
    fitting = [r for r in rows if r["fits"]]
    size_text = f"{drawn[0]:.1f} x {drawn[1]:.1f} x {drawn[2]:.1f} mm"
    kept = f"the {margin:g} mm kept free at each edge of the bed"

    def describe(row: dict) -> str:
        axis = min(range(3), key=lambda i: row["spare"][i])
        how = row.get("turned", "as modelled")
        ways = ([] if how == "as modelled" else [f"printed turned {how}"]) + (
            ["turned a quarter turn on the bed"] if row["needs_quarter_turn"] else [])
        # The margin is kept in X and Y only; say so when that is where the part is tight.
        edge = f" beyond {kept}" if as_prepared and axis < 2 else ""
        return (f"Fits the {row['name']}'s reach{' when ' if ways else ''}{' and '.join(ways)} "
                f"with {row['spare'][axis]:.1f} mm to spare in {'XYZ'[axis]}{edge}.")

    def too_big(row: dict) -> str:
        axis = min(range(3), key=lambda i: row["spare"][i])
        over = f"by {-row['spare'][axis]:.1f} mm in {'XYZ'[axis]}"
        reach = f"reach {row['reach'][0]:g} x {row['reach'][1]:g} x {row['reach'][2]:g} mm"
        if not as_prepared:
            return f"too large for the {row['name']} {over} ({reach})"
        how = row["turned"] if row["turned"] == "as modelled" else f"turned {row['turned']}"
        return (f"too large for the {row['name']} whichever way up it is printed ({over} "
                f"{how}; {reach} less {kept})")

    if not fitting:
        return CheckResult(
            "fits_build_volume", FAIL, ERROR,
            f"The part is {size_text}: " + "; ".join(too_big(r) for r in rows)
            + ". Make it smaller or split it into pieces.",
            part.name, data,
        )
    if len(fitting) < len(rows):
        row = first if first["fits"] else fitting[0]
        return CheckResult(
            "fits_build_volume", WARN, ERROR,
            describe(row) + " It is " + "; ".join(too_big(r) for r in rows if not r["fits"]) + ".",
            part.name, data,
        )
    return CheckResult("fits_build_volume", PASS, ERROR, describe(first), part.name, data)
