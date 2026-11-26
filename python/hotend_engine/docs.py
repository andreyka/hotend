"""The `docs` op: search the signatures and docstrings of the installed CAD libraries.

The index is built by importing build123d, bd_warehouse and (when installed) hotend_fit and
reading their public classes, functions and methods, so every excerpt matches the pinned
versions exactly. Nothing is cached on disk; building the index takes about as long as
importing the libraries.
"""

from __future__ import annotations

import importlib
import importlib.metadata
import importlib.util
import inspect
import math
import pkgutil
import re
from dataclasses import dataclass

PACKAGES = ("build123d", "bd_warehouse", "hotend_fit")
DISTRIBUTIONS = {"build123d": "build123d", "bd_warehouse": "bd-warehouse",
                 "hotend_fit": "hotend-python"}
MAX_DOC_CHARS = 1600
_WORD = re.compile(r"[A-Za-z][a-z0-9]*|[0-9]+")


@dataclass
class Entry:
    name: str
    kind: str
    module: str
    signature: str
    doc: str
    name_tokens: frozenset[str]
    doc_tokens: dict[str, int]


def _stem(word: str) -> str:
    word = word.lower()
    for suffix in ("ing", "es", "ed", "s"):
        if len(word) > len(suffix) + 2 and word.endswith(suffix):
            return word[: -len(suffix)]
    return word


def tokens(text: str) -> list[str]:
    """Lower-cased word stems; CamelCase and snake_case names are split into their words."""
    return [_stem(w) for w in _WORD.findall(text)]


def _signature(obj: object) -> str:
    try:
        return str(inspect.signature(obj))  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return ""


def _entry(name: str, kind: str, module: str, obj: object) -> Entry | None:
    doc = inspect.getdoc(obj) or ""
    signature = "" if kind == "property" else _signature(obj)
    if not doc and not signature:
        return None
    counts: dict[str, int] = {}
    for token in tokens(doc):
        counts[token] = counts.get(token, 0) + 1
    return Entry(name, kind, module, signature, doc, frozenset(tokens(name.split(".", 1)[-1])), counts)


def _module_entries(module, public_root: object | None) -> list[Entry]:
    entries = []
    exported = set(getattr(public_root, "__all__", ())) if public_root is not None else set()
    for attr, obj in vars(module).items():
        if attr.startswith("_") or getattr(obj, "__module__", None) != module.__name__:
            continue
        prefix = public_root.__name__ if attr in exported else module.__name__  # type: ignore[union-attr]
        qualified = f"{prefix}.{attr}"
        if inspect.isclass(obj):
            entry = _entry(qualified, "class", module.__name__, obj)
            if entry:
                entries.append(entry)
            for member_name, member in vars(obj).items():
                if member_name.startswith("_"):
                    continue
                if isinstance(member, property):
                    kind, target = "property", member.fget
                elif isinstance(member, (staticmethod, classmethod)):
                    kind, target = "method", member.__func__
                elif inspect.isfunction(member):
                    kind, target = "method", member
                else:
                    continue
                entry = _entry(f"{qualified}.{member_name}", kind, module.__name__, target)
                if entry:
                    entries.append(entry)
        elif inspect.isfunction(obj):
            entry = _entry(qualified, "function", module.__name__, obj)
            if entry:
                entries.append(entry)
    return entries


def build_index() -> tuple[list[Entry], dict[str, str | None]]:
    """Index every installed package of PACKAGES. Returns (entries, versions)."""
    entries: list[Entry] = []
    versions: dict[str, str | None] = {}
    for package_name in PACKAGES:
        if importlib.util.find_spec(package_name) is None:
            versions[package_name] = None
            continue
        package = importlib.import_module(package_name)
        try:
            versions[package_name] = importlib.metadata.version(DISTRIBUTIONS[package_name])
        except importlib.metadata.PackageNotFoundError:
            versions[package_name] = getattr(package, "__version__", "unknown")
        modules = [package]
        for info in pkgutil.walk_packages(package.__path__, prefix=f"{package_name}."):
            try:
                modules.append(importlib.import_module(info.name))
            except Exception:  # noqa: BLE001, S112 - optional extras (VTK, Jupyter helpers)
                continue
        for module in modules:
            entries += _module_entries(module, package)
    return entries, versions


def search(entries: list[Entry], query: str, limit: int) -> list[dict]:
    """The `limit` best entries for the query: name matches count most, then docstring words,
    each weighted by how rare the word is across the index."""
    wanted = list(dict.fromkeys(tokens(query)))
    if not wanted or not entries:
        return []
    frequency = {
        t: sum(1 for e in entries if t in e.name_tokens or t in e.doc_tokens) for t in wanted
    }
    scored = []
    for entry in entries:
        score = 0.0
        for token in wanted:
            if not frequency[token]:
                continue
            rarity = math.log(1 + len(entries) / frequency[token])
            if token in entry.name_tokens:
                score += 6.0 * rarity / math.sqrt(len(entry.name_tokens))
            score += rarity * min(entry.doc_tokens.get(token, 0), 3) / 3
        if score > 0:
            if entry.kind in ("class", "function"):
                score *= 1.25
            scored.append((score, entry))
    scored.sort(key=lambda pair: (-pair[0], pair[1].name))
    results = []
    for score, entry in scored[:limit]:
        doc = entry.doc
        if len(doc) > MAX_DOC_CHARS:
            doc = doc[:MAX_DOC_CHARS].rsplit("\n", 1)[0] + "\n[...]"
        results.append({
            "name": entry.name, "kind": entry.kind, "module": entry.module,
            "signature": entry.signature, "doc": doc, "score": round(score, 3),
        })
    return results
