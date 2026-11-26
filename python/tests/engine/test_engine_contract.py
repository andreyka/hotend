"""CONTRACT, ASSEMBLY and PRINT: validated strictly before the script runs, and the
contract's requirements measured on the solids when the request asks (`checks.contract`)."""

from __future__ import annotations

import pytest
from build123d import Align, Box, Pos

from hotend_engine.checks import CHECK_IDS, OPTIONAL_CHECK_IDS, CheckEnv, run_checks
from hotend_engine.checks.contract import contract_questions
from hotend_engine.context import BuildContext
from hotend_engine.errors import ModelError
from hotend_engine.model import check_declared_parts, load_model
from hotend_engine.parts import NamedPart
from hotend_engine.request import CheckOptions
from hotend_engine.tessellate import tessellate
from hotend_fit.contract import parse_contract

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)

SCRIPT = '''
PARAMS = {"size": {"default": 30.0, "min": 20.0, "max": 60.0, "provenance": "guessed"}}
%s
def build(p, ctx):
    return None
'''
GOOD_CONTRACT = ('CONTRACT = {"protocol": 0, "name": "Box", "parts": ["box"], '
                 '"requirements": [%s]}')
CAVITY = ('{"id": "holds", "kind": "cavity_min", "part": "box", "size": [20.0, 20.0, 5.0], '
          '"provenance": "measured"}')


def load(tmp_path, declarations: str):
    path = tmp_path / "model.py"
    path.write_text(SCRIPT % declarations, encoding="utf-8")
    return load_model(path)


# --- validation --------------------------------------------------------------------------------


def test_valid_declarations_are_read_from_the_source_without_running_it(tmp_path):
    model = load(tmp_path, "\n".join([
        GOOD_CONTRACT % CAVITY, "",
        'ASSEMBLY = {"lid": {"mate": "box.rim", "note": "snaps on"}}',
        'PRINT = {"lid": {"up": "-z"}, "box": {"up": [0, 1, 0]}}',
    ]))
    assert model.contract.name == "Box" and model.contract.requirements[0].id == "holds"
    assert model.assembly.mates["lid"].part == "box"
    assert model.print_hints.parts["lid"].vector == (0.0, 0.0, -1.0)
    assert model.print_hints.parts["box"].vector == (0.0, 1.0, 0.0)
    assert model.declaration_lines == {"CONTRACT": 3, "ASSEMBLY": 5, "PRINT": 6}
    # A model that declares nothing is as before.
    plain = load(tmp_path, "")
    assert plain.contract is None and plain.assembly is None and plain.print_hints is None


@pytest.mark.parametrize(("declaration", "error_type", "fragment"), [
    (GOOD_CONTRACT % CAVITY.replace("20.0, 20.0, 5.0", '20.0, "wide", 5.0'),
     "ContractError", "requirements.0.cavity_min.size.1: Input should be a valid number"),
    (GOOD_CONTRACT % CAVITY.replace("cavity_min", "cavity"),
     "ContractError", "requirements.0: Input tag 'cavity' found using 'kind'"),
    (GOOD_CONTRACT % CAVITY.replace('"measured"', '"sure", "colour": "red"'),
     "ContractError", "requirements.0.cavity_min.colour: Extra inputs are not permitted"),
    (GOOD_CONTRACT % CAVITY.replace('"part": "box"', '"part": "lid"'),
     "ContractError", "requirement 'holds' names part 'lid', which is not in parts (box)"),
    ('CONTRACT = {"protocol": 0, "name": "Box", "parts": ["box", 7], "requirements": []}',
     "ContractError", "parts.1: Input should be a valid string"),
    ('CONTRACT = {"name": "Box"}', "ContractError", "parts: Field required"),
    ('CONTRACT = "it holds a power strip"', "ContractError", "CONTRACT is not valid: value:"),
    ("CONTRACT = dict(name='Box')", "ContractError", "CONTRACT must be a literal dict"),
    (GOOD_CONTRACT % "" + "\nCONTRACT = {}", "ContractError", "assigned more than once"),
    ('ASSEMBLY = {"lid": {"mate": 5}}', "AssemblyError", "lid.mate: Input should be a valid"),
    ('ASSEMBLY = {"lid": {"mate": "box.rim", "glue": True}}',
     "AssemblyError", "lid.glue: Extra inputs are not permitted"),
    ('ASSEMBLY = {"lid": {"mate": "lid.rim"}}', "AssemblyError", "cannot mate with itself"),
    ('PRINT = {"box": {"up": "sideways"}}', "PrintError", "box.up: Input should be '+x'"),
    ('PRINT = {"box": {"up": [0, 0, 0]}}', "PrintError", "up must not be the zero vector"),
    ('PRINT = {"box": "flat"}', "PrintError", "box: Input should be a valid dictionary"),
    ('PRINT = {"box": {"up": "+z"}, "Lid!": {"up": "+z"}}', "PrintError", "Lid!"),
])
def test_junk_declarations_fail_the_build_and_name_the_field(tmp_path, declaration,
                                                              error_type, fragment):
    with pytest.raises(ModelError) as caught:
        load(tmp_path, declaration)
    assert caught.value.type == error_type
    assert fragment in caught.value.message, caught.value.message
    assert caught.value.line is not None and caught.value.line >= 3


def test_a_declaration_that_names_a_part_build_did_not_return_fails(tmp_path):
    model = load(tmp_path, GOOD_CONTRACT % CAVITY)
    check_declared_parts(model, ["box", "lid"])
    with pytest.raises(ModelError) as caught:
        check_declared_parts(model, ["case"])
    assert caught.value.type == "ContractError" and caught.value.line == 3
    assert caught.value.message == "CONTRACT names part(s) 'box', but build returned 'case'"
    for text, error_type in (('ASSEMBLY = {"lid": {"mate": "box"}}', "AssemblyError"),
                             ('PRINT = {"lid": {"up": "-z"}}', "PrintError")):
        with pytest.raises(ModelError) as caught:
            check_declared_parts(load(tmp_path, text), ["box"])
        assert caught.value.type == error_type and "'lid'" in caught.value.message


def test_a_junk_contract_is_a_model_error_of_the_build(builds):
    job = builds["broken_contract"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "model_error" and result["parts"] == []
    error = result["error"]
    assert error["type"] == "ContractError" and error["line"] == 9
    assert error["message"] == (
        "CONTRACT is not valid: requirements.0.cavity_min.size.1: Input should be a valid "
        "number, unable to parse string as a number")
    # Nothing was built: the script did not run.
    assert job.files() == {"model.py", "request.json", "result.json"}


# --- the requirements measured on the solids -------------------------------------------------


def rows(result: dict) -> dict[str, dict]:
    found = [c for c in result["checks"] if c["id"] == "contract"]
    assert all(c["data"]["kind"] == "requirement" for c in found)
    return {c["data"]["requirement"]: c for c in found}


def test_every_requirement_is_a_check_with_its_provenance(builds):
    job = builds["print_hint"]
    assert job.returncode == 0, job.stderr
    result = job.result
    found = rows(result)
    assert list(found) == ["card_fits", "brick_fits", "fits_the_shelf", "fits_the_slot",
                           "tray_walls", "post_reaches", "dome_clicks_on"]
    assert {name: row["status"] for name, row in found.items()} == {
        "card_fits": "pass", "brick_fits": "fail", "fits_the_shelf": "pass",
        "fits_the_slot": "fail", "tray_walls": "warn", "post_reaches": "skipped",
        "dome_clicks_on": "fail",
    }
    assert all(row["severity"] == "error" for row in found.values())
    # An unmet requirement that is not a guess stops the build like any failed check.
    assert result["status"] == "check_error"

    fits = found["card_fits"]
    assert fits["part"] == "tray" and fits["data"]["provenance"] == "measured"
    assert fits["data"]["requirement_kind"] == "cavity_min" and fits["data"]["met"] is True
    assert fits["data"]["params"] == ["inside"] and fits["data"]["question"] is None
    assert fits["message"] == ("Requirement 'card_fits' (measured) is met: a 40 x 40 x 8 mm "
                               "cuboid fits inside 'tray'.")

    brick = found["brick_fits"]
    assert brick["data"]["met"] is False and brick["data"]["fits_at_most"]["x"] == 40.0
    assert brick["message"] == (
        "Requirement 'brick_fits' (measured) is not met: no 45 x 40 x 8 mm cuboid fits "
        "inside 'tray' (something stands in the free space); with the other two sizes "
        "kept, at most about 40.0 mm along X fits.")

    assert found["fits_the_shelf"]["data"]["actual"] == [44.0, 44.0, 10.0]
    slot = found["fits_the_slot"]
    assert slot["part"] == "post" and slot["data"]["provenance"] == "standard"
    assert "20.00 mm too large along Z" in slot["message"]

    walls = found["tray_walls"]
    assert walls["data"]["met"] is True and walls["data"]["thinnest"] == pytest.approx(2.0)
    assert "(style) is met: thinnest wall found is 2.00 mm; the minimum is 1.20 mm" in (
        walls["message"])

    # Not checkable: said with its reason, never passed and never left out.
    reach = found["post_reaches"]
    assert reach["status"] == "skipped" and reach["data"]["met"] is None
    assert reach["message"].startswith("Not checkable: requirement 'post_reaches'")
    assert reach["data"]["not_checkable"] is True
    assert "has no place on the solid" in reach["data"]["reason"]
    assert reach["data"]["reason"] in reach["message"]

    snap = found["dome_clicks_on"]
    assert snap["part"] is None and snap["data"]["parts"] == ["post", "dome"]
    assert snap["message"] == (
        "Requirement 'dome_clicks_on' (standard) is not met: the model has no snap lid card "
        "with a snap fit between 'post' and 'dome'.")


def test_a_requirement_that_rests_on_a_guess_is_a_question_for_the_user(builds):
    result = builds["print_hint"].result
    walls = rows(result)["tray_walls"]
    question = {
        "requirement": "tray_walls", "guessed_params": ["wall"],
        "text": "The wall of 'tray' of at least 1.2 mm depends on a value that was guessed "
                "(wall). Please measure it.",
    }
    # Met, and still only a warning: a guess that holds proves nothing.
    assert walls["status"] == "warn" and walls["data"]["question"] == question
    assert walls["message"].endswith("It rests on a guess: " + question["text"])
    assert result["questions"] == [question]


def test_the_cable_box_contract_is_measured_and_its_guesses_are_asked(builds):
    job = builds["cable_box_asked"]
    assert job.returncode == 0, job.stderr
    result = job.result
    assert result["status"] == "ok"
    found = rows(result)
    assert {name: (row["status"], row["data"]["met"]) for name, row in found.items()} == {
        "holds_power_strip": ("warn", True), "cables_pass": ("warn", None),
        "lid_snaps": ("warn", True), "lid_screwed": ("pass", True),
        "walls_print": ("pass", True),
    }
    # The bundled example meets its own inside size: the screw boss stands in a post
    # outside the inside, so the 120 x 60 x 36 mm the contract asks for goes in (it is
    # still a warning only because the size is a guess).
    strip = found["holds_power_strip"]
    assert "is met: a 120 x 60 x 36 mm cuboid fits inside 'box'" in strip["message"]
    assert "fits_at_most" not in strip["data"]
    assert "It rests on a guess: " in strip["message"]
    assert found["cables_pass"]["data"]["not_checkable"] is True

    snaps = found["lid_snaps"]
    assert "is met: the snap lid card with a snap fit between 'box' and 'lid' is built by" in (
        snaps["message"])
    (card,) = snaps["data"]["cards"]
    assert card["joint_ok"] and card["pairs_failed"] == 0 and card["pairs"] > 0
    assert found["lid_screwed"]["data"]["provenance"] == "standard"

    assert [q["requirement"] for q in result["questions"]] == [
        "holds_power_strip", "cables_pass", "lid_snaps"]
    assert all(q["text"] for q in result["questions"])


def test_nothing_changes_for_a_request_that_does_not_ask(builds):
    plain = builds["lidded_box"].result
    assert "questions" not in plain
    assert "contract" not in {c["id"] for c in plain["checks"]}
    assert "contract" not in CHECK_IDS and OPTIONAL_CHECK_IDS == ("contract",)
    # Asked of a model that declares no contract: said, not passed over.
    asked = builds["lidded_box_asked"].result
    (row,) = [c for c in asked["checks"] if c["id"] == "contract"]
    assert row["status"] == "skipped" and row["message"] == (
        "Skipped: the model declares no CONTRACT.")
    assert asked["questions"] == []


def env_for(shapes: dict, requirements: list[dict], specs: dict | None = None, **options):
    parts = [NamedPart(name, shape, "#4c78a8") for name, shape in shapes.items()]
    parsed = parse_contract({"protocol": 0, "name": "Test", "parts": list(shapes),
                             "requirements": requirements})
    return CheckEnv(parts, BuildContext(None, None, None), CheckOptions(**options),
                    tessellate(parts), None, contract=parsed, param_specs=specs or {})


def test_the_cavity_is_the_free_space_a_cuboid_goes_into():
    cup = Box(44, 34, 20, align=BOTTOM) - Pos(0, 0, 2) * Box(40, 30, 20, align=BOTTOM)
    post = Pos(15, 10, 2) * Box(6, 6, 18, align=BOTTOM)  # a boss in one corner

    def requirement(size):
        return {"id": "holds", "kind": "cavity_min", "part": "cup", "size": size,
                "provenance": "measured"}

    def met(shape, size):
        env = env_for({"cup": shape}, [requirement(size)], contract=True, only=("contract",))
        (result,) = run_checks(env)
        return result

    # Drawn exactly to size it holds the cuboid; a tenth more does not go in.
    assert met(cup, [40.0, 30.0, 18.0]).status == "pass"
    assert met(cup, [40.1, 30.0, 18.0]).status == "fail"
    assert met(cup, [40.0, 30.0, 18.1]).status == "fail"
    # The boss takes the corner: the full size no longer fits, a narrower one does.
    blocked = met(cup + post, [40.0, 30.0, 18.0])
    assert blocked.status == "fail"
    assert blocked.data["fits_at_most"] == {"x": 32.0, "y": 22.0, "z": 0.0}
    assert "at most about 32.0 mm along X, 22.0 mm along Y fits" in blocked.message
    assert met(cup + post, [32.0, 30.0, 18.0]).status == "pass"
    assert met(cup + post, [40.0, 22.0, 18.0]).status == "pass"
    # A solid block has no free space at all.
    solid = met(Box(44, 34, 20, align=BOTTOM), [1.0, 1.0, 1.0])
    assert solid.status == "fail" and "there is no free space inside it" in solid.message


def test_a_guessed_requirement_is_never_more_than_a_warning():
    cup = Box(44, 34, 20, align=BOTTOM) - Pos(0, 0, 2) * Box(40, 30, 20, align=BOTTOM)
    specs = {"width": {"default": 40.0, "provenance": "guessed"},
             "depth": {"default": 30.0, "provenance": "measured"}}
    requirements = [
        {"id": "own_tag", "kind": "cavity_min", "part": "cup", "size": [60.0, 30.0, 18.0],
         "provenance": "guessed"},
        {"id": "by_param", "kind": "outer_max", "part": "cup", "size": [50.0, 40.0, 25.0],
         "provenance": "measured", "params": ["width"]},
        {"id": "measured", "kind": "outer_max", "part": "cup", "size": [50.0, 40.0, 25.0],
         "provenance": "measured", "params": ["depth"]},
    ]
    env = env_for({"cup": cup}, requirements, specs, contract=True, only=("contract",))
    own, by_param, measured = run_checks(env)
    assert own.status == "warn" and "is NOT met" in own.message and own.data["met"] is False
    assert by_param.status == "warn" and "is met" in by_param.message
    assert by_param.data["question"]["guessed_params"] == ["width"]
    assert measured.status == "pass" and measured.data["question"] is None
    assert [q["requirement"] for q in contract_questions(env)] == ["own_tag", "by_param"]

    # Not asked for: no result at all. Named in `only`: it runs.
    silent = env_for({"cup": cup}, requirements, specs)
    assert "contract" not in {result.id for result in run_checks(silent)}
    named = env_for({"cup": cup}, requirements, specs, only=("contract",))
    assert [result.id for result in run_checks(named)] == ["contract"] * 3


def test_a_requirement_whose_measurement_crashes_holds_the_version_back(monkeypatch):
    """A measurement that raises must not leave a measured requirement `skipped`."""
    import importlib

    contract_check = importlib.import_module("hotend_engine.checks.contract")
    from hotend_engine.checks import has_blocking_failure

    def broken(env, requirement):
        raise ZeroDivisionError("boom")

    monkeypatch.setitem(contract_check._MEASURES, "cavity_min", broken)
    cup = Box(44, 34, 20, align=BOTTOM) - Pos(0, 0, 2) * Box(40, 30, 20, align=BOTTOM)
    env = env_for({"cup": cup}, [{"id": "holds", "kind": "cavity_min", "part": "cup",
                                  "size": [40.0, 30.0, 18.0], "provenance": "measured"}],
                  contract=True, only=("contract",))
    (result,) = run_checks(env)
    assert result.status == "fail" and result.severity == "error"
    assert has_blocking_failure([result])
    assert result.data["could_not_run"] is True
    assert result.data["exception"] == "ZeroDivisionError: boom"
    assert "not proven" in result.message and "boom" not in result.message


def test_a_requirement_that_cannot_be_checked_is_reported_with_its_reason(monkeypatch):
    """`opening` and `lid_seat`, two dimensions named in words, were once reported only as
    skipped. Every requirement the check cannot
    evaluate is a result marked not checkable, with the reason, and never a pass."""
    import importlib

    contract_check = importlib.import_module("hotend_engine.checks.contract")
    from hotend_engine.checks import has_blocking_failure

    cup = Box(44, 34, 20, align=BOTTOM) - Pos(0, 0, 2) * Box(40, 30, 20, align=BOTTOM)
    specs = {"inner": {"default": 40.0, "provenance": "guessed"}}
    requirements = [
        {"id": "opening", "kind": "dimension", "part": "cup", "name": "inside diameter",
         "value": 40.0, "provenance": "measured"},
        {"id": "lid_seat", "kind": "dimension", "part": "cup", "name": "plug ring",
         "value": 40.0, "provenance": "calibrated", "params": ["inner"]},
        {"id": "holds", "kind": "cavity_min", "part": "cup", "size": [40.0, 30.0, 18.0],
         "provenance": "measured"},
    ]
    # A kind this check has no measurement for (a future one): not checkable, not a crash.
    monkeypatch.delitem(contract_check._MEASURES, "cavity_min")
    env = env_for({"cup": cup}, requirements, specs, contract=True, only=("contract",))
    opening, seat, holds = run_checks(env)
    assert [r.data["requirement"] for r in (opening, seat, holds)] == [
        "opening", "lid_seat", "holds"]
    for result in (opening, seat, holds):
        assert result.data["not_checkable"] is True and result.data["met"] is None
        assert result.data["reason"] and result.data["reason"] in result.message
        assert result.message.startswith("Not checkable: ")
        assert result.status != "pass"
    assert opening.status == "skipped"
    assert "('inside diameter') has no place on the solid" in opening.data["reason"]
    # Resting on a guess it is a warning with the question, and still not checkable.
    assert seat.status == "warn" and "is not checkable" in seat.message
    assert seat.data["question"]["guessed_params"] == ["inner"]
    assert holds.status == "skipped"
    assert holds.data["reason"] == "Hotend has no measurement for a requirement of kind " \
        "`cavity_min`"
    assert not has_blocking_failure([opening, seat, holds])
