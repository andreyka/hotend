"""A flexible (TPU) case for a phone: one piece, printed back down with no support.

The phone is the component `sample_phone`, an invented device that stands in for a real
one: every size comes from its data, the cavity is a `stretch` pocket from the fit
resolver, and every opening is a keep-out or an opening of one of its features.
The numbers of the component that nobody has verified are parameters with the provenance
`guessed`: their defaults are the component's values, and a measurement moves the opening.

Frame: origin at the centre of the phone's outline on its back, +Z to the screen,
+Y to the top edge. The case is printed as modelled, its flat back on the bed. Nothing
hangs over in that position: the rim leans in over the phone's rounded front edge at less
than 45 degrees, and the long button openings are open towards the screen, so no strip of
wall has to bridge them.
"""

from math import radians, tan

from build123d import *

PARAMS = {
    "wall": {"default": 1.5, "min": 1.2, "max": 1.8, "unit": "mm",
             "label": "Side wall thickness", "provenance": "guessed"},
    "back": {"default": 2.0, "min": 1.6, "max": 2.4, "unit": "mm",
             "label": "Back thickness", "provenance": "standard"},
    "lip_height": {"default": 1.0, "min": 0.8, "max": 1.3, "unit": "mm",
                   "label": "Rim above the screen glass, phone pushed against the rim",
                   "provenance": "standard"},
    "lip_reach": {"default": 1.0, "min": 0.7, "max": 1.0, "unit": "mm",
                  "label": "Rim reach over the phone's front edge", "provenance": "guessed"},
    "button_height": {"default": 5.0, "min": 4.5, "max": 5.5, "unit": "mm",
                      "label": "Check: mode and volume buttons, centre above the back",
                      "provenance": "guessed"},
    "usb_c_height": {"default": 4.6, "min": 4.2, "max": 5.0, "unit": "mm",
                     "label": "Check: USB-C opening, centre above the back",
                     "provenance": "guessed"},
    "port_height": {"default": 4.0, "min": 3.5, "max": 4.5, "unit": "mm",
                    "label": "Check: holes beside USB-C, centre above the back",
                    "provenance": "guessed"},
    "receiver_from_top": {"default": 2.0, "min": 1.6, "max": 2.4, "unit": "mm",
                          "label": "Check: receiver slot, centre below the top edge",
                          "provenance": "guessed"},
    "window_end": {"default": 37.0, "min": 35.0, "max": 39.0, "unit": "mm",
                   "label": "Check: top edge to where the back is flat below the camera",
                   "provenance": "guessed"},
    "window_side": {"default": 3.0, "min": 2.5, "max": 3.5, "unit": "mm",
                    "label": "Check: side edge to where the back starts to rise at the camera",
                    "provenance": "guessed"},
    "window_curl": {"default": 32.0, "min": 30.0, "max": 34.0, "unit": "mm",
                    "label": "Check: top edge to where the raised area ends at the sides",
                    "provenance": "guessed"},
}

CONTRACT = {
    "protocol": 0,
    "name": "TPU case for the sample phone",
    "parts": ["case"],
    "requirements": [
        {"id": "phone_fit", "kind": "reference_fit", "reference": "sample_phone",
         "fit_class": "stretch", "provenance": "calibrated",
         "note": "The cavity is the resolver's stretch pocket around the stated outline."},
        {"id": "screen_rim", "kind": "standoff", "part": "case", "reference": "sample_phone",
         "side": "+z", "value": 0.7, "provenance": "standard", "params": ["lip_height"],
         "note": "The phone's rule: glass at least 0.7 off a flat surface. Measured with "
                 "the phone pushed against the rim, face down."},
        {"id": "back_thickness", "kind": "standoff", "part": "case", "reference": "sample_phone",
         "side": "-z", "value": 1.6, "at_most": 2.4, "provenance": "standard",
         "region": [[-30.0, -40.0], [30.0, 20.0]], "params": ["back"],
         "note": "The phone's rule: 2.4 at most, uniform. Measured over the square around "
                 "the 60 mm circle of the magnet ring (centre 75 from the top)."},
        {"id": "side_wall", "kind": "dimension", "part": "case",
         "name": "side wall thickness", "value": 1.5, "provenance": "guessed", "params": ["wall"],
         "note": "Published TPU cases use 1.4 to 2.5; 1.5 keeps the case slim."},
        {"id": "rim_reach", "kind": "dimension", "part": "case",
         "name": "rim reach over the phone's front edge", "value": 1.0, "provenance": "guessed",
         "params": ["lip_reach"],
         "note": "Measured from the phone's edge at the long sides. The receiver keep-out "
                 "and the cover glass (both 1.5 from the edge) limit it."},
        {"id": "left_buttons", "kind": "dimension", "part": "case",
         "name": "mode and volume buttons above the back", "value": 5.0,
         "provenance": "guessed", "params": ["button_height"]},
        {"id": "usb_c_position", "kind": "dimension", "part": "case",
         "name": "USB-C centre above the back", "value": 4.6,
         "provenance": "guessed", "params": ["usb_c_height"]},
        {"id": "bottom_holes", "kind": "dimension", "part": "case",
         "name": "bottom holes above the back", "value": 4.0,
         "provenance": "guessed", "params": ["port_height"]},
        {"id": "receiver_slot", "kind": "dimension", "part": "case",
         "name": "receiver slot below the top edge", "value": 2.0,
         "provenance": "guessed", "params": ["receiver_from_top"]},
        {"id": "camera_window", "kind": "dimension", "part": "case",
         "name": "camera window: end of the raised area below the top edge", "value": 37.0,
         "provenance": "guessed",
         "params": ["window_end", "window_side", "window_curl"],
         "note": "The phone marks an area just below the raised surround where a case must be "
                 "uniform; the window's lower corners are pulled up to keep the back whole "
                 "over it, which may cover the first millimetre of the surround's foot."},
        {"id": "thinnest_wall", "kind": "wall_min", "part": "case", "value": 0.8,
         "provenance": "style",
         "note": "Nothing thinner than two lines of a 0.4 mm nozzle."},
        {"id": "stays_slim", "kind": "outer_max", "part": "case", "size": [84.5, 134.5, 14.0],
         "provenance": "style",
         "note": "Phone 80 x 130 x 10 plus walls, back and rim."},
    ],
}

PRINT = {"case": {"up": "+z",
                  "note": "Flat back on the bed: no support, the rim prints last."}}

# Looks and print details with no fit behind them (mm).
BACK_BEVEL = 0.6      # bevel of the outer edge at the back (also against elephant's foot)
RIM_BEVEL = 0.6       # bevel of the outer edge of the rim
RIM_EASE = 0.4        # least bevel of the rim's inner top edge
RIM_SLOPE = 1.05      # rise per run of the rim's underside: 43.6 degrees from upright
SEAT_PLAY = 0.03      # the rim's underside clears the phone's shoulder by this much
WINDOW_BEVEL = 0.5    # bevel of the camera window's edge
WINDOW_LEAN = 0.9     # its run per rise: 42 degrees from upright, it faces the bed
WINDOW_CORNER = 2.5   # radius of the camera window's lower corners
AREA_MARGIN = 1.0     # the window's edge, bevel included, stays this far from the marked area
SEAT = 0.1            # the window and the wall wedges start this far inside the cavity wall


def _slab(pocket, offset, z0, z1):
    """The phone's outline grown by `offset`, as a prism from z0 to z1 (the same outline,
    corner for corner, as the leaning faces of `pocket.taper`)."""
    return pocket.taper(z0, z1, 0.0, offset=offset)


def _bevelled(pocket, offset, z0, z1, low, high):
    """The phone's outline grown by `offset` from z0 to z1 with its bottom and top rims
    bevelled at 45 degrees: three pieces over the same corners, every face flat. (A
    chamfer over the outline's many short edges leaves faces whose area the geometry
    kernel computes wrongly, and with them the volume.)"""
    body = _slab(pocket, offset, z0 + low, z1 - high)
    top = pocket.taper(z1 - high, z1, high, offset=offset)
    bottom = pocket.taper(z0, z0 + low, low, offset=offset).mirror(Plane.XY.offset(z0 + low / 2))
    return body + top + bottom


def _span(x0, x1, y0, y1, z0, z1):
    """A box between two corners."""
    return Pos((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2) * Box(
        abs(x1 - x0), abs(y1 - y0), abs(z1 - z0))


def _both(room, dx=0.0, dy=0.0, dz=0.0):
    """The component's own shape and, when a "Check" value moves it, the moved copy too:
    an opening never gets smaller than the maker's data asks, and never misses the place
    where the phone was measured."""
    if max(abs(dx), abs(dy), abs(dz)) < 0.005:
        return [room]
    return [room, Pos(dx, dy, dz) * room]


def _bounds(rooms):
    """One bounding box around several shapes."""
    box = rooms[0].bounding_box()
    for room in rooms[1:]:
        box = box.add(room.bounding_box())
    return box


def _notch(box, side, inner, outer, top, radius):
    """A button opening through a side wall, open towards the screen: from the lower edge
    of the buttons' opening `box` up through the rim, with rounded lower corners, and
    widening at 45 degrees at both ends from the wall's inside (`inner`) to its outside
    (`outer`), so a fingertip reaches the button. `side` is +1 (the +X wall) or -1."""
    y0, y1, low = box.min.Y, box.max.Y, box.min.Z
    height = (top - low) + radius + 2
    profile = Plane.YZ * RectangleRounded(y1 - y0, height, radius)
    slot = Pos(side * (inner + outer) / 2, (y0 + y1) / 2, low + height / 2) * extrude(
        profile, amount=(outer - inner) / 2 + 2, both=True)
    # 45 degrees from the wall's inside to 1 mm past its outside.
    grow = outer + 1.0 - inner
    for end, sign in ((y1, 1), (y0, -1)):
        ear = Polygon(
            (side * inner, end - sign * 0.5), (side * inner, end),
            (side * (outer + 1.0), end + sign * grow),
            (side * (outer + 1.0), end - sign * 0.5), align=None)
        slot += Pos(0, 0, low + radius) * extrude(ear, amount=height, dir=(0, 0, 1))
    return slot


def build(p, ctx):
    phone = ctx.component("sample_phone")
    width, height, depth = phone.size
    half_w, half_h = width / 2, height / 2
    pocket = phone.reference.pocket("stretch", ctx=ctx, name="phone")
    side_gap = (pocket.size[0] - width) / 2     # drawn gap at the long sides
    end_gap = (pocket.size[1] - height) / 2     # and at the two ends

    # The phone's limits win over the sliders.
    back = min(p.back, phone.rule("max_back_thickness"))
    standoff = max(p.lip_height, phone.rule("min_glass_standoff"))
    outer = pocket.offset + p.wall
    wall_in, wall_out = half_w + side_gap, half_w + side_gap + p.wall

    # ---- the rim: it leans in over the phone's rounded front edge ----------------------
    # Its underside is a slope that clears the shoulder of the phone's edge section (in
    # the room the fit keeps for the phone) by SEAT_PLAY. Face down, the phone slides
    # onto that slope; the rim's top is set from where it then rests.
    front = [(d, z) for d, z in phone.edge_section() if z > depth / 2]
    seat = max(z * pocket.scale + pocket.top_gap - RIM_SLOPE * d * pocket.scale
               for d, z in front) + SEAT_PLAY          # the slope's height at the wall
    reach = side_gap + p.lip_reach                     # wall to the rim's inner edge
    travel = min(seat + RIM_SLOPE * (d + side_gap) - z for d, z in front)
    top = depth + travel + standoff
    # The phone's rule: how wide the case opens from the edge of the active display.
    flat_start = (width - phone.envelope["display_active"].value[0]) / 2
    open_tan = tan(radians(phone.rule("touchscreen_opening_deg") / 2))
    ease = max(RIM_EASE, open_tan * (top - depth) - (flat_start - p.lip_reach) + 0.02)
    under = seat + RIM_SLOPE * reach                   # underside at the rim's inner edge
    ease = min(ease, top - under - 0.1)

    # ---- outer shape and the cavity -----------------------------------------------------
    case = _bevelled(pocket, outer, -back, top, BACK_BEVEL, RIM_BEVEL)
    lean = reach + 0.4                                 # the slope runs past the inner edge
    case -= _slab(pocket, pocket.offset, 0.0, seat)
    case -= pocket.taper(seat, seat + RIM_SLOPE * lean, lean)
    pocket.place(at=Pos(0, 0, 0))
    case -= _slab(pocket, pocket.offset - reach, seat, top + 1)
    case -= pocket.taper(top - ease, top + 1, -(ease + 1), offset=pocket.offset - reach)
    # ---- camera window: the whole raised area of the back, to the top wall -------------
    plateau = phone.feature("camera_plateau")
    low = -back - 1
    plateau_open = plateau.opening(ctx, "loose", reach=back + 2)
    gap = (plateau_open.bounding_box().size.X - plateau.size[0]) / 2   # the resolver's
    window_y = half_h - p.window_end - gap          # lower edge of the camera window
    window_x = half_w - p.window_side + gap         # its side edges
    curl_y = half_h - p.window_curl - gap           # where the raised area ends at the sides
    straight_x = plateau.value("blend_straight_x") + gap
    far_y = half_h + 12
    # The area of uniform thickness just below the raised surround: the back stays
    # whole over it, so the window's lower corners are pulled up around it.
    area = phone.feature("back_area_corner")
    keep_x = abs(area.center[0]) - area.size[0] / 2 - AREA_MARGIN
    keep_y = area.center[1] + area.size[1] / 2 + AREA_MARGIN
    corner = [(straight_x, window_y)]
    chord_y = window_y + (keep_x - straight_x) / (window_x - straight_x) * (curl_y - window_y)
    if straight_x < keep_x < window_x and keep_y > chord_y + 0.05:
        corner.append((keep_x, keep_y))
    side_y = max(curl_y, keep_y + 0.5) if len(corner) == 2 else curl_y
    corner.append((window_x, side_y))
    outline = ([(-x, y) for x, y in reversed(corner)] + corner
               + [(window_x, far_y), (-window_x, far_y)])
    window = Polygon(*outline, align=None)
    rounded = [v for v in window.vertices() if v.Y < far_y - 1]
    window = fillet(rounded, WINDOW_CORNER)
    inside = _slab(pocket, pocket.offset - SEAT, low, 1.0)
    case -= extrude(Pos(0, 0, low) * window, amount=back + 2) & inside
    run = WINDOW_BEVEL * WINDOW_LEAN
    bevel = extrude(Pos(0, 0, low) * offset(window, amount=run), amount=WINDOW_BEVEL + 1)
    bevel = chamfer(bevel.edges().group_by(Axis.Z)[-1], run, WINDOW_BEVEL,
                    reference=bevel.faces().sort_by(Axis.Z)[-1])
    case -= bevel & inside
    # Beside the window the floor rises to the side walls at 45 degrees instead of a flat
    # ledge: room for the back's unverified rise towards the plateau near the sides.
    wall_x = wall_in - SEAT
    wedge = Plane.XZ * Polygon(
        (window_x - 0.5, 1.0), (wall_x + 1.0, 1.0),
        (window_x - 0.5, -(wall_x - window_x + 0.5)), align=None)
    wedge = extrude(wedge, amount=height, both=True)
    wedge &= _span(0, half_w + 5, side_y + WINDOW_CORNER, far_y, low, 2)
    wedge &= inside
    case -= wedge
    case -= wedge.mirror(Plane.YZ)

    # The stated plateau (its ends are round, so it lies well inside the window), the
    # lens, the flash and the rear microphone must clear as well.
    case -= plateau_open
    lens = phone.feature("rear_camera")
    for name in ("rear_camera", "flash", "rear_mic"):
        case -= phone.feature(name).keep_out(reach=back + 4)

    # ---- buttons: cut-outs open towards the screen, flared for a fingertip -------------
    through = p.wall + 2
    inward = p.lip_reach + 0.5       # past the rim's inner edge, so no strip is left
    side = phone.feature("side_button")
    opening = side.opening(ctx, "loose", reach=through, inward=inward)
    box = opening.bounding_box()
    play = (box.size.Y - side.size[1]) / 2                 # the resolver's
    case -= opening
    case -= _notch(box, 1, wall_in, wall_out, top, 2.5 * play)

    left_shift = p.button_height - phone.feature("volume_up").center[2]
    for names in (("volume_up", "volume_down"), ("mode_button",)):
        rooms = [room for name in names for room in _both(
            phone.feature(name).opening(ctx, "loose", reach=through, inward=inward),
            dz=left_shift)]
        for room in rooms:
            case -= room
        case -= _notch(_bounds(rooms), -1, wall_in, wall_out, top, 2.5 * play)

    # The touch strip's keep-out is taller than the phone is thick and flares from the
    # phone's surface, so the wall is open there up to the screen.
    strip = phone.feature("touch_strip")
    room = strip.keep_out(reach=through + 1, inward=inward)
    case -= room
    box = room.bounding_box()
    cy, cz, half_l = strip.center[1], strip.center[2], strip.keep_outs[0].size[1].value / 2
    spread = (box.max.X - half_w) * tan(radians(strip.keep_outs[0].flare.value))
    case -= _span(half_w - inward, half_w, cy - half_l, cy + half_l, cz, top + 1)
    fan = Polygon((half_w, cy - half_l), (box.max.X, cy - half_l - spread),
                  (box.max.X, cy + half_l + spread), (half_w, cy + half_l), align=None)
    case -= Pos(0, 0, cz) * extrude(fan, amount=top + 1 - cz, dir=(0, 0, 1))

    # ---- bottom edge: USB-C notch (open towards the screen) and two pairs of holes -----
    usb = phone.feature("usb_c")
    rooms = _both(usb.keep_out(reach=through + 1, inward=inward),
                  dz=p.usb_c_height - usb.center[2])
    box = _bounds(rooms)
    for room in rooms:
        case -= room
    case -= _span(box.min.X, box.max.X, box.min.Y, box.max.Y, box.center().Z, top + 1)

    port_shift = p.port_height - phone.feature("bottom_mic_1").center[2]
    for pair in (("bottom_mic_1", "bottom_port_1"), ("bottom_mic_2", "bottom_port_2")):
        ends = [_both(phone.feature(name).keep_out(reach=through + 1), dz=port_shift)
                for name in pair]
        for room in ends[0] + ends[1]:
            case -= room
        a, b = _bounds(ends[0]), _bounds(ends[1])
        # One slot per pair instead of two overlapping circles.
        case -= _span(min(a.center().X, b.center().X), max(a.center().X, b.center().X),
                      a.min.Y, a.max.Y, min(a.min.Z, b.min.Z), max(a.max.Z, b.max.Z))

    # ---- front: the receiver slot keeps its keep-out whatever the rim does -------------
    receiver = phone.feature("receiver")
    for room in _both(receiver.keep_out(reach=standoff + travel + 2, inward=0.5),
                      dy=(half_h - p.receiver_from_top) - receiver.center[1]):
        case -= room

    # The "Check" parameters start as the component's own numbers; say when one differs.
    stated = {
        "button_height": phone.feature("volume_up").center[2],
        "usb_c_height": usb.center[2],
        "port_height": phone.feature("bottom_mic_1").center[2],
        "receiver_from_top": half_h - receiver.center[1],
        "window_end": half_h - plateau.value("blend_limit_y"),
        "window_curl": half_h - plateau.value("blend_side_y"),
    }
    changed = [f"{name} {p[name]:g} (component: {value:.2f})"
               for name, value in stated.items() if abs(p[name] - value) > 0.005]
    print("check values: " + ("; ".join(changed) + " differ from the component's data"
                              if changed else "all equal to the component's data"))
    print(f"case for {phone.name}: phone {width} x {height} x {depth} mm")
    print(f"stretch fit: drawn gap {side_gap:+.3f} mm per long side and {end_gap:+.3f} mm per "
          f"end (aims at {pocket.clearance.target:+.3f} printed), offset {pocket.offset:+.3f}, "
          f"scale {pocket.scale:.5f}")
    print(f"opening play (loose): plateau {gap:.3f} mm, buttons {play:.3f} mm per side")
    print(f"wall {p.wall}, back {back}; rim reaches {p.lip_reach:.2f} mm over the phone's "
          f"edge at the long sides and {p.lip_reach - (end_gap - side_gap):.2f} mm at the ends")
    print(f"rim: its underside leans in from z = {seat:.3f} at the wall to {under:.3f} at its "
          f"inner edge; face down the phone slides {travel:.3f} mm onto it, and the rim's top "
          f"(z = {top:.3f}) is then {standoff:.2f} mm beyond the glass; inner top edge "
          f"bevelled {ease:.2f} mm")
    print(f"camera window: lower edge {half_h - window_y:.2f} mm from the top, corners pulled "
          f"up to {half_h - side_y:.2f} mm at the sides; the lens stands "
          f"{lens.size[2] - back:.2f} mm proud of the back and is not protected")
    return {"case": case}
