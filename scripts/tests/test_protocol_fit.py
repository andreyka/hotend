"""docs/PROTOCOL.md section 4.2 says what is enforced about typed clearances; the engine agrees.

The sentences below are in the document, and each is shown on a real build, driven the way
the core drives the engine (a job folder with request.json, then
`python -I -m hotend_engine.worker <job-dir>`, then result.json). If the engine starts to
say something about hand-made joints without being asked, stops warning about them when it is
asked (`checks.undeclared_joints`), or stops verifying card faces, this fails: change the
document in the same change.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "python" / "tests"))

from result_contract import check_job_result

PROTOCOL = Path(__file__).resolve().parents[2] / "docs" / "PROTOCOL.md"

# What the document says (compared with runs of white space collapsed).
VERIFIED = ("`fit_pairs` verifies the mating faces a feature card made, and nothing else.")
CARD_FACE_FAILS = ("A card face that does not have the clearance this build's resolver issued "
                   "for it is a `fail`")
HAND_MADE_NOT_VERIFIED = ("A joint the script makes by hand with a typed gap is never "
                          "verified, in a model with or without cards.")
SILENT_UNLESS_ASKED = ("Unless the request asks, nothing looks for mating surfaces outside the "
                       "declared pairs, so such a build is `ok` without a word")
WARNED_WHEN_ASKED = ("With `checks.undeclared_joints` it is reported as a `warn` (`data.kind` = "
                     "`undeclared`, section 4.4)")
# What it said before, and what no check does.
OLD_PROMISE = "A literal clearance in a feature card or a model that uses a card is a check failure"

CONTEXT = {
    "printer": {"id": "bambu-x2d", "nozzles": [
        {"id": 1, "role": "main", "diameter": 0.4,
         "reach": {"x": [0, 256], "y": [0, 256], "z": [0, 260]}},
        {"id": 0, "role": "auxiliary", "diameter": 0.4,
         "reach": {"x": [20.5, 256], "y": [0, 256], "z": [0, 256]}}]},
    "material": {"id": "pla-basic", "family": "PLA"},
}

# A base and a cover joined by one peg_hole card. `HAND_MADE` adds a second peg and hole
# with a typed gap of 0.02 mm per side (a slide fit on this printer needs far more);
# `BORED_OUT` widens the card's own hole by a typed 0.3 mm after the card made it.
MODEL = '''
from build123d import *
from hotend_fit import cards

PARAMS = {{}}
BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)
HAND_MADE = {hand_made}
BORED_OUT = {bored_out}


def build(p, ctx):
    base = Box(60, 30, 10, align=BOTTOM)
    cover = Pos(0, 0, 10) * Box(60, 30, 3, align=BOTTOM)
    if HAND_MADE:
        gap = 0.02
        base -= Pos(15, 0, 4) * Cylinder(3.0 + gap, 6.0, align=BOTTOM)
        cover += Pos(15, 0, 4.5) * Cylinder(3.0, 5.5, align=BOTTOM)
    joint = cards.peg_hole(diameter=6.0, length=6.0, fit="slide", ctx=ctx)
    base = joint.apply_a(base, at=Pos(-15, 0, 10))
    cover = joint.apply_b(cover, at=Pos(-15, 0, 10))
    if BORED_OUT:
        base -= Pos(-15, 0, 4) * Cylinder(3.0 + 0.3, 6.0, align=BOTTOM)
    return {{"base": base, "cover": cover}}
'''
# `asked` is the request's `checks.undeclared_joints`.
VARIANTS = {
    "card_only": {"hand_made": False, "bored_out": False, "asked": False},
    "card_and_hand_made_joint": {"hand_made": True, "bored_out": False, "asked": False},
    "card_hole_bored_out": {"hand_made": False, "bored_out": True, "asked": False},
    "card_only_asked": {"hand_made": False, "bored_out": False, "asked": True},
    "card_and_hand_made_joint_asked": {"hand_made": True, "bored_out": False, "asked": True},
}


def _build(job: Path, flags: dict) -> dict:
    job.mkdir()
    script = MODEL.format(hand_made=flags["hand_made"], bored_out=flags["bored_out"])
    (job / "model.py").write_text(script, encoding="utf-8")
    request = {"protocol": 0, "op": "build", "model_path": "model.py", "context": CONTEXT,
               "outputs": {"step": False, "brep": False, "mesh": False, "views": []}}
    if flags["asked"]:
        request["checks"] = {"undeclared_joints": True}
    (job / "request.json").write_text(json.dumps(request), encoding="utf-8")
    done = subprocess.run([sys.executable, "-I", "-m", "hotend_engine.worker", str(job)],
                          capture_output=True, text=True, timeout=180, check=False)
    assert done.returncode == 0, f"{job.name}: worker exit {done.returncode}\n{done.stderr}"
    check_job_result(job)
    return json.loads((job / "result.json").read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def builds(tmp_path_factory) -> dict[str, dict]:
    root = tmp_path_factory.mktemp("protocol-fit")
    with ThreadPoolExecutor(max_workers=len(VARIANTS)) as pool:
        futures = {name: pool.submit(_build, root / name, flags)
                   for name, flags in VARIANTS.items()}
        return {name: future.result() for name, future in futures.items()}


def _protocol() -> str:
    return re.sub(r"\s+", " ", PROTOCOL.read_text(encoding="utf-8"))


def _joint_checks(result: dict) -> list[tuple]:
    """Every result of the two checks that look at joints, without the measured numbers."""
    return sorted(
        (check["id"], check["status"], check["data"].get("kind"), check["data"].get("pair"))
        for check in result["checks"] if check["id"] in ("fit_pairs", "parts_do_not_intersect"))


def test_the_document_says_what_is_checked_and_no_more():
    text = _protocol()
    assert OLD_PROMISE not in text, "PROTOCOL.md promises a check that does not exist"
    for sentence in (VERIFIED, CARD_FACE_FAILS, HAND_MADE_NOT_VERIFIED, SILENT_UNLESS_ASKED,
                     WARNED_WHEN_ASKED):
        assert sentence in text, f"PROTOCOL.md section 4.2 no longer says: {sentence}"


def test_the_card_alone_builds_and_its_pairs_pass(builds):
    result = builds["card_only"]
    assert result["status"] == "ok", result["error"]
    pairs = [c for c in result["checks"]
             if c["id"] == "fit_pairs" and c["data"].get("kind") == "pair"]
    assert pairs and all(c["status"] == "pass" for c in pairs), pairs


def test_a_hand_made_joint_with_a_typed_gap_is_not_detected_unless_asked(builds):
    plain, hand_made = builds["card_only"], builds["card_and_hand_made_joint"]
    # The joint is really there: both parts changed.
    for before, after in zip(plain["parts"], hand_made["parts"], strict=True):
        assert before["name"] == after["name"]
        assert abs(before["volume"] - after["volume"]) > 100
    # And no check that looks at joints says anything different about the model.
    assert hand_made["status"] == "ok", hand_made["error"]
    assert _joint_checks(hand_made) == _joint_checks(plain)
    assert not any(c["status"] == "fail" for c in hand_made["checks"])


def test_a_hand_made_joint_is_warned_about_when_the_request_asks(builds):
    plain, hand_made = builds["card_only_asked"], builds["card_and_hand_made_joint_asked"]
    # The card's own joint is never reported as undeclared.
    assert _joint_checks(plain) == _joint_checks(builds["card_only"])
    undeclared = [c for c in hand_made["checks"] if c["data"].get("kind") == "undeclared"]
    assert len(undeclared) == 1, hand_made["checks"]
    (found,) = undeclared
    assert found["id"] == "fit_pairs" and found["status"] == "warn"
    assert found["data"]["parts"] == ["base", "cover"]
    # The typed gap is 0.02 mm on the radius: far inside the 1 mm that is looked at.
    assert found["data"]["gap_max"] <= 1.0
    assert "no feature card made the faces" in found["message"]
    # A warning: the joint is pointed out and still not verified, so the build stands.
    assert hand_made["status"] == "ok", hand_made["error"]
    assert not any(c["status"] == "fail" for c in hand_made["checks"])
    # Everything else the joint checks say is what they say without the option.
    rest = [row for row in _joint_checks(hand_made) if row[2] != "undeclared"]
    assert rest == _joint_checks(builds["card_and_hand_made_joint"])


def test_a_card_face_given_a_typed_clearance_fails(builds):
    result = builds["card_hole_bored_out"]
    assert result["status"] == "check_error"
    failed = [c for c in result["checks"] if c["status"] == "fail"]
    assert failed and all(c["id"] == "fit_pairs" for c in failed), failed
    assert any(c["data"].get("kind") == "pair" for c in failed), failed
