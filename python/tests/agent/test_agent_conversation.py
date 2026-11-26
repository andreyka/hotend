"""Prompt regression without a provider: what the agent is told to do, shown on a whole
conversation.

The long cable box conversation (`builtin:cable_box_print_demo`) is played through the real
agent process, one process per message of the maker, with the conversation so far handed on
as the core does. A stand-in gateway answers with what the real gateway answered (recorded
with the real engine and the stand-in slicer; test_agent_demo.py holds those answers against
the engine). The tests then read the conversation the way a person would: was the contract
written first and kept true, was a question for the user asked and answered, was the plate
summary acted on, was the fit gauge suggested, and was a print only ever proposed.

This is a written conversation, so it shows that the scripts, the gateway's words and the
prompt agree; what a real model does with the prompt is the subject of EVALS.md.
"""

from __future__ import annotations

import ast
from dataclasses import dataclass
from typing import Any

import pytest
from agent_demo_script import PROFILE, USER_MESSAGES, Replay, recorded
from agent_fake_core import TOOL_SPECS, AgentProcess

from hotend_agent import evals
from hotend_agent.prompt import SYSTEM_PROMPT

pytestmark = pytest.mark.timeout(180)


@dataclass
class Run:
    message: str
    #: Everything the agent said in this run.
    text: str
    #: The text of its last turn: what the maker is left with.
    last: str
    #: (tool, arguments, the gateway's answer), in order.
    calls: list[tuple[str, dict[str, Any], str]]
    stop_reason: str
    requests: int
    #: The agent's events, in order: ("text", text) and ("tool", name).
    order: list[tuple[str, str]]


@pytest.fixture(scope="module")
def conversation() -> list[Run]:
    gateway = Replay()
    answers = [result["text"] for result in recorded()]
    history: list[Any] = []
    runs: list[Run] = []
    for message in USER_MESSAGES:
        before = gateway.played
        with AgentProcess(script="builtin:cable_box_print_demo", tools=gateway) as agent:
            agent.core.run(message, history=history)
            result = agent.core.result(60)["result"]
            assert agent.wait() == 0, agent.stderr
            core = agent.core
        assert core.violations == [] and core.llm_requests == []
        order: list[tuple[str, str]] = []
        for event in core.events:
            if event["type"] == "text_delta":
                if order and order[-1][0] == "text":
                    order[-1] = ("text", order[-1][1] + event["text"])
                else:
                    order.append(("text", event["text"]))
            elif event["type"] == "tool_start":
                order.append(("tool", event["name"]))
        said = [text for kind, text in order if kind == "text"]
        calls = [(name, args, answers[before + index])
                 for index, (name, args) in enumerate(core.tool_calls)]
        assert before + len(calls) == gateway.played
        runs.append(Run(message, core.text(), said[-1].strip(), calls, result["stop_reason"],
                        result["usage"]["requests"], order))
        history = result["history"]
    assert gateway.played == len(answers)
    return runs


def _declared(code: str, name: str) -> Any:
    """The literal a model script assigns to `PARAMS` or `CONTRACT`."""
    for node in ast.parse(code).body:
        if isinstance(node, ast.Assign) and getattr(node.targets[0], "id", None) == name:
            return ast.literal_eval(node.value)
    raise AssertionError(f"the script declares no {name}")


def _requirement(code: str, name: str) -> dict[str, Any]:
    return next(r for r in _declared(code, "CONTRACT")["requirements"] if r["id"] == name)


def _writes(run: Run) -> list[tuple[str, str]]:
    """(code, the gateway's answer) of the run's `write_model_code` calls."""
    return [(args["code"], answer) for name, args, answer in run.calls
            if name == "write_model_code"]


def test_the_conversation_runs_to_its_end(conversation: list[Run]) -> None:
    assert [run.stop_reason for run in conversation] == ["done"] * 4
    assert [run.requests for run in conversation] == [5, 2, 5, 2]
    assert [[name for name, _, _ in run.calls] for run in conversation] == [
        ["list_cards", "write_model_code", "write_model_code", "render_views", "render_views"],
        ["write_model_code"],
        ["write_model_code", "measure", "slice", "write_model_code", "measure", "slice"],
        ["propose_print"],
    ]
    offered = {spec["name"] for spec in TOOL_SPECS}
    assert {name for run in conversation for name, _, _ in run.calls} <= offered
    # No call was refused: the scripted agent reacts to answers, not to errors.
    assert not [answer for run in conversation for _, _, answer in run.calls
                if answer.startswith("error")]


def test_the_contract_comes_first_and_is_part_of_every_version(conversation: list[Run]) -> None:
    first = conversation[0]
    kind, opening = first.order[0]
    assert kind == "text" and opening.startswith("Here is what I will make.")
    # Every number is named with where it comes from, before any geometry is written.
    for word in ("guessed", "style", "standard", "your printer's calibration"):
        assert word in opening, word
    assert first.order.index(("tool", "write_model_code")) > 0
    for run in conversation:
        for code, _ in _writes(run):
            contract = _declared(code, "CONTRACT")
            assert {r["id"] for r in contract["requirements"]} == {
                "holds_power_strip", "cables_pass", "lid_snaps", "lid_screwed", "walls_print"}
    # A failed check is fixed in the model; the contract is not loosened to pass it.
    (thin, thin_answer), (good, _) = _writes(first)
    assert "FAIL contract [box]: Requirement 'walls_print'" in thin_answer
    assert _declared(thin, "CONTRACT") == _declared(good, "CONTRACT")
    assert _declared(thin, "PARAMS")["wall"] != _declared(good, "PARAMS")["wall"]


def test_a_question_for_the_user_is_asked_and_answered(conversation: list[Run]) -> None:
    first, second, third, _ = conversation
    # The build reports three questions; the agent does not answer them and asks the first
    # one, alone, in plain words, at the end of its message.
    _, answer = _writes(first)[-1]
    assert "questions for the user (3; ask them, do not answer them yourself):" in answer
    assert answer.index("- [holds_power_strip]") < answer.index("- [cables_pass]") < (
        answer.index("- [lid_snaps]"))
    assert evals.questions_asked(first.last) == 1
    assert first.last.count("?") == 1 and "one at a time" in first.last
    question = first.last[:first.last.index("?") + 1].rsplit(": ", 1)[-1]
    assert question == "how long, how wide and how high is your power strip with the plugs in?"
    assert "A ruler is enough." in first.last
    for jargon in ("cavity_min", "holds_power_strip", "provenance", "CONTRACT", "requirement"):
        assert jargon not in first.last, jargon
    before = _writes(first)[-1][0]
    assert _requirement(before, "holds_power_strip")["provenance"] == "guessed"

    # The maker answers. The numbers they gave go into the contract and the parameters,
    # tagged as measured, in the same write as the geometry; the question is closed.
    for number in ("180", "50", "35"):
        assert number in second.message
    (sized, answer), = _writes(second)
    strip = _requirement(sized, "holds_power_strip")
    assert (strip["kind"], strip["size"], strip["provenance"]) == (
        "cavity_min", [180.0, 50.0, 35.0], "measured")
    params, old = _declared(sized, "PARAMS"), _declared(before, "PARAMS")
    for name in strip["params"]:
        assert params[name]["provenance"] == "measured" and params[name] != old[name], name
    assert "questions for the user (2;" in answer and "[holds_power_strip]" not in answer
    # What the maker did not answer stays a guess, and the next question is asked, alone.
    assert _requirement(sized, "cables_pass")["provenance"] == "guessed"
    assert params["cable"]["provenance"] == "guessed"
    assert evals.questions_asked(second.last) == 1
    assert "how thick is the thickest cable" in second.last and "my guess" in second.last

    # The second answer closes the second question the same way.
    assert "9 mm" in third.message
    cable, answer = _writes(third)[0]
    slot = _requirement(cable, "cables_pass")
    assert slot["provenance"] == "measured" and "9 mm" in slot["note"]
    assert _declared(cable, "PARAMS")["cable"]["provenance"] == "measured"
    assert "questions for the user (1;" in answer and "- [lid_snaps]" in answer
    # The fit that nobody measured is never tagged as measured by the agent.
    for run in conversation:
        for code, _ in _writes(run):
            assert _requirement(code, "lid_snaps")["provenance"] == "guessed"


def test_no_message_asks_more_than_one_question(conversation: list[Run]) -> None:
    for run in conversation:
        assert evals.questions_asked(run.text) <= 1, run.text
    assert [evals.questions_asked(run.last) for run in conversation] == [1, 1, 1, 0]


def test_a_skipped_requirement_is_not_reported_as_passed(conversation: list[Run]) -> None:
    third = conversation[2]
    answer = _writes(third)[0][1]
    assert ("- NOT CHECKABLE contract [box]: Not checkable: requirement 'cables_pass'"
            in answer)
    # The agent says the check proved nothing and measures the slot itself.
    assert third.order.index(("tool", "measure")) < third.order.index(("tool", "slice"))
    told = third.order[third.order.index(("tool", "measure")) - 1][1]
    assert "not checkable, which proves nothing" in told and "measuring it myself" in told
    name, args, measured = third.calls[1]
    assert name == "measure" and '"distance":4.5' in measured
    assert args["queries"][0]["b"] == {"part": "box"}


def test_a_preparation_warning_is_acted_on(conversation: list[Run]) -> None:
    third = conversation[2]
    names = [name for name, _, _ in third.calls]
    first_slice, second_slice = (i for i, name in enumerate(names) if name == "slice")
    _, args, summary = third.calls[first_slice]
    assert args == {"profile": PROFILE}
    # The plate summary of the first slice: the box needs support.
    assert "orientation on the plate:" in summary
    assert "- box: as modelled." in summary and "about 48 mm2 needs support" in summary
    assert "plate check warn overhangs" in summary
    # The agent does not go on to a print job: its next step changes the model, and it says
    # why in the maker's words.
    assert names[first_slice + 1] == "write_model_code"
    assert "propose_print" not in names
    reaction = third.order[third.order.index(("tool", "slice")) + 1]
    assert reaction[0] == "text" and "would need support" in reaction[1]
    assert "steeper than 45 degrees" in reaction[1]
    before, roof = _writes(third)[0][0], _writes(third)[1][0]
    assert _declared(before, "PARAMS") == _declared(roof, "PARAMS")
    assert _declared(before, "CONTRACT") == _declared(roof, "CONTRACT")
    changed = [line for line in roof.splitlines() if line not in before.splitlines()]
    assert changed and all("slot" in line or "outline" in line or "half" in line
                           or "shoulder" in line or "support" in line for line in changed), changed
    assert "status: ok" in _writes(third)[1][1] and "warn overhangs" not in _writes(third)[1][1]
    # A changed model is checked and sliced again, and the warning is gone.
    assert names[first_slice + 2:] == ["measure", "slice"]
    again = third.calls[second_slice][2]
    assert "status: ok (version 5," in again and "status: ok (version 4," in summary
    assert "- box: as modelled." in again and "about 48 mm2 needs support" not in again
    assert "plate check warn overhangs" not in again and "every piece fits a plate" in again
    # What cannot be designed away is told to the maker, with what to do about it.
    assert "plate check warn fit_orientation" in again
    assert "across the layers" in third.last and "Push the lid on gently." in third.last
    # The stand-in slicer's warnings are passed on, not hidden.
    assert "warning fake_slicer" in again and "stand-in slicer" in third.last
    assert "slice again with your real slicer before you print" in third.last


def test_the_fit_gauge_is_suggested_before_a_first_print(conversation: list[Run]) -> None:
    first, _, third, fourth = conversation
    # Why: every build warns that the printer is not calibrated, and one question is about it.
    for run in conversation[:3]:
        for _, answer in _writes(run):
            assert "- warn fit_pairs: This printer is not calibrated" in answer
            assert "- [lid_snaps] Nobody has measured the snap fit" in answer
    # The warning is not a reason to change the code: no version draws a clearance itself.
    for run in conversation:
        for code, _ in _writes(run):
            assert "cards.snap_lid(" in code and "clearance" not in code.split("def build")[1]
    # Before any print job exists, the agent suggests the gauge, says how the maker gets it
    # (it has no tool for it) and leaves the decision to them as its one question.
    assert not [name for run in conversation[:3] for name, _, _ in run.calls
                if name == "propose_print"]
    assert "I suggest printing the fit gauge" in third.last
    assert "`hotend gauge`" in third.last and "`hotend calibrate`" in third.last
    assert third.last.rstrip().endswith(
        "Would you like to print the gauge first, or shall I queue the box for your review "
        "as it is?")
    assert "`hotend gauge`" in SYSTEM_PROMPT and "`hotend calibrate`" in SYSTEM_PROMPT
    # It was announced with the first version, and it is still what is open at the end.
    assert "Your printer's fits have not been measured" in first.last
    assert "Still open: the snap fit on your printer. Print the fit gauge" in fourth.last
    # The harness that scores a real model finds the suggestion the same way.
    assert any("gauge" in run.text.lower() for run in conversation[:3])


def test_a_print_is_proposed_only_when_asked_and_never_claimed(conversation: list[Run]) -> None:
    third, fourth = conversation[2], conversation[3]
    # The maker was told what is open and asked for the job anyway.
    assert "gauge" in third.last and "Queue the box for my review anyway" in fourth.message
    (name, args, answer), = fourth.calls
    assert name == "propose_print" and set(args) == {"note"}
    assert "gauge first" in args["note"] and "slice again before printing" in args["note"]
    # The job is for the version that was sliced last; nothing was changed in between.
    assert "status: ok (version 5," in third.calls[-1][2] and third.calls[-1][0] == "slice"
    assert answer.startswith("pending print job ") and "created for version 5." in answer
    assert "no tool can start the print" in answer
    assert "waits for your review" in fourth.last
    assert "I have not started a print and I cannot" in fourth.last
    assert "only you can confirm a job, in Hotend's own confirmation" in fourth.last
    for run in conversation:
        assert evals.claims_a_print(run.text) is None, run.text
    # The check that says so would notice a claim.
    assert evals.claims_a_print("I have started the print on your printer.")
    assert evals.claims_a_print("The job is now printing.")
