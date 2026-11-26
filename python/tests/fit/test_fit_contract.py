"""The product contract: validation and the questions it leaves for the user."""

from __future__ import annotations

import copy

import pytest
from pydantic import ValidationError

from hotend_fit.contract import PROVENANCES, Contract, open_questions, parse_contract

# The example of docs/PROTOCOL.md section 4.1.
EXAMPLE = {
    "protocol": 0,
    "name": "Desk cable box",
    "parts": ["box", "lid"],
    "requirements": [
        {"id": "holds_power_strip", "kind": "cavity_min", "part": "box",
         "size": [250, 55, 40], "provenance": "measured"},
        {"id": "lid_snaps", "kind": "fit", "card": "snap_lid", "between": ["box", "lid"],
         "fit_class": "snap", "provenance": "calibrated"},
    ],
}


def test_the_protocol_example_validates_and_asks_nothing():
    contract = parse_contract(EXAMPLE)
    assert isinstance(contract, Contract) and contract.parts == ("box", "lid")
    assert [r.kind for r in contract.requirements] == ["cavity_min", "fit"]
    assert contract.requirements[0].size == (250.0, 55.0, 40.0)
    assert open_questions(contract) == []
    assert parse_contract(contract.model_dump(mode="json")) == contract


def test_every_provenance_tag_is_accepted_and_nothing_else():
    for tag in PROVENANCES:
        data = copy.deepcopy(EXAMPLE)
        data["requirements"][0]["provenance"] = tag
        assert parse_contract(data).requirements[0].provenance == tag
    data = copy.deepcopy(EXAMPLE)
    data["requirements"][0]["provenance"] = "assumed"
    with pytest.raises(ValidationError):
        parse_contract(data)
    del data["requirements"][0]["provenance"]
    with pytest.raises(ValidationError, match="provenance"):
        parse_contract(data)


def test_requirements_resting_on_guessed_numbers_become_questions():
    data = copy.deepcopy(EXAMPLE)
    data["requirements"][0]["provenance"] = "guessed"
    data["requirements"][1]["provenance"] = "guessed"
    data["requirements"].append({"id": "looks", "kind": "outer_max", "part": "lid",
                                 "size": [300, 80, 5], "provenance": "style"})
    questions = open_questions(parse_contract(data))
    assert [q.requirement for q in questions] == ["holds_power_strip", "lid_snaps"]
    assert "250 x 55 x 40" in questions[0].text and "measure" in questions[0].text
    assert "fit gauge" in questions[1].text and "'box' and 'lid'" in questions[1].text


def test_a_guessed_parameter_makes_a_measured_requirement_a_question():
    data = copy.deepcopy(EXAMPLE)
    data["requirements"][0]["params"] = ["width", "depth"]
    contract = parse_contract(data)
    specs = {"width": {"provenance": "guessed", "label": "Inside width"},
             "depth": {"provenance": "measured", "label": "Inside depth"}}
    (question,) = open_questions(contract, specs)
    assert question.requirement == "holds_power_strip" and question.guessed_params == ("width",)
    assert "Inside width" in question.text and "Inside depth" not in question.text
    specs["width"]["provenance"] = "measured"
    assert open_questions(contract, specs) == []
    # A parameter the model does not define cannot be traced, so it is asked about too.
    (untraced,) = open_questions(contract, {"width": specs["width"]})
    assert untraced.guessed_params == ("depth",)
    # Without the model's parameters only the requirement's own tag counts.
    assert open_questions(contract) == []


def _with(change) -> dict:
    data = copy.deepcopy(EXAMPLE)
    change(data)
    return data


INVALID = {
    "unknown part": lambda d: d["requirements"][0].update(part="tray"),
    "fit with an unknown part": lambda d: d["requirements"][1].update(between=["box", "cap"]),
    "fit of a part with itself": lambda d: d["requirements"][1].update(between=["box", "box"]),
    "repeated requirement id": lambda d: d["requirements"][1].update(id="holds_power_strip"),
    "repeated part": lambda d: d.update(parts=["box", "box"]),
    "no parts": lambda d: d.update(parts=[]),
    "unknown fit class": lambda d: d["requirements"][1].update(fit_class="free"),
    "unknown card": lambda d: d["requirements"][1].update(card="hinge"),
    "unknown kind": lambda d: d["requirements"][0].update(kind="looks_nice"),
    "unknown key": lambda d: d["requirements"][0].update(clearance=0.3),
    "negative size": lambda d: d["requirements"][0].update(size=[250, -55, 40]),
    "two-number size": lambda d: d["requirements"][0].update(size=[250, 55]),
    "bad id": lambda d: d["requirements"][0].update(id="Holds Power Strip"),
    "wrong protocol": lambda d: d.update(protocol=1),
    "extra top-level key": lambda d: d.update(author="ai"),
    "non-finite number": lambda d: d["requirements"][0].update(size=[250, float("inf"), 40]),
}


@pytest.mark.parametrize("case", INVALID)
def test_an_inconsistent_contract_is_rejected(case):
    with pytest.raises(ValidationError):
        parse_contract(_with(INVALID[case]))


def test_the_other_requirement_kinds_validate():
    data = copy.deepcopy(EXAMPLE)
    data["requirements"] += [
        {"id": "thin", "kind": "wall_min", "part": "box", "value": 1.2, "provenance": "style"},
        {"id": "slot", "kind": "dimension", "part": "box", "name": "cable slot width",
         "value": 12.0, "tolerance": 0.2, "provenance": "guessed"},
    ]
    contract = parse_contract(data)
    (question,) = open_questions(contract)
    assert question.requirement == "slot" and "Cable slot width" in question.text


def test_a_reference_fit_requirement_validates_and_asks_for_the_ring_gauge():
    data = {"protocol": 0, "name": "Holder", "parts": ["basket"], "requirements": [
        {"id": "figure_drops_in", "kind": "reference_fit", "reference": "figure",
         "fit_class": "cradle", "provenance": "guessed"},
        {"id": "phone_is_held", "kind": "reference_fit", "reference": "sample_phone",
         "provenance": "calibrated", "params": ["grip"]},
    ]}
    contract = parse_contract(data)
    assert [r.kind for r in contract.requirements] == ["reference_fit", "reference_fit"]
    assert contract.requirements[1].fit_class is None
    assert "cradle" in contract.requirements[0].subject()
    questions = open_questions(contract, {"grip": {"default": 1, "provenance": "guessed"}})
    assert [q.requirement for q in questions] == ["figure_drops_in", "phone_is_held"]
    assert all("ring gauge" in q.text for q in questions)
    assert questions[1].guessed_params == ("grip",)
    for bad in ({"fit_class": "press"}, {"reference": "Not A Name"}, {"part": "basket"}):
        broken = copy.deepcopy(data)
        broken["requirements"][0].update(bad)
        with pytest.raises(ValidationError):
            parse_contract(broken)
