"""The system prompt names the real things: the tools the core offers, the checks the engine
runs, the fit classes, the cards and the script convention. A rename elsewhere fails here.

The tools are those of the core's registry (fixtures/tool_specs.json, which the core's own
test `real_agent` keeps equal to the registry): the prompt names every one of them and no
tool that is not there.
"""

from __future__ import annotations

import ast
import re
import typing
from pathlib import Path
from typing import Any

import pytest
from agent_fake_core import TOOL_SPECS

from hotend_agent.prompt import PROMPT_VERSION, RESEARCH_PROMPT, SYSTEM_PROMPT
from hotend_engine.checks import CHECK_IDS, OPTIONAL_CHECK_IDS
from hotend_engine.model import PROVENANCES, SPEC_KEYS, UNITS, validate_param_specs
from hotend_fit import cards, contract
from hotend_fit.components import list_components
from hotend_fit.fit import FIT_CLASSES

OFFERED = {spec["name"] for spec in TOOL_SPECS}
REQUIREMENTS = (contract.CavityMin, contract.OuterMax, contract.WallMin, contract.Dimension,
                contract.Fit)
CONTRACT_KINDS = {typing.get_args(model.model_fields["kind"].annotation)[0]
                  for model in REQUIREMENTS}


def _schema_words(schema: object) -> set[str]:
    """Every property name and enum value of a tool's input schema."""
    words: set[str] = set()
    if isinstance(schema, dict):
        words |= set(schema.get("properties", {}))
        words |= {value for value in schema.get("enum", []) if isinstance(value, str)}
        for value in schema.values():
            words |= _schema_words(value)
    elif isinstance(schema, list):
        for value in schema:
            words |= _schema_words(value)
    return words


#: Everything a lower-case name in the prompt may be besides a tool. Each set comes from the
#: code that defines it, so the prompt cannot name a check, a kind or an argument that is gone.
VOCABULARY = (
    set(CHECK_IDS) | set(OPTIONAL_CHECK_IDS) | set(cards.CARDS) | set(FIT_CLASSES)
    | set(PROVENANCES) | set(UNITS) | set(SPEC_KEYS)
    | CONTRACT_KINDS | set(contract.Contract.model_fields)
    | {name for model in REQUIREMENTS for name in model.model_fields}
    | {word for spec in TOOL_SPECS for word in _schema_words(spec["input_schema"])}
    # result statuses, and names of the script convention that are not data of any module
    | {"ok", "check_error", "model_error", "skipped", "at", "apply_a", "apply_b", "hole_delta",
       "peg_delta", "hotend_fit", "bd_warehouse", "w_in"}
    # the shipped components, by id
    | set(list_components())
)


def named(word: str) -> bool:
    return f"`{word}`" in SYSTEM_PROMPT


def test_the_version_is_a_date_and_a_serial() -> None:
    assert re.fullmatch(r"20\d\d-\d\d-\d\d\.\d+", PROMPT_VERSION)


def test_the_prompt_stays_short() -> None:
    # Unchanged on 2026-11-25 (the example search is in the research section alone).
    # 12000 until 2026-11-22; the rules on facts the harness gets itself and the components
    # section added about 2100 characters.
    assert 4000 < len(SYSTEM_PROMPT) < 14500, len(SYSTEM_PROMPT)
    assert SYSTEM_PROMPT.isascii()
    assert max(len(line) for line in SYSTEM_PROMPT.splitlines()) < 1300


def test_the_prompt_names_every_tool_in_the_registry_and_no_other() -> None:
    assert len(OFFERED) == len(TOOL_SPECS) == 14
    assert not OFFERED & VOCABULARY, "a tool shares its name with another word of the prompt"
    quoted = set(re.findall(r"`([a-z][a-z0-9_]*)`", SYSTEM_PROMPT))
    # A quoted lower-case name is a tool unless the engine, the fit library or a tool's own
    # schema defines it as something else.
    tools_named = quoted - VOCABULARY
    assert tools_named - OFFERED == set(), "the prompt names tools the registry does not have"
    assert OFFERED - tools_named == set(), "the prompt does not name these tools"
    # Outside quotes too: no name with an underscore that nothing defines.
    for word in set(re.findall(r"\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b", SYSTEM_PROMPT)):
        assert word in OFFERED or word in VOCABULARY, f"the prompt names `{word}`"


def test_the_prompt_offers_no_tool_for_what_only_the_maker_can_do() -> None:
    """Gauges, calibration and the confirmation of a print are the maker's, in Hotend. The
    prompt may name the commands; it must not suggest a tool."""
    for name in ("gauge", "calibrate", "calibration", "confirm", "print", "questions"):
        assert name not in OFFERED and f"`{name}`" not in SYSTEM_PROMPT, name
    text = " ".join(SYSTEM_PROMPT.split())
    assert "`hotend gauge`, then `hotend calibrate`); you have no tool for either" in text
    assert "You cannot start a print, and neither can any tool" in text


def test_every_check_fit_class_and_card_is_named() -> None:
    for check in (*CHECK_IDS, *OPTIONAL_CHECK_IDS):
        assert named(check), check
    for fit_class in FIT_CLASSES:
        assert named(fit_class), fit_class
    for card in cards.CARDS:
        assert named(card), card
    for word in ("apply_a", "apply_b", "ctx.fit.clearance(fit_class, nominal)", ".hole_delta",
                 ".peg_delta"):
        assert word in SYSTEM_PROMPT, word


def test_the_script_convention_matches_the_engine() -> None:
    # What only web research uses is named in the research section, which a run without
    # research does not get.
    research_only = ("web", "source")
    for word in research_only:
        assert not named(word) and f"`{word}`" in RESEARCH_PROMPT, word
    for provenance in PROVENANCES:
        assert named(provenance) or provenance in research_only, provenance
    for unit in UNITS:
        assert named(unit), unit
    for word in ("PARAMS", "def build(p, ctx):", "[a-z][a-z0-9_]{0,31}", "`default`",
                 "`provenance`", "`min`", "`max`", "`unit`", "`label`", "literal dict",
                 "from hotend_fit import cards", "from build123d import *"):
        assert word in SYSTEM_PROMPT, word
    for key in SPEC_KEYS:
        assert named(key) or key in research_only, key
    for status in ("ok", "check_error", "model_error", "skipped"):
        assert named(status), status


def test_the_contract_and_print_declarations_match_the_fit_library() -> None:
    assert CONTRACT_KINDS == {"cavity_min", "outer_max", "wall_min", "dimension", "fit"}
    for kind in CONTRACT_KINDS:
        assert named(kind), kind
    for key in ("id", "kind", "provenance", "params", "note", "part", "size", "value", "card",
                "between", "fit_class", "name"):
        assert named(key), key
        assert any(key in model.model_fields for model in REQUIREMENTS), key
    # The two literal examples in the prompt are valid declarations.
    example = re.search(r"`PRINT = (\{[^`]+\})`", SYSTEM_PROMPT)
    assert example and contract.parse_print(ast.literal_eval(example.group(1)))
    assert named("+x") and named("-z") and {"+x", "-z"} <= set(contract.AXES)
    shown = re.search(r"`CONTRACT = \{([^`]+)\}`", SYSTEM_PROMPT)
    assert shown
    assert set(re.findall(r'"([a-z]+)":', shown.group(1))) == set(contract.Contract.model_fields)


def test_the_example_in_the_prompt_is_a_valid_params_block() -> None:
    example = SYSTEM_PROMPT.split("```python\n")[1].split("```")[0]
    tree = ast.parse(example)
    params = next(node.value for node in tree.body
                  if isinstance(node, ast.Assign) and node.targets[0].id == "PARAMS")
    specs = validate_param_specs(ast.literal_eval(params))
    assert set(specs) == {"width", "wall"}
    assert any(isinstance(node, ast.FunctionDef) and node.name == "build"
               and [a.arg for a in node.args.args] == ["p", "ctx"] for node in tree.body)


def test_the_working_rules_are_all_there() -> None:
    text = " ".join(SYSTEM_PROMPT.split())
    for rule in (
        # the contract, first and kept true
        "Contract first. Before any geometry, restate the request as a short contract",
        "every number with where it comes from",
        "Never guess a critical number silently",
        "the contract included as `CONTRACT`",
        "Keep the contract true",
        "never loosen the contract to pass one",
        # facts Hotend can get are not the maker's problem: a component, then research, and
        # only then the maker, saying why (see test_facts_come_from_the_harness_first)
        "never make a fact Hotend can get the maker's problem",
        ("in this order: a component (see Bought devices); web research, when this run has it; "
        "only then the maker"),
        # questions for the user: real choices, few, each with a recommendation
        "Ask the maker two kinds of question only: their real choices",
        "at most three in a message, each with the option you recommend",
        '"go with your suggestion" is a full answer',
        "The answers are the maker's, never yours",
        "tag it `measured` in `PARAMS` and `CONTRACT`, and rebuild",
        # fits and checks
        "comes from a feature card",
        "Never write a clearance",
        "Fix what it shows before doing anything else",
        # look review: the standard views come with an `ok` write, and the answer says what
        # was checked in them (the runner asks once when it does not)
        "ends with Hotend's standard views (iso, front, top)",
        "Then write one line beginning `looked at:` that says what you checked",
        "Call `render_views` only if the views leave a doubt",
        "A build whose status is `ok` ends with Hotend's standard views",
        "A `skipped` check proved nothing",
        "at most about six sliders",
        "narrow the declared range where a check fails",
        'Say "checked", never "guaranteed"',
        # the print-preparation summary is read and acted on
        "Read all of its answer",
        "React before you go on",
        "A part needs support: turn it with `PRINT`, reshape the overhang",
        "or split the part",
        "Little of it lies on the bed: add a flat",
        "A piece did not fit: make it smaller or split it",
        "A changed model needs a new `slice`",
        # the gauge before a first print on a guessed calibration
        "Fit gauge first",
        "suggest printing the fit gauge before the first print of anything that mates",
        # a print is proposed, never started and never claimed
        "`propose_print` only queues the latest slice as a job for the maker to review",
        "Do not call it while a question for the user is open",
        "You cannot start a print",
        "Never say that a print was started, is running or was sent to a printer",
        "what is still a guess or not verified and how it gets settled before the first print",
        # a snapshot names the state the maker wants to keep (how, in the tool's description)
        "Call `snapshot_model` when the maker wants to keep a state",
        # the docs and the script's output are tools to use
        "use it instead of memory whenever an API call fails",
        "`print()` output comes back with the build result; use it to debug",
    ):
        assert rule in text, rule
    snapshot = next(spec for spec in TOOL_SPECS if spec["name"] == "snapshot_model")
    assert snapshot["description"].startswith(
        "When the maker wants to keep this state, name the current version as a snapshot: use "
        "their name for it, or a short one from their words"
    )


def test_facts_come_from_the_harness_first() -> None:
    """The rule: Hotend helps to find the numbers and does not make them the maker's
    problem. A component comes first, web research second, the maker last and only with the
    reason; questions are real choices with a recommendation."""
    text = " ".join(SYSTEM_PROMPT.split())
    component, research, maker = (text.index(word) for word in (
        "a component (see Bought devices)", "web research, when this run has it",
        "only then the maker"))
    assert component < research < maker
    for rule in (
        # research that is off is said in one line the runner recognises, with the reason
        # the run's note gives
        ("write one line that begins `web research is off:` and names what you would look up "
        "and why it is off (Hotend's note on the maker's message says why)"),
        "Hotend puts the matching button next to your message",
        # what is the maker's to give, and that they are told why
        ("A number is the maker's to give only when it is a one-off thing only they have "
        "(their shelf, a hand-made part), or when no component has it and research is off or "
        "found nothing; say which"),
        # such a number is asked, one at a time, and the build waits for it
        ("Ask for such a critical number and stop: one at a time, in plain words, saying what "
        "to measure and how, and build only when it is answered"),
        ("If the maker asks to see something first, or the number is not critical, go on with "
        "a guess, tag it `guessed`, make it a parameter and say so; never guess silently"),
        # every number keeps its source
        ("Every number keeps its source in the contract: the component, the page and quote, or "
        "the maker"),
        # step 5 allows those measurements next to the choices, and says why they are theirs
        "and a measurement step 1 leaves to them, one per message, saying why it is theirs",
        # "rather than a ruler" is for a component's unverified numbers only
        ("For an unverified number of a component, say what it means for the fit and offer the "
        "cheapest check (research, a test print of that area or the fit gauge) rather than a "
        "ruler; a guessed number only the maker has is a measurement as in step 1"),
        # the summary ends with what is asked now, whichever kind
        ("then end with what you ask now (the one measurement, or the choices, each with your "
        "recommendation), or say that nothing is open"),
    ):
        assert rule in text, rule
    # Step 5 does not contradict step 1: no "choices only", and the ruler rule is scoped.
    assert "real choices only" not in text
    assert text.count("rather than a ruler") == 1
    assert "the choices you are asking about" not in text
    # The marker the prompt asks for is the one the runner reads.
    from hotend_agent.runner import RESEARCH_OFF

    assert "`web research is off:`" in text
    assert RESEARCH_OFF.search("web research is off: the phone's camera bar outline")
    assert "with calipers or a ruler" not in text


def test_the_prompt_tells_the_agent_that_components_exist() -> None:
    text = " ".join(SYSTEM_PROMPT.split())
    assert "\n# Bought devices\n" in SYSTEM_PROMPT
    for rule in (
        "Hotend may have the device as a component",
        "call `list_components` first",
        "Build around a listed device with `ctx.component(id)`",
        "never with sizes from memory, and do not look its size up or ask for it",
        ("A component whose name says invented is a sample and no real device: use it only "
         "when it is asked for by its name"),
        "A device that is not listed has no component: get its size as step 1 says",
        "Tell the maker where the numbers come from",
        "name the features whose numbers are not verified and what that means for the fit",
        "offer research or a test print of that area instead of a measurement",
    ):
        assert rule in text, rule
    # No device is promised by name: what is there is what `list_components` answers.
    assert "a phone, a board" not in text
    # The recipe the prompt points to is what `read_docs` finds for those words.
    from hotend_engine import docs

    entries, _ = docs.build_index()
    found = [hit["name"] for hit in docs.search(entries, "bought components", 3)]
    assert "hotend_fit.capabilities.bought_components" in found
    # A component number is a maker's drawing: `standard`, named so in step 1.
    assert "a component listed by `list_components`" in text
    # The research section knows the components too, and does not search for their size.
    research = " ".join(RESEARCH_PROMPT.split())
    assert "A device `list_components` lists is known: use the component and do not search " \
        "for its size" in research
    assert "if research finds nothing, say so and only then ask" in research


def test_the_component_listing_names_the_sample_phone_with_its_sources() -> None:
    """What `list_components` returns: the gateway searches the engine's docs index for the
    entries of `hotend_fit.components.catalogue` (an introduction and one entry per
    component, as `list_cards` reads the cards) and joins them; the entries are checked here
    against the data."""
    from hotend_engine import docs
    from hotend_fit.components import catalogue

    entries, versions = docs.build_index()
    assert versions["hotend_fit"]
    module = "hotend_fit.components.catalogue"
    ours = sorted(e.name for e in entries if e.module == module)
    assert ours == [f"{module}.shipped_component_{cid}" for cid in list_components()] + [
        f"{module}.shipped_components"]
    # The query the gateway sends finds the introduction first and every component.
    hits = docs.search(entries, catalogue.QUERY, 20)
    assert hits[0]["name"] == f"{module}.shipped_components"
    found = {hit["name"] for hit in hits if hit["module"] == module}
    assert found == set(ours)
    for hit in hits:
        assert not hit["doc"].endswith("[...]"), f"{hit['name']} is cut off by the docs index"
    intro = hits[0]["doc"]
    assert "`ctx.component(id)`" in intro and "sample_phone" in intro
    assert "whose name says invented is a sample for examples and tests" in intro
    (line,) = [hit["doc"] for hit in hits if hit["name"].endswith("shipped_component_sample_phone")]
    assert line.startswith("- sample_phone: Sample phone (invented)") and "80 x 130 x 10 mm" in line
    assert "its body size is verified" in line
    assert "sample (invented_sample) https://example.com/hotend/sample-phone" in line
    assert "sample_estimate (estimate) https://example.com/" in line
    assert "not shipped" not in line
    data = catalogue.shipped_component_sample_phone()
    assert data["unverified"] == 29 and f"{data['numbers']} numbers, 29 not verified" in line
    assert data["unverified_by_feature"]["camera_plateau"] == 7 and "camera_plateau (7)" in line
    # One entry per component stays below the index's cut, however many ship.
    assert len(line) < docs.MAX_DOC_CHARS - 300


def test_a_component_that_cannot_be_read_names_no_path(tmp_path: Path, monkeypatch: Any) -> None:
    from hotend_fit import components
    from hotend_fit.components import catalogue

    (tmp_path / "broken.json").write_text("{not json", "utf-8")
    monkeypatch.setattr(components, "DATA_DIR", tmp_path)
    line = catalogue._describe("broken")
    assert line.startswith("- broken: its data cannot be used (the data of 'broken' cannot be read")
    assert str(tmp_path) not in line and "/" not in line
    (tmp_path / "gone.json").mkdir()
    with pytest.raises(components.ComponentError) as raised:
        components.load_component("gone")
    assert str(tmp_path) not in str(raised.value)


def test_the_prompt_reads_the_gateways_words() -> None:
    """The phrases the prompt tells the model to look for are the ones the gateway writes: the
    recorded answers of the long demo conversation contain each of them."""
    from agent_demo_script import recorded

    answers = "\n".join(result["text"] for result in recorded())
    for phrase in ("status:", "questions for the user", "orientation on the plate:",
                   "plate check"):
        assert f"`{phrase}`" in SYSTEM_PROMPT and phrase in answers, phrase
    for phrase in ("needs support", "lies flat on the bed", "NOT CHECKABLE contract",
                   "This printer is not calibrated"):
        assert phrase in answers, phrase
