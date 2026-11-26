"""Print preparation: how each part lies on the bed, and where on which plate.

A model is drawn the way the product is used (a lid sits on its box, rails down). Printed
like that it would need support. Preparation turns each part into the orientation that
prints best and lays the turned parts out on plates.

**Orientation.** The candidates are the part as modelled, every direction in which the part
has a planar face it can stand on (at least `MIN_STAND_AREA_MM2`, lying on the outside of the
part), and the orientation the model or the user asks for. Each candidate is measured:

- `contact`: planar area that lies on the bed (exact B-rep face areas);
- `overhang`: area facing down more steeply than the material prints without support
  (planar faces exactly, curved faces per triangle of the tessellation);
- `height` above the bed;
- `fit_overhang`: the part of `overhang` that is on mating faces of declared fits (a mating
  face printed on support does not keep its clearance);
- `weak_snaps`: snap rails (or other parts a feature card says must bend) that would stand
  upright, so that the layers lie across the bending part.

The score, lower is better, is in mm2 of support or its stated equivalent:

    score = overhang                                1 per mm2 that needs support
          + FIT_SUPPORT_WEIGHT * fit_overhang       3 more per mm2 when that is a mating face
          + SNAP_PENALTY * weak_snaps               150 per snap printed across its layers
          + tipping                                 100 for each unit by which
                                                    height / sqrt(contact) exceeds 2;
                                                    1000 when under 20 mm2 lies on the bed
          + height                                  1 per mm

The lowest score wins, but the part stays as modelled unless another orientation is better
by more than `KEEP_MARGIN` (25); equal scores prefer more contact. An
orientation that the model declares (`PRINT`) or the request gives is used instead, unless
it cannot print: nothing flat on the bed, or the part does not fit the nozzle's reach that
way. Then the best-scoring candidate is used and the sentence says so. The weights are
general FDM practice, not measurements.

**Fitting the reach** is one rule, `reach_spare`: a piece fits when its footprint, as it lies
or a quarter turn about Z further, is inside the X and Y reach less `margin` on each side,
and its height is inside the Z reach. Only candidates that fit are scored against each other
(all of them when none fits), so a part that is too long as modelled is stood another way up
when that fits. The orientation choice, the plating and the `fits_build_volume` check of a
build that judges the prepared plate all use this rule.

**Plating.** A deterministic shelf layout: the turned parts, largest footprint depth first,
are put in rows from the front left of the bed with `gap` between them and `margin` to the
edge of the reach of the nozzle that prints each part. A part is turned a quarter turn about
Z when it only fits that way. When a row does not fit the plate a new plate is started; a
part that fits no empty plate is reported as unplaced. This is not a nesting optimiser.

**Room for the brim.** The caller may ask for a `clearance`: the widest brim of the
preparation plus what it wants to keep free around it. Every footprint is grown by it on
all sides before it is laid out, so two pieces lie `2 x clearance + gap` apart and a piece
lies `clearance + margin` from the edge of the reach. `keepouts` are boxes of the bed that
no grown footprint may touch (the line the printer draws before a print): a piece that
would touch one is moved along its row past the zone, or its row is started behind the
zone. With `anchor` = `centre` the pieces of a plate are then moved together so that their
grown bounding box is centred on the reach, as far as every piece stays inside its own
nozzle's reach and clear of the zones. The engine is told the clearance; which brim a
piece needs is decided by the print settings rules, not here.

**A piece that is not placed** says why in a sentence (`reason`) and in one word (`cause`):
`clearance` when this layout would have placed it on an empty plate without its clearance
(the brim has no room, the piece has), `keepout` when the keep-out zones leave it no room
even then, `too_large` when it is larger than its nozzle's reach inside the margin, and
`no_reach` when the request gives its nozzle no reach. The word is for the caller that
chose the clearance: only the first is solved by a narrower brim.

Everything here is computed from the B-rep and its tessellation; no model code runs.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from .parts import NamedPart
from .tessellate import PartMesh, Tessellation

MIN_STAND_AREA_MM2 = 20.0
BED_TOLERANCE_MM = 0.01
DEFAULT_MAX_OVERHANG_DEG = 45.0
FIT_SUPPORT_WEIGHT = 3.0
SNAP_PENALTY = 150.0
TIP_WEIGHT = 100.0
TIP_FREE_RATIO = 2.0
NO_CONTACT_PENALTY = 1000.0
HEIGHT_WEIGHT = 1.0
KEEP_MARGIN = 25.0
# A bending part counts as upright when its length is within about 45 degrees of vertical.
UPRIGHT_COS = 0.7
MAX_FACE_CANDIDATES = 48
MAX_REPORTED_CANDIDATES = 8
# An overhang smaller than this is not worth a word (the `overhangs` check uses the same).
SUPPORT_NOTE_MM2 = 4.0
SCORE_DIGITS = 3
DEFAULT_GAP_MM = 5.0
DEFAULT_MARGIN_MM = 2.0
MAX_INSTANCES = 256
FRONT_LEFT, CENTRE = "front_left", "centre"
# Why a piece was not placed (`cause` of an unplaced entry, module doc).
NO_REACH, TOO_LARGE, CLEARANCE, KEEPOUT = "no_reach", "too_large", "clearance", "keepout"
# How far a grown footprint is put from a keep-out zone, so that the two do not touch.
KEEPOUT_SLACK_MM = 0.001
# A size may exceed its room by this much and still fit (floating point, not millimetres).
REACH_TOLERANCE_MM = 1e-6

_AXIS_WORDS = (
    ((0.0, 0.0, 1.0), "as modelled"),
    ((0.0, 0.0, -1.0), "upside down"),
    ((1.0, 0.0, 0.0), "onto its -X side"),
    ((-1.0, 0.0, 0.0), "onto its +X side"),
    ((0.0, 1.0, 0.0), "onto its -Y side"),
    ((0.0, -1.0, 0.0), "onto its +Y side"),
)


def overhang_threshold(limit_deg: float) -> float:
    """A surface tilted `limit_deg` from vertical has a normal whose upward component is
    -sin(limit); anything below this faces down too steeply."""
    return -math.sin(math.radians(limit_deg)) - 1e-6


def rotation_to_up(up: np.ndarray) -> np.ndarray:
    """The rotation matrix that turns the model so that direction `up` points along +Z.

    The least rotation that does it; for `up` = -Z a half turn about X (the part is flipped
    like a page, front edge staying in front)."""
    u = np.asarray(up, dtype=float)
    u = u / np.linalg.norm(u)
    z = np.array([0.0, 0.0, 1.0])
    cos = float(u @ z)
    if cos > 1.0 - 1e-12:
        return np.eye(3)
    if cos < -1.0 + 1e-12:
        return np.diag([1.0, -1.0, -1.0])
    axis = np.cross(u, z)
    sin = float(np.linalg.norm(axis))
    axis = axis / sin
    k = np.array([[0.0, -axis[2], axis[1]], [axis[2], 0.0, -axis[0]], [-axis[1], axis[0], 0.0]])
    return np.eye(3) + sin * k + (1.0 - cos) * (k @ k)


def quarter_turn() -> np.ndarray:
    return np.array([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]])


def location_of(rotation: np.ndarray, translation: np.ndarray) -> Any:
    """A rigid placement as a build123d Location."""
    from build123d import Location
    from OCP.gp import gp_Trsf

    transformation = gp_Trsf()
    transformation.SetValues(*[
        float(v) for row in range(3) for v in (*rotation[row], translation[row])
    ])
    return Location(transformation)


@dataclass
class PartFit:
    """What the declared fits say about one part: its mating faces (face index -> label of
    the first pair that uses it) and the directions along which its bending parts are long."""

    faces: dict[int, str] = field(default_factory=dict)
    flex: list[tuple[tuple[float, float, float], str]] = field(default_factory=list)


def fit_features(parts: list[NamedPart], resolver: Any) -> dict[str, PartFit]:
    """Per part name, the fit faces and bending directions the declared cards put on it.
    Empty when nothing is declared or hotend_fit cannot be used."""
    declared = list(getattr(resolver, "declared_fits", None) or [])
    features: dict[str, PartFit] = {}
    if not declared:
        return features
    try:
        from hotend_fit.verify import frame_direction, locate_sides
    except ImportError:
        return features
    shapes = [(part.name, part.shape) for part in parts]
    faces = {part.name: list(part.shape.faces()) for part in parts}
    halves: dict[tuple[int, str], dict[str, int]] = {}
    for side in locate_sides(shapes, declared):
        index = next((i for i, face in enumerate(faces[side.part])
                      if face.is_same(side.face)), None)
        if index is None:
            continue
        features.setdefault(side.part, PartFit()).faces.setdefault(index, side.pair.label)
        tally = halves.setdefault((id(side.pair.owner), side.half), {})
        tally[side.part] = tally.get(side.part, 0) + 1
    for card in getattr(resolver, "cards", None) or []:
        for key, axis in card.flex_axes().items():
            frame, tally = card.frames.get(key), halves.get((id(card), key))
            if frame is None or not tally:
                continue
            host = max(tally, key=tally.get)
            features.setdefault(host, PartFit()).flex.append(
                (frame_direction(frame, axis), f"{card.kind.replace('_', ' ')} '{card.name}'")
            )
    return features


class PartGeometry:
    """The faces of one part as arrays, measured once and then judged for any `up`."""

    def __init__(self, part: NamedPart, mesh: PartMesh | None) -> None:
        from build123d import GeomType

        self.part = part
        normals, areas, points, planar_index = [], [], [], []
        curved = []
        for index, face in enumerate(part.shape.faces()):
            if face.geom_type == GeomType.PLANE:
                center = face.center()
                normals.append(tuple(face.normal_at(center)))
                areas.append(float(face.area))
                points.append(tuple(center))
                planar_index.append(index)
            else:
                curved.append(index)
        self.planar_normals = np.array(normals, dtype=float).reshape(-1, 3)
        self.planar_areas = np.array(areas, dtype=float)
        self.planar_points = np.array(points, dtype=float).reshape(-1, 3)
        self.planar_index = np.array(planar_index, dtype=int)
        self.curved_faces = curved
        usable = mesh is not None and len(mesh.triangles) > 0
        if usable and mesh.face_map_ok and curved:
            tri_normals, tri_areas = mesh.triangle_normals_and_areas()
            keep = np.isin(mesh.triangle_face, curved)
            self.tri_normals, self.tri_areas = tri_normals[keep], tri_areas[keep]
            self.tri_face = mesh.triangle_face[keep]
            self.unmeasured = 0
        else:
            self.tri_normals, self.tri_areas = np.zeros((0, 3)), np.zeros(0)
            self.tri_face = np.zeros(0, dtype=int)
            self.unmeasured = len(curved)
        if usable:
            self.points = mesh.vertices
        else:
            self.points = np.array([tuple(v) for v in part.shape.vertices()], dtype=float)

    def measure(self, up: np.ndarray, threshold: float, fit: PartFit | None = None) -> dict:
        """contact, overhang, height, footprint and the fit measures for one direction."""
        up = np.asarray(up, dtype=float)
        up = up / np.linalg.norm(up)
        along = self.points @ up
        low, high = float(along.min()), float(along.max())
        facing = self.planar_normals @ up
        on_bed = (facing < -0.9999) & (np.abs(self.planar_points @ up - low) <= BED_TOLERANCE_MM)
        hangs = (facing < threshold) & ~on_bed
        tri_hangs = self.tri_normals @ up < threshold
        overhang = float(self.planar_areas[hangs].sum() + self.tri_areas[tri_hangs].sum())
        fit_overhang, fit_faces, weak = 0.0, [], []
        if fit is not None:
            for face, label in fit.faces.items():
                area = float(self.planar_areas[hangs & (self.planar_index == face)].sum()
                             + self.tri_areas[tri_hangs & (self.tri_face == face)].sum())
                if area > 1e-6:
                    fit_overhang += area
                    fit_faces.append({"face": face, "pair": label, "area": area})
            weak = [label for axis, label in fit.flex
                    if abs(float(np.asarray(axis) @ up)) >= UPRIGHT_COS]
        turned = self.points @ rotation_to_up(up).T
        size = turned.max(axis=0) - turned.min(axis=0)
        return {
            "contact": float(self.planar_areas[on_bed].sum()),
            "overhang": overhang,
            "height": high - low,
            "footprint": [float(size[0]), float(size[1])],
            "fit_overhang": fit_overhang,
            "fit_faces": sorted(fit_faces, key=lambda f: -f["area"])[:10],
            "weak_snaps": weak,
        }

    def stand_directions(self) -> list[np.ndarray]:
        """Every `up` for which a planar face of at least MIN_STAND_AREA_MM2 lies on the
        outside of the part, largest face first."""
        seen: dict[tuple, float] = {}
        if len(self.planar_normals) == 0:
            return []
        reach = self.points @ self.planar_normals.T  # (points, faces)
        outermost = reach.max(axis=0)
        offsets = np.einsum("ij,ij->i", self.planar_points, self.planar_normals)
        for normal, area, offset, limit in zip(self.planar_normals, self.planar_areas, offsets,
                                               outermost, strict=True):
            if offset < limit - BED_TOLERANCE_MM:
                continue  # the part reaches past this face: it cannot lie on the bed
            key = tuple(np.round(-normal, 6) + 0.0)
            seen[key] = seen.get(key, 0.0) + float(area)
        ranked = sorted(((area, key) for key, area in seen.items()
                         if area >= MIN_STAND_AREA_MM2), key=lambda item: (-item[0], item[1]))
        return [np.array(key) for _, key in ranked[:MAX_FACE_CANDIDATES]]


def score_of(measures: dict) -> tuple[float, dict]:
    """The stated score of one candidate (module doc) and its terms."""
    contact, height = measures["contact"], measures["height"]
    if contact < MIN_STAND_AREA_MM2:
        tipping = NO_CONTACT_PENALTY
    else:
        tipping = TIP_WEIGHT * max(0.0, height / math.sqrt(contact) - TIP_FREE_RATIO)
    terms = {
        "support": measures["overhang"],
        "fit_support": FIT_SUPPORT_WEIGHT * measures["fit_overhang"],
        "weak_snaps": SNAP_PENALTY * len(measures["weak_snaps"]),
        "tipping": tipping,
        "height": HEIGHT_WEIGHT * height,
    }
    return round(sum(terms.values()), SCORE_DIGITS), terms


def describe_up(up: np.ndarray) -> str:
    """Plain words for an orientation: how the part is turned from the way it was drawn."""
    for axis, words in _AXIS_WORDS:
        if float(np.asarray(axis) @ up) > 1.0 - 1e-6:
            return words
    return "so that its ({:.2f}, {:.2f}, {:.2f}) direction points up".format(*up)


def how_printed(up: np.ndarray) -> str:
    """`as modelled` or `turned <how>`, for a sentence about a part in that orientation."""
    words = describe_up(up)
    return words if words == "as modelled" else "turned " + words


def reach_room(reach_size: tuple[float, float, float],
               margin: float) -> tuple[float, float, float]:
    """The room (x, y, z) a piece may occupy in a reach of `reach_size`: `margin` is kept
    free at each edge of the bed, none in height."""
    return (reach_size[0] - 2 * margin, reach_size[1] - 2 * margin, reach_size[2])


def reach_spare(size: tuple[float, float, float],
                room: tuple[float, float, float]) -> tuple[tuple[float, float, float], bool]:
    """What is left of `room` around a piece of `size` (x, y, z as it stands on the bed):
    ((spare x, spare y, spare z), quarter turn). The piece may be turned a quarter turn
    about Z; that is reported only when it fits that way and no other. A negative spare is
    what the piece is too large by. This is the one rule for "fits the reach" (module doc)."""
    width, depth, height = size
    lying = (room[0] - width, room[1] - depth, room[2] - height)
    turned = (room[0] - depth, room[1] - width, room[2] - height)
    if not spare_fits(lying) and spare_fits(turned):
        return turned, True
    return lying, False


def spare_fits(spare: tuple[float, float, float]) -> bool:
    """Whether a `reach_spare` says the piece fits."""
    return min(spare) >= -REACH_TOLERANCE_MM


def _fits_reach(measures: dict, reach_size: tuple[float, float, float] | None,
                margin: float) -> bool:
    if reach_size is None:
        return True
    size = (*measures["footprint"], measures["height"])
    return spare_fits(reach_spare(size, reach_room(reach_size, margin))[0])


@dataclass
class Choice:
    """The orientation chosen for one part and why."""

    part: str
    up: np.ndarray
    source: str  # as_modelled | face | preferred
    measures: dict
    score: float
    terms: dict
    as_modelled: dict
    candidates: list[dict]
    overridden: str | None  # why a preferred orientation was not used
    preferred_by: str | None
    sentence: str = ""
    # Whether the chosen orientation, and the part as modelled, fit the reach they were
    # chosen for (true when no reach was given). The chosen one fits whenever any does.
    fits_reach: bool = True
    modelled_fits_reach: bool = True

    @property
    def rotation(self) -> np.ndarray:
        return rotation_to_up(self.up)

    def to_json(self) -> dict:
        return {
            "part": self.part,
            "source": self.source,
            "up": [float(v) for v in self.up],
            "turned": describe_up(self.up),
            "score": self.score,
            "terms": self.terms,
            "measures": self.measures,
            "as_modelled": self.as_modelled,
            "preferred_by": self.preferred_by,
            "overridden": self.overridden,
            "candidates": self.candidates,
            "explanation": self.sentence,
        }


def _support_words(measures: dict) -> str:
    if measures["overhang"] < SUPPORT_NOTE_MM2:
        return "nothing needs support"
    return f"about {measures['overhang']:.0f} mm2 needs support"


def _sentence(choice: Choice) -> str:
    """One plain sentence: how the part is printed and why."""
    chosen, modelled = choice.measures, choice.as_modelled
    name = f"'{choice.part}'"
    footing = (f"{chosen['contact']:.0f} mm2 lies flat on the bed"
               if chosen["contact"] >= 1.0 else "no flat face lies on the bed")
    how = how_printed(choice.up)
    text = (f"{name} is {'printed ' if how == 'as modelled' else ''}{how}: {footing} and "
            f"{_support_words(chosen)}")
    if choice.source != "as_modelled":
        if choice.source == "preferred":
            text += f", as {choice.preferred_by} asks"
        elif choice.fits_reach and not choice.modelled_fits_reach:
            text += " (as modelled it does not fit the nozzle's reach)"
        elif modelled["overhang"] >= chosen["overhang"] + SUPPORT_NOTE_MM2:
            text += f" (as modelled about {modelled['overhang']:.0f} mm2 would)"
        elif modelled["height"] > chosen["height"] + 1e-6:
            text += (f" (as modelled it would stand {modelled['height']:.0f} mm tall on "
                     f"{modelled['contact']:.0f} mm2)")
    if chosen["fit_overhang"] >= SUPPORT_NOTE_MM2:
        text += (f"; about {chosen['fit_overhang']:.0f} mm2 of that is on mating faces, "
                 "which will not keep their fit on support")
    if chosen["weak_snaps"]:
        text += ("; the snap rails stand upright, across the layers, which is their weaker "
                 "direction")
    if choice.overridden:
        text += f"; {choice.preferred_by} asked for another orientation, which {choice.overridden}"
    return text + "."


def choose_orientation(
    geometry: PartGeometry,
    limit_deg: float,
    fit: PartFit | None = None,
    preferred: tuple[float, float, float] | None = None,
    preferred_by: str | None = None,
    reach_size: tuple[float, float, float] | None = None,
    margin: float = 0.0,
    pinned: bool = False,
) -> Choice:
    """Measure and score the candidates of one part and pick one (module doc).

    With `pinned` the preferred orientation is used whatever it scores: the caller chose
    it in an earlier pass and its plan rests on it."""
    threshold = overhang_threshold(limit_deg)
    candidates: list[dict] = []

    def add(up: np.ndarray, source: str) -> dict:
        up = np.asarray(up, dtype=float)
        up = up / np.linalg.norm(up)
        for known in candidates:
            if float(known["up"] @ up) > 1.0 - 1e-9:
                return known
        measures = geometry.measure(up, threshold, fit)
        score, terms = score_of(measures)
        candidate = {"up": up, "source": source, "measures": measures, "score": score,
                     "terms": terms, "fits": _fits_reach(measures, reach_size, margin),
                     "order": len(candidates)}
        candidates.append(candidate)
        return candidate

    modelled = add(np.array([0.0, 0.0, 1.0]), "as_modelled")
    for up in geometry.stand_directions():
        add(up, "face")
    asked = None
    if preferred is not None:
        asked = add(np.array(preferred, dtype=float), "preferred")

    fitting = [c for c in candidates if c["fits"]] or candidates
    best = min(fitting, key=lambda c: (c["score"], -c["measures"]["contact"], c["order"]))
    # Identity, not equality: the candidates hold arrays, which do not compare to a bool.
    if any(c is modelled for c in fitting) and modelled["score"] <= best["score"] + KEEP_MARGIN:
        best = modelled
    overridden = None
    if asked is not None:
        if pinned:
            best = asked
        elif not asked["fits"] and any(c["fits"] for c in candidates):
            overridden = "does not fit the nozzle's reach"
        elif asked["measures"]["contact"] < 1.0 and best["measures"]["contact"] >= 1.0:
            overridden = "leaves nothing flat on the bed"
        else:
            best = asked
    source = best["source"]
    if asked is not None and best is asked:
        source = "preferred"
    elif best is modelled:
        source = "as_modelled"
    ranked = sorted(candidates, key=lambda c: (c["score"], c["order"]))
    choice = Choice(
        part=geometry.part.name, up=best["up"], source=source, measures=best["measures"],
        score=best["score"], terms=best["terms"], as_modelled=modelled["measures"],
        candidates=[
            {"up": [float(v) for v in c["up"]], "turned": describe_up(c["up"]),
             "source": c["source"], "score": c["score"], "fits_reach": c["fits"],
             "contact": c["measures"]["contact"], "overhang": c["measures"]["overhang"],
             "height": c["measures"]["height"], "fit_overhang": c["measures"]["fit_overhang"],
             "weak_snaps": len(c["measures"]["weak_snaps"])}
            for c in ranked[:MAX_REPORTED_CANDIDATES]
        ],
        overridden=overridden, preferred_by=preferred_by if asked is not None else None,
        fits_reach=best["fits"], modelled_fits_reach=modelled["fits"],
    )
    choice.sentence = _sentence(choice)
    return choice


def transformed_mesh(mesh: PartMesh, rotation: np.ndarray, translation: np.ndarray,
                     name: str | None = None) -> PartMesh:
    """A part's tessellation moved by a rigid placement (no new meshing)."""
    return PartMesh(
        name=name or mesh.name, color=mesh.color, path=mesh.path,
        vertices=mesh.vertices @ rotation.T + translation,
        normals=mesh.normals @ rotation.T,
        triangles=mesh.triangles, triangle_face=mesh.triangle_face,
        segments=mesh.segments @ rotation.T + translation,
        face_map_ok=mesh.face_map_ok, edge_map_ok=mesh.edge_map_ok,
    )


def turned_part(part: NamedPart, mesh: PartMesh | None,
                rotation: np.ndarray) -> tuple[NamedPart, PartMesh | None, np.ndarray]:
    """The part turned by `rotation` and set down on z = 0 with the low corner of its
    bounding box at the origin. Returns (part, mesh, translation)."""
    turned = location_of(rotation, np.zeros(3)) * part.shape
    low = np.array(tuple(turned.bounding_box().min), dtype=float)
    shape = location_of(rotation, -low) * part.shape
    moved = NamedPart(part.name, shape, part.color)
    return moved, (None if mesh is None else transformed_mesh(mesh, rotation, -low)), -low


# --------------------------------------------------------------------------------------
# plating
# --------------------------------------------------------------------------------------


@dataclass(eq=False)
class Item:
    """One piece to put on a plate, already turned and standing on z = 0 at the origin."""

    name: str  # the object's name on the plate (`lid`, or `lid.2` for the second copy)
    part: str
    copy: int
    shape: Any
    mesh: PartMesh | None
    color: str
    rotation: np.ndarray  # model -> turned
    translation: np.ndarray
    size: tuple[float, float, float]
    nozzle: Any  # request.Nozzle or None


@dataclass(eq=False)
class Placed:
    item: Item
    plate: int
    rotation: np.ndarray
    translation: np.ndarray
    shape: Any
    mesh: PartMesh | None
    low: tuple[float, float, float]
    high: tuple[float, float, float]
    quarter_turn: bool
    # How far the plate's pieces were moved together after the shelf layout (`anchor`).
    offset: tuple[float, float] = (0.0, 0.0)

    def to_json(self) -> dict:
        return {
            "name": self.item.name, "part": self.item.part, "copy": self.item.copy,
            "nozzle_id": None if self.item.nozzle is None else self.item.nozzle.id,
            "quarter_turn": self.quarter_turn,
            # 3 x 4, row by row: plate coordinates = R * model coordinates + t.
            "transform": [float(v) for row in range(3)
                          for v in (*self.rotation[row], self.translation[row])],
            "bbox": {"min": [round(v, 6) for v in self.low],
                     "max": [round(v, 6) for v in self.high]},
        }


def _allowed(nozzle: Any, margin: float) -> tuple[float, float, float, float, float] | None:
    """(x low, x high, y low, y high, z room) a part of this nozzle may occupy."""
    if nozzle is None or nozzle.reach is None:
        return None
    reach = nozzle.reach
    return (reach.x[0] + margin, reach.x[1] - margin, reach.y[0] + margin,
            reach.y[1] - margin, reach.z[1] - reach.z[0])


def _nozzle_words(nozzle: Any) -> str:
    return f"{nozzle.role} nozzle" if nozzle.role else f"nozzle {nozzle.id}"


def _touches(low: tuple[float, float], high: tuple[float, float], zone: Any) -> bool:
    """Whether the box from `low` to `high` touches a keep-out zone."""
    slack = KEEPOUT_SLACK_MM / 2
    return (low[0] < zone.max[0] + slack and high[0] > zone.min[0] - slack
            and low[1] < zone.max[1] + slack and high[1] > zone.min[1] - slack)


def touched_zones(low: tuple[float, float], high: tuple[float, float], clearance: float,
                  keepouts: Any) -> list[str]:
    """The ids of the keep-out zones that a piece's footprint, grown by `clearance`,
    touches."""
    grown_low = (low[0] - clearance, low[1] - clearance)
    grown_high = (high[0] + clearance, high[1] + clearance)
    return [zone.id for zone in keepouts if _touches(grown_low, grown_high, zone)]


def lay_out(items: list[Item], gap: float, margin: float, clearance: float = 0.0,
            keepouts: Any = (), anchor: str = FRONT_LEFT) -> tuple[list[Placed], list[dict]]:
    """Shelf layout (module doc). Returns the placed items, with their plate number from 1,
    and the items that fit no plate, each with the reason."""
    keepouts = tuple(keepouts)

    def offsets_of(slots: list, zones: tuple) -> dict[int, tuple[float, float]]:
        return {number: _anchor_offset(
            [(items[index].nozzle, x, y, width, depth)
             for index, plate_of, x, y, width, depth, _ in slots if plate_of == number],
            margin, zones) if anchor == CENTRE else (0.0, 0.0)
            for number in {slot[1] for slot in slots}}

    slots = unplaced = offsets = None
    if anchor == CENTRE and keepouts:
        # Pieces that end up in the middle of the bed are usually far from every zone.
        # Then the rows need not be broken around a zone at the front: lay them out as if
        # there were none, centre them, and keep that when nothing touches a zone.
        slots, unplaced = _shelf(items, gap, margin, clearance, ())
        offsets = offsets_of(slots, ())
        if any(_touches((x + offsets[number][0], y + offsets[number][1]),
                        (x + offsets[number][0] + width, y + offsets[number][1] + depth), zone)
               for _, number, x, y, width, depth, _ in slots for zone in keepouts):
            slots = None
    if slots is None:
        slots, unplaced = _shelf(items, gap, margin, clearance, keepouts)
        offsets = offsets_of(slots, keepouts)
    placed: list[tuple[int, Placed]] = []
    for index, number, x, y, width, depth, turned in slots:
        item = items[index]
        offset = offsets[number]
        # The piece itself lies `clearance` inside its grown footprint.
        x, y = x + clearance + offset[0], y + clearance + offset[1]
        width, depth = width - 2 * clearance, depth - 2 * clearance
        spin = quarter_turn() if turned else np.eye(3)
        # A quarter turn about the origin puts the footprint at x from -(its depth) to 0.
        shift = np.array([x + (item.size[1] if turned else 0.0), y, 0.0])
        mesh = None if item.mesh is None else transformed_mesh(item.mesh, spin, shift, item.name)
        placed.append((index, Placed(
            item=item, plate=number, rotation=spin @ item.rotation,
            translation=spin @ item.translation + shift,
            shape=location_of(spin, shift) * item.shape, mesh=mesh,
            low=(x, y, 0.0), high=(x + width, y + depth, item.size[2]), quarter_turn=turned,
            offset=offset,
        )))
    placed.sort(key=lambda entry: (entry[1].plate, entry[0]))
    return [entry for _, entry in placed], unplaced


def _shelf(items: list[Item], gap: float, margin: float, clearance: float,
           keepouts: tuple) -> tuple[list[tuple], list[dict]]:
    """The rows of the shelf layout: (index, plate, x, y, width, depth, turned) for the
    grown footprint of every piece that found a place, and the pieces that found none."""
    order = sorted(
        range(len(items)),
        key=lambda i: (-min(items[i].size[0], items[i].size[1]),
                       -max(items[i].size[0], items[i].size[1]), i),
    )
    # (index, plate, x, y, width, depth, turned): where each piece's grown footprint lies.
    slots: list[tuple[int, int, float, float, float, float, bool]] = []
    unplaced: list[dict] = []
    # The row being filled: where the next piece goes in X, where the row starts in Y and
    # how deep its deepest piece is. -inf stands for "at the low edge of the reach".
    plate, cursor_x, row_y, row_depth = 1, -math.inf, -math.inf, 0.0
    room_words = f"with a {margin:g} mm margin" + (
        f" and {clearance:g} mm of clearance around it" if clearance > 0.0 else "")
    for index in order:
        item = items[index]
        room = _allowed(item.nozzle, margin)
        if room is None:
            unplaced.append({"name": item.name, "part": item.part, "copy": item.copy,
                             "reason": "the request gives no nozzle reach to place it in",
                             "cause": NO_REACH})
            continue
        x_low, x_high, y_low, y_high, z_room = room
        height = item.size[2]

        def lying(grow: float, item: Item = item, room: tuple = room) -> tuple:
            """(width, depth, turned, fits) of the footprint grown by `grow`: lying along
            X by default; a quarter turn when it only fits the other way."""
            x_low, x_high, y_low, y_high, _ = room
            width, depth = item.size[0] + 2 * grow, item.size[1] + 2 * grow
            turned = width < depth
            if turned:
                width, depth = depth, width
            # The height is judged apart (below): the brim grows the footprint only.
            spare, quarter = reach_spare((width, depth, 0.0),
                                         (x_high - x_low, y_high - y_low, 0.0))
            if quarter:
                width, depth, turned = depth, width, not turned
            return width, depth, turned, spare_fits(spare)

        # The footprint grown by the clearance is what is laid out.
        width, depth, turned, lies = lying(clearance)
        if not lies or height > z_room + REACH_TOLERANCE_MM:
            bare = (clearance > 0.0 and height <= z_room + REACH_TOLERANCE_MM
                    and lying(0.0)[3])
            unplaced.append({
                "name": item.name, "part": item.part, "copy": item.copy,
                "reason": (
                    f"it is {item.size[0]:.1f} x {item.size[1]:.1f} x {item.size[2]:.1f} mm "
                    f"as turned for printing, larger than the {_nozzle_words(item.nozzle)}'s "
                    f"reach of {x_high - x_low:.1f} x {y_high - y_low:.1f} x {z_room:.1f} mm "
                    f"({room_words})"),
                "cause": CLEARANCE if bare else TOO_LARGE,
            })
            continue

        def spot(cursor: float, row: float, width: float = width, depth: float = depth,
                 room: tuple = room) -> tuple[float, float] | None:
            """Where the piece goes in a row that starts at `row`, from `cursor` on: moved
            along the row past every zone it would touch. None when the row has no room."""
            x_low, x_high, y_low, y_high, _ = room
            x, y = max(cursor, x_low), max(row, y_low)
            moved = True
            while moved:
                moved = False
                for zone in keepouts:
                    if _touches((x, y), (x + width, y + depth), zone):
                        x, moved = zone.max[0] + KEEPOUT_SLACK_MM, True
            if x + width <= x_high + 1e-6 and y + depth <= y_high + 1e-6:
                return x, y
            return None

        def empty_row(row: float, spot: Any = spot, y_low: float = room[2]) -> tuple | None:
            """A row of its own from `row` on; behind a zone when the zone leaves no room
            beside it."""
            found = spot(-math.inf, row)
            for zone in sorted(keepouts, key=lambda zone: zone.max[1]):
                if found is not None:
                    break
                behind = zone.max[1] + KEEPOUT_SLACK_MM
                if behind > max(row, y_low):
                    found = spot(-math.inf, behind)
            return found

        # This row, else the next row, else an empty plate.
        if row_depth > 0.0:
            found = spot(cursor_x, row_y)
            if found is None:
                found = empty_row(row_y + row_depth + gap)
                if found is not None:
                    row_depth = 0.0
            if found is None:
                found = empty_row(-math.inf)
                if found is not None:
                    plate, row_depth = plate + 1, 0.0
        else:
            found = empty_row(row_y)  # nothing lies on this plate yet
        if found is None:
            # Only a keep-out zone can leave no room on an empty plate. The row in hand
            # stays as it is for the pieces that follow. Would the piece have lain there
            # without its clearance?
            bare_width, bare_depth, _, _ = lying(0.0)
            bare = clearance > 0.0 and empty_row(
                -math.inf, lambda cursor, row, spot=spot, width=bare_width, depth=bare_depth:
                spot(cursor, row, width, depth)) is not None
            unplaced.append({
                "name": item.name, "part": item.part, "copy": item.copy,
                "reason": (
                    f"it is {item.size[0]:.1f} x {item.size[1]:.1f} mm as turned for "
                    f"printing and finds no room beside the bed's keep-out zones "
                    f"({room_words})"),
                "cause": CLEARANCE if bare else KEEPOUT,
            })
            continue
        x, y = found
        if row_depth == 0.0:
            row_y = y
        cursor_x = x + width + gap
        row_depth = max(row_depth, depth)
        slots.append((index, plate, x, y, width, depth, turned))

    return slots, unplaced


def _anchor_offset(grown: list[tuple[Any, float, float, float, float]], margin: float,
                   keepouts: tuple) -> tuple[float, float]:
    """How far to move one plate's grown footprints (nozzle, x, y, width, depth) together
    so that their bounding box is centred on the reach. Each piece stays inside its own
    nozzle's reach; an axis along which the move would touch a keep-out zone is given up."""
    rooms = [_allowed(nozzle, margin) for nozzle, *_ in grown]
    moves = []
    for axis in (0, 1):
        low = min(entry[1 + axis] for entry in grown)
        high = max(entry[1 + axis] + entry[3 + axis] for entry in grown)
        centre = (max(room[2 * axis] for room in rooms)
                  + min(room[2 * axis + 1] for room in rooms)) / 2
        least = max(room[2 * axis] - entry[1 + axis]
                    for room, entry in zip(rooms, grown, strict=True))
        most = min(room[2 * axis + 1] - (entry[1 + axis] + entry[3 + axis])
                   for room, entry in zip(rooms, grown, strict=True))
        # The shelf layout is inside every reach, so 0 always lies between the two.
        moves.append(min(max(centre - (low + high) / 2, min(least, 0.0)), max(most, 0.0)))
    for dx, dy in ((moves[0], moves[1]), (0.0, moves[1]), (moves[0], 0.0)):
        if not any(_touches((x + dx, y + dy), (x + dx + width, y + dy + depth), zone)
                   for _, x, y, width, depth in grown for zone in keepouts):
            return (dx, dy)
    return (0.0, 0.0)


def plate_tessellation(placed: list[Placed]) -> Tessellation:
    """The meshes of one plate's objects, for the views and the checks."""
    meshes = {p.item.name: p.mesh for p in placed if p.mesh is not None}
    return Tessellation({}, meshes, sum(len(m.triangles) for m in meshes.values()), 0.0)
