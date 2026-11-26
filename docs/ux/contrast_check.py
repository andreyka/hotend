#!/usr/bin/env python3
"""Contrast check for the colour tokens of docs/ux/DESIGN_SPEC.md.

Reads the token values from the first CSS block of a file that defines `--app-bg` (the
specification itself by default, or a stylesheet given as the first argument), computes the
WCAG 2.x contrast ratio of every pair the specification relies on, and exits 1 when a pair is
below its floor (4.5 for text, 3.0 for control boundaries, icons and parts on the stage; a
part counts as seen when its fill, or its edge against both the stage and the part, reaches 3.0).

    python3 docs/ux/contrast_check.py                # check the specification
    python3 docs/ux/contrast_check.py tokens.css     # check the application's stylesheet
    python3 docs/ux/contrast_check.py --write        # rewrite the tables of section 7.3
    python3 docs/ux/contrast_check.py --verify-doc   # also fail if those tables are stale

Standard library only. The tables between the `contrast:begin` and `contrast:end` markers of
the specification are this script's output; do not edit them by hand.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

SPEC = Path(__file__).with_name("DESIGN_SPEC.md")
BEGIN, END = "<!-- contrast:begin -->", "<!-- contrast:end -->"
SURFACES = ["bg", "panel", "raised", "subtle", "hover"]


def luminance(colour: str) -> float:
    value = colour.lstrip("#")
    channels = [int(value[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    linear = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]


def ratio(a: str, b: str) -> float:
    la, lb = luminance(a), luminance(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


def read_tokens(text: str) -> dict[str, dict[str, str]]:
    """{'dark': {'app-bg': '#0E0F11', ...}, 'light': {...}} from the token stylesheet."""
    blocks = re.findall(r"```css\n(.*?)```", text, flags=re.DOTALL) or [text]
    source = next((block for block in blocks if "--app-bg" in block), None)
    if source is None:
        sys.exit("no CSS block that defines --app-bg was found")
    themes: dict[str, dict[str, str]] = {}
    for selector, body in re.findall(r"([^{}]+)\{([^{}]*)\}", source):
        theme = "light" if "light" in selector else "dark" if "dark" in selector else None
        if theme is None:
            continue
        found = dict(re.findall(r"--([a-z0-9-]+):\s*(#[0-9A-Fa-f]{6})\s*;", body))
        themes.setdefault(theme, {}).update(found)
    for theme in ("dark", "light"):
        if "app-bg" not in themes.get(theme, {}):
            sys.exit(f"the {theme} theme does not define --app-bg")
    return themes


class Report:
    def __init__(self, themes: dict[str, dict[str, str]]) -> None:
        self.themes = themes
        self.lines: list[str] = []
        self.failures: list[str] = []

    def colour(self, theme: str, name: str) -> str:
        tokens = self.themes[theme]
        for key in (f"app-{name}", f"hotend-{name}", name):
            if key in tokens:
                return tokens[key]
        sys.exit(f"the {theme} theme has no token for `{name}`")

    def lowest(self, theme: str, fg: str, backgrounds: list[str]) -> tuple[float, str]:
        pairs = [(ratio(self.colour(theme, fg), self.colour(theme, bg)), bg) for bg in backgrounds]
        return min(pairs)

    def row(self, label: str, fg: str, backgrounds: list[str], floor: float) -> None:
        cells = []
        for theme in ("dark", "light"):
            value, where = self.lowest(theme, fg, backgrounds)
            if value < floor:
                self.failures.append(f"{theme}: {label} is {value:.2f} on {where}, below {floor}")
            note = f" (on `{where}`)" if len(backgrounds) > 1 else ""
            cells.append(f"{value:.2f}{note}")
        self.lines.append(f"| {label} | {cells[0]} | {cells[1]} | {floor:.1f} |")

    def silhouette(self, label: str, part: str, edge: str, stage: list[str], floor: float) -> None:
        """The better of two ways to see a part on the stage, at the stage end where it is worst:
        the part's fill against the stage, or its edge against both the stage and the part."""
        cells = []
        for theme in ("dark", "light"):
            def seen(end: str) -> tuple[float, str]:
                fill = ratio(self.colour(theme, part), self.colour(theme, end))
                line = min(ratio(self.colour(theme, edge), self.colour(theme, end)),
                           ratio(self.colour(theme, edge), self.colour(theme, part)))
                return (fill, "fill") if fill >= line else (line, "edge")
            (value, how), where = min((seen(end), end) for end in stage)
            if value < floor:
                self.failures.append(f"{theme}: {label} is {value:.2f} on {where}, below {floor}")
            cells.append(f"{value:.2f} (by its {how}, on `{where}`)")
        self.lines.append(f"| {label} | {cells[0]} | {cells[1]} | {floor:.1f} |")

    def heading(self, title: str) -> None:
        if self.lines:
            self.lines.append("")
        self.lines += [f"**{title}**", "", "| Pair | Dark, lowest | Light, lowest | Floor |",
                       "| --- | --- | --- | --- |"]


def build(themes: dict[str, dict[str, str]]) -> Report:
    r = Report(themes)
    everywhere = SURFACES + ["active", "stage-top", "stage-bottom"]
    r.heading("Text (WCAG 1.4.3)")
    r.row("`text` on every surface and both ends of the stage", "text", everywhere, 4.5)
    r.row("`text-2` on every surface and both ends of the stage", "text-2", everywhere, 4.5)
    r.row("`text-3` on `bg`, `panel`, `raised`, `subtle`, `hover`", "text-3", SURFACES, 4.5)
    for name in ("accent", "ok", "warn", "fail", "sim"):
        r.row(f"`{name}` as text on `bg`, `panel`, `raised`, `subtle`, `hover`", name, SURFACES, 4.5)
    for name in ("accent", "ok", "warn", "fail", "sim"):
        r.row(f"`{name}` on `{name}-wash`", name, [f"{name}-wash"], 4.5)
    washes = [f"{name}-wash" for name in ("accent", "ok", "warn", "fail", "sim")]
    r.row("`text` on any wash", "text", washes, 4.5)
    r.row("`text-2` on any wash", "text-2", washes, 4.5)
    r.row("`on-fill` on `accent-fill` and `accent-fill-hover`", "on-fill",
          ["accent-fill", "accent-fill-hover"], 4.5)
    r.row("`on-fill` on `fail-fill` and `fail-fill-hover`", "on-fill",
          ["fail-fill", "fail-fill-hover"], 4.5)
    r.heading("Control boundaries, icons and focus (WCAG 1.4.11)")
    r.row("`control` (field, slider and stepper borders) on `bg`, `panel`, `raised`, `subtle`",
          "control", ["bg", "panel", "raised", "subtle"], 3.0)
    r.row("`accent-fill` (primary button, slider range) against `bg`, `panel`, `raised`, `subtle`",
          "accent-fill", ["bg", "panel", "raised", "subtle"], 3.0)
    r.row("`accent-fill-hover` against `bg`, `panel`, `raised`, `subtle`",
          "accent-fill-hover", ["bg", "panel", "raised", "subtle"], 3.0)
    r.row("`fail-fill` (Stop) against `panel`, `raised`", "fail-fill", ["panel", "raised"], 3.0)
    r.row("`accent` focus ring on every surface and both ends of the stage", "accent", everywhere, 3.0)
    for name in ("ok", "warn", "fail", "sim"):
        r.row(f"`{name}` icon on every surface", name, SURFACES + ["active"], 3.0)
    r.heading("The stage (WCAG 1.4.11)")
    ends = ["stage-top", "stage-bottom"]
    r.row("`stage-plate` (plate outline) on both ends of the stage", "stage-plate", ends, 3.0)
    # A part is told from the stage by its fill or, where the fill is close to the stage (the
    # light theme's pale tints), by its 1 px edge: the edge is then at least 3:1 against the
    # stage and against the part.
    for index in range(1, 9):
        r.silhouette(f"`part-{index}` told from both ends of the stage, by its fill or by `stage-edge`",
                     f"part-{index}", "stage-edge", ends, 3.0)
    r.row("`stage-edge` against every part tint", "stage-edge",
          [f"part-{index}" for index in range(1, 9)], 3.0)
    return r


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    source = Path(args[0]) if args else SPEC
    report = build(read_tokens(source.read_text(encoding="utf-8")))
    table = "\n".join(report.lines)
    if "--write" in flags or "--verify-doc" in flags:
        text = SPEC.read_text(encoding="utf-8")
        if BEGIN not in text or END not in text:
            sys.exit("the specification has no contrast markers")
        head, rest = text.split(BEGIN, 1)
        old, tail = rest.split(END, 1)
        if "--write" in flags:
            SPEC.write_text(f"{head}{BEGIN}\n{table}\n{END}{tail}", encoding="utf-8")
        elif old.strip() != table.strip():
            report.failures.append("the tables of section 7.3 are not this script's output; "
                                   "run it with --write")
    print(table)
    print()
    for failure in report.failures:
        print("FAIL", failure)
    print(f"{len(report.lines)} lines, {len(report.failures)} below their floor")
    return 1 if report.failures else 0


if __name__ == "__main__":
    sys.exit(main())
