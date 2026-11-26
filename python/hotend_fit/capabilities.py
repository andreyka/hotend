"""What a model can do with real things, in one place for the agent's docs tool.

The `docs` op indexes the classes and functions of `hotend_fit` by their docstrings; the
functions here exist so that a search for "reference", "phone", "TPU", "stretch" or "gauge"
finds a short, complete recipe. Each also returns the data it describes, so the text and
the code cannot drift apart.
"""

from __future__ import annotations

from typing import Any


def reference_objects() -> dict:
    """How to build a part around a reference object (a real thing given as an STL, 3MF or
    STEP file the user chose: a figure, a scanned part).

    The request lists it in `context.references` (name, path inside the job folder, units,
    scale, kind `rigid` or `soft`, and `plate`/`object` when the file holds several). The
    model gets it as `ctx.references["name"]`, a `hotend_fit.reference.Reference`:

        thing = ctx.references["dumpling"]
        thing.size, thing.bbox, thing.volume     # exact, in mm; provenance "measured"
        thing.hull()                             # convex hull as a Solid
        thing.section(20.0)                      # outline 20 mm above its lowest point
        thing.silhouette()                       # largest horizontal outline (Sketch)
        pocket = thing.pocket("cradle", ctx=ctx, shape="cylinder")
        part = pocket.cut(part, at=Pos(0, 0, floor))
        pocket.radius, pocket.depth, pocket.size, pocket.gap   # to size walls with

    It stands on z = 0 with the centre of its footprint on the Z axis. Never type a
    clearance: the pocket takes it from `ctx.fit` (`cradle` for a soft thing, `loose`,
    `slide`, `snug` for a rigid one, `stretch` for a flexible shell). The `reference_fit`
    check proves the room is free all round and that the thing goes in and out along the
    pocket's `direction`; name a lid in `removable`. The reference is shown as a ghost in
    the viewer and is never exported or printed. State it in CONTRACT as kind
    `reference_fit`. The result's `questions` ask about the file's size and a guessed
    room: pass them on. Returns the pocket shapes and fits.
    """
    from .reference import POCKET_FITS, SHAPES

    return {"shapes": list(SHAPES), "fits": list(POCKET_FITS)}


def bought_components() -> dict:
    """How to build a part around a bought device (a phone) whose shape comes from the
    maker's drawing: `hotend_fit.components`.

        phone = ctx.component("sample_phone")
        pocket = phone.reference.pocket("stretch", ctx=ctx)     # the body, corner profile included
        shell = Pos(0, 0, -back) * pocket.room(offset=pocket.offset + wall, top=back + lip)
        case = pocket.cut(shell, at=Pos(0, 0, 0))
        case -= phone.feature("usb_c").keep_out(reach=wall + 2)        # the maker's keep-out
        case -= phone.feature("side_button").opening(ctx, "loose", reach=wall + 2)
        phone.rule("max_back_thickness")                               # the maker's limit

    Frame of a phone: origin at the centre of the outline on the back, +Z to the screen,
    +Y to the top edge. Features (buttons, ports, camera, magnets) have `center`, `size`,
    `solid`, `keep_out()`, `opening(ctx)` and `value(name)`. Every number carries
    provenance, tolerance and a source URL. A feature with a number that is not verified
    has a `question`; using it puts the question in the result and a warning in
    `reference_fit`, so the model is not reported as fully proven. The check also fails a
    part that collides with a proud feature or leaves material in a keep-out the model
    asked for. `sample_phone` is an invented device, only for examples and tests. Returns
    the ids of the components.
    """
    from .components import list_components

    return {"components": list_components()}


def leaning_lips_and_stand_off() -> dict:
    """How to make a rim that leans over a rounded edge and prove what it gives: a phone
    case's rim above the screen glass, a back of uniform thickness.

    Where the maker states the section of the body's edge (`phone.edge_section()`: points
    (distance inwards, height above the back)), the body has rounded edges and
    `reference_fit` proves the room that follows them (`pocket.fitted`), so a lip can lean
    into the corner the rounded edge leaves:

        case -= pocket.taper(0.0, seat, 0.0)                    # the room up to the slope
        case -= pocket.taper(seat, seat + slope * run, run)     # the leaning underside
        pocket.place(at=Pos(0, 0, 0))

    `pocket.taper(low, high, inset)` is the outline between two heights with its upper end
    moved in corner for corner: flat faces, at an angle that prints without support (slope
    1.05 is 43.6 degrees from upright). Build bevels of the outline the same way: a
    chamfer over its many short edges makes the kernel compute the solid's volume wrongly
    (`valid_solid` warns). State what must stand beyond the thing in CONTRACT as kind
    `standoff` (`part`, `reference`, `side`, `value`, optional `at_most`, `feature`,
    `region`): the rim above the glass is `side: "+z"`, a back of uniform thickness is
    `side: "-z"` with `at_most` and a `region`. The check pushes the thing towards that
    side as far as the parts let it go before it measures, so a rim the phone slides under
    counts for no more than it gives. Returns the contract kind and the pocket methods.
    """
    return {"contract_kind": "standoff", "pocket": ["taper", "room", "fitted"]}


def flexible_materials() -> dict:
    """What Hotend assumes about flexible filament (TPU, Shore 95A and firmer) and how to
    design for it: `hotend_fit.materials`.

    `ctx.fit.material` gives the facts of the request's material: `flexible`,
    `wall_mm` (1.2 to 2.5 mm for TPU: thinner is floppy), `max_overhang_deg` (45),
    `stretch_strain` (how far a rim may be stretched to put the part on, 8 % of its
    width, a guess) and `nozzle`. TPU prints only on a direct-drive nozzle: on the Bambu
    Lab X2D that is the main nozzle, and the `prepare` op fails a TPU part given to the
    other one (`material_nozzle`). Only a flexible material may use the `stretch` fit;
    a stretch fit in PLA or PETG fails `reference_fit` with the reason. All of it is
    tagged `guessed` until the fit's gauge has been printed. Returns the material table.
    """
    from .materials import list_materials

    return {"materials": list_materials()}


def reference_fit_classes() -> dict:
    """The two fit classes for a printed part that holds a real thing, used through
    `reference.pocket(fit, ctx=ctx)`; see `hotend_fit.fit` for the numbers.

    - `stretch`: a flexible shell (a TPU phone case) drawn so that it prints slightly
      smaller than the thing and grips it. Flexible materials only. The check wants at
      least half of the thing's rim held and a lip no deeper than the material stretches.
    - `cradle`: a soft thing (a squishy figure) dropped into a rigid holder with visible
      room, about 1.5 mm plus 1 % of its size per side.

    The printed-pair classes `loose`, `slide` and `snug` also work for a rigid thing in a
    rigid part. Both reference classes are guesses until their gauge is printed. Returns
    each class with its target and window.
    """
    from .fit import REFERENCE_FIT_CLASSES

    return {name: {"base": spec.base, "per_mm": spec.per_mm, "meaning": spec.meaning,
                   "window": [spec.low, spec.high], "flexible_only": spec.flexible_only,
                   "provenance": spec.provenance}
            for name, spec in REFERENCE_FIT_CLASSES.items()}


def reference_fit_gauges(pocket: Any = None) -> dict:
    """The test print of a reference fit: `pocket.gauge()` in a model, or the `gauge` op
    with `{"card": "pocket", "args": {"reference": name, "fit": ..., "shape": ...}}`.

    It is a ring 5 mm tall with a 2 mm wall whose inside is the pocket's outline. For a
    `cradle` (or another clearance fit) the thing must drop through it without being
    squeezed. For a `stretch` fit it is a thin band of the case's rim: it must stretch
    over the thing and stay on when shaken. Print it before the product; its two inside
    widths are the dimensions to measure for the `calibrate` op. With a pocket given,
    returns that pocket's gauge dimensions.
    """
    if pocket is None:
        from .reference import GAUGE_HEIGHT_MM, GAUGE_WALL_MM

        return {"height": GAUGE_HEIGHT_MM, "wall": GAUGE_WALL_MM}
    return {"dimensions": pocket.gauge_set().dimensions}
