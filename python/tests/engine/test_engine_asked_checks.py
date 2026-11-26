"""What a build says only when the request asks (`checks.undeclared_joints`,
`checks.orientation`), and the print notes a build leaves for the `prepare` op."""

from __future__ import annotations

import json

import pytest
from build123d import Box

from hotend_engine.parts import (
    MAX_INDEX_BYTES,
    NamedPart,
    load_parts,
    load_print_notes,
    save_parts,
)


def kinds(result: dict) -> list[str]:
    return [c["data"].get("kind") for c in result["checks"] if c["id"] == "fit_pairs"]


def test_a_hand_made_lid_lip_is_warned_about_as_an_undeclared_joint(builds):
    job = builds["lidded_box_asked"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "ok"  # a warning: the version still stands
    (found,) = [c for c in result["checks"] if c["data"].get("kind") == "undeclared"]
    assert found["id"] == "fit_pairs" and found["status"] == "warn" and found["part"] is None
    assert found["data"]["parts"] == ["box", "lid"]
    # The script types `gap` = 0.3 mm between the lip and the inside of the box.
    assert found["data"]["gap"] == pytest.approx(0.3, abs=0.01)
    assert found["data"]["area"] > 400 and found["data"]["reach"] == 1.0
    assert found["message"].startswith("'box' and 'lid' face each other about 0.30 mm apart")
    assert "no feature card made the faces" in found["message"]

    # Not asked: the same model says only that it declares no fits, as before.
    plain = builds["lidded_box"].result
    (only,) = [c for c in plain["checks"] if c["id"] == "fit_pairs"]
    assert only["status"] == "skipped" and only["message"] == "Skipped: no declared fits."


def test_joints_made_by_feature_cards_are_not_reported_as_undeclared(builds):
    result = builds["cable_box_asked"].result
    found = kinds(result)
    assert "undeclared" not in found
    assert found.count("pair") >= 8 and "joint" in found
    assert not [c for c in result["checks"] if c["status"] == "fail"]


def test_a_build_leaves_print_notes_beside_the_solids(builds):
    hinted = json.loads((builds["print_hint"].dir / "model.parts.json").read_text("utf-8"))
    assert [part["name"] for part in hinted["parts"]] == ["post", "dome", "tray"]
    assert hinted["print"] == {
        "orientations": {"post": [0.0, 0.0, 1.0], "dome": [0.0, 0.0, -1.0]}, "fits": []}

    cable = builds["cable_box_asked"].dir
    notes = load_print_notes(cable)
    assert notes["orientations"] == {} and not notes["fits_left_out"]
    assert sorted({card["card"] for card in notes["fits"]}) == ["screw_boss", "snap_lid"]
    assert (cable / "model.parts.json").stat().st_size < MAX_INDEX_BYTES
    # The parts are read back as before; the notes are an extra key.
    assert [part.name for part in load_parts(cable)] == ["box", "lid"]


def test_print_notes_never_outgrow_the_index_and_junk_is_ignored(tmp_path):
    parts = [NamedPart("part", Box(10, 10, 10), "#4c78a8")]
    small = {"orientations": {"part": [0.0, 0.0, -1.0]}, "fits": [{"card": "peg_hole"}]}
    assert save_parts(parts, tmp_path, small) is True
    assert load_print_notes(tmp_path) == {
        "orientations": {"part": (0.0, 0.0, -1.0)}, "fits": [{"card": "peg_hole"}],
        "fits_left_out": False}

    huge = {"orientations": {}, "fits": [{"card": "peg_hole", "name": "x" * 200}] * 400}
    assert save_parts(parts, tmp_path, huge) is False
    assert (tmp_path / "model.parts.json").stat().st_size < MAX_INDEX_BYTES
    assert load_print_notes(tmp_path) == {"orientations": {}, "fits": None,
                                          "fits_left_out": True}
    assert [part.name for part in load_parts(tmp_path)] == ["part"]

    index = tmp_path / "model.parts.json"
    for junk in ('{"protocol": 0, "parts": [], "print": "upside down"}', "not json", "[]",
                 ('{"print": {"orientations": {"part": [0, 0, 0], "Bad Name": [0, 0, 1],'
                  ' "other": "up", "nan": [0, 0, 1e400]}, "fits": {"a": 1}}}')):
        index.write_text(junk, encoding="utf-8")
        notes = load_print_notes(tmp_path)
        assert notes in ({}, {"orientations": {}, "fits": None, "fits_left_out": False}), junk
    index.unlink()
    assert load_print_notes(tmp_path) == {}
