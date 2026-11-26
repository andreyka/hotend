"""A plain flexible case for a bought phone (the invented sample phone): the body is held
by a stretch fit, the back stays under the maker's limit, and every opening comes from the
component's features. Printed in a rigid material the stretch fit must be refused."""

from build123d import *

PARAMS = {
    "wall": {"default": 1.6, "min": 1.2, "max": 2.5, "unit": "mm",
             "label": "Side wall", "provenance": "guessed"},
    "back": {"default": 1.6, "min": 1.0, "max": 2.0, "unit": "mm",
             "label": "Back thickness", "provenance": "guessed"},
    "lip": {"default": 1.2, "min": 0.6, "max": 2.5, "unit": "mm",
            "label": "Lip over the front edge", "provenance": "guessed"},
    "lip_height": {"default": 1.0, "min": 0.8, "max": 2.0, "unit": "mm",
                   "label": "Lip above the glass", "provenance": "standard"},
}


def build(p, ctx):
    phone = ctx.component("sample_phone")
    pocket = phone.reference.pocket("stretch", ctx=ctx)
    back = min(p.back, phone.rule("max_back_thickness"))
    lip_height = max(p.lip_height, phone.rule("min_glass_standoff"))

    # The shell is the pocket's own outline grown by the wall, from under the back to
    # above the glass; the pocket then cuts the room for the phone out of it.
    shell = Pos(0, 0, -back) * pocket.room(offset=pocket.offset + p.wall, top=back + lip_height)
    case = pocket.cut(shell, at=Pos(0, 0, 0))
    # The screen opening leaves a lip over the front edge.
    case -= pocket.room(offset=pocket.offset - p.lip, top=lip_height + 1)

    through_wall = p.wall + 2
    for name in ("side_button", "mode_button", "volume_up", "volume_down"):
        case -= phone.feature(name).opening(ctx, "loose", reach=through_wall)
    for name in ("bottom_mic_1", "bottom_port_1", "bottom_mic_2", "bottom_port_2", "receiver"):
        case -= phone.feature(name).keep_out(reach=through_wall + lip_height)
    # The stated keep-outs of the touch strip and of the USB-C plug are taller than the
    # phone is thick, so they are open notches: from their middle up, the lip is cut away.
    top = pocket.size[2] + lip_height + 1
    for name in ("touch_strip", "usb_c"):
        room = phone.feature(name).keep_out(reach=through_wall, inward=p.lip + 0.5)
        box = room.bounding_box()
        middle = box.center()
        case -= room
        case -= Pos(middle.X, middle.Y, middle.Z) * Box(
            box.size.X, box.size.Y, top - middle.Z,
            align=(Align.CENTER, Align.CENTER, Align.MIN))
    case -= phone.feature("camera_plateau").opening(ctx, "loose", reach=back + 1)
    print("case for", phone.name, [round(v, 2) for v in pocket.size])
    return {"case": case}
