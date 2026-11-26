"""A wall bracket: an L-shaped plate with two wall screw holes, two shelf holes and fillets."""

from build123d import *

PARAMS = {
    "width": {"default": 40.0, "min": 20.0, "max": 120.0, "unit": "mm",
              "label": "Width", "provenance": "style"},
    "arm": {"default": 60.0, "min": 30.0, "max": 150.0, "unit": "mm",
            "label": "Arm length", "provenance": "style"},
    "thickness": {"default": 4.0, "min": 2.4, "max": 8.0, "unit": "mm",
                  "label": "Thickness", "provenance": "guessed"},
    "screw": {"default": 4.5, "min": 3.0, "max": 6.5, "unit": "mm",
              "label": "Screw hole diameter", "provenance": "standard"},
    "holes": {"default": 2, "min": 1, "max": 4, "unit": "count",
              "label": "Holes per arm", "provenance": "style"},
}


def build(p, ctx):
    t, arm, width = p.thickness, p.arm, p.width
    with BuildPart() as bracket:
        # The L profile in the XZ plane, extruded across the width.
        with BuildSketch(Plane.XZ):
            with BuildLine():
                Polyline((0, 0), (arm, 0), (arm, t), (t, t), (t, arm), (0, arm), close=True)
            make_face()
        extrude(amount=width / 2, both=True)
        # Round the inside corner for strength and the outer ends for looks.
        inner = bracket.edges().filter_by(Axis.Y).sort_by(Axis.X)[2:4].sort_by(Axis.Z)[0]
        fillet(inner, radius=t)
        fillet(bracket.edges().filter_by(Axis.Y).group_by(Axis.X)[-1], radius=t / 4)
        # Screw holes through the horizontal arm (shelf side) ...
        spacing = (arm - 3 * t) / max(p.holes, 1)
        shelf = [(2.5 * t + spacing * (i + 0.5), 0) for i in range(p.holes)]
        with Locations(Plane.XY.offset(t)), Locations(*shelf):
            Hole(p.screw / 2)
        # ... and through the vertical arm (wall side).
        wall = [(0, 2.5 * t + spacing * (i + 0.5)) for i in range(p.holes)]
        with Locations(Plane.YZ.offset(t)), Locations(*wall):
            Hole(p.screw / 2)
    return {"bracket": bracket.part}
