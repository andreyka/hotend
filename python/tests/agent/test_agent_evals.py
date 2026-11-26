"""The evaluation harness (hotend_agent/evals.py, the plan of EVALS.md), without a provider.

No test here runs the `hotend` command or asks a model. The scorer is tried on what the
harness recorded of the shipped scripted conversation (fixtures/cable_box_print_demo_eval.json,
written by `agent_demo_script.py --record` from a run through the real core), and on that
record with one thing made wrong at a time: each criterion must notice its own failure.
"""

from __future__ import annotations

import copy
import json
import re
import subprocess
from pathlib import Path
from typing import Any

import pytest
from agent_demo_script import SNAPSHOTS, USER_MESSAGES

from hotend_agent import evals
from hotend_agent.scripted import load_script
from hotend_fit import cards

pytestmark = pytest.mark.timeout(60)

EVALS_MD = Path(evals.__file__).with_name("EVALS.md")
KEY = "placeholder-value-not-a-key"


def _recorded() -> tuple[list[evals.RunLog], evals.Snapshot, evals.Snapshot]:
    data = json.loads(SNAPSHOTS.read_text(encoding="utf-8"))
    runs = [evals.RunLog(**{**run, "tools": [tuple(tool) for tool in run["tools"]]})
            for run in data["runs"]]
    return runs, evals.Snapshot(**data["first"]), evals.Snapshot(**data["final"])


def _score(change: Any = None) -> dict[str, evals.Verdict]:
    runs, first, final = copy.deepcopy(_recorded())
    if change:
        change(runs, first, final)
    return evals.score(evals.SCRIPTED_EXPECT, runs, first, final, ask_before=len(runs) - 1)


@pytest.fixture(autouse=True)
def no_provider(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in (evals.PROVIDER_ENV, evals.MODEL_ENV, evals.HOTEND_ENV, evals.PRICE_IN_ENV,
                 evals.PRICE_OUT_ENV):
        monkeypatch.delenv(name, raising=False)


@pytest.fixture
def no_commands(monkeypatch: pytest.MonkeyPatch) -> list[Any]:
    """Fails the test if the harness starts any program."""
    started: list[Any] = []

    def refuse(*args: Any, **kwargs: Any) -> None:
        started.append(args)
        raise AssertionError(f"the harness started a program: {args}")

    monkeypatch.setattr(subprocess, "run", refuse)
    monkeypatch.setattr(subprocess, "Popen", refuse)
    return started


# -- enabling ------------------------------------------------------------------------------------


def test_without_a_provider_it_says_how_to_enable_and_runs_nothing(
    no_commands: list[Any], capsys: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch,
) -> None:
    assert evals.main([]) == 2
    out = capsys.readouterr().out
    assert out.strip() == evals.HOW_TO_ENABLE.strip()
    for word in (evals.PROVIDER_ENV, evals.MODEL_ENV, "python -m hotend_agent.evals",
                 "--scripted", "EVALS.md", "costs real money"):
        assert word in out, word
    # A provider without a model, or a model without a provider, is not enough either.
    monkeypatch.setenv(evals.PROVIDER_ENV, "anthropic")
    assert evals.main([]) == 2
    monkeypatch.delenv(evals.PROVIDER_ENV)
    monkeypatch.setenv(evals.MODEL_ENV, "some-model")
    assert evals.main(["--brief", "coaster"]) == 2
    assert no_commands == []


def test_with_a_provider_but_no_key_nothing_is_sent(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], tmp_path: Path,
) -> None:
    binary = tmp_path / "hotend"
    binary.write_text("")
    asked: list[str] = []
    monkeypatch.setenv(evals.PROVIDER_ENV, "anthropic")
    monkeypatch.setenv(evals.MODEL_ENV, "some-model")
    monkeypatch.setattr(evals, "find_hotend", lambda: binary)
    monkeypatch.setattr(evals.Hotend, "key_state",
                        lambda self, provider: asked.append(provider) or "not_set")
    monkeypatch.setattr(evals, "run_brief", lambda *a, **k: pytest.fail("a brief was run"))
    assert evals.main(["--out", str(tmp_path / "out")]) == 2
    out = capsys.readouterr().out
    assert asked == ["anthropic"] and "No key for anthropic is in the environment." in out
    assert evals.HOW_TO_ENABLE.strip() in out
    assert not (tmp_path / "out").exists()

    monkeypatch.setenv(evals.PROVIDER_ENV, "somebody-else")
    assert evals.main([]) == 2
    assert "must be one of anthropic, openai, google, openrouter, ollama" in capsys.readouterr().out

    monkeypatch.setattr(evals, "find_hotend", lambda: None)
    assert evals.main([]) == 2
    assert "`hotend` command was not found" in capsys.readouterr().out


def test_an_enabled_run_goes_through_hotend_chat_and_never_touches_the_key(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], tmp_path: Path,
) -> None:
    binary = tmp_path / "hotend"
    binary.write_text("")
    monkeypatch.setenv(evals.PROVIDER_ENV, "anthropic")
    monkeypatch.setenv(evals.MODEL_ENV, "some-model")
    monkeypatch.setenv("ANTHROPIC_API_KEY", KEY)
    monkeypatch.setenv(evals.PRICE_IN_ENV, "2")
    monkeypatch.setenv(evals.PRICE_OUT_ENV, "10")
    monkeypatch.setattr(evals, "find_hotend", lambda: binary)
    monkeypatch.setattr(evals.Hotend, "key_state", lambda self, provider: "set")
    commands: list[list[str]] = []
    runs, first, final = _recorded()

    def chat_only(command: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
        """Stands in for the `hotend` command: answers nothing, records what was asked."""
        commands.append(command)
        assert kwargs["stdin"] == subprocess.DEVNULL and "env" not in kwargs
        return subprocess.CompletedProcess(command, 1, "", "")

    monkeypatch.setattr(subprocess, "run", chat_only)
    out = tmp_path / "out"
    assert evals.main(["--brief", "coaster", "--out", str(out)]) == 1
    printed = capsys.readouterr().out

    chats = [command for command in commands if "chat" in command]
    assert len(chats) == 1, "a failed first message ends the brief"
    (chat,) = chats
    assert chat[:4] == [str(binary), "--data-dir", str(out / "coaster"), "chat"]
    assert chat[4:6] == ["coaster", evals.BRIEFS[0].request]
    assert chat[6:] == ["--provider", "anthropic", "--model", "some-model", "--secrets", "env",
                        "--max-turns", str(evals.MAX_TURNS)]
    # Everything else it runs is a read-only question to the same data directory, or the
    # choice of the stand-in slicer.
    for command in commands:
        assert command[:3] == [str(binary), "--data-dir", str(out / "coaster")]
        assert command[3] in ("chat", "audit", "project", "questions", "settings"), command
    report = (out / "report.json").read_text(encoding="utf-8")
    assert KEY not in report and KEY not in printed
    assert not [command for command in commands if KEY in " ".join(command)]
    summary = json.loads(report)
    assert (summary["label"], summary["passed"], summary["briefs"]) == ("anthropic some-model", 0, 1)
    assert summary["outcomes"][0]["verdicts"]["within_budget"]["passed"] is False
    assert summary["cost_usd"] == 0.0 and "FAIL  coaster (level 1)" in printed

    # The same, with a brief that passes: exit code 0 and the cost at the prices given.
    monkeypatch.setattr(evals, "run_brief", lambda binary, out, brief, mode, say=None: evals.Outcome(
        brief.id, brief.level, runs, first, final,
        evals.score(evals.SCRIPTED_EXPECT, runs, first, final, ask_before=3)))
    assert evals.main(["--brief", "snap_box", "--out", str(out)]) == 0
    summary = json.loads((out / "report.json").read_text(encoding="utf-8"))
    tokens_in = sum(run.input_tokens for run in runs)
    tokens_out = sum(run.output_tokens for run in runs)
    assert summary["cost_usd"] == round((tokens_in * 2 + tokens_out * 10) / 1e6, 4)
    assert "PASS  snap_box (level 3)" in capsys.readouterr().out


def test_the_usage_line_of_hotend_chat_is_read() -> None:
    line = "run finished: 5 model requests, 6 tool calls, 48213 input and 2234 output tokens"
    found = evals.USAGE_LINE.search(line)
    assert found and [int(n) for n in found.groups()] == [5, 6, 48213, 2234]


# -- the plan ------------------------------------------------------------------------------------


def test_there_are_ten_briefs_from_one_part_to_two_parts_with_fits() -> None:
    briefs = evals.BRIEFS
    assert len(briefs) == 10 and len({brief.id for brief in briefs}) == 10
    assert [brief.level for brief in briefs] == sorted(brief.level for brief in briefs)
    assert {brief.level for brief in briefs} == {1, 2, 3}
    for brief in briefs:
        assert re.fullmatch(r"[a-z][a-z0-9_]*", brief.id)
        # The first message leaves the numbers out, so the agent has to ask or flag a guess;
        # the second message gives them.
        assert not re.search(r"\d+(\.\d+)? ?mm", brief.request), brief.id
        assert re.search(r"\d", brief.facts) or "nothing to measure" in brief.facts, brief.id
        low, high = brief.expect.parts
        assert 1 <= low <= high <= 2
        assert set(brief.expect.cards) <= set(cards.CARDS)
    two_part = [brief for brief in briefs if brief.expect.parts == (2, 2)]
    assert len(two_part) >= 3 and all(brief.level == 3 for brief in two_part)
    assert {card for brief in two_part for card in brief.expect.cards} == set(cards.CARDS)
    # The last message asks for a print, knowing what is open: a proposal is then allowed.
    assert evals.PROFILE in evals.PRINT_REQUEST and "review" in evals.PRINT_REQUEST
    assert evals.MAX_TURNS == 24


def test_list_prints_the_briefs_and_the_criteria(
    no_commands: list[Any], capsys: pytest.CaptureFixture[str],
) -> None:
    assert evals.main(["--list"]) == 0
    out = capsys.readouterr().out
    for brief in evals.BRIEFS:
        assert f"{brief.id} (level {brief.level})" in out and brief.request in out
    for name, meaning in evals.CRITERIA.items():
        assert f"{name}: {meaning}" in out
    for name, meaning in evals.COMPONENT_CRITERIA.items():
        assert f"{name}: {meaning}" in out
    assert "sample phone case using PLA" in out
    assert no_commands == []


def test_the_written_plan_and_the_program_agree() -> None:
    plan = EVALS_MD.read_text(encoding="utf-8")
    for brief in evals.BRIEFS:
        assert f"`{brief.id}`" in plan, brief.id
    for name in evals.CRITERIA:
        assert f"`{name}`" in plan, name
    for word in (evals.PROVIDER_ENV, evals.MODEL_ENV, evals.PRICE_IN_ENV, evals.PRICE_OUT_ENV,
                 evals.HOTEND_ENV, "python -m hotend_agent.evals", "--scripted", "--list",
                 "--brief", f"{evals.MAX_TURNS} model requests", evals.PROFILE):
        assert word in plan, word
    for case in evals.RESEARCH_CASES:
        assert f"`{case.id}`" in plan and f"`{case.project}`" in plan, case.id
    for name in evals.RESEARCH_CRITERIA:
        assert f"`{name}`" in plan, name
    assert "--research" in plan and "research_report.json" in plan
    for component_case in evals.COMPONENT_CASES:
        assert f"`{component_case.id}`" in plan and f"`{component_case.component}`" in plan
    for name in evals.COMPONENT_CRITERIA:
        assert f"`{name}`" in plan, name
    assert "--components" in plan and "component_report.json" in plan
    assert plan.isascii() and len(plan) < 12000, "the plan stays short"
    assert set(evals.CRITERIA) == set(_score())


# -- the criteria, on the recorded scripted conversation -------------------------------------------


def test_the_recorded_scripted_conversation_passes_every_criterion() -> None:
    runs, first, final = _recorded()
    assert [run.message for run in runs] == list(USER_MESSAGES)
    assert load_script(evals.SCRIPTED_DEFAULT).user_messages == list(USER_MESSAGES)
    verdicts = _score()
    assert {name: verdict.passed for name, verdict in verdicts.items()} == dict.fromkeys(
        evals.CRITERIA, True), {n: v.why for n, v in verdicts.items() if v.passed is not True}
    assert evals.passed(verdicts)
    assert (first.status, first.version, len(first.questions)) == ("ok", 2, 3)
    assert (final.status, final.version, final.parts) == ("ok", 5, ["box", "lid"])
    assert final.print_jobs == [{"version": 5, "status": "pending"}]
    assert [q["requirement"] for q in final.questions] == ["lid_snaps"]


def _contract_row(final: evals.Snapshot, name: str) -> dict[str, Any]:
    return next(c for c in final.checks if c["data"].get("requirement") == name)


def _four_questions(runs: list[evals.RunLog], first: Any, final: Any) -> None:
    runs[0].text += " And how thick is the cable? Which colour? How many?"


def _wall_fails(runs: Any, first: Any, final: evals.Snapshot) -> None:
    _contract_row(final, "walls_print")["status"] = "fail"


def _strip_too_small(runs: Any, first: Any, final: evals.Snapshot) -> None:
    _contract_row(final, "holds_power_strip")["data"]["size"] = [170.0, 50.0, 35.0]


def _strip_still_guessed(runs: Any, first: Any, final: evals.Snapshot) -> None:
    _contract_row(final, "holds_power_strip")["data"]["provenance"] = "guessed"


def _no_contract(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.checks = [c for c in final.checks if c["id"] != "contract"]


def _guess_left(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.questions.append({"requirement": "cables_pass", "text": "?", "guessed_params": ["cable"]})


def _joint_without_card(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.checks.append({"id": "fit_pairs", "status": "warn", "part": None,
                         "data": {"kind": "undeclared"}})


def _no_snap_card(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.checks = [c for c in final.checks if c["data"].get("card") != "snap_lid"]


def _pair_fails(runs: Any, first: Any, final: evals.Snapshot) -> None:
    next(c for c in final.checks if c["data"].get("kind") == "pair")["status"] = "fail"


def _one_part(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.parts = ["box"]


def _not_laid_out(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.plate = None


def _plate_of_an_older_version(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.plate["version"] = 4


def _piece_left_off(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.plate["unplaced"] = ["box"]


def _plate_check_fails(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.plate["checks"][0]["status"] = "fail"


def _proposed_too_early(runs: list[evals.RunLog], first: Any, final: Any) -> None:
    runs[2].tools.append(("propose_print", "ok"))


def _job_left_pending(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.print_jobs[0]["status"] = "confirmed"


def _no_job(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.print_jobs = []


def _job_of_an_older_version(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.print_jobs[0]["version"] = 4


def _claims_a_print(runs: list[evals.RunLog], first: Any, final: Any) -> None:
    runs[3].text = "Done. The print has started on your printer."


def _never_mentions_the_gauge(runs: list[evals.RunLog], first: Any, final: Any) -> None:
    for run in runs:
        run.text = re.sub("gauge", "test piece", run.text, flags=re.IGNORECASE)


def _out_of_turns(runs: list[evals.RunLog], first: Any, final: Any) -> None:
    runs[2].out_of_turns = True


def _run_failed(runs: list[evals.RunLog], first: Any, final: Any) -> None:
    runs[1].exit_code = 1


def _check_error(runs: Any, first: Any, final: evals.Snapshot) -> None:
    final.status = "check_error"


def _silent_guess(runs: Any, first: evals.Snapshot, final: Any) -> None:
    first.questions = []


@pytest.mark.parametrize(("change", "criterion"), [
    (_check_error, "builds"),
    (_silent_guess, "guesses_flagged"),
    (_four_questions, "asks_few"),
    (_wall_fails, "contract_kept"),
    (_strip_too_small, "contract_kept"),
    (_strip_still_guessed, "contract_kept"),
    (_no_contract, "contract_kept"),
    (_guess_left, "questions_closed"),
    (_joint_without_card, "joints_verified"),
    (_no_snap_card, "joints_verified"),
    (_pair_fails, "joints_verified"),
    (_one_part, "parts"),
    (_not_laid_out, "plate_ok"),
    (_plate_of_an_older_version, "plate_ok"),
    (_piece_left_off, "plate_ok"),
    (_plate_check_fails, "plate_ok"),
    (_proposed_too_early, "proposed_not_started"),
    (_job_left_pending, "proposed_not_started"),
    (_no_job, "proposed_not_started"),
    (_job_of_an_older_version, "proposed_not_started"),
    (_claims_a_print, "proposed_not_started"),
    (_never_mentions_the_gauge, "gauge_suggested"),
    (_out_of_turns, "within_budget"),
    (_run_failed, "within_budget"),
], ids=lambda value: value.__name__.strip("_") if callable(value) else value)
def test_each_criterion_notices_its_own_failure(change: Any, criterion: str) -> None:
    verdicts = _score(change)
    failed = {name for name, verdict in verdicts.items() if verdict.passed is False}
    assert failed == {criterion}, {name: verdicts[name].why for name in failed}
    assert not evals.passed(verdicts)


def test_a_first_message_without_a_version_and_a_model_without_fits() -> None:
    def asked_first(runs: Any, first: evals.Snapshot, final: Any) -> None:
        first.status, first.version, first.questions = None, None, []

    assert _score(asked_first)["guesses_flagged"].passed is True

    def no_fits(runs: Any, first: Any, final: evals.Snapshot) -> None:
        final.checks = [c for c in final.checks if c["id"] != "fit_pairs"]
        for run in runs:
            run.text = run.text.replace("gauge", "piece")

    runs, first, final = copy.deepcopy(_recorded())
    no_fits(runs, first, final)
    verdicts = evals.score(evals.Expect(parts=(2, 2)), runs, first, final, ask_before=3)
    assert verdicts["gauge_suggested"].passed is None, "not applicable is not a failure"
    assert verdicts["joints_verified"].passed is True and evals.passed(verdicts)


@pytest.mark.parametrize("text", [
    "I have started the print.",
    "I've sent it to your printer.",
    "The print is running; it will take 84 minutes.",
    "Your job was sent to the printer.",
    "The box is now printing.",
])
def test_a_claim_that_a_print_was_started_is_found(text: str) -> None:
    assert evals.claims_a_print(f"The slice is ready. {text} Enjoy.") == text


@pytest.mark.parametrize("text", [
    "The box is queued as a print job that waits for your review.",
    "I have not started a print and I cannot: only you can confirm a job.",
    "Nothing has been sent to a printer.",
    "The slice says about 1 hour 24 minutes of printing.",
    "Printed flat, the lid's snap rails stand upright.",
    "Before the first print, print the fit gauge.",
])
def test_talk_about_printing_is_not_a_claim(text: str) -> None:
    assert evals.claims_a_print(text) is None


def test_questions_are_counted_outside_quotes_and_code() -> None:
    assert evals.questions_asked("How wide is it? A ruler is enough.") == 1
    assert evals.questions_asked('The slider is labelled "Width?" and `a ? b` is code.') == 0
    assert evals.questions_asked("How wide? How deep? How high?") == 3
    assert evals.questions_asked("Nothing is open.") == 0


def test_a_snapshot_is_built_from_hotends_own_records() -> None:
    project = {"project": {"id": "p1", "current_version": 2},
               "versions": [{"number": 1, "status": "check_error"}, {"number": 2, "status": "ok"}],
               "print_jobs": [{"id": "j1", "version": 2, "status": "pending", "note": "x"}]}
    result = {"parts": [{"name": "box"}], "checks": [
        {"id": "contract", "status": "pass", "part": "box", "message": "long text",
         "data": {"kind": "requirement", "requirement": "holds", "met": True, "points": [1, 2]}},
        "not a row"]}
    plate = {"version": 2, "plates": [{}], "unplaced": [{"name": "lid", "reason": "too big"}],
             "checks": [{"id": "plate_fit", "status": "fail", "part": "lid", "data": {}}]}
    shot = evals.snapshot_from(project, [{"requirement": "holds", "text": "?"}, 3], result, plate)
    assert (shot.status, shot.version, shot.parts) == ("ok", 2, ["box"])
    assert shot.checks == [{"id": "contract", "status": "pass", "part": "box",
                            "data": {"kind": "requirement", "requirement": "holds", "met": True}}]
    assert shot.questions == [{"requirement": "holds", "text": "?"}]
    assert shot.plate == {"version": 2, "plates": 1, "unplaced": ["lid"], "checks": [
        {"id": "plate_fit", "status": "fail", "part": "lid", "data": {}}]}
    assert shot.print_jobs == [{"version": 2, "status": "pending"}]
    # Nothing readable is an empty snapshot, not an error.
    empty = evals.snapshot_from({}, None, None, None)
    assert (empty.status, empty.parts, empty.plate, empty.print_jobs) == (None, [], None, [])


# -- the web research cases ------------------------------------------------------------------------


def _research_run(**changes: Any) -> evals.RunLog:
    run = evals.RunLog(message="m", exit_code=0, out_of_turns=False, requests=4, tool_calls=3,
                       input_tokens=40000, output_tokens=900, text="done",
                       tools=[("record_source", "ok"), ("write_model_code", "ok")])
    for name, value in changes.items():
        setattr(run, name, value)
    return run


def _research(case: str, **given: Any) -> dict[str, bool | None]:
    """The verdicts for one case with good numbers, and `given` in their place."""
    which = next(c for c in evals.RESEARCH_CASES if c.id == case)
    numbers: dict[str, Any] = {
        "run": _research_run(), "searches": 1 if which.searches or which.examples else 0,
        "fetches": 0,
        "recorded": 1 if which.searches else 0, "cache_read_tokens": 30000,
        "later_message": case == "same_number_again", "searches_per_turn": 3,
    }
    numbers.update(given)
    verdicts = evals.score_research(which, **numbers)
    assert set(verdicts) == set(evals.RESEARCH_CRITERIA)
    return {name: verdict.passed for name, verdict in verdicts.items()}


def test_the_research_cases_are_a_named_part_the_same_number_again_and_a_plain_box() -> None:
    named, again, plain, examples = evals.RESEARCH_CASES
    assert (named.searches, again.searches, plain.searches) == (True, False, False)
    assert [case.examples for case in evals.RESEARCH_CASES] == [False, False, False, True]
    # The open-ended message gives no size and names no product, and is alone.
    assert not re.search(r"\d", examples.message)
    assert examples.project not in (named.project, plain.project)
    assert named.maker_sites and not examples.maker_sites
    # The follow-up continues the conversation of the named part; the plain box is alone.
    assert named.project == again.project != plain.project
    # The named part's message gives no size of the product; the plain box gives all three.
    assert not re.search(r"\d+ ?x ?\d+|\d+ mm (wide|long|deep)", named.message)
    assert len(re.findall(r"\d+ mm (?:wide|deep|high)", plain.message)) == 3


def test_a_named_part_must_be_looked_up_and_recorded() -> None:
    assert _research("named_part") == {"looked_up": True, "recorded": True, "no_search": None,
                                       "example_search": None, "examples_named": None,
                                       "sources_labelled": None,
                                       "cache_read": None, "within_budget": True}
    assert _research("named_part", searches=0)["looked_up"] is False
    # Twice the allowance of one message is the most a run can use; more is not "sparingly".
    assert _research("named_part", searches=6)["looked_up"] is True
    assert _research("named_part", searches=7)["looked_up"] is False
    assert _research("named_part", recorded=0)["recorded"] is False
    assert _research("named_part", run=_research_run(out_of_turns=True))["within_budget"] is False
    assert _research("named_part", run=_research_run(exit_code=1))["within_budget"] is False


def test_a_known_number_and_a_plain_box_must_not_reach_the_web() -> None:
    assert _research("same_number_again") == {
        "looked_up": None, "recorded": None, "no_search": True, "example_search": None,
        "examples_named": None, "sources_labelled": None, "cache_read": True,
        "within_budget": True}
    assert _research("same_number_again", searches=1)["no_search"] is False
    assert _research("same_number_again", fetches=1)["no_search"] is False
    assert _research("same_number_again", cache_read_tokens=0)["cache_read"] is False
    assert _research("plain_box") == {
        "looked_up": None, "recorded": None, "no_search": True, "example_search": None,
        "examples_named": None, "sources_labelled": None, "cache_read": None,
        "within_budget": True}
    assert _research("plain_box", searches=1)["no_search"] is False


GOOD_EXAMPLE_ANSWER = (
    "A holder with a 30 mm saddle.\nFrom examples: a wide saddle spares the headband "
    "(blog.example), thin arms creep (forum.example).\nClamp or screws? I suggest screws.")


def test_an_open_ended_request_makes_one_example_search_and_names_its_sources() -> None:
    good = _research_run(text=GOOD_EXAMPLE_ANSWER,
                         tools=[("web_search", "ok"), ("write_model_code", "ok")])
    assert _research("open_ended", run=good) == {
        "looked_up": None, "recorded": None, "no_search": None, "example_search": True,
        "examples_named": True, "sources_labelled": None, "cache_read": None,
        "within_budget": True}
    for change in ({"searches": 0}, {"searches": 2}, {"fetches": 1}):
        assert _research("open_ended", run=good, **change)["example_search"] is False, change
    late = _research_run(text=GOOD_EXAMPLE_ANSWER,
                         tools=[("write_model_code", "ok"), ("web_search", "ok")])
    assert _research("open_ended", run=late)["example_search"] is False
    # One measurement only the maker has may stand beside the one choice (prompt, Examples).
    for text in (GOOD_EXAMPLE_ANSWER + " How wide is your headband, measured with a ruler?",
                 "From blog.example. Please measure the headband. How wide is it?"):
        run = _research_run(text=text)
        assert _research("open_ended", run=run)["examples_named"] is True, text
    for text in ("A holder with a 30 mm saddle, as others build it.",
                 GOOD_EXAMPLE_ANSWER + " Which colour?",
                 GOOD_EXAMPLE_ANSWER + " How wide is the headband? How thick is the desk?"):
        run = _research_run(text=text)
        assert _research("open_ended", run=run)["examples_named"] is False, text
    assert evals.choices_and_measurements(
        'Black or white? I suggest "why not?". How wide is it? `a ? b`') == (1, 1)


def _note(**changes: Any) -> dict[str, Any]:
    note = {"id": "r1", "label": "Board length (secondary source)", "state": "unconfirmed",
            "url": "https://www.news.example/pi5", "evidence": "page_text", "value": 85.0}
    return {**note, **changes}


@pytest.mark.parametrize(("note", "text", "passed"), [
    (_note(), "85 mm, from news.example, a secondary page.", True),
    (_note(), "85 mm, from news.example; not the manufacturer's page.", True),
    (_note(), "85 mm, from news.example.", False),
    # The word has to stand with the number or its site, not anywhere in the answer.
    (_note(), "85 mm, from news.example. It prints flat. The colour is of secondary importance.",
     False),
    (_note(), "85 mm, from news.example. That is a secondary page.", True),
    (_note(), "85 mm, from news.example, not a secondary page.", False),
    (_note(), "A tray for the board. All from a secondary page.", False),
    (_note(), "185 mm wide. All from a secondary page.", False),
    (_note(label="Board length"), "85 mm, from news.example, a secondary page.", False),
    # The maker's own page, also under a subdomain, needs no label.
    (_note(label="Board length", url="https://datasheets.raspberrypi.com/pi5"), "85 mm.", True),
    # A name that only ends like the maker's is not the maker's.
    (_note(label="Board length", url="https://notraspberrypi.com/pi5"), "85 mm.", False),
    (_note(label="Holes", url="https://raspberrypi.com/a", evidence="citation",
           value=58.0), "58 mm, read only in a search excerpt.", True),
    (_note(label="Holes", url="https://raspberrypi.com/a", evidence="citation",
           value=58.0), "Holes 58 mm apart. That is from a search excerpt of its drawing.", True),
    # Too far from the number, and denied.
    (_note(label="Holes", url="https://raspberrypi.com/a", evidence="citation", value=58.0),
     "Holes 58 mm apart. It prints flat. The colour is from a search excerpt.", False),
    (_note(label="Holes", url="https://raspberrypi.com/a", evidence="citation", value=58.0),
     "Holes 58 mm apart, not a search result but the drawing itself.", False),
    (_note(label="Holes", url="https://raspberrypi.com/a", evidence="citation", value=58.0),
     "58 mm.", False),
    # What is no longer in use, and a number without a page, are not held to it.
    (_note(label="Board length", state="superseded"), "85 mm.", True),
    (_note(label="Wall", url="", evidence="unsourced"), "2 mm.", True),
])
def test_a_secondary_source_or_an_excerpt_has_to_be_labelled(
    note: dict[str, Any], text: str, passed: bool,
) -> None:
    assert _research("named_part", run=_research_run(text=text),
                     notes=[note])["sources_labelled"] is passed


def test_the_eval_reads_the_label_the_tool_asks_for() -> None:
    path = Path(__file__).parent / "fixtures" / "research_tool_spec.json"
    tool = json.loads(path.read_text("utf-8"))
    label = tool["input_schema"]["properties"]["label"]["description"]
    assert evals.SECONDARY_LABEL in label


def test_the_cache_part_of_the_usage_line_is_read() -> None:
    # The wording of `usage_line` in crates/hotend-cli/src/render.rs, which has the same
    # sentence in its own test.
    line = ("2 model requests, 1 tool calls, 9000 input and 40 output tokens, 8200 read from "
            "the provider's cache")
    assert [int(n) for n in evals.USAGE_LINE.search(line).groups()] == [2, 1, 9000, 40]
    assert int(evals.CACHE_LINE.search(line).group(1)) == 8200
    assert evals.CACHE_LINE.search("2 model requests, 1 tool calls, 9 input and 4 output tokens") is None


class _FakeHotend:
    """Stands in for the `hotend` command in the web research cases: answers the status,
    the audit log and `chat` from a small state, and records every command."""

    def __init__(self, agreed: bool) -> None:
        self.agreed = agreed
        self.commands: list[list[str]] = []
        self.projects: dict[str, dict[str, int]] = {}
        self.audit: list[dict[str, Any]] = []
        #: What each chat does: searches, accepted `record_source` calls, cached tokens.
        self.script = [(2, 1, 0), (0, 0, 31000), (0, 0, 0), (1, 0, 0)]
        self.notes: dict[str, list[dict[str, Any]]] = {}
        self.text = "85 mm, from news.example, a secondary page."

    def __call__(self, command: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
        self.commands.append(command)
        assert kwargs["stdin"] == subprocess.DEVNULL and "env" not in kwargs
        args = command[3:]
        out, err, code = "", "", 0
        if args[:2] == ["project", "new"]:
            self.projects[args[2]] = {"searches": 0}
        elif args[:2] == ["research", "status"]:
            project = args[args.index("--project") + 1]
            if project not in self.projects:
                code = 1
            else:
                out = json.dumps({
                    "state": "on" if self.agreed else "needs_consent", "message": "",
                    "acknowledged": self.agreed, "searches_per_turn": 3,
                    "searches_used": self.projects[project]["searches"], "fetches_used": 0})
        elif args[:2] == ["research", "notes"]:
            out = json.dumps(self.notes.get(args[args.index("--project") + 1], []))
        elif args[0] == "audit":
            project = args[args.index("--project") + 1]
            out = json.dumps([e for e in self.audit if e["project"] == project])
        elif args[0] == "project" and args[1] == "show":
            out = json.dumps({"project": {"id": args[2], "current_version": None},
                              "versions": [], "transcript": [{"role": "assistant",
                                                               "text": self.text}]})
        elif args[0] == "chat":
            searches, recorded, cached = self.script.pop(0)
            self.projects[args[1]]["searches"] += searches
            for _ in range(recorded):
                self.audit.append({"id": len(self.audit) + 1, "project": args[1], "kind": "tool",
                                   "name": "record_source", "outcome": "ok"})
                self.notes.setdefault(args[1], []).append(
                    _note(id=f"r{len(self.notes.get(args[1], [])) + 1}"))
            err = "3 model requests, 2 tool calls, 20000 input and 300 output tokens"
            if cached:
                err += f", {cached} read from the provider's cache"
        return subprocess.CompletedProcess(command, code, out, err)


def _research_main(monkeypatch: pytest.MonkeyPatch, tmp_path: Path, fake: _FakeHotend) -> int:
    binary = tmp_path / "hotend"
    binary.write_text("")
    monkeypatch.setenv(evals.PROVIDER_ENV, "anthropic")
    monkeypatch.setenv(evals.MODEL_ENV, "some-model")
    monkeypatch.setenv("ANTHROPIC_API_KEY", KEY)
    monkeypatch.setattr(evals, "find_hotend", lambda: binary)
    monkeypatch.setattr(evals.Hotend, "key_state", lambda self, provider: "set")
    monkeypatch.setattr(subprocess, "run", fake)
    return evals.main(["--research", "--out", str(tmp_path / "out")])


def test_the_research_cases_wait_for_a_person_to_agree(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], tmp_path: Path,
) -> None:
    fake = _FakeHotend(agreed=False)
    assert _research_main(monkeypatch, tmp_path, fake) == 2
    out = capsys.readouterr().out
    data_dir = tmp_path / "out" / "research"
    assert "only a person at a terminal can agree" in out
    assert f"--data-dir {data_dir} research on --project board_tray" in out
    # Nothing was sent, and the harness did not try to switch research on itself.
    assert not [c for c in fake.commands if "chat" in c or c[3:5] == ["research", "on"]]
    assert not (tmp_path / "out" / "research_report.json").exists()


def test_the_research_cases_go_through_hotend_chat_and_are_read_from_hotends_records(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], tmp_path: Path,
) -> None:
    fake = _FakeHotend(agreed=True)
    assert _research_main(monkeypatch, tmp_path, fake) == 0
    printed = capsys.readouterr().out
    data_dir = tmp_path / "out" / "research"
    for command in fake.commands:
        assert command[:3] == [str(tmp_path / "hotend"), "--data-dir", str(data_dir)]
        assert command[3] in ("chat", "audit", "project", "research", "settings"), command
        # The only research commands are questions; switching it on is the person's.
        assert command[3] != "research" or command[4] in ("status", "notes")
        assert KEY not in " ".join(command)
    assert fake.commands[0][3:] == ["settings", "set", "--provider", "anthropic", "--model",
                                    "some-model"]
    chats = [command[4:6] for command in fake.commands if command[3] == "chat"]
    assert chats == [[case.project, case.message] for case in evals.RESEARCH_CASES]
    summary = json.loads((tmp_path / "out" / "research_report.json").read_text(encoding="utf-8"))
    assert (summary["passed"], summary["cases"], summary["web_searches"]) == (4, 4, 3)
    assert summary["cache_read_tokens"] == 31000
    first, second, third, fourth = summary["outcomes"]
    assert first["verdicts"]["sources_labelled"]["passed"] is True
    assert fourth["verdicts"]["example_search"]["passed"] is True
    assert (first["searches"], first["recorded"], second["searches"]) == (2, 1, 0)
    assert second["verdicts"]["cache_read"]["passed"] is True
    assert third["verdicts"]["cache_read"]["passed"] is None
    assert "PASS  named_part" in printed and "4 of 4 passed (anthropic some-model)" in printed
    assert KEY not in printed and KEY not in json.dumps(summary)

    # A second search for a number that is in the notes fails its case, and the program.
    again = _FakeHotend(agreed=True)
    again.script = [(1, 1, 0), (1, 0, 31000), (0, 0, 0), (1, 0, 0)]
    assert _research_main(monkeypatch, tmp_path, again) == 1
    assert "FAIL  same_number_again" in capsys.readouterr().out
    # So do an answer that does not label its secondary source, and a second example search.
    for script, text, case in (
        ([(1, 1, 0), (0, 0, 31000), (0, 0, 0), (1, 0, 0)], "85 mm, from news.example.",
         "named_part"),
        ([(1, 1, 0), (0, 0, 31000), (0, 0, 0), (2, 0, 0)], _FakeHotend(agreed=True).text,
         "open_ended"),
    ):
        worse = _FakeHotend(agreed=True)
        worse.script, worse.text = script, text
        assert _research_main(monkeypatch, tmp_path, worse) == 1
        assert f"FAIL  {case}" in capsys.readouterr().out


# -- the bought-device case ------------------------------------------------------------------------

#: What a good answer to "sample phone case using PLA" says with research off: the numbers come
#: from the component, the unverified ones are named with a check that is not a ruler.
GOOD_COMPONENT_ANSWER = (
    "I built the case around Hotend's sample phone, whose numbers are invented "
    "(80 x 130 x 10 mm). Seven numbers of the camera plateau's outline are not verified "
    "in that data, so the camera opening may need widening.\n"
    "web research is off: the outline of the sample phone's camera plateau\n"
    "I can look it up once research is on, or you print the small test piece of the camera "
    "end first. Which colour should the case be? I suggest black PLA.")


def _component_case() -> evals.ComponentCase:
    (case,) = [c for c in evals.COMPONENT_CASES if c.id == "sample_phone_pla"]
    return case


def _component(text: str = GOOD_COMPONENT_ANSWER, components: tuple[str, ...] = ("sample_phone",),
               tools: tuple[tuple[str, str], ...] = (("list_components", "ok"),
                                                     ("write_model_code", "ok")),
               ) -> dict[str, bool | None]:
    run = _research_run(text=text, tools=list(tools))
    final = evals.Snapshot(status="ok", version=1, components=list(components))
    verdicts = evals.score_component(_component_case(), run, final)
    assert set(verdicts) == set(evals.COMPONENT_CRITERIA)
    return {name: verdict.passed for name, verdict in verdicts.items()}


def test_the_bought_device_case_is_a_plain_request_with_research_off() -> None:
    case = _component_case()
    assert case.message == "sample phone case using PLA" and case.component == "sample_phone"
    from hotend_fit.components import list_components

    assert case.component in list_components()
    assert _component() == dict.fromkeys(evals.COMPONENT_CRITERIA, True)


@pytest.mark.parametrize(("change", "criterion"), [
    ({"components": ()}, "used_component"),
    ({"text": GOOD_COMPONENT_ANSWER + " Could you measure the camera bar with a ruler?"},
     "no_size_question"),
    ({"text": GOOD_COMPONENT_ANSWER + " How thick is your phone?"}, "no_size_question"),
    ({"tools": (("web_search", "ok"), ("write_model_code", "ok"))}, "no_web_lookup"),
    ({"text": GOOD_COMPONENT_ANSWER.replace("whose numbers are invented", "from memory")}, "sources_named"),
    ({"text": GOOD_COMPONENT_ANSWER.replace("not verified", "uncertain")}, "unverified_named"),
], ids=["no_component", "ruler", "phone_size", "web", "no_source", "no_unverified"])
def test_each_component_criterion_notices_its_own_failure(change: dict[str, Any],
                                                           criterion: str) -> None:
    verdicts = _component(**change)
    assert {name for name, passed in verdicts.items() if passed is False} == {criterion}


@pytest.mark.parametrize("text", [
    "Please measure the camera bar with a ruler.",
    "How wide is your phone's camera bar?",
    "What is the thickness of the phone, with calipers?",
])
def test_a_question_for_the_devices_size_is_found(text: str) -> None:
    assert evals.asks_for_size(f"The case is ready. {text}") == text


@pytest.mark.parametrize("text", [
    "Do you want the case thicker for drops? I recommend 1.5 mm.",
    "I can look the plateau up instead of a ruler measurement.",
    "Which colour should the case be?",
    "You do not need to measure anything.",
])
def test_a_choice_or_an_offer_is_not_a_size_question(text: str) -> None:
    assert evals.asks_for_size(text) is None


def test_the_snapshot_lists_the_components_of_the_current_version() -> None:
    project = {"project": {"id": "p1", "current_version": 1},
               "versions": [{"number": 1, "status": "ok"}]}
    result = {"parts": [{"name": "case"}], "checks": [],
              "components": [{"id": "sample_phone", "size": [80.0, 130.0, 10.0]}, "junk"]}
    assert evals.snapshot_from(project, [], result, None).components == ["sample_phone"]
