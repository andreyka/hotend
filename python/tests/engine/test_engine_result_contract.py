"""The worker writes no field the core refuses (`tests/result_contract.py`).

Two nets. Every worker run of the tests is checked as it happens (the helpers call
`check_job_result`); here, in addition, the engine's source is read for the keys it writes
into each object of a result, so that a field no test happens to produce is noticed too."""

from __future__ import annotations

import ast
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "python" / "tests"))

import result_contract
from result_contract import check_result, contract, fields, violations

import hotend_engine.model as engine_model
import hotend_engine.request as engine_request
from hotend_engine import worker
from hotend_fit import contract as fit_contract

ENGINE, FIT = REPO / "python" / "hotend_engine", REPO / "python" / "hotend_fit"


def _tree(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"))


def _functions(path: Path, name: str) -> list[ast.FunctionDef]:
    """The functions of that name, methods included (`Class.method` names one of them)."""
    owner, _, plain = name.rpartition(".")
    scopes = [node for node in ast.walk(_tree(path))
              if isinstance(node, ast.ClassDef) and node.name == owner] if owner else [_tree(path)]
    found = [node for scope in scopes for node in ast.walk(scope)
             if isinstance(node, ast.FunctionDef) and node.name == plain]
    assert found, f"{path.name} has no function {name}: update this test"
    return found


# A dict merged into an object of a result with `**`, whose own keys are read elsewhere
# (the place is named). Anything else spread into one fails: its keys would not be seen.
SPREAD: dict[str, str] = {}
# A value that is no display and is bound to the variable of an object, by its exact text,
# and why its keys are known. A call of `_base_result` is known by its node, not here.
MADE_ELSEWHERE = {
    "_OPS[op](request, job_dir, timings)": "the run_<op> functions, all scanned for `result`",
    "choice.to_json()": "orientation: returned(Choice.to_json)",
    "component.report()": "component: returned(Component.report)",
    "reference.report()": "an entry of `references`, opaque JSON to the core",
    "entry['dimensions']": "gauge_dimension: the cards' gauge_dimensions and Pocket.gauge_set",
    "zip(pieces, owners, strict=True)": "gauge: the displays appended to `owners` before",
}
# A call an object is handed to, by the name called, and why it writes nothing unseen.
HANDED_TO = {
    "_write_plates": "scanned for `result` and `plate`",
    "_write_result": "writes the file",
    "dumps": "the JSON text",
}
# A statement that uses an object in another way, by its function and exact text, and why its
# keys are known all the same.
KNOWN_USES: dict[tuple[str, str], str] = {
    ("run_build", "result['artifacts'] = artifacts"): "artifacts: scanned under its own name",
    ("_references_report", "references.append(entry)"): "opaque JSON to the core",
    ("_references_report", "components.append(entry)"): "component: `entry` is scanned",
    ("_write_plates", "plates.append(plate)"): "plate: `plate` is scanned",
    ("run_prepare", "result['orientations'].append(entry)"): "orientation: `entry` is scanned",
    ("run_gauge", "result['gauges'].append(entry)"): "gauge: `entry` is scanned",
    ("run_gauge", "owners.append((entry, name))"): "read back as MADE_ELSEWHERE says",
    ("run_gauge", "entry['parts'].append(name)"): "a name",
    ("run_gauge", "dimension['what'].endswith"): "a text",
    ("contract_questions",
     ("questions = [question.model_dump(mode='json') for question in "
      "open_questions(env.contract, env.param_specs)]")): "question: fit_contract.Question's fields",
}
# A value that is no display and is returned, appended or put under a key where a display is
# looked for: the function, the exact text, and why.
OTHER_VALUES: dict[tuple[str, str], str] = {
    ("export_formats", "artifacts['stl'][part.name]"): "a map by part name, not an object",
    ("_write_plates", "plate['views'][view]"): "a map by view name, not an object",
}


def _keys(node: ast.AST) -> set[str]:
    """The keys of a dict display. A `**spread` is followed only where `SPREAD` says how."""
    assert isinstance(node, ast.Dict), f"no dict display: {ast.unparse(node)}"
    for key, value in zip(node.keys, node.values, strict=True):
        assert key is not None or ast.unparse(value) in SPREAD, (
            f"line {node.lineno}: what does `**{ast.unparse(value)}` write into a result? Write "
            "the keys out, or name the place that has them in SPREAD")
    keys = [key for key in node.keys if key is not None]
    assert all(isinstance(key, ast.Constant) for key in keys), "a computed key"
    return {key.value for key in keys}


def _names(node: ast.AST, name: str) -> bool:
    return isinstance(node, ast.Name) and node.id == name


def _parents(function: ast.AST) -> dict[ast.AST, ast.AST]:
    return {child: node for node in ast.walk(function) for child in ast.iter_child_nodes(node)}


def _own(function: ast.FunctionDef) -> list[ast.AST]:
    """The nodes of a function without those of the functions inside it."""
    found, left = [], list(ast.iter_child_nodes(function))
    while left:
        node = left.pop()
        found.append(node)
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            left.extend(ast.iter_child_nodes(node))
    return found


def _unread(function: ast.FunctionDef, node: ast.AST, variable: str) -> str:
    line = getattr(node, "lineno", "?")
    return (f"{function.name}, line {line}: `{ast.unparse(node)[:120]}` uses `{variable}` in a "
            "way whose keys this test cannot read; write them out, or say here why they are known")


def _bound(function: ast.FunctionDef, node: ast.AST, variable: str,
           parent: dict[ast.AST, ast.AST]) -> set[str]:
    """The keys of what the name node `node` is bound to; fails when they are not known."""
    at = parent[node]
    if isinstance(at, (ast.Assign, ast.AnnAssign)):
        value = at.value
        if value is None:
            return set()
        if isinstance(value, ast.Dict):
            return _keys(value)
        if isinstance(value, ast.Call) and _names(value.func, "_base_result"):
            return set()
        assert ast.unparse(value) in MADE_ELSEWHERE, _unread(function, at, variable)
        return set()
    while isinstance(at, (ast.Tuple, ast.List, ast.Starred)):
        at = parent[at]
    # A loop (also in a comprehension), by what it runs over; anything else fails: `|=`, a
    # tuple target, `with ... as`, `:=`, `del`, an import.
    assert isinstance(at, (ast.For, ast.comprehension)) and ast.unparse(
        at.iter) in MADE_ELSEWHERE, _unread(function, at, variable)
    return set()


def _is_text(node: ast.AST) -> bool:
    """A text written out, also one put together with `+`."""
    return isinstance(node, ast.JoinedStr) or (
        isinstance(node, ast.Constant) and isinstance(node.value, str)) or (
            isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add) and _is_text(node.left))


# Methods of a dict that write nothing into it.
READS = {"get", "items", "keys", "values", "copy"}


def _used(function: ast.FunctionDef, node: ast.AST, variable: str,
          parent: dict[ast.AST, ast.AST]) -> set[str]:
    """The keys a use of the name node `node` writes. Every use that is not known fails."""
    at = parent[node]
    if isinstance(at, ast.Subscript) and at.value is node:
        above = parent[at]
        if isinstance(at.ctx, ast.Store):
            assert isinstance(at.slice, ast.Constant), _unread(function, at, variable)
            return {at.slice.value}
        # What is inside is read, or a list in it gets a display or a known object. Anything
        # else could write into it: `v["k"]["x"] = 1`, `v["k"].update(...)`, `w = v["k"]`.
        if isinstance(above, ast.Attribute) and isinstance(parent[above], ast.Call):
            call = parent[above]
            inside = above.attr in READS or (
                above.attr == "append" and len(call.args) == 1 and (
                    _is_text(call.args[0])
                    or (function.name, ast.unparse(call)) in KNOWN_USES))
        elif isinstance(above, ast.Subscript) and above.value is at:
            inside = (isinstance(above.ctx, ast.Load) and not isinstance(
                parent[above], (ast.Assign, ast.Subscript, ast.Attribute, ast.Call))) or (
                    function.name, ast.unparse(above)) in OTHER_VALUES
        elif isinstance(above, ast.Call) and above.func is not at:
            inside = isinstance(above.func, ast.Name) and above.func.id in {"len", "bool"}
        else:
            inside = isinstance(above, (ast.Compare, ast.If, ast.While, ast.BoolOp, ast.UnaryOp))
        loop = (isinstance(above, (ast.For, ast.comprehension)) and above.iter is at
                and ast.unparse(at) in MADE_ELSEWHERE)
        assert inside or loop or (function.name, ast.unparse(above)) in KNOWN_USES, _unread(
            function, above, variable)
        return set()
    if isinstance(at, ast.Attribute):
        call = parent[at]
        assert isinstance(call, ast.Call) and call.func is at, _unread(function, at, variable)
        if at.attr == "update":
            assert not call.keywords and len(call.args) == 1, _unread(function, call, variable)
            if isinstance(call.args[0], ast.Name) and call.args[0].id in FOLLOWED:
                return set()
            return _keys(call.args[0])
        if at.attr == "setdefault":
            assert call.args and isinstance(call.args[0], ast.Constant), _unread(
                function, call, variable)
            return {call.args[0].value}
        assert at.attr in READS, _unread(function, call, variable)
        return set()
    if isinstance(at, (ast.Return, ast.Compare, ast.If, ast.While)):
        return set()
    if (function.name, ast.unparse(at)) in KNOWN_USES:
        return set()
    # Handed on: straight, or in a tuple, to a call that is known.
    call = parent[at] if isinstance(at, ast.Tuple) else at
    if isinstance(call, ast.expr) and (function.name, ast.unparse(call)) in KNOWN_USES:
        return set()
    if isinstance(at, ast.Tuple) and isinstance(call, ast.Return):
        return set()
    assert isinstance(call, ast.Call) and at is not call.func, _unread(function, at, variable)
    name = call.func.id if isinstance(call.func, ast.Name) else getattr(call.func, "attr", "")
    assert name in HANDED_TO, _unread(function, call, variable)
    return set()


# A variable merged into another object, whose own keys are read where it is made.
FOLLOWED = {"exported": "artifacts, hotend_engine.export.export_formats"}


def written(function: ast.FunctionDef, variable: str) -> set[str]:
    """The keys a function writes into the dict `variable`: by `v = {...}`, `v["k"] = ...`,
    `v["k"] += ...`, `v.update({...})` and `v.setdefault("k", ...)`. Every other use of the
    name fails unless it only reads or a list above says why its keys are known.

    What stays unseen: a write into an object that a list above lets be handed on (by a
    function of `HANDED_TO`, through a loop of `MADE_ELSEWHERE` or a statement of
    `KNOWN_USES`), into the value another variable holds before it is put under a key
    (`result["x"] = other` is read as the key `x`, not into `other`), or in another module than
    the ones read here. The runs' own check is for those."""
    parent = _parents(function)
    # The function itself and its own parameters may have the name.
    own = [function, *function.args.posonlyargs, *function.args.args, *function.args.kwonlyargs]
    found: set[str] = set()
    for node in ast.walk(function):
        if _names(node, variable):
            found |= (_used if isinstance(node.ctx, ast.Load) else _bound)(
                function, node, variable, parent)
        # Every other way to bind the name is a text in the tree: `global`, an import, `except
        # ... as`, a match pattern, a function or class inside, a parameter of one.
        if node in own:
            continue
        for name, value in ast.iter_fields(node):
            texts = value if isinstance(value, list) else [value]
            assert variable not in [text for text in texts if isinstance(text, str)] or (
                isinstance(node, (ast.Name, ast.Attribute, ast.keyword, ast.Constant))), _unread(
                    function, node, variable)
    return found


def _display(function: ast.FunctionDef, value: ast.AST) -> set[str]:
    """The keys of a display, or none for a value `OTHER_VALUES` knows; anything else fails."""
    if isinstance(value, ast.Dict):
        return _keys(value)
    assert (function.name, ast.unparse(value)) in OTHER_VALUES, (
        f"{function.name}, line {value.lineno}: `{ast.unparse(value)[:120]}` is no dict display, "
        "so its keys are not seen; write them out, or name it in OTHER_VALUES")
    return set()


def returned(function: ast.FunctionDef) -> set[str]:
    """The keys of the dict a function returns as a display. Another value returned fails."""
    found = [node.value for node in _own(function)
             if isinstance(node, ast.Return) and node.value is not None]
    assert any(isinstance(value, ast.Dict) for value in found), (
        f"{function.name} returns no dict display: update this test")
    return set().union(*(_display(function, value) for value in found))


def appended(function: ast.FunctionDef, variable: str) -> set[str]:
    """The keys of the dicts a function appends to the list `variable`. Any other way to fill
    it, or to hand it on before it is returned, fails."""
    parent = _parents(function)
    found: list[set[str]] = []
    for node in ast.walk(function):
        if not _names(node, variable):
            continue
        at = parent[node]
        if (function.name, ast.unparse(at)) in KNOWN_USES:
            continue
        if isinstance(node.ctx, ast.Store):
            # `v = []`, also as `v, w = [], set()`.
            made = at.value if isinstance(at, (ast.Assign, ast.AnnAssign)) else None
            if isinstance(at, ast.Tuple) and isinstance(parent[at], ast.Assign) and isinstance(
                    parent[at].value, ast.Tuple) and len(parent[at].value.elts) == len(at.elts):
                made = parent[at].value.elts[at.elts.index(node)]
            assert isinstance(made, ast.List) and not made.elts, _unread(function, at, variable)
            continue
        if isinstance(at, (ast.Return, ast.If, ast.Compare, ast.comprehension)):
            continue
        call = parent[at]
        if isinstance(at, ast.Tuple) and isinstance(call, ast.Return):
            continue
        assert (isinstance(at, ast.Attribute) and at.attr == "append" and isinstance(
            call, ast.Call) and call.func is at and len(call.args) == 1), _unread(
                function, at, variable)
        found.append(_display(function, call.args[0]))
    assert found, f"{function.name} appends no dict to `{variable}`: update this test"
    return set().union(*found)


def dicts_with(function: ast.FunctionDef, key: str) -> set[str]:
    """The keys of every dict display in a function that has the key `key`."""
    found = [_keys(node) for node in ast.walk(function) if isinstance(node, ast.Dict)]
    return set().union(*(keys for keys in found if key in keys))


def subscripted(function: ast.FunctionDef, variable: str, key: str) -> set[str]:
    """The keys of the dict displays assigned to `variable[key]`. Another value put there
    fails."""
    found = [node.value for node in ast.walk(function)
             if isinstance(node, (ast.Assign, ast.AugAssign))
             and any(isinstance(t, ast.Subscript) and _names(t.value, variable)
                     and getattr(t.slice, "value", None) == key
                     for t in (node.targets if isinstance(node, ast.Assign) else [node.target]))]
    assert any(isinstance(value, ast.Dict) for value in found), (
        f"{function.name} writes no dict to {variable}[{key!r}]")
    return set().union(*(_display(function, value) for value in found))


def _worker(name: str) -> ast.FunctionDef:
    return _functions(ENGINE / "worker.py", name)[0]


def worker_result_keys() -> set[str]:
    """Every key any op the core requests writes into the result itself."""
    never = set(contract()["ops_the_core_never_requests"])
    keys: set[str] = set()
    for node in ast.walk(_tree(ENGINE / "worker.py")):
        if isinstance(node, ast.FunctionDef) and node.name.removeprefix("run_") not in never:
            keys |= written(node, "result")
    return keys | returned(_worker("_base_result"))


def scanned() -> dict[str, set[str]]:
    """Object of the list -> the keys the engine's source writes into it."""
    def one(path: Path, name: str) -> ast.FunctionDef:
        (function,) = _functions(path, name)
        return function

    prepare, printfacts = ENGINE / "prepare.py", ENGINE / "printfacts.py"
    components = FIT / "components" / "__init__.py"
    contract_check, reference_check = (ENGINE / "checks" / f"{name}.py"
                                       for name in ("contract", "reference_fit"))
    choice, placed = (one(prepare, f"{owner}.to_json") for owner in ("Choice", "Placed"))
    gauge_dimensions = set()
    for path in sorted((FIT / "cards").glob("*.py")):
        for node in ast.walk(_tree(path)):
            # The base class builds none: a card without a gauge has no dimensions.
            if isinstance(node, ast.FunctionDef) and node.name == "gauge_dimensions":
                gauge_dimensions |= dicts_with(node, "drawn")
    gauge_dimensions |= dicts_with(one(FIT / "reference.py", "Pocket.gauge_set"), "drawn")
    gauge = _worker("run_gauge")
    boxes = set().union(*(returned(f) for f in _functions(ENGINE / "parts.py", "bbox_json")))
    return {
        "result": worker_result_keys(),
        "error": (returned(one(ENGINE / "errors.py", "to_json"))
                  | subscripted(_worker("run_build"), "result", "error")
                  | subscripted(_worker("run_sweep"), "result", "error")),
        "param_spec": set(engine_model.SPEC_KEYS),
        "bbox": boxes,
        "part": returned(one(ENGINE / "parts.py", "summarize")),
        "check": returned(one(ENGINE / "checks" / "base.py", "CheckResult.to_json")),
        "artifacts": (written(_worker("run_build"), "artifacts")
                      | subscripted(_worker("run_views"), "result", "artifacts")
                      | written(one(ENGINE / "export.py", "export_formats"), "artifacts")),
        "doc": appended(one(ENGINE / "docs.py", "search"), "results"),
        "question": (appended(one(contract_check, "contract_questions"), "questions")
                     | appended(one(reference_check, "reference_questions"), "questions")
                     | appended(one(components, "Component.questions"), "asked")
                     | set(fit_contract.Question.model_fields)),
        # The other `entry` of the function describes a reference file (opaque to the core).
        "component": (returned(one(components, "Component.report"))
                      | (written(_worker("_references_report"), "entry") - {"pockets", "used"})),
        "orientation": returned(choice) | written(_worker("run_prepare"), "entry"),
        "plate": written(_worker("_write_plates"), "plate"),
        "layout": subscripted(_worker("_write_plates"), "plate", "layout"),
        "plate_item": returned(placed),
        "unplaced": appended(one(prepare, "_shelf"), "unplaced"),
        "gauge": written(gauge, "entry"),
        "gauge_dimension": gauge_dimensions | written(gauge, "dimension"),
        "gauge_clearance": dicts_with(gauge, "fit_class"),
        "change": appended(one(FIT / "calibration.py", "calibrate_from_gauge"), "changes"),
        "sweep": set(engine_request.SweepReport.model_fields),
        "sweep_sample": set(engine_request.SweepSample.model_fields),
        "sweep_failure": set(engine_request.SweepFailure.model_fields),
        "sweep_requirement": set(engine_request.SweepRequirement.model_fields),
        "plate_facts": returned(one(printfacts, "plate_facts")),
        "piece_facts": returned(one(printfacts, "piece_facts")),
    }


# Objects built in many small steps: held to the list by the runs alone (every print settings
# test of the engine passes them through `check_job_result`).
RUN_ONLY = {"overhang_region", "inner_loop", "fit_at_bed"}


@pytest.mark.parametrize("name", sorted(contract()["objects"]))
def test_the_source_writes_exactly_the_fields_the_core_accepts(name):
    if name in RUN_ONLY:
        return
    listed = {key for key, spec in fields(name).items() if spec != "core_only"}
    keys = scanned()[name]
    assert not keys - listed, (
        f"the engine writes {sorted(keys - listed)} into `{name}` and the core refuses it: see "
        f"{result_contract.LIST.name}")
    assert not listed - keys, (
        f"the list has {sorted(listed - keys)} for `{name}`, which the source no longer writes "
        "where this test looks: take it out of the list and of the Rust type, or update the test")


def test_the_worker_has_the_listed_ops_and_no_other():
    assert set(worker._OPS) == set(contract()["ops"]) | set(
        contract()["ops_the_core_never_requests"])


def test_the_enumerations_agree():
    def allowed(name: str, field: str) -> list[str]:
        return fields(name)[field].split("|")[0].removesuffix("?").removeprefix(
            "one_of:").split(",")

    assert set(allowed("param_spec", "provenance")) == set(engine_model.PROVENANCES)
    assert set(allowed("param_spec", "unit")) == set(engine_model.UNITS)
    assert set(allowed("component", "provenance")) == set(fit_contract.PROVENANCES)
    assert "|".join(allowed("result", "stage")) == "all|orient|plate"


def test_the_question_about_a_reference_file_is_within_the_core_s_bound():
    # The longest name a request accepts gives the longest id a question can have.
    longest = f"reference_{'a' * 32}"
    question = {"requirement": longest, "text": "t", "guessed_params": []}
    assert violations([question], "[@question]", "q") == []


def test_every_test_that_runs_the_worker_checks_its_results():
    """A module that starts `python -m hotend_engine.worker` checks what comes back."""
    unchecked, seen = [], 0
    for root in (REPO / "python" / "tests", REPO / "scripts" / "tests"):
        for path in root.rglob("*.py"):
            if "hotend_engine.worker" not in path.read_text(encoding="utf-8"):
                continue
            constants = {node.value for node in ast.walk(_tree(path))
                         if isinstance(node, ast.Constant)}
            if "hotend_engine.worker" in constants and path != Path(__file__).resolve():
                seen += 1
                calls = {node.func.id for node in ast.walk(_tree(path))
                         if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)}
                if "check_job_result" not in calls:
                    unchecked.append(str(path.relative_to(REPO)))
    assert unchecked == [] and seen >= 4


# Ways to get a key into an object that the scan cannot read: each is put into the source of
# the real worker, where the scan of `run_build` passes without it.
ESCAPES = [
    "result |= extra()",
    "result = dict(protocol=0)",
    "result = extra()",
    "result, other = extra()",
    "(result, other), third = extra()",
    "[result, *other] = extra()",
    "result = _base_result('build') | {'surprise': 1}",
    "result = {**_base_result('build'), 'surprise': 1}",
    "result = other = _base_result('build') | {'surprise': 1}",
    "result = module._base_result('build')",
    "result = {'protocol': 0, **extra()}",
    "result: dict = extra()",
    "result.update(extra())",
    "result.update(surprise=1)",
    "result.update({'protocol': 0, **extra()})",
    "result.update([('surprise', 1)])",
    "result.setdefault(key, 1)",
    "result[key] = 1",
    "result['artifacts']['surprise'] = 1",
    "result.__setitem__('surprise', 1)",
    "result.__ior__(extra())",
    "put = result.update",
    "alias = result",
    "alias = result or {}",
    "alias: dict = result",
    "alias, other = result, 1",
    "box = [result]",
    "box = {'inside': result}",
    "box.inside = result",
    "fill(result)",
    "fill(target=result)",
    "fill(*[result])",
    "dict.update(result, surprise=1)",
    "setattr(box, 'inside', result)",
    "box.extend([result])",
    "result = fill(result)",
    "(lambda: result)()",
    "for result in extra(): pass",
    "[None for result in extra()]",
    "with extra() as result: pass",
    "(result := extra())",
    "del result",
    "global result",
    "result['artifacts'].update({'surprise': 1})",
    "result['artifacts'].setdefault('surprise', 1)",
    "result['artifacts'].__setitem__('surprise', 1)",
    "inside = result['artifacts']",
    "inside, other = result['artifacts'], 1",
    "fill(result['artifacts'])",
    "box.append(result['artifacts'])",
    "inside = result['parts'][0]",
    "result['parts'][0].update({'surprise': 1})",
    "for part in result['parts']: part['surprise'] = 1",
    "result['parts'].append(alias)",
    "result['parts'].extend([alias])",
    "box.append(result)",
    "box.append((result, 1))",
    "import x as result",
    "import result",
    "from x import result",
    "from x import y as result",
    "def result(): pass",
    "class result: pass",
    "def fill(result): result['surprise'] = 1",
    "fill = lambda result: result.update({'surprise': 1})",
    "def fill(*result): pass",
]
# The same, for ways that need a block.
ESCAPES += [f"if True:\n        {block}" for block in (
    "try: pass\n        except ValueError as result: pass",
    "match box:\n            case result: pass",
    "match box:\n            case [*result]: pass",
    "match box:\n            case {'a': 1, **result}: pass",
)]


def _mutant(line: str) -> ast.FunctionDef:
    """`run_build` of the real worker with `line` after its result was made."""
    source = (ENGINE / "worker.py").read_text(encoding="utf-8")
    made = '    result = _base_result("build")\n'
    assert source.count(made) == 1
    tree = ast.parse(source.replace(made, f"{made}    {line}\n"))
    (function,) = [node for node in ast.walk(tree)
                   if isinstance(node, ast.FunctionDef) and node.name == "run_build"]
    return function


@pytest.mark.parametrize("line", ESCAPES)
def test_the_scan_fails_on_a_write_whose_keys_it_cannot_read(line):
    assert written(_mutant("pass"), "result")
    with pytest.raises(AssertionError):
        written(_mutant(line), "result")


@pytest.mark.parametrize("line", [
    "result['surprise'] = 1",
    "result['surprise'] += 1",
    "result['surprise'], other = 1, 2",
    "result.update({'surprise': 1})",
    "result.setdefault('surprise', 1)",
    "result = {'surprise': 1}",
    "for result['surprise'] in extra(): pass",
])
def test_a_new_key_in_the_worker_is_found(line):
    keys = written(_mutant(line), "result")
    assert "surprise" in keys and "surprise" not in fields("result")


@pytest.mark.parametrize("body", [
    "return extra()",
    "return {'a': 1} | extra()",
    "return dict(a=1)",
    "entry = {'a': 1}\n    return entry",
    "return {'a': 1, **extra()}",
    "return [{'a': 1}]",
])
def test_a_return_that_is_no_display_fails(body):
    function = ast.parse(f"def f():\n    if x:\n        return {{'a': 1}}\n    {body}\n").body[0]
    with pytest.raises(AssertionError):
        returned(function)


@pytest.mark.parametrize("body", [
    "rows.append(extra())",
    "rows.append({'a': 1} | extra())",
    "rows.extend([{'b': 2}])",
    "rows.insert(0, {'b': 2})",
    "rows += [{'b': 2}]",
    "rows = [{'b': 2}]",
    "rows[0] = {'b': 2}",
    "rows[0]['b'] = 2",
    "alias = rows",
    "fill(rows)",
    "put = rows.append",
])
def test_a_list_filled_in_another_way_fails(body):
    function = ast.parse(
        f"def f():\n    rows = []\n    rows.append({{'a': 1}})\n    {body}\n    return rows\n"
    ).body[0]
    with pytest.raises(AssertionError):
        appended(function, "rows")


@pytest.mark.parametrize("body", ["v['k'] = extra()", "v['k'] |= extra()", "v['k'] = {**extra()}"])
def test_another_value_under_a_key_fails(body):
    function = ast.parse(f"def f():\n    v['k'] = {{'a': 1}}\n    {body}\n").body[0]
    with pytest.raises(AssertionError):
        subscripted(function, "v", "k")


def test_the_scan_reads_the_writes_it_knows():
    function = ast.parse(
        "def run_build():\n    result = {'a': 1}\n    result['b'] = 2\n    result['c'] += 3\n"
        "    result.update({'d': 4})\n    result.setdefault('e', 5)\n    other = {**more()}\n"
    ).body[0]
    assert written(function, "result") == {"a", "b", "c", "d", "e"}


def test_what_the_core_limits_is_limited_here_too():
    base = {"protocol": 0, "op": "build", "status": "ok"}
    maxima = contract()["max_items"]
    assert maxima["result.warnings"] == 64
    check_result({**base, "warnings": ["w"] * 64})
    with pytest.raises(AssertionError, match="65 entries, the core takes 64"):
        check_result({**base, "warnings": ["w"] * 65})
    question = {"requirement": "a", "text": "t", "guessed_params": []}
    with pytest.raises(AssertionError, match="513 entries"):
        check_result({**base, "questions": [question] * 513})
    with pytest.raises(AssertionError, match="65 entries"):
        check_result({**base, "params": {f"p{n}": 1.0 for n in range(65)}})
    table = {"pad": "x" * (contract()["max_json_bytes"]["result.table"] - len('{"pad":""}'))}
    check_result({**base, "op": "calibrate", "table": table})
    with pytest.raises(AssertionError, match="bytes of JSON"):
        check_result({**base, "op": "calibrate", "table": {"pad": table["pad"] + "x"}})
    # Every path of the limits is a field of the list.
    for path in (*maxima, *contract()["max_json_bytes"], *contract()["other_bounds"]):
        name, field = path.split(".")
        assert field in fields(name), path


def test_a_result_file_larger_than_the_core_reads_fails(tmp_path):
    good = '{"protocol": 0, "op": "build", "status": "ok"}'
    (tmp_path / "result.json").write_text(good, encoding="utf-8")
    result_contract.check_job_result(tmp_path)
    padded = good + " " * (contract()["max_result_bytes"] - len(good) + 1)
    (tmp_path / "result.json").write_text(padded, encoding="utf-8")
    with pytest.raises(AssertionError, match="the core takes 4194304"):
        result_contract.check_job_result(tmp_path)


def test_the_check_refuses_what_the_core_refuses():
    good = {"protocol": 0, "op": "build", "status": "ok", "error": None, "timings": {}}
    check_result(good)
    for change in (
        {"surprise": 1},
        {"op": "mystery"},
        {"status": "great"},
        {"components": [{"id": "sample_phone"}]},
        {"params": {"Width": 1.0}},
        {"params": {"width": True}},
        {"questions": [{"requirement": "web:r1", "text": "t", "guessed_params": []}]},
        {"questions": [{"requirement": "a", "text": "t", "guessed_params": [], "web": None}]},
        {"artifacts": {"step": "model.step", "exe": "x"}},
        {"checks": [{"id": "min_wall", "status": "pass", "severity": "error", "message": "m",
                     "part": "../x", "data": {}}]},
    ):
        with pytest.raises(AssertionError, match="the core would refuse"):
            check_result({**good, **change})
