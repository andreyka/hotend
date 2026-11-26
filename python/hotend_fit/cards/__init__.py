"""Paired feature cards (docs/PROTOCOL.md section 4.3).

Each card builds both mating halves of one joint from one definition, exposes named ports,
emits its own smallest test gauge and declares the fit pairs the engine verifies. Cards take
no clearance arguments: every gap comes from `ctx.fit`.

    from hotend_fit import cards
    card = cards.snap_lid(opening=(80, 60), wall=2.0, ctx=ctx)
    box = card.apply_a(box, at=Pos(0, 0, height))
    lid = card.apply_b(lid, at=Pos(0, 0, height))
"""

from __future__ import annotations

from typing import Any

from .base import MAX_DECLARED_CARDS, Card, CardError, Gauge, Port, frame_from_json
from .peg_hole import PegHole
from .screw_boss import ScrewBoss
from .snap_lid import SnapLid

snap_lid = SnapLid
peg_hole = PegHole
screw_boss = ScrewBoss

CARDS: dict[str, type[Card]] = {"snap_lid": SnapLid, "peg_hole": PegHole, "screw_boss": ScrewBoss}


def _signature(name: str, card: type[Card]) -> str:
    """The card's call signature without `self` and the private parameters."""
    import inspect

    shown = []
    keyword_only = False
    for parameter in inspect.signature(card.__init__).parameters.values():
        if parameter.name == "self" or parameter.name.startswith("_"):
            continue
        if parameter.kind is inspect.Parameter.KEYWORD_ONLY and not keyword_only:
            shown.append("*")
            keyword_only = True
        default = "" if parameter.default is inspect.Parameter.empty else f"={parameter.default!r}"
        shown.append(f"{parameter.name}{default}")
    return f"cards.{name}({', '.join(shown)})"


def list_cards() -> list[dict]:
    """The cards a model can use: name, call signature, the names of the two halves (which
    are also the port names) and the card's documentation."""
    import inspect

    listing = []
    for name, card in CARDS.items():
        doc = inspect.getdoc(card) or ""
        listing.append({
            "name": name,
            "summary": doc.split("\n", 1)[0],
            "signature": _signature(name, card),
            "half_a": card.half_a,
            "half_b": card.half_b,
            "ports": [card.half_a, card.half_b],
            "doc": doc,
        })
    return listing


def _public_arguments(card: type[Card]) -> set[str]:
    import inspect

    return {
        name for name in inspect.signature(card.__init__).parameters
        if name not in ("self", "ctx", "name") and not name.startswith("_")
    }


def _plain(value: Any) -> bool:
    """True for the JSON values a card argument can be: a number, text, a flag, nothing, or
    a short list of numbers."""
    import math

    if value is None or isinstance(value, (bool, str)):
        return not isinstance(value, str) or len(value) <= 64
    if isinstance(value, (int, float)):
        return math.isfinite(value)
    return (isinstance(value, list) and len(value) <= 4
            and all(isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)
                    for v in value))


def restore_cards(resolver: Any, declarations: Any) -> list[str]:
    """Rebuild cards from `Card.declaration()` data on `resolver`, as if a build had applied
    them: each card registers itself and its pairs there, with the declared frames.

    The data is treated as untrusted: only the card's kind, its public arguments and the two
    frames are read, every clearance, probe and solid comes from the card's own definition
    and this resolver's calibration. Returns one sentence per declaration that could not be
    rebuilt; such a card is not registered, so the caller must report it as a failure.
    """
    if not isinstance(declarations, list):
        return ["the declared fits are not a list of cards"]
    problems = []
    if len(declarations) > MAX_DECLARED_CARDS:
        problems.append(
            f"{len(declarations)} cards were declared; only the first {MAX_DECLARED_CARDS} "
            "are verified"
        )
    for index, raw in enumerate(declarations[:MAX_DECLARED_CARDS]):
        label = f"declared card {index + 1}"
        try:
            if not isinstance(raw, dict) or set(raw) != {"card", "name", "args", "frames"}:
                raise CardError("it is not a card declaration")
            kind, name, args, frames = raw["card"], raw["name"], raw["args"], raw["frames"]
            if not isinstance(kind, str) or kind not in CARDS:
                raise CardError("its kind is not a known card")
            label = f"{kind} card {index + 1}"
            if not isinstance(name, str):
                raise CardError("its name is not text")
            if (not isinstance(args, dict) or not set(args) <= _public_arguments(CARDS[kind])
                    or not all(_plain(value) for value in args.values())):
                raise CardError("its arguments are not those of the card")
            if not isinstance(frames, dict) or set(frames) != {"a", "b"}:
                raise CardError("its frames are malformed")
            placed = {key: None if frames[key] is None else frame_from_json(frames[key])
                      for key in ("a", "b")}
            if "opening" in args and isinstance(args["opening"], list):
                args = {**args, "opening": tuple(args["opening"])}
            card = CARDS[kind](**args, ctx=resolver, name=name)
            card._restore_frames(placed)
        except Exception as exc:  # noqa: BLE001 - untrusted data may break a card anywhere
            problems.append(f"{label} could not be rebuilt: {str(exc)[:200]}")
    return problems


__all__ = [
    "CARDS", "Card", "CardError", "Gauge", "PegHole", "Port", "ScrewBoss", "SnapLid",
    "list_cards", "peg_hole", "restore_cards", "screw_boss", "snap_lid",
]
