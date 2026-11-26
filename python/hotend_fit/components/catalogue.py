"""The components as the agent sees them before it uses one: the gateway's
`list_components` reads these docstrings through the engine's docs index (as `list_cards`
reads the cards), and `read_docs` finds them too.

`shipped_components()` holds the introduction and every id; each component has a function
`shipped_component_<id>()` of its own whose docstring is one line written from its data: id,
name, size, the provenance of its body, how many of its numbers are not verified (by feature)
and its sources with their URLs. One entry per component keeps every entry well under the docs
index's cut (1600 characters), however many components ship. Each function also returns the
data its docstring was written from, so the text and the data cannot drift apart.
"""

from __future__ import annotations

from typing import Any

from . import ComponentError, catalogue_entry, list_components, load_component

#: The words the gateway searches for; every entry of this module has them in its name.
QUERY = "shipped component"
PREFIX = "shipped_component_"


def _line(entry: dict) -> str:
    width, height, depth = (f"{value:g}" for value in entry["size"])
    features = ", ".join(f"{name} ({count})" for name, count in entry["unverified_by_feature"].items())
    body = ("its body size is verified" if not entry["body_unverified"]
            else f"its body size is NOT verified ({', '.join(entry['body_unverified'])})")
    sources = "; ".join(f"{key} ({source['kind']}) {source['url']}"
                        for key, source in entry["sources"].items())
    local = " (a file of this installation, not shipped with Hotend)" if entry["local"] else ""
    return (f"- {entry['id']}: {entry['name']}, {entry['kind']}{local}, {width} x {height} x {depth} mm "
            f"(width x height x depth), {body} (provenance {entry['body_provenance']}). "
            f"{entry['numbers']} numbers, {entry['unverified']} not verified"
            + (f", in: {features}" if features else "") + f". Sources: {sources}.")


def _describe(component_id: str) -> str:
    """The catalogue line of one component, or why its data cannot be used. A
    reason never names a path on this computer."""
    try:
        return _line(catalogue_entry(load_component(component_id)))
    except ComponentError as exc:
        return f"- {component_id}: its data cannot be used ({exc})"


def shipped_components() -> list[str]:
    # The docstring is written below from the installed data.
    return list_components()


def _intro() -> str:
    ids = list_components()
    return (
        "The bought devices Hotend has as components: "
        + (", ".join(ids) if ids else "none") + f" ({len(ids)}). "
        "Each has a line of its own below (id: name, size, sources, numbers not verified). Use "
        "`ctx.component(id)` for any of them instead of asking the maker for its size or "
        "looking it up; the recipe is hotend_fit.capabilities.bought_components. A feature "
        "with numbers that are not verified carries a question and makes `reference_fit` "
        "warn: say which, and offer research or a test print of that area. A component "
        "whose name says invented is a sample for examples and tests: it is no real device, "
        "so use it only when it is asked for by its name.")


shipped_components.__doc__ = _intro()


def _component_function(component_id: str) -> Any:
    def entry() -> dict:
        return catalogue_entry(load_component(component_id))

    name = f"{PREFIX}{component_id}"
    entry.__name__ = entry.__qualname__ = name
    entry.__module__ = __name__
    entry.__doc__ = _describe(component_id)
    return entry


globals().update({f"{PREFIX}{component_id}": _component_function(component_id)
                  for component_id in list_components()})
