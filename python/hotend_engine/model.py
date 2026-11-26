"""Loading and running a model script (docs/PROTOCOL.md section 2).

Guarantees:
- `PARAMS` is read from the source as a literal and validated before any of the script runs,
  so a script with bad parameters never executes.
- Request values are merged over the defaults and nothing is clamped: an unknown, non-numeric
  or out-of-range value raises ModelError naming the parameter.
- Every failure caused by the script is a ModelError with the line in the script and a
  traceback that contains only the script's own frames.
- What the script prints is captured and returned, never written to the worker's stdout.
- `CONTRACT`, `ASSEMBLY` and `PRINT`, when a script has them, are read the same way as
  `PARAMS` (literals, never executed) and validated strictly against `hotend_fit.contract`
  before any of the script runs; a declaration that does not validate is a ModelError that
  names the bad field. `check_declared_parts` then holds them against the parts `build`
  returned.
"""

from __future__ import annotations

import ast
import contextlib
import inspect
import io
import math
import re
import traceback
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .context import BuildContext
from .errors import ModelError

NAME_RE = re.compile(r"[a-z][a-z0-9_]{0,31}\Z")
# `web`: the number was found on the web and recorded as a research note of the project;
# the spec names the note in `source`. Hotend shows such a number with its page and asks
# the maker to confirm it (docs/WEB_RESEARCH.md).
PROVENANCES = ("measured", "standard", "calibrated", "guessed", "style", "web")
UNITS = ("mm", "deg", "count", "ratio")
SPEC_KEYS = ("default", "min", "max", "unit", "label", "provenance", "source")
NOTE_ID_RE = re.compile(r"r[0-9]{1,6}\Z")
MAX_PARAMS = 64
MAX_PARTS = 64
MAX_SOURCE_BYTES = 512 * 1024
STDOUT_LIMIT = 16_000


class Params(dict):
    """Parameter values by name; `p["width"]` and `p.width` both work."""

    def __getattr__(self, name: str) -> Any:
        try:
            return self[name]
        except KeyError:
            raise AttributeError(
                f"no parameter named {name!r}; PARAMS defines: {', '.join(self) or 'nothing'}"
            ) from None


@dataclass
class LoadedModel:
    path: Path
    filename: str
    source: str
    code: Any
    param_specs: dict[str, dict]
    params_line: int | None
    build_line: int | None
    # Validated declarations (hotend_fit.contract objects), None when the script has none.
    contract: Any = None
    assembly: Any = None
    print_hints: Any = None
    declaration_lines: dict[str, int] = field(default_factory=dict)


class StdoutCapture:
    """Collects the script's prints, keeping at most STDOUT_LIMIT bytes."""

    def __init__(self) -> None:
        self._buffer = io.StringIO()

    @contextlib.contextmanager
    def capturing(self):
        with contextlib.redirect_stdout(self._buffer):
            yield

    def text(self) -> tuple[str, bool]:
        raw = self._buffer.getvalue().encode("utf-8", errors="replace")
        if len(raw) <= STDOUT_LIMIT:
            return raw.decode("utf-8", errors="replace"), False
        return raw[:STDOUT_LIMIT].decode("utf-8", errors="ignore"), True


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _source_line(source: str, line: int | None) -> str:
    lines = source.splitlines()
    if line is None or not 1 <= line <= len(lines):
        return ""
    return lines[line - 1].strip()


def _rule_error(model: LoadedModel | None, type_: str, message: str, line: int | None,
                filename: str = "", source: str = "") -> ModelError:
    filename = model.filename if model else filename
    source = model.source if model else source
    trace = ""
    if line is not None:
        trace = f'  File "{filename}", line {line}\n    {_source_line(source, line)}\n'
    return ModelError(type_, message, line, f"{trace}{type_}: {message}")


def validate_param_specs(raw: Any) -> dict[str, dict]:
    """Check a literal PARAMS value and return the specs with `unit` and `label` filled in.

    Raises ValueError with a message that names the parameter and the rule it broke.
    """
    if not isinstance(raw, dict):
        raise ValueError("PARAMS must be a dict of parameter specs")  # noqa: TRY004
    if len(raw) > MAX_PARAMS:
        raise ValueError(f"PARAMS has {len(raw)} parameters; the limit is {MAX_PARAMS}")
    specs: dict[str, dict] = {}
    for name, spec in raw.items():
        if not isinstance(name, str) or not NAME_RE.match(name):
            raise ValueError(
                f"parameter name {name!r} is not allowed; use lower-case letters, digits and "
                "underscores, starting with a letter, at most 32 characters"
            )
        if not isinstance(spec, dict):
            raise ValueError(  # noqa: TRY004 - one exception type for every broken rule
                f"parameter {name!r}: the spec must be a dict, got {spec!r}"
            )
        unknown = [key for key in spec if key not in SPEC_KEYS]
        if unknown:
            raise ValueError(
                f"parameter {name!r}: unknown key(s) {', '.join(map(repr, unknown))}; "
                f"allowed keys are {', '.join(SPEC_KEYS)}"
            )
        if "default" not in spec:
            raise ValueError(f"parameter {name!r}: 'default' is required")
        for key in ("default", "min", "max"):
            if key in spec and not _is_number(spec[key]):
                raise ValueError(f"parameter {name!r}: {key!r} must be a number, got {spec[key]!r}")
        low, high, default = spec.get("min"), spec.get("max"), spec["default"]
        if low is not None and high is not None and low > high:
            raise ValueError(f"parameter {name!r}: min {low} is greater than max {high}")
        if (low is not None and default < low) or (high is not None and default > high):
            raise ValueError(
                f"parameter {name!r}: default {default} is outside its range "
                f"[{'-inf' if low is None else low}, {'inf' if high is None else high}]"
            )
        provenance = spec.get("provenance")
        if provenance not in PROVENANCES:
            raise ValueError(
                f"parameter {name!r}: 'provenance' must be one of {', '.join(PROVENANCES)}, "
                f"got {provenance!r}"
            )
        source = spec.get("source")
        if provenance == "web":
            if not isinstance(source, str) or not NOTE_ID_RE.match(source):
                raise ValueError(
                    f"parameter {name!r}: a 'web' parameter needs 'source', the id of the "
                    f"research note the number was recorded under (like 'r1'), got {source!r}; "
                    "record the number with record_source first"
                )
        elif source is not None:
            raise ValueError(
                f"parameter {name!r}: 'source' goes only with the provenance 'web', "
                f"not with {provenance!r}"
            )
        unit = spec.get("unit", "mm")
        if unit not in UNITS:
            raise ValueError(
                f"parameter {name!r}: 'unit' must be one of {', '.join(UNITS)}, got {unit!r}"
            )
        if unit == "count" and any(
            float(spec[key]) != int(spec[key]) for key in ("default", "min", "max") if key in spec
        ):
            raise ValueError(f"parameter {name!r}: a 'count' parameter needs whole numbers")
        label = spec.get("label", name)
        if not isinstance(label, str) or not 1 <= len(label) <= 80:
            raise ValueError(f"parameter {name!r}: 'label' must be text of 1 to 80 characters")
        specs[name] = {**spec, "unit": unit, "label": label}
    return specs


def merge_params(specs: dict[str, dict], given: dict[str, Any]) -> Params:
    """Defaults overlaid with the request's values. Raises ValueError naming a bad parameter."""
    values = Params()
    for name in given:
        if name not in specs:
            raise ValueError(
                f"unknown parameter {name!r}; the model defines: {', '.join(specs) or 'none'}"
            )
    for name, spec in specs.items():
        value = given.get(name, spec["default"])
        if not _is_number(value):
            raise ValueError(f"parameter {name!r} must be a number, got {value!r}")
        low, high = spec.get("min"), spec.get("max")
        if (low is not None and value < low) or (high is not None and value > high):
            raise ValueError(
                f"parameter {name!r} = {value} is outside its allowed range "
                f"[{'-inf' if low is None else low}, {'inf' if high is None else high}]"
            )
        if spec["unit"] == "count":
            if float(value) != int(value):
                raise ValueError(f"parameter {name!r} is a count and must be whole, got {value}")
            values[name] = int(value)
        else:
            values[name] = float(value)
    return values


def load_model(path: Path) -> LoadedModel:
    """Read, parse and statically validate a model script without running it."""
    filename = path.name
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise ModelError("ModelNotFound", f"cannot read the model script {filename}: {exc}") from exc
    if len(raw) > MAX_SOURCE_BYTES:
        raise ModelError("ModelTooLarge", f"the model script is larger than {MAX_SOURCE_BYTES} bytes")
    try:
        source = raw.decode("utf-8")
        tree = ast.parse(source, filename=filename)
        code = compile(tree, filename, "exec")
    except (SyntaxError, UnicodeDecodeError, ValueError) as exc:
        line = getattr(exc, "lineno", None)
        message = getattr(exc, "msg", None) or str(exc)
        text = (getattr(exc, "text", None) or "").rstrip("\n")
        trace = f'  File "{filename}", line {line}\n    {text.strip()}\n' if line else ""
        raise ModelError(
            "SyntaxError", message, line, f"{trace}{type(exc).__name__}: {message}"
        ) from exc

    def rule(type_: str, message: str, line: int | None) -> ModelError:
        return _rule_error(None, type_, message, line, filename, source)

    assignments = [
        node
        for node in tree.body
        if (isinstance(node, ast.Assign)
            and any(isinstance(t, ast.Name) and t.id == "PARAMS" for t in node.targets))
        or (isinstance(node, ast.AnnAssign)
            and isinstance(node.target, ast.Name) and node.target.id == "PARAMS")
    ]
    if not assignments:
        raise rule(
            "ParamsError",
            "the model script must define PARAMS = {...} at the top level (it may be empty)",
            None,
        )
    if len(assignments) > 1:
        raise rule("ParamsError", "PARAMS is assigned more than once", assignments[1].lineno)
    node = assignments[0]
    try:
        literal = ast.literal_eval(node.value) if node.value is not None else None
    except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError):
        raise rule(
            "ParamsError",
            "PARAMS must be a literal dict: numbers and strings only, no names, expressions "
            "or function calls",
            node.lineno,
        ) from None
    try:
        specs = validate_param_specs(literal)
    except ValueError as exc:
        raise rule("ParamsError", str(exc), node.lineno) from None

    builds = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "build"]
    if not builds:
        raise rule(
            "BuildMissing", "the model script must define a top-level function build(p, ctx)", None
        )
    loaded = LoadedModel(path, filename, source, code, specs, node.lineno, builds[-1].lineno)
    _load_declarations(loaded, tree)
    return loaded


# name in the script -> (attribute of LoadedModel, error type, parser in hotend_fit.contract)
_DECLARATIONS = {
    "CONTRACT": ("contract", "ContractError", "parse_contract"),
    "ASSEMBLY": ("assembly", "AssemblyError", "parse_assembly"),
    "PRINT": ("print_hints", "PrintError", "parse_print"),
}


def _load_declarations(model: LoadedModel, tree: ast.Module) -> None:
    """Read CONTRACT, ASSEMBLY and PRINT from the source as literals and validate them."""
    for name, (attribute, error_type, parser) in _DECLARATIONS.items():
        nodes = [
            node for node in tree.body
            if (isinstance(node, ast.Assign)
                and any(isinstance(t, ast.Name) and t.id == name for t in node.targets))
            or (isinstance(node, ast.AnnAssign)
                and isinstance(node.target, ast.Name) and node.target.id == name)
        ]
        if not nodes:
            continue
        if len(nodes) > 1:
            raise _rule_error(model, error_type, f"{name} is assigned more than once",
                              nodes[1].lineno)
        node = nodes[0]
        try:
            literal = ast.literal_eval(node.value) if node.value is not None else None
        except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError):
            raise _rule_error(
                model, error_type,
                f"{name} must be a literal dict: numbers, text and lists only, no names, "
                "expressions or function calls",
                node.lineno,
            ) from None
        try:
            from pydantic import ValidationError

            from hotend_fit import contract as declarations
        except ImportError:
            raise _rule_error(
                model, error_type,
                f"{name} needs the hotend_fit package, which is not installed in this engine",
                node.lineno,
            ) from None
        try:
            parsed = getattr(declarations, parser)(literal)
        except ValidationError as exc:
            raise _rule_error(
                model, error_type,
                f"{name} is not valid: {declarations.describe_errors(exc)}", node.lineno,
            ) from None
        setattr(model, attribute, parsed)
        model.declaration_lines[name] = node.lineno


def check_declared_parts(model: LoadedModel, part_names: list[str]) -> None:
    """Raise ModelError when CONTRACT, ASSEMBLY or PRINT names a part `build` did not
    return."""
    declared = (
        ("CONTRACT", "ContractError", model.contract.parts if model.contract else ()),
        ("ASSEMBLY", "AssemblyError", model.assembly.parts_named() if model.assembly else ()),
        ("PRINT", "PrintError", tuple(model.print_hints.parts) if model.print_hints else ()),
    )
    for name, error_type, named in declared:
        missing = [part for part in named if part not in part_names]
        if missing:
            raise _rule_error(
                model, error_type,
                f"{name} names part(s) {', '.join(map(repr, missing))}, but build returned "
                f"{', '.join(map(repr, part_names))}",
                model.declaration_lines.get(name),
            )


def _script_error(model: LoadedModel, exc: BaseException) -> ModelError:
    """ModelError for an exception raised while running the script, trimmed to its frames."""
    frames = [f for f in traceback.extract_tb(exc.__traceback__) if f.filename == model.filename]
    lines = []
    for frame in frames:
        lines.append(f'  File "{model.filename}", line {frame.lineno}, in {frame.name}\n')
        text = _source_line(model.source, frame.lineno)
        if text:
            lines.append(f"    {text}\n")
    line = frames[-1].lineno if frames else None
    if isinstance(exc, SyntaxError) and exc.filename == model.filename:
        line = exc.lineno
    message = str(exc) or type(exc).__name__
    summary = "".join(traceback.format_exception_only(type(exc), exc)).strip()
    return ModelError(
        type(exc).__name__,
        message[:4000],
        line,
        ("Traceback (model script frames only):\n" + "".join(lines) if lines else "")
        + summary[:4000],
    )


def run_build(
    model: LoadedModel, params: Params, ctx: BuildContext, capture: StdoutCapture
) -> Any:
    """Execute the script and call build(p, ctx). Returns whatever build returned."""
    namespace: dict[str, Any] = {"__name__": "hotend_model", "__file__": model.filename}
    try:
        with capture.capturing():
            exec(model.code, namespace)  # noqa: S102 - running the model is this module's job
            build = namespace.get("build")
            if not callable(build):
                raise _rule_error(
                    model, "BuildMissing", "build is not a function", model.build_line
                )
            try:
                inspect.signature(build).bind(params, ctx)
            except TypeError:
                raise _rule_error(
                    model,
                    "BuildSignature",
                    "build must accept exactly two arguments: build(p, ctx)",
                    model.build_line,
                ) from None
            return build(Params(params), ctx)
    except ModelError:
        raise
    except KeyboardInterrupt:
        raise
    except BaseException as exc:  # SystemExit and friends are model errors too
        raise _script_error(model, exc) from exc


def rule_error(model: LoadedModel, type_: str, message: str, line: int | None = None) -> ModelError:
    """A ModelError for a broken rule, pointing at `line` (default: the build function)."""
    return _rule_error(model, type_, message, model.build_line if line is None else line)
