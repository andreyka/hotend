"""A soft figure in a steamer basket: the basket and its lift-off cover.

The dumpling is a reference object (`ctx.references["dumpling"]`): every size of the
holder is derived from it, so the same script re-fits to another dumpling. Three printed
pieces: `basket` (a deep basket drawn as stacked steamer tiers: slatted floor, banded
wall, a locating lip inside the rim; `show` says how much of the dumpling stands above the
edge), `lid` (the cover: a rim band and a ringed dome with a woven top; it sits on
the rim around the lip and lifts off in one piece; over a shallow basket its wall is drawn
as further tiers) and `knob` (pressed into the lid).
"""

import math

from build123d import *
from hotend_fit import cards

PARAMS = {
    "count": {"default": 1, "min": 1, "max": 4, "unit": "count",
              "label": "How many dumplings", "provenance": "style"},
    "snug": {"default": 0, "min": 0, "max": 1, "unit": "count",
             "label": "How snug (0 = roomy, drops in freely; 1 = close)",
             "provenance": "guessed"},
    "show": {"default": 0.29, "min": 0.0, "max": 0.75, "unit": "ratio",
             "label": "Share of the dumpling that stands above the basket's edge "
                      "(small = deep basket)",
             "provenance": "style"},
    "wall": {"default": 3.2, "min": 3.0, "max": 4.8, "unit": "mm",
             "label": "Wall thickness", "provenance": "style"},
    "stack": {"default": 1, "min": 0, "max": 1, "unit": "count",
              "label": "The wall is drawn as stacked baskets (1) or is one plain wall (0)",
              "provenance": "style"},
    "lid_lift": {"default": 0.0, "min": 0.0, "max": 80.0, "unit": "mm",
                 "label": "Lift the cover (to look inside)", "provenance": "style"},
    "lid_aside": {"default": 0, "min": 0, "max": 1, "unit": "count",
                  "label": "Set the cover down beside the basket (1)", "provenance": "style"},
}

CONTRACT = {
    "protocol": 0,
    "name": "Steamer basket for a soft figure",
    "parts": ["basket", "lid", "knob"],
    "requirements": [
        {"id": "dumpling_drops_in_freely", "kind": "reference_fit", "reference": "dumpling",
         "provenance": "guessed", "params": ["snug"],
         "note": "a soft thing in a rigid holder: visible air all round, never squeezed"},
        {"id": "knob_presses_into_lid", "kind": "fit", "card": "peg_hole",
         "between": ["lid", "knob"], "fit_class": "press", "provenance": "calibrated",
         "note": "from the printer's calibration; a guess for as long as it has none"},
        {"id": "basket_wall_prints", "kind": "wall_min", "part": "basket", "value": 0.8,
         "provenance": "standard", "params": ["wall"],
         "note": "two passes of the 0.4 mm nozzle"},
        {"id": "lid_wall_prints", "kind": "wall_min", "part": "lid", "value": 0.8,
         "provenance": "standard", "params": ["wall"],
         "note": "two passes of the 0.4 mm nozzle"},
    ],
}

ASSEMBLY = {
    "lid": {"mate": "basket.rim", "note": "lifts off; located by the lip inside the rim"},
    "knob": {"mate": "lid.top", "note": "pressed in once"},
}

# The lid and the knob print upside down: flat top on the bed, no support.
PRINT = {"lid": {"up": "-z"}, "knob": {"up": "-z"}}

# Style constants (mm). None of them is a clearance.
FLOOR = 2.4        # slat thickness: 12 layers of 0.2 mm
SLAT = 7.0         # width of a floor slat
SLOT = 3.0         # opening between two slats
TIE = 5.0          # width of the two cross battens that tie the slats together
BAND = 1.2         # how far a band stands out of the wall
LIP_T = 1.2        # locating lip inside the rim: three passes of the nozzle
LIP_H = 3.6        # its height
TOP = 2.4          # thickness of the lid's top
EDGE = 0.6         # bevel on edges that touch the print bed, and the line between two tiers
SLOPE = 1.2        # rise per mm of overhang: steeper than 45 degrees, prints without support
DOME_SLOPE = 1.4   # the dome as a whole: each ring is a SLOPE ramp and a small upright step
RING = 4.8         # height of one ring of the dome
DOME = 0.22        # the dome rises this share of the outside diameter (a real lid: 0.2 to 0.25)
DOME_MIN = 0.08    # ... and at least this much, however deep the basket
TIER = 0.185       # a tier is about this share of the outside diameter tall (real: 0.15 to 0.22)
TIER_MIN = 8.0     # a tier lower than this is not drawn
SEAM = 0.4         # depth of the lap seam line in a plain wall
PEG_D = 6.0        # knob peg (the peg_hole card sizes the hole)
PEG_L = 5.4        # its length: the lid is this thick under the knob
WEAVE_PITCH = 7.0  # woven lattice engraved on the lid's top (it grows with a large lid)
WEAVE_STRIP = 4.4  # width of one woven strip
WEAVE_BAR = 0.8    # a square bar on edge cuts a V 0.57 mm deep and 1.1 mm wide
WEAVE_GAP = 1.0    # a strip's edge stops this far short of the strip it dives under
WOOD = "#b07a3f"
DARK_WOOD = "#7a4a22"


def _turned(points):
    """A solid of revolution about Z from (radius, height) points, counter-clockwise."""
    return revolve(Plane.XZ * Polygon(*points), Axis.Z)


def _seats(count, radius):
    """Where the dumplings sit: one in the middle, or a ring of rooms that touch."""
    if count == 1:
        return [(0.0, 0.0)]
    ring = radius / math.sin(math.pi / count)
    start = 90.0 + (180.0 / count if count % 2 == 0 else 0.0)
    angles = [math.radians(start + 360.0 * k / count) for k in range(count)]
    return [(ring * math.cos(a), ring * math.sin(a)) for a in angles]


def _tier_bands(z0, z1):
    """The bands of one steamer tier between two heights: at the foot and at the rim, at
    the rim only when it is low, or one band when it is lower still."""
    height = z1 - z0
    band = min(7.0, max(2.8, 0.2 * height))
    chamfer = BAND * SLOPE
    if height - 2 * band - 2 * chamfer >= 2.0:
        return [(z0, z0 + band), (z1 - band, z1)]
    if height - band - chamfer >= 2.0:
        return [(z1 - band, z1)]
    return [(z0, z1)]


def _wall(z0, z1, bands, lines, r_plain, r_band, top_bevel):
    """The outside of a banded wall from z0 up to z1 as (radius, height) points, and the
    plain stretches between its bands. Every band grows out of the wall through a chamfer
    that prints without support both ways up; `lines` are heights where a fine V line
    runs round a band (the joint between two tiers); it prints either way up too."""
    chamfer = BAND * SLOPE
    merged = []
    for low, high in sorted(bands):
        if merged and low - merged[-1][1] < 2 * chamfer + 2.0:
            merged[-1][1] = max(merged[-1][1], high)
        else:
            merged.append([low, high])
    if merged[0][0] - z0 < chamfer + 2.0:
        merged[0][0] = z0
    if z1 - merged[-1][1] < chamfer + 2.0:
        merged[-1][1] = z1
    start = r_band if merged[0][0] == z0 else r_plain
    points = [(start - EDGE, z0), (start, z0 + EDGE * SLOPE)]
    plains = []
    level = z0
    for low, high in merged:
        if low > level:
            plains.append((level, low - chamfer))
            points += [(r_plain, low - chamfer), (r_band, low)]
        for line in lines:
            if low + 2 * EDGE <= line <= high - 2 * EDGE:
                points += [(r_band, line - EDGE * SLOPE), (r_band - EDGE, line),
                           (r_band, line + EDGE * SLOPE)]
        if high < z1:
            points += [(r_band, high), (r_plain, high + chamfer)]
            level = high + chamfer
        else:
            level = z1
    if level < z1:
        plains.append((level, z1))
        points.append((r_plain, z1))
    elif top_bevel:
        points += [(r_band, z1 - EDGE), (r_band - EDGE, z1)]
    else:
        points.append((r_band, z1))
    return points, plains


def _seam(radius, zones, angle):
    """The lap seam of the bamboo strip: a fine upright groove in each plain stretch."""
    return [Rot(0, 0, angle) * Pos(radius, 0, (z0 + z1) / 2) * Box(2 * SEAM, 1.0, z1 - z0 - 1.2)
            for z0, z1 in zones if z1 - z0 >= 3.0]


def build(p, ctx):
    dumpling = ctx.references["dumpling"]
    tall = dumpling.size[2]
    fit_class = "cradle" if p.snug == 0 else "loose"
    # The room follows the dumpling's rounded shape (its convex hull plus the fit's gap),
    # so the dome can slope in over its shoulders like a real steamer lid.
    pockets = [dumpling.pocket(fit_class, ctx=ctx, shape="hull",
                               removable=("lid", "knob"), name=f"dumpling_{i + 1}")
               for i in range(p.count)]
    room = pockets[0]
    seat_x, seat_y, _ = room.center
    seats = _seats(p.count, room.radius)
    for pocket, (x, y) in zip(pockets, seats):
        pocket.place(Pos(x - seat_x, y - seat_y, FLOOR))

    # ---- sizes, all derived from the dumpling and its room ---------------------------
    # The inside of the wall is the circle around the room (or around all the rooms).
    r_in = room.radius + max(math.hypot(x, y) for x, y in seats)
    r_plain = r_in + p.wall
    r_band = r_plain + BAND
    # The edge one sees is the top of the locating lip: `show` of the dumpling is above it.
    z_edge = FLOOR + (1.0 - p.show) * tall
    z_rim = z_edge - LIP_H                                         # where the lid sits
    if z_rim < FLOOR + 3.0:
        raise ValueError("the basket would be lower than its own floor and lip; make "
                         "'show' smaller so the wall is taller")

    # Lid on the rim: a sliding fit around the lip, resolved for this printer.
    lip_fit = ctx.fit.clearance("slide", 2 * (r_in + LIP_T))
    lip_top = ctx.fit.clearance("loose", LIP_H, axis="z")
    r_lip = r_in + LIP_T - lip_fit.peg_delta
    r_lid_in = r_in + LIP_T + lip_fit.hole_delta
    lid_wall = r_plain - r_lid_in
    if lid_wall < 1.2:
        raise ValueError("the cover's wall would be thinner than 1.2 mm; make 'wall' larger")

    knob_fit = cards.peg_hole(diameter=PEG_D, length=PEG_L, fit="press", ctx=ctx,
                              through=True, name="knob_peg")
    boss = PEG_L - TOP                       # the lid is as thick as the peg is long there
    r_boss = knob_fit.hole_radius + 2.4

    # The cover stands on the rim, or is lifted, or stands on the table beside the basket.
    lift = -z_rim if p.lid_aside else p.lid_lift
    aside = Pos(2 * r_band + 12.0, 0, 0) if p.lid_aside else Pos(0, 0, 0)
    z_skirt = z_rim + lift
    # The cover is a straight wall, then a dome (a cone, ringed outside) that slopes in as
    # far as the dumpling's room allows, then a flat top. Its lowest point over the
    # dumpling is the boss under the knob: that, not the ceiling, stays above the room.
    # The cone starts where it clears every corner of the room and the lip.
    corners = [(v.X - seat_x, v.Y - seat_y, v.Z + FLOOR) for v in room.envelope.vertices()]
    reach = max(z + DOME_SLOPE * (math.hypot(x + sx, y + sy) - r_lid_in)
                for x, y, z in corners for sx, sy in seats)
    z_low = FLOOR + room.size[2] + lift
    z_ceiling = z_low + boss
    z_cone = max(z_skirt + LIP_H + lip_top.gap + 1.0, reach + lift,
                 z_ceiling - DOME * 2 * r_band)
    z_ceiling = max(z_ceiling, z_cone + DOME_MIN * 2 * r_band)
    z_cone = max(z_cone, z_ceiling - DOME_SLOPE * (r_lid_in - r_boss - 3.0))
    r_ceiling = r_lid_in - (z_ceiling - z_cone) / DOME_SLOPE
    z_top = z_ceiling + TOP
    r_flat = r_band - (z_top - z_cone) / DOME_SLOPE

    # ---- basket ---------------------------------------------------------------------
    # A deep basket is drawn as a stack of tiers about as tall as a real steamer's, each
    # with its own bands and seam; a fine line runs where two baskets would meet.
    stack = max(1, round(z_rim / (TIER * 2 * r_band))) if p.stack else 1
    if z_rim / stack < TIER_MIN:
        stack = max(1, int(z_rim // TIER_MIN))
    stack_h = z_rim / stack
    bands, lines = [], []
    for k in range(stack):
        bands += _tier_bands(k * stack_h, (k + 1) * stack_h)
        lines.append((k + 1) * stack_h)
    outside, zones = _wall(0.0, z_rim, bands, lines[:-1], r_plain, r_band, True)
    ring = _turned(
        [(r_in, 0.0)] + outside
        + [(r_lip, z_rim), (r_lip, z_rim + LIP_H - 0.4), (r_lip - 0.4, z_rim + LIP_H),
           (r_in, z_rim + LIP_H)])

    # Slatted floor, printed flat on the bed: slats along Y, tied by two cross battens.
    low_align = (Align.CENTER, Align.CENTER, Align.MIN)
    floor = Cylinder(r_in + p.wall / 2, FLOOR, align=low_align)
    pitch = SLAT + SLOT
    slots = int(2 * (r_in - 6.0) // pitch) + 1
    tie_y = 0.45 * r_in
    spans = [(-r_in - 1.0, -tie_y - TIE / 2), (-tie_y + TIE / 2, tie_y - TIE / 2),
             (tie_y + TIE / 2, r_in + 1.0)]
    openings = []
    for k in range(slots):
        x = (k - (slots - 1) / 2) * pitch
        for y0, y1 in spans:
            openings.append(Pos(x, (y0 + y1) / 2, FLOOR / 2) * Box(SLOT, y1 - y0, FLOOR + 2.0))
    basket = ring + (floor - openings)
    for k, zone in enumerate(zones):
        cuts = _seam(r_plain, [zone], -25 - 70 * k)
        if cuts:
            basket = basket - cuts

    # ---- lid: a rim band and the dome, over further tiers when the basket is shallow ----
    # Over a deep basket the straight wall is only the rim band. A taller one is divided
    # into tiers about as tall as the basket's, drawn the same way.
    lid_band = min(7.0, max(2.8, 0.2 * stack_h))
    straight = z_cone - z_skirt
    tiers = 0
    if p.stack and straight - lid_band >= TIER_MIN:
        tiers = max(1, round((straight - lid_band) / stack_h))
    tier_h = (straight - lid_band) / tiers if tiers else 0.0
    bands = [(z_cone - lid_band, z_cone)] if tiers else [(z_skirt, z_cone)]
    lines = []
    for k in range(tiers):
        bands += _tier_bands(z_skirt + k * tier_h, z_skirt + (k + 1) * tier_h)
        lines.append(z_skirt + (k + 1) * tier_h)
    skirt, lid_zones = _wall(z_skirt, z_cone, bands, lines, r_plain, r_band, False)

    # The dome: rings like coiled bamboo. Each ring is a ramp as steep as every other
    # overhang of the model and a small upright step, so it prints upside down without
    # support.
    rings = max(1, round((z_top - z_cone) / (RING * max(1.0, r_band / 50.0))))
    ring_h = (z_top - z_cone) / rings
    step = ring_h * (1.0 - SLOPE / DOME_SLOPE)
    dome = []
    for k in range(rings):
        radius = r_band - (k + 1) * (ring_h - step) / SLOPE
        dome += [(radius, z_cone + (k + 1) * ring_h - step), (radius, z_cone + (k + 1) * ring_h)]
    lid = _turned(
        [(0.0, z_ceiling - boss), (r_boss, z_ceiling - boss), (r_boss, z_ceiling),
         (r_ceiling, z_ceiling), (r_lid_in, z_cone), (r_lid_in, z_skirt)]
        + skirt + dome[:-1] + [(r_flat, z_top), (0.0, z_top)])
    for k, zone in enumerate(lid_zones):
        cuts = _seam(r_plain, [zone], 40 + 55 * k)
        if cuts:
            lid = lid - cuts

    # Knob proportions follow the lid's size.
    r_cap = min(14.0, max(7.5, 0.18 * r_band))
    r_neck = max(PEG_D / 2 + 1.5, 0.55 * r_cap)
    neck_h = 0.6 * r_cap
    flare = r_cap - r_neck
    cap_h = 0.3 * r_cap
    bevel = 0.25 * r_cap

    # Woven look on the flat top: strips that pass over and under one another, drawn by
    # their edges. A strip shows its two edges where it lies on top and stops short of the
    # strips it dives under. Clear of the knob and of the top's rim.
    r_weave = r_flat - 1.6
    keep = r_neck + 1.2
    grow = max(1.0, r_flat / (4.4 * WEAVE_PITCH))   # about nine strips across any lid
    pitch = WEAVE_PITCH * grow
    half = WEAVE_STRIP * grow / 2
    bar = WEAVE_BAR * min(grow, 2.0)
    dash = 2 * (pitch - half - WEAVE_GAP * grow)
    steps = int(r_weave // pitch) + 1
    grooves = []
    for i in range(-steps, steps + 1):
        for j in range(-steps, steps + 1):
            along = 0.0 if (i + j) % 2 == 0 else 90.0     # which strip lies on top here
            for side in (-half, half):
                ends = [(-dash / 2, side), (dash / 2, side), (0.0, side)]
                turned = [(u * math.cos(math.radians(along)) - v * math.sin(math.radians(along))
                           + i * pitch,
                           u * math.sin(math.radians(along)) + v * math.cos(math.radians(along))
                           + j * pitch) for u, v in ends]
                if any(not keep + bar < math.hypot(x, y) < r_weave - bar for x, y in turned):
                    continue
                x, y = turned[2]
                grooves.append(Rot(0, 0, 45) * Pos(x, y, z_top) * Rot(0, 0, along)
                               * Rot(45, 0, 0) * Box(dash, bar, bar))
    lid = lid - grooves

    # ---- knob -----------------------------------------------------------------------
    z_cap = z_top + neck_h + flare
    knob = _turned(
        [(0.0, z_top), (r_neck, z_top), (r_neck, z_top + neck_h), (r_cap, z_cap),
         (r_cap, z_cap + cap_h), (r_cap - bevel, z_cap + cap_h + bevel * SLOPE),
         (0.0, z_cap + cap_h + bevel * SLOPE)])

    # The knob's peg and its hole come from one card; nothing is reshaped afterwards.
    lid = knob_fit.apply_a(aside * lid, at=aside * Pos(0, 0, z_top))
    knob = knob_fit.apply_b(aside * knob, at=aside * Pos(0, 0, z_top))

    basket.color = Color(WOOD)
    lid.color = Color(WOOD)
    knob.color = Color(DARK_WOOD)

    closed = z_top - lift
    print(f"dumpling {dumpling.size[0]:.1f} x {dumpling.size[1]:.1f} x {tall:.1f} mm, "
          f"{fit_class} room per side {room.gap:.2f} mm (printed aim {room.clearance.target:.2f})")
    print(f"basket inside {2 * r_in:.1f} mm, outside {2 * r_band:.1f} mm, {stack} tier(s) of "
          f"{stack_h:.1f} mm, edge {z_edge:.1f} mm above the table, {z_edge - FLOOR:.1f} mm "
          f"({100 * (z_edge - FLOOR) / tall:.0f} %) up the dumpling: {FLOOR + tall - z_edge:.1f} mm "
          f"of it ({100 * (FLOOR + tall - z_edge) / tall:.0f} %) stands above the edge")
    print(f"lid inside {2 * r_lid_in:.2f} mm over a lip of {2 * r_lip:.2f} mm "
          f"(slide fit, {lip_fit.gap:.2f} mm per side), wall {lid_wall:.2f} mm, "
          f"{tiers} tier(s) of {tier_h:.1f} mm, dome {z_top - z_cone:.1f} mm with {rings} rings, "
          f"flat top {2 * r_flat:.1f} mm with {len(grooves)} weave grooves")
    print(f"closed: {closed:.1f} mm to the lid's top, "
          f"{closed + neck_h + flare + cap_h + bevel * SLOPE:.1f} mm with the knob; the "
          f"lid's lowest point (the boss under the knob) is {z_low - lift - FLOOR - tall:.2f} mm "
          f"above the dumpling's top, its ceiling {z_ceiling - lift - FLOOR - tall:.2f} mm")
    return {"basket": basket, "lid": lid, "knob": knob}
