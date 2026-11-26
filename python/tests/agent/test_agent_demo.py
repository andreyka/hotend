"""The two cable box conversations that ship with the agent.

They must stay the conversations they claim to be. The scripts are the generated ones; every
call is one the core's registry knows; the short script is the beginning of the long one; and
what the recorded gateway answers say about each version (the answers the scripted agent reads
and reacts to) is what the real engine says about it: the planted mistake fails `min_wall`
and the contract, the fix is the example model, the maker's answers close the questions one
by one, and the roof over the cable slot removes the support the plate summary reported.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

import pytest
from agent_demo_script import (
    CABLE_QUERY,
    MODEL,
    PROFILE,
    SHIPPED,
    SHIPPED_PRINT,
    USER_MESSAGES,
    Replay,
    build,
    build_print,
    print_versions,
    recorded,
    script_calls,
    versions,
)
from agent_fake_core import TOOL_SPECS, AgentProcess

from hotend_agent.scripted import Script, ScriptError, load_script

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from result_contract import check_job_result

pytestmark = pytest.mark.timeout(240)

X2D_CONTEXT = {
    "printer": {"id": "bambu-x2d", "nozzles": [
        {"id": 1, "role": "main", "diameter": 0.4,
         "reach": {"x": [0, 256], "y": [0, 256], "z": [0, 260]}},
        {"id": 0, "role": "auxiliary", "diameter": 0.4,
         "reach": {"x": [20.5, 256], "y": [0, 256], "z": [0, 256]}}]},
    "material": {"id": "pla-basic", "family": "PLA"},
}
#: The check options the core sends for a version of a product (CheckOptions::product in
#: crates/hotend-core/src/engine/request.rs).
PRODUCT_CHECKS = {
    "enabled": True, "min_wall": 0.8, "verify_determinism": False, "sample_ranges": False,
    "orientation": "prepared", "contract": True, "undeclared_joints": True,
}
DESIGN_TOOLS = ["list_cards", "write_model_code", "write_model_code", "render_views",
                "render_views"]
PRINT_TOOLS = ["write_model_code", "write_model_code", "measure", "slice", "write_model_code",
               "measure", "slice", "propose_print"]


@pytest.mark.parametrize(("shipped", "generated", "name"), [
    (SHIPPED, build, "cable_box_demo"), (SHIPPED_PRINT, build_print, "cable_box_print_demo")])
def test_the_shipped_scripts_are_the_generated_ones(shipped: Path, generated: Any, name: str) -> None:
    assert json.loads(shipped.read_text(encoding="utf-8")) == generated(), (
        "run `uv run python tests/agent/agent_demo_script.py`")
    script = load_script(f"builtin:{name}")
    assert isinstance(script, Script) and script.name == name
    assert load_script(str(shipped)) == script


def test_the_short_script_is_the_beginning_of_the_long_one() -> None:
    short, long = load_script("builtin:cable_box_demo"), load_script("builtin:cable_box_print_demo")
    assert long.turns[:len(short.turns)] == short.turns and len(short.turns) == 5
    assert short.user_messages == long.user_messages[:1] == [USER_MESSAGES[0]]
    assert long.user_messages == list(USER_MESSAGES)
    # One message of the maker for each turn that ends a run, and the last turn ends one.
    for script in (short, long):
        assert sum(not turn.calls for turn in script.turns) == len(script.user_messages)
        assert not script.turns[-1].calls
    # What the core's own tests of the short script rely on (crates/hotend-core/tests/
    # real_agent.rs and the demo of hotend-cli): its words and its five calls.
    said = [turn.say for turn in short.turns]
    assert "min_wall" in said[2] and said[-1].endswith("before the first print.")
    assert [tool for tool, _ in script_calls(build())] == DESIGN_TOOLS


def test_a_script_carries_one_user_message_for_each_run_or_none(tmp_path: Path) -> None:
    turns = [{"say": "a", "calls": [{"tool": "list_cards"}]}, {"say": "b"}, {"say": "c"}]
    script = {"protocol": 0, "name": "two_runs", "turns": turns}
    assert Script.model_validate(script).user_messages == []
    assert Script.model_validate({**script, "user_messages": ["1", "2"]}).user_messages == ["1", "2"]
    for wrong in (["1"], ["1", "2", "3"]):
        path = tmp_path / f"{len(wrong)}.json"
        path.write_text(json.dumps({**script, "user_messages": wrong}), encoding="utf-8")
        with pytest.raises(ScriptError, match="there must be one message for each"):
            load_script(str(path))


def test_the_scripts_write_the_planted_mistake_then_the_example_model() -> None:
    calls = script_calls(build_print())
    assert [tool for tool, _ in calls] == DESIGN_TOOLS + PRINT_TOOLS
    thin, good = calls[1][1]["code"], calls[2][1]["code"]
    assert good == MODEL.read_text(encoding="utf-8")
    changed = [(a, b) for a, b in zip(thin.splitlines(), good.splitlines(), strict=True)
               if a != b]
    assert len(changed) == 1 and '"wall"' in changed[0][0], changed
    assert (thin, good) == versions()
    assert tuple(args["code"] for tool, args in calls[5:] if tool == "write_model_code") == (
        print_versions())


def test_every_call_is_one_the_registry_knows() -> None:
    specs = {spec["name"]: spec["input_schema"] for spec in TOOL_SPECS}
    for tool, args in script_calls(build_print()):
        assert set(args) <= set(specs[tool]["properties"]), tool
        assert set(specs[tool].get("required", [])) <= set(args), tool
    profile = specs["slice"]["properties"]["profile"]
    assert re.fullmatch(profile["pattern"], PROFILE)
    assert len(json.dumps(CABLE_QUERY)) < 200 and CABLE_QUERY["kind"] == "distance"
    # The recording holds one answer for each call, in order.
    assert [result["tool"] for result in recorded()] == DESIGN_TOOLS + PRINT_TOOLS


# -- the recorded answers against the real engine ------------------------------------------------


def _engine(folder: Path, request: dict[str, Any]) -> dict[str, Any]:
    """Runs one op of the real engine worker in a job folder, as the core does."""
    (folder / "request.json").write_text(json.dumps({"protocol": 0, **request}), encoding="utf-8")
    done = subprocess.run(
        [sys.executable, "-I", "-m", "hotend_engine.worker", str(folder)],
        capture_output=True, text=True, timeout=150, check=False,
    )
    assert done.returncode == 0, done.stderr[-2000:]
    check_job_result(folder)
    return json.loads((folder / "result.json").read_text(encoding="utf-8"))


def _engine_version(folder: Path, code: str, sliced: bool) -> dict[str, Any]:
    """Builds a script with the product's check options and, for a version the conversation
    slices, measures the cable and lays out the plate."""
    folder.mkdir()
    (folder / "model.py").write_text(code, encoding="utf-8")
    out = {"build": _engine(folder, {
        "op": "build", "model_path": "model.py", "params": {}, "context": X2D_CONTEXT,
        "outputs": {"step": False, "brep": sliced, "mesh": False, "views": []},
        "checks": PRODUCT_CHECKS,
    })}
    if sliced:
        out["measure"] = _engine(folder, {"op": "measure", "queries": [CABLE_QUERY]})
        out["prepare"] = _engine(folder, {"op": "prepare", "context": X2D_CONTEXT,
                                          "threemf": False, "views": []})
    shutil.rmtree(folder)
    return out


def _counts(checks: list[dict[str, Any]]) -> str:
    """The gateway's summary line of a build's checks."""
    count = {status: sum(c["status"] == status for c in checks)
             for status in ("fail", "warn", "pass", "skipped")}
    unknown = sum(_not_checkable(c) for c in checks)
    return (f"checks: {count['fail']} fail, {count['warn']} warn, {count['pass']} pass, "
            f"{count['skipped'] - unknown} skipped"
            + (f", {unknown} not checkable" if unknown else ""))


def _not_checkable(check: dict[str, Any]) -> bool:
    return check["status"] == "skipped" and check["data"].get("not_checkable") is True


def test_the_real_engine_says_what_the_recorded_answers_say(tmp_path: Path) -> None:
    thin, good = versions()
    sized, cable, roof = print_versions()
    names = ("thin", "good", "sized", "cable", "roof")
    with ThreadPoolExecutor(4) as pool:
        thin_v, good_v, sized_v, cable_v, roof_v = pool.map(
            _engine_version, (tmp_path / name for name in names),
            (thin, good, sized, cable, roof), (False, False, False, True, True))
    answers = recorded()
    writes = [r["text"] for r in answers if r["tool"] == "write_model_code"]
    measures = [r["text"] for r in answers if r["tool"] == "measure"]
    slices = [r["text"] for r in answers if r["tool"] == "slice"]

    # Every build: the status, the count of each outcome, every line that is not a pass and
    # every question are the engine's.
    for version, text in zip((thin_v, good_v, sized_v, cable_v, roof_v), writes, strict=True):
        result = version["build"]
        assert text.startswith(f"status: {result['status']}\n")
        assert _counts(result["checks"]) in text
        for check in result["checks"]:
            if check["status"] != "pass":
                mark = {"fail": "FAIL", "warn": "warn", "skipped": "skipped"}[check["status"]]
                if _not_checkable(check):
                    mark = "NOT CHECKABLE"
                assert f"- {mark} {check['id']}" in text and check["message"][:100] in text
        assert f"questions for the user ({len(result['questions'])};" in text
        for question in result["questions"]:
            assert f"- [{question['requirement']}] {question['text']}" in text

    def rows(version: dict[str, Any], status: str) -> set[tuple[str, str | None]]:
        return {(c["id"], c["part"]) for c in version["build"]["checks"] if c["status"] == status}

    def asked(version: dict[str, Any]) -> list[str]:
        return [q["requirement"] for q in version["build"]["questions"]]

    def requirement(version: dict[str, Any], name: str) -> dict[str, Any]:
        return next(c for c in version["build"]["checks"] if c["id"] == "contract"
                    and c["data"].get("requirement") == name)

    # The planted mistake: the wall check fails, and so does the contract's own wall.
    assert thin_v["build"]["status"] == "check_error"
    assert rows(thin_v, "fail") == {("min_wall", "box"), ("contract", "box")}
    assert requirement(thin_v, "walls_print")["status"] == "fail"

    # The fix is clean, every clearance passes, and three things rest on a guess. The guessed
    # space for the power strip is met as built (the example meets its own requirement), and
    # is still only a warning because it is a guess.
    assert good_v["build"]["status"] == "ok" and not rows(good_v, "fail")
    fits = [c for c in good_v["build"]["checks"] if c["id"] == "fit_pairs"]
    pairs = [c for c in fits if c["data"]["kind"] == "pair"]
    assert len(pairs) == 12 and all(c["status"] == "pass" for c in pairs)
    assert [c["status"] for c in fits if c["data"]["kind"] == "joint"] == ["pass", "pass"]
    (unproven,) = [c for c in fits if c["data"]["kind"] == "robustness"]
    assert unproven["status"] == "warn" and "not calibrated" in unproven["message"]
    assert not [c for c in fits if c["data"]["kind"] == "undeclared" and c["status"] != "pass"]
    assert asked(good_v) == ["holds_power_strip", "cables_pass", "lid_snaps"]
    strip = requirement(good_v, "holds_power_strip")
    assert strip["status"] == "warn" and strip["data"]["met"] is True
    assert "a 120 x 60 x 36 mm cuboid fits inside 'box'" in strip["message"]
    assert ("overhangs", "box") in rows(good_v, "warn")
    assert ("overhangs", "lid") not in rows(good_v, "warn"), "judged as it will be printed"

    # The maker's first answer: the space for the strip is measured and met, one question less.
    strip = requirement(sized_v, "holds_power_strip")
    assert strip["status"] == "pass" and strip["data"]["met"] is True
    assert strip["data"]["provenance"] == "measured"
    assert sorted(strip["data"]["size"]) == [35.0, 50.0, 180.0]
    assert asked(sized_v) == ["cables_pass", "lid_snaps"]
    sizes = {part["name"]: part["bbox"] for part in sized_v["build"]["parts"]}
    # 184 x 54 mm inside with 2 mm walls; the screw post stands 6 mm further out at one end.
    assert [hi - lo for lo, hi in zip(sizes["box"]["min"], sizes["box"]["max"], strict=True)] == [
        pytest.approx(194.0), pytest.approx(58.0), pytest.approx(46.0)]
    assert sizes["lid"]["max"][2] == pytest.approx(48.4)

    # The second answer: the slot is measured by the maker, but the contract check cannot
    # measure a named dimension and says so; only the printer's snap fit is still open.
    slot = requirement(cable_v, "cables_pass")
    assert slot["status"] == "skipped" and slot["data"]["provenance"] == "measured"
    assert slot["data"]["not_checkable"] is True and slot["message"].startswith("Not checkable")
    assert asked(cable_v) == asked(roof_v) == ["lid_snaps"]
    assert cable_v["build"]["questions"][0]["guessed_params"] == []

    # So the agent measures it: 4.5 mm from the centre of a 9 mm cable to the nearest wall.
    for version, text in zip((cable_v, roof_v), measures, strict=True):
        (found,) = version["measure"]["results"]
        assert found["ok"] and found["distance"] == pytest.approx(4.5) and not found["inside"]
        assert text.startswith('1. {"distance":4.5,')

    # The plate: the flat top of the slot needs support, the roof removes it. The lid is
    # turned upside down in both, with its snap rails across the layers.
    for version, text, support in ((cable_v, slices[0], True), (roof_v, slices[1], False)):
        plate = version["prepare"]
        assert plate["status"] == "ok" and not plate["unplaced"] and len(plate["plates"]) == 1
        assert "every piece fits a plate" in text and "DID NOT FIT" not in text
        turned = {o["part"]: o for o in plate["orientations"]}
        assert (turned["box"]["turned"], turned["lid"]["turned"]) == ("as modelled", "upside down")
        for name, orientation in turned.items():
            assert f"- {name}: {orientation['turned']}. {orientation['explanation']}" in text
        assert (turned["box"]["measures"]["overhang"] > 0) is support
        assert turned["lid"]["measures"]["weak_snaps"]
        warned = {(c["id"], c["part"]) for c in plate["checks"] if c["status"] == "warn"}
        assert warned == ({("overhangs", "box")} if support else set()) | {
            ("fit_orientation", "lid")}
        assert not [c for c in plate["checks"] if c["status"] == "fail"]
        for check in plate["checks"]:
            if check["status"] == "warn":
                assert f"plate check warn {check['id']}: {check['message']}" in text
        assert ("plate check warn overhangs" in text) is support
    assert "about 48 mm2 needs support" in slices[0]
    assert "'box' is printed as modelled: 10942 mm2 lies flat on the bed and nothing needs "\
           "support." in slices[1]
    assert rows(cable_v, "warn") - rows(roof_v, "warn") == {("overhangs", "box")}


# -- the short conversation through the real agent process ----------------------------------------


def test_the_demo_runs_from_the_first_message_to_the_summary() -> None:
    gateway = Replay(build())
    with AgentProcess(script="builtin:cable_box_demo", tools=gateway) as agent:
        agent.core.run(USER_MESSAGES[0])
        message = agent.core.result(60)
        assert agent.wait() == 0
        core = agent.core
    result = message["result"]
    assert result["stop_reason"] == "done", result
    thin, good = versions()
    assert [name for name, _ in core.tool_calls] == DESIGN_TOOLS and gateway.played == 5
    assert core.tool_calls[1][1]["code"] == thin and core.tool_calls[2][1]["code"] == good
    assert core.tool_calls[4][1] == {"views": ["front"], "section": {"plane": "XZ"}}
    assert [(e["ok"], e["summary"], e["images"]) for e in core.events_of("tool_end")][1:] == [
        (True, "status: check_error", 0), (True, "status: ok", 0),
        (True, "views of version 2: iso", 1),
        (True, "views of version 2: front (section XZ at 0 mm)", 1)]
    text = core.text()
    assert text.startswith("Here is what I will make.")
    assert "the min_wall check failed" in text
    assert text.rstrip().endswith("before the first print.")
    assert result["usage"]["requests"] == 5 and result["usage"]["tool_calls"] == 5
    assert core.llm_requests == [], "the scripted mode never asks for a model"
    assert core.violations == []


def test_the_replayed_gateway_refuses_a_call_out_of_order() -> None:
    gateway = Replay()
    refused = gateway("propose_print", {"note": "too early"})
    assert refused["ok"] is False and refused["error"]["code"] == "unknown_tool"
    assert gateway("list_cards", {})["ok"] is True and gateway.played == 1
