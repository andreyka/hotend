"""The harness gets the facts itself: a bought device's size comes from the shipped component,
never from the maker's ruler or a guess, and research that is off is offered, not demanded.

A plain request ("sample phone case using PLA", web research not agreed) is played as a
written conversation through the real agent process: no model is asked. The stand-in gateway
answers `list_components` and `read_docs` with what the real gateway reads from the engine's
docs index (the same Python functions), so the listing the agent sees is the installation's.
The runner's side is deterministic: an answer with a line `web research is off: ...` ends the
run with the structured `research_needed` event the window turns into its action.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from agent_fake_core import AgentProcess, text_outcome
from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, UserPromptPart

from hotend_agent import evals
from hotend_agent.runner import RESEARCH_OFF, Research, research_wanted
from hotend_engine import docs
from hotend_fit.components import catalogue

pytestmark = pytest.mark.timeout(120)

REQUEST = "sample phone case using PLA"
OFF_PARAMS = json.loads((Path(__file__).parent / "fixtures" / "research" / "off_params.json")
                        .read_text("utf-8"))

#: The model a careful agent writes: the phone's body and features from the component, no
#: number of the phone typed by hand. (Not built here: the gateway's answer is canned.)
MODEL = '''\
from build123d import *

PARAMS = {
    "wall": {"default": 1.6, "min": 1.2, "max": 2.4, "unit": "mm",
             "label": "Side wall thickness", "provenance": "style"},
}

CONTRACT = {"protocol": 0, "name": "Sample phone case", "parts": ["case"], "requirements": [
    {"id": "back_thin", "kind": "wall_min", "part": "case", "value": 1.2,
     "provenance": "standard", "note": "the phone's case rules, from the component"}]}


def build(p, ctx):
    phone = ctx.component("sample_phone")
    pocket = phone.reference.pocket("loose", ctx=ctx)
    back = 1.6
    shell = Pos(0, 0, -back) * pocket.room(offset=pocket.offset + p.wall, top=back + 1.0)
    case = pocket.cut(shell, at=Pos(0, 0, 0))
    case -= phone.feature("usb_c").keep_out(reach=p.wall + 2)
    case -= phone.feature("camera_plateau").keep_out(reach=back + 2)
    return {"case": case}
'''

SUMMARY = (
    "I made a slim PLA case around Hotend's sample phone, an invented device: 80 x 130 x "
    "10 mm and every button, port and the camera come from its invented data, so it fits no "
    "real phone. The pocket's room comes from your printer's calibration.\n\n"
    "Not verified in that data: 7 numbers of the camera plateau's outline and the flash's "
    "keep-out, so the camera opening may need widening by a few tenths of a millimetre.\n"
    "web research is off: the outline of the sample phone's camera plateau\n"
    "With research on I can look it up; or print the small test piece of the camera end "
    "first, which checks the opening in 15 minutes.\n\n"
    "One choice: a 1.6 mm wall (my suggestion, slim and stiff enough in PLA) or 2.0 mm for "
    "more drop protection?")


def gateway_answer(name: str, args: dict[str, Any]) -> dict[str, Any]:
    """The answers of the real gateway for the read-only tools, from the same functions; a
    canned build result for the write."""
    if name == "list_components":
        # As the gateway joins them: the introduction, then one line per component.
        entries, _ = docs.build_index()
        found = [e for e in entries if e.module == catalogue.__name__]
        intro = next(e.doc for e in found if e.name.endswith(".shipped_components"))
        lines = sorted(e.doc for e in found if ".shipped_component_" in e.name)
        return text_outcome(intro + "\n\n" + "\n".join(lines) + "\n")
    if name == "read_docs":
        entries, _ = docs.build_index()
        hits = docs.search(entries, f"{args.get('topic', '')} {args.get('query', '')}", 3)
        return text_outcome("\n".join(f"## {hit['name']}\n{hit['doc']}" for hit in hits))
    if name == "write_model_code":
        return text_outcome(
            "status: ok\nversion 1 created\nparts: case 83.3 x 133.5 x 13.2 mm\n"
            "components: sample_phone (Sample phone (invented)), 29 numbers not verified\n"
            "questions for the user (2; ask them, do not answer them yourself):\n"
            "- sample_phone_camera_plateau: What is the outline of the camera plateau?\n"
            "- sample_phone_flash: How much room does the flash need?\n")
    return text_outcome(f"error (unexpected_tool): {name}")


def script(tmp_path: Path) -> str:
    turns = [
        {"expect": [REQUEST, ("[Hotend: web research is not available in this run: the maker "
                    "has not agreed to web research yet")],
         "say": "You named a phone: Hotend may ship its data. Let me check.",
         "calls": [{"tool": "list_components", "args": {}}]},
        {"expect": ["- sample_phone: Sample phone (invented)", "example.com/hotend/sample-phone"],
         "say": "Hotend has the sample phone, an invented device. Reading how to use it.",
         "calls": [{"tool": "read_docs", "args": {"query": "bought components"}}]},
        {"expect": ['ctx.component("sample_phone")'],
         "say": "Contract: a slim PLA case; the phone's size and features come from the "
                "component (standard, its own data); the wall is style.",
         "calls": [{"tool": "write_model_code",
                    "args": {"code": MODEL, "note": "case around the sample_phone component"}}]},
        {"expect": ["status: ok", "29 numbers not verified"], "say": SUMMARY},
    ]
    path = tmp_path / "sample_phone_research_off.json"
    path.write_text(json.dumps({"protocol": 0, "name": "sample_phone_research_off",
                                "user_messages": [REQUEST], "turns": turns}), "utf-8")
    return str(path)


@pytest.fixture
def played(tmp_path: Path) -> Any:
    with AgentProcess(script=script(tmp_path), tools=gateway_answer) as agent:
        # The parameter the core sends when nobody agreed: no search, and why.
        agent.core.run(REQUEST, research=dict(OFF_PARAMS, state="needs_consent",
                                              source="keenable"))
        result = agent.core.result(60)["result"]
        assert agent.wait() == 0, agent.stderr
    assert agent.core.violations == [] and agent.core.llm_requests == []
    assert result["stop_reason"] == "done", result
    return agent.core


def test_the_first_move_is_the_component_not_the_maker(played: Any) -> None:
    names = [name for name, _ in played.tool_calls]
    assert names == ["list_components", "read_docs", "write_model_code"]
    # Nothing was looked up on the web, and no question was put before the model was built.
    assert not {"web_search", "web_fetch"} & set(names)
    before_build = played.text().split("Contract:")[0]
    assert "?" not in before_build
    code = played.tool_calls[2][1]["code"]
    assert 'ctx.component("sample_phone")' in code
    # No number of the phone is typed by hand: its size is the component's.
    for number in ("80", "130", "10"):
        assert number not in code, number


def test_the_answer_names_its_sources_and_what_is_not_verified(played: Any) -> None:
    text = played.text()
    assert "invented data" in text
    assert "Not verified" in text and "camera plateau" in text
    assert evals.asks_for_size(text) is None, "the maker is not asked for the phone's size"
    assert "ruler" not in text.lower()
    # Questions are real choices with a recommendation.
    assert evals.questions_asked(text) == 1 and "my suggestion" in text
    final = evals.Snapshot(status="ok", version=1, components=["sample_phone"])
    run = evals.RunLog(message=REQUEST, exit_code=0, out_of_turns=False, requests=4,
                       tool_calls=3, input_tokens=0, output_tokens=0, text=text,
                       tools=[(name, "ok") for name, _ in played.tool_calls])
    (case,) = evals.COMPONENT_CASES
    verdicts = evals.score_component(case, run, final)
    assert all(v.passed for v in verdicts.values()), {n: v.why for n, v in verdicts.items()}


def test_research_that_is_off_ends_the_run_with_the_structured_marker(played: Any) -> None:
    marked = [event for event in played.events if "research_needed" in event]
    assert len(marked) == 1
    (event,) = marked
    # An existing event type, without text: the transcript is unchanged by it.
    assert event == {"type": "text_delta", "text": "", "research_needed": {
        "lookup": "the outline of the sample phone's camera plateau", "state": "needs_consent",
        "source": "keenable"}}
    assert played.events.index(event) == len(played.events) - 1 or all(
        e["type"] == "usage" for e in played.events[played.events.index(event) + 1:])


def _answer(text: str) -> ModelResponse:
    return ModelResponse(parts=[TextPart(text)])


def test_the_marker_is_read_from_the_final_answer_only() -> None:
    """The runner gives it the answers of the run (not the stored history); only the final
    answer counts, the one the maker reads next to the action."""
    old = _answer("web research is off: an old lookup")
    asked = ModelRequest(parts=[UserPromptPart("go on")])
    assert research_wanted([old, asked, _answer("All done, nothing is open.")]) is None
    assert research_wanted([old, asked, _answer("web research is off: the new one")]) == "the new one"
    assert research_wanted([old]) == "an old lookup"
    # A response with tool calls only is not an answer: the one before it counts.
    calls_only = ModelResponse(parts=[TextPart("  ")])
    assert research_wanted([_answer("web research is off: x"), calls_only]) == "x"
    assert research_wanted([_answer("Fine.\n**Web research is off:** the Brenta basket's "
                                    "inside diameter")]) == "the Brenta basket's inside diameter"
    assert research_wanted([_answer("I could use web research if it is on.")]) is None
    long = research_wanted([_answer("web research is off: " + "x" * 500)])
    assert long is not None and len(long) == 200
    assert not RESEARCH_OFF.search("Turning web research is off: no")


@pytest.mark.parametrize(("state", "source", "said"), [
    ("needs_consent", "keenable", "the maker has not agreed to web research yet"),
    ("off", "off", "no web research source is chosen in Hotend's settings"),
    ("off", "built_in", "web research is switched off for this project"),
    ("budget_used", "built_in", "this project has used its web research allowance"),
    ("stopped", "tavily", "this project has used its web research allowance"),
    ("needs_key", "tavily", "the chosen search service needs an API key"),
    ("provider_refused", "built_in", "the AI provider refused web search for this account"),
    ("unsupported_provider", "built_in", "the chosen AI provider has no web research in Hotend"),
])
def test_a_run_without_search_is_told_why(state: str, source: str, said: str) -> None:
    research = Research(dict(OFF_PARAMS, state=state, source=source), "anthropic")
    assert (research.search, research.state, research.source) == (False, state, source)
    note = research.off_note()
    assert note.startswith("[Hotend: web research is not available in this run: ")
    assert said in note and note.endswith(".]")


def test_no_note_without_a_reason_or_with_search() -> None:
    # The fixture of a run from before the reason was sent: nothing changes in its request.
    assert Research(OFF_PARAMS, "anthropic").off_note() == ""
    assert Research(dict(OFF_PARAMS, state="bogus", source="x"), "anthropic").off_note() == ""
    on = dict(OFF_PARAMS, search=True, state="on", source="built_in")
    assert Research(on, "anthropic").off_note() == ""
