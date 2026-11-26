"""What the core accepts in `result.json`, checked on every result the worker writes in tests.

The core's types refuse a field they do not know (`malformed_result`), so a field the worker
writes and the core does not list breaks the build for the user while every test that reads
`result.json` as plain JSON stays green. The list both sides are held to is
`crates/hotend-core/tests/fixtures/engine_result_fields.json`; the Rust test
`engine_result_contract` proves that the core's types accept exactly what it says, the largest
counts and sizes included.

Every test helper that runs the worker calls `check_job_result` on the job folder
(`tests/engine/test_engine_result_contract.py` looks for helpers that do not).
"""

from __future__ import annotations

import json
import re
from functools import cache
from pathlib import Path
from typing import Any

LIST = (Path(__file__).resolve().parents[2] / "crates" / "hotend-core" / "tests" / "fixtures"
        / "engine_result_fields.json")
_PIECE = r"[a-z0-9_]+(\.[a-z0-9_]+){0,2}"
# The texts with a fixed form, as `crates/hotend-core/src/engine/result.rs` checks them.
PATTERNS = {
    "name": re.compile(r"[a-z][a-z0-9_]{0,31}\Z"),
    "piece": re.compile(rf"(?=.{{1,100}}\Z){_PIECE}\Z"),
    "question_id": re.compile(r"[a-z][a-z0-9_]{0,63}\Z"),
    "check_id": re.compile(r"[a-z_]{1,64}\Z"),
    "short_id": re.compile(r"[A-Za-z0-9_:]{1,64}\Z"),
    "color": re.compile(r"#[0-9a-fA-F]{6}\Z"),
    "view": re.compile(r"(iso|front|back|left|right|top|bottom)\Z"),
    "path": re.compile(r".{1,200}\Z"),
    "text": re.compile(r"", re.DOTALL),
}
U32 = 2**32


@cache
def contract() -> dict:
    return json.loads(LIST.read_text(encoding="utf-8"))


def fields(object_name: str) -> dict[str, str]:
    """The fields of one object of the list with their types."""
    return contract()["objects"][object_name]


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _json_bytes(value: Any) -> int:
    """The length of a value as compact JSON text, the form whose size the core limits."""
    return len(json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8"))


def _too_large(value: Any, path: str, at: str) -> list[str]:
    """Where the field `path` (`object.field`) has more entries or bytes than the core takes."""
    problems = []
    most = contract()["max_items"].get(path)
    if most is not None and isinstance(value, (list, tuple, dict)) and len(value) > most:
        problems.append(f"{at}: {len(value)} entries, the core takes {most}")
    limit = contract()["max_json_bytes"].get(path)
    if limit is not None and value is not None and (size := _json_bytes(value)) > limit:
        problems.append(f"{at}: {size} bytes of JSON, the core takes {limit}")
    return problems


def _object(value: Any, name: str, at: str) -> list[str]:
    if not isinstance(value, dict):
        return [f"{at}: not an object ({name})"]
    listed, problems = fields(name), []
    for key, item in value.items():
        if listed.get(key, "core_only") == "core_only":
            problems.append(f"{at}.{key}: the core does not accept this field of `{name}`")
        else:
            problems += violations(item, listed[key], f"{at}.{key}")
            problems += _too_large(item, f"{name}.{key}", f"{at}.{key}")
    for key, spec in listed.items():
        if key not in value and not spec.endswith("?") and spec != "core_only":
            problems.append(f"{at}.{key}: the core needs this field of `{name}`")
    return problems


def violations(value: Any, spec: str, at: str) -> list[str]:
    """Every place where `value` is not what the type `spec` of the list says."""
    spec = spec.removesuffix("?")
    if spec.endswith("|null"):
        if value is None:
            return []
        spec = spec.removesuffix("|null")
    if spec == "any":
        return []
    if spec.startswith("@"):
        return _object(value, spec[1:], at)
    if spec.startswith("one_of:"):
        allowed = spec.removeprefix("one_of:").split(",")
        return [] if value in allowed else [f"{at}: {value!r} is not one of {allowed}"]
    if spec.startswith("["):
        item, _, count = spec[1:-1].partition(";")
        if not isinstance(value, (list, tuple)) or (count and len(value) != int(count)):
            return [f"{at}: not a list {spec}"]
        return [p for i, entry in enumerate(value) for p in violations(entry, item, f"{at}[{i}]")]
    if spec.startswith("{"):
        key_type, _, item = spec[1:-1].partition(":")
        if not isinstance(value, dict):
            return [f"{at}: not an object {spec}"]
        bad = [f"{at}: {key!r} is no {key_type}" for key in value
               if not PATTERNS[key_type].match(key)]
        return bad + [p for k, entry in value.items() for p in violations(entry, item, f"{at}.{k}")]
    good = {
        "object": lambda: isinstance(value, dict),
        "bool": lambda: isinstance(value, bool),
        "number": lambda: _is_number(value),
        "integer": lambda: _is_number(value) and isinstance(value, int),
        "count": lambda: _is_number(value) and isinstance(value, int) and 0 <= value < U32,
    }.get(spec, lambda: isinstance(value, str) and PATTERNS[spec].match(value) is not None)()
    return [] if good else [f"{at}: {str(value)[:80]!r} is no {spec}"]


def check_result(result: dict, where: str = "result") -> None:
    """Fails when the core would refuse this result of the worker."""
    op = result.get("op")
    if op in contract()["ops_the_core_never_requests"]:
        return
    problems = violations(result, "@result", where)
    if op is not None and op not in contract()["ops"]:
        problems.append(f"{where}.op: the core knows no op {op!r}")
    assert not problems, (
        "the core would refuse this result (malformed_result). Teach "
        "crates/hotend-core/src/engine/result.rs the field and list it in "
        f"{LIST.name}, or do not write it:\n" + "\n".join(problems[:20]))


def check_job_result(job_dir: Path) -> None:
    """`check_result` on what a worker left in its job folder; a worker that ended without a
    result (a crash, a folder that does not exist) has nothing to check."""
    path = Path(job_dir) / "result.json"
    if path.is_file():
        size, most = path.stat().st_size, contract()["max_result_bytes"]
        assert size <= most, f"{path.parent.name}: result.json has {size} bytes, the core takes {most}"
        check_result(json.loads(path.read_text(encoding="utf-8")), f"{path.parent.name}: result")
