"""Every material family has a print card, and every print card a family.

The print cards live with the print settings rules (`crates/hotend-settings/data/
print-cards.toml`); the families live in `hotend_fit.materials`. A family on one side only
would give a design Hotend can make and cannot print, or a card nothing can ask for.

Both sides also read a flexible material's hardness from its id. The rules refuse a grade that
is too soft and slow a soft one down, so a hardness the two sides read differently would let
a spool past one of them: `crates/hotend-settings/tests/data/shore-table.json` is one table,
held against the rules by their own test and against `hotend_fit.materials` here."""

from __future__ import annotations

import json
import tomllib
from pathlib import Path

from hotend_fit.materials import FLEXIBLE_FAMILIES, list_materials, material_facts

SETTINGS = Path(__file__).resolve().parents[3] / "crates/hotend-settings"
CARDS = SETTINGS / "data/print-cards.toml"
SHORE_TABLE = SETTINGS / "tests/data/shore-table.json"


def cards() -> list[dict]:
    with CARDS.open("rb") as file:
        return tomllib.load(file)["card"]


def test_every_family_has_exactly_one_print_card_and_the_reverse():
    families = [material["family"] for material in list_materials()]
    carded = [card["family"] for card in cards()]

    assert len(carded) == len(set(carded)), "a family has two print cards"
    assert sorted(carded) == sorted(families)


def test_a_print_card_is_flexible_exactly_when_its_family_is():
    flexible = {card["family"] for card in cards() if card["flexible"]}

    assert flexible == set(FLEXIBLE_FAMILIES)


def test_the_hardness_of_an_id_is_read_like_the_rules_read_it():
    rows = json.loads(SHORE_TABLE.read_text(encoding="utf-8"))["rows"]
    assert len(rows) >= 12

    for family in FLEXIBLE_FAMILIES:
        for row in rows:
            wanted = None if row["shore_a"] is None else f"{row['shore_a']}A"
            read = material_facts({"id": row["id"], "family": family}).shore
            assert read == wanted, row["id"]
