"""Model script loading: PARAMS validation, parameter merging, error reporting.

These tests need no CAD library: the scripts they run are plain Python.
"""

from __future__ import annotations

import textwrap

import pytest

from hotend_engine.errors import ModelError
from hotend_engine.model import (
    STDOUT_LIMIT,
    Params,
    StdoutCapture,
    load_model,
    merge_params,
    run_build,
    validate_param_specs,
)

GOOD = {"default": 10.0, "min": 1.0, "max": 20.0, "provenance": "style"}


def test_valid_specs_get_unit_and_label_defaults():
    specs = validate_param_specs({"width": GOOD, "count": {
        "default": 2, "min": 1, "max": 4, "unit": "count", "label": "How many",
        "provenance": "standard"}})
    assert specs["width"] == {**GOOD, "unit": "mm", "label": "width"}
    assert specs["count"]["label"] == "How many"


@pytest.mark.parametrize(
    ("params", "fragment"),
    [
        ([], "must be a dict"),
        ({"Width": GOOD}, "name 'Width' is not allowed"),
        ({"2nd": GOOD}, "name '2nd' is not allowed"),
        ({"a" * 33: GOOD}, "is not allowed"),
        ({"w": 5.0}, "'w': the spec must be a dict"),
        ({"w": {**GOOD, "step": 1}}, "'w': unknown key(s) 'step'"),
        ({"w": {"min": 1.0, "provenance": "style"}}, "'w': 'default' is required"),
        ({"w": {**GOOD, "default": "10"}}, "'w': 'default' must be a number"),
        ({"w": {**GOOD, "default": True}}, "'w': 'default' must be a number"),
        ({"w": {**GOOD, "max": float("inf")}}, "'w': 'max' must be a number"),
        ({"w": {**GOOD, "min": 30.0}}, "'w': min 30.0 is greater than max 20.0"),
        ({"w": {**GOOD, "default": 25.0}}, "'w': default 25.0 is outside its range"),
        ({"w": {"default": 1.0}}, "'w': 'provenance' must be one of"),
        ({"w": {**GOOD, "provenance": "invented"}}, "got 'invented'"),
        ({"w": {**GOOD, "unit": "inch"}}, "'w': 'unit' must be one of"),
        ({"w": {**GOOD, "provenance": "web"}}, "'w': a 'web' parameter needs 'source'"),
        ({"w": {**GOOD, "provenance": "web", "source": "https://evil.example"}},
         "the id of the research note"),
        ({"w": {**GOOD, "provenance": "web", "source": 1}}, "a 'web' parameter needs 'source'"),
        ({"w": {**GOOD, "source": "r1"}}, "'source' goes only with the provenance 'web'"),
        ({"w": {**GOOD, "unit": "count", "default": 2.5}}, "'w': a 'count' parameter needs whole"),
        ({"w": {**GOOD, "label": ""}}, "'w': 'label' must be text"),
        ({f"p{i}": GOOD for i in range(65)}, "the limit is 64"),
    ],
)
def test_invalid_specs_name_the_parameter_and_the_rule(params, fragment):
    with pytest.raises(ValueError, match="PARAMS|parameter") as caught:
        validate_param_specs(params)
    assert fragment in str(caught.value)


def test_a_number_from_the_web_names_its_research_note():
    specs = validate_param_specs({"w": {**GOOD, "provenance": "web", "source": "r12"}})
    assert specs["w"]["provenance"] == "web" and specs["w"]["source"] == "r12"


def test_merge_uses_defaults_and_request_values():
    specs = validate_param_specs({"width": GOOD, "n": {
        "default": 2, "min": 1, "max": 4, "unit": "count", "provenance": "style"}})
    values = merge_params(specs, {"width": 12})
    assert values == {"width": 12.0, "n": 2}
    assert isinstance(values["width"], float) and isinstance(values["n"], int)
    assert merge_params(specs, {"n": 3.0})["n"] == 3
    # the range ends are allowed
    assert merge_params(specs, {"width": 20.0})["width"] == 20.0
    assert merge_params(specs, {"width": 1})["width"] == 1.0


@pytest.mark.parametrize(
    ("given", "fragment"),
    [
        ({"width": 20.001}, "parameter 'width' = 20.001 is outside its allowed range [1.0, 20.0]"),
        ({"width": 0.5}, "parameter 'width' = 0.5 is outside"),
        ({"height": 3}, "unknown parameter 'height'; the model defines: width, n"),
        ({"width": "12"}, "parameter 'width' must be a number"),
        ({"width": None}, "parameter 'width' must be a number"),
        ({"width": float("nan")}, "parameter 'width' must be a number"),
        ({"n": 2.5}, "parameter 'n' is a count and must be whole"),
    ],
)
def test_merge_never_clamps(given, fragment):
    specs = validate_param_specs({"width": GOOD, "n": {
        "default": 2, "min": 1, "max": 4, "unit": "count", "provenance": "style"}})
    with pytest.raises(ValueError, match="parameter") as caught:
        merge_params(specs, given)
    assert fragment in str(caught.value)


def test_params_allow_item_and_attribute_access():
    p = Params(width=3.0)
    assert p["width"] == 3.0 and p.width == 3.0
    with pytest.raises(AttributeError, match="no parameter named 'depth'"):
        _ = p.depth


def _script(tmp_path, source: str):
    path = tmp_path / "model.py"
    path.write_text(source, encoding="utf-8")
    return path


HEADER = 'PARAMS = {"size": {"default": 2.0, "min": 1.0, "max": 5.0, "provenance": "style"}}\n'


def _run(tmp_path, source: str, given: dict | None = None):
    model = load_model(_script(tmp_path, source))
    capture = StdoutCapture()
    params = merge_params(model.param_specs, given or {})
    return run_build(model, params, ctx=None, capture=capture), capture


def test_runs_build_with_params_and_ctx(tmp_path):
    returned, capture = _run(tmp_path, HEADER + textwrap.dedent("""
        def build(p, ctx):
            print("size is", p.size)
            return {"double": p["size"] * 2, "ctx": ctx}
        """), {"size": 4})
    assert returned == {"double": 8.0, "ctx": None}
    assert capture.text() == ("size is 4.0\n", False)


def test_module_level_prints_are_captured_too(tmp_path, capsys):
    _, capture = _run(tmp_path, 'print("loading")\n' + HEADER + "def build(p, ctx):\n    return 1\n")
    assert capture.text()[0] == "loading\n"
    assert capsys.readouterr().out == ""


def test_stdout_is_capped_at_16_kb(tmp_path):
    _, capture = _run(tmp_path, HEADER + textwrap.dedent("""
        def build(p, ctx):
            for i in range(4000):
                print("line", i, "x" * 20)
        """))
    text, truncated = capture.text()
    assert truncated is True
    assert len(text.encode()) <= STDOUT_LIMIT == 16_000
    assert text.startswith("line 0 ")


@pytest.mark.parametrize(
    ("source", "type_", "fragment", "line"),
    [
        ("def build(p, ctx):\n    return 1\n", "ParamsError", "must define PARAMS", None),
        ("PARAMS = dict(a=1)\ndef build(p, ctx):\n    return 1\n", "ParamsError",
         "must be a literal dict", 1),
        (("W = 3\nPARAMS = {'w': {'default': W, 'provenance': 'style'}}\n"
          "def build(p, ctx):\n    return 1\n"), "ParamsError", "must be a literal dict", 2),
        ("PARAMS = {}\nPARAMS = {}\ndef build(p, ctx):\n    return 1\n", "ParamsError",
         "assigned more than once", 2),
        ("PARAMS = {}\n", "BuildMissing", "must define a top-level function build", None),
        ("PARAMS = {}\ndef build(p):\n    return 1\n", "BuildSignature",
         "exactly two arguments", 2),
        ("PARAMS = {}\ndef build(p, ctx):\n  return (\n", "SyntaxError", "never closed", 3),
    ],
)
def test_broken_rules_are_model_errors(tmp_path, source, type_, fragment, line):
    with pytest.raises(ModelError) as caught:
        model = load_model(_script(tmp_path, source))
        run_build(model, Params(), None, StdoutCapture())
    error = caught.value
    assert error.type == type_
    assert fragment in error.message
    assert error.line == line
    assert error.to_json()["traceback"].rstrip().endswith(f"{type_}: {error.message}")


def test_script_that_would_break_rules_is_never_executed(tmp_path):
    marker = tmp_path / "ran.txt"
    source = (f"open({str(marker)!r}, 'w').close()\n"
              "PARAMS = {'w': {'default': 1.0, 'provenance': 'nonsense'}}\n"
              "def build(p, ctx):\n    return 1\n")
    with pytest.raises(ModelError):
        load_model(_script(tmp_path, source))
    assert not marker.exists()


def test_exception_traceback_is_trimmed_to_the_script(tmp_path):
    source = HEADER + textwrap.dedent("""
        import json

        def helper(text):
            return json.loads(text)

        def build(p, ctx):
            print("before")
            return helper("{broken")
        """)
    model = load_model(_script(tmp_path, source))
    capture = StdoutCapture()
    with pytest.raises(ModelError) as caught:
        run_build(model, Params(size=2.0), None, capture)
    error = caught.value
    assert error.type == "JSONDecodeError"
    assert error.line == 6
    lines = error.traceback.splitlines()
    assert lines[0] == "Traceback (model script frames only):"
    assert lines[1] == '  File "model.py", line 10, in build'
    assert lines[2] == '    return helper("{broken")'
    assert lines[3] == '  File "model.py", line 6, in helper'
    assert lines[4] == "    return json.loads(text)"
    assert lines[5].startswith("json.decoder.JSONDecodeError: ")
    assert "json/decoder.py" not in error.traceback and "hotend_engine" not in error.traceback
    assert capture.text()[0] == "before\n"


def test_error_at_import_time_has_the_module_line(tmp_path):
    source = HEADER + "import no_such_module_anywhere\ndef build(p, ctx):\n    return 1\n"
    with pytest.raises(ModelError) as caught:
        run_build(load_model(_script(tmp_path, source)), Params(), None, StdoutCapture())
    assert caught.value.type == "ModuleNotFoundError"
    assert caught.value.line == 2
    assert "in <module>" in caught.value.traceback


def test_sys_exit_in_a_script_is_a_model_error(tmp_path):
    source = HEADER + "import sys\ndef build(p, ctx):\n    sys.exit(3)\n"
    with pytest.raises(ModelError) as caught:
        run_build(load_model(_script(tmp_path, source)), Params(), None, StdoutCapture())
    assert caught.value.type == "SystemExit"
    assert caught.value.line == 4


def test_missing_file_and_bad_encoding(tmp_path):
    with pytest.raises(ModelError) as caught:
        load_model(tmp_path / "absent.py")
    assert caught.value.type == "ModelNotFound"
    bad = tmp_path / "model.py"
    bad.write_bytes(b"PARAMS = {}\n\xff\xfe\n")
    with pytest.raises(ModelError) as caught:
        load_model(bad)
    assert caught.value.type == "SyntaxError"
