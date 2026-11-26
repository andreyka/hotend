"""What README.md and docs/PROTOCOL.md say about the repository itself, held to the checkout.

These read files only; nothing is built or run. They fail when a crate or an application is
added without a line in README's "Where things are", when the scripts read a setting their
header does not list, or when two places of the protocol disagree about which deliveries
`printers_list` reports.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
README = (REPO / "README.md").read_text(encoding="utf-8")
PROTOCOL = (REPO / "docs" / "PROTOCOL.md").read_text(encoding="utf-8")


def _section(text: str, heading: str) -> str:
    """The text from a heading line to the next heading of the same or a higher level."""
    level = heading.split(" ")[0]
    start = text.index(f"\n{heading}\n")
    rest = text[start + len(heading) + 2:]
    end = re.search(rf"^#{{1,{len(level)}}} ", rest, flags=re.MULTILINE)
    return rest[:end.start()] if end else rest


def _where_things_are() -> dict[str, str]:
    """README's table: every path named in a first column, with the row's description."""
    paths = {}
    for line in _section(README, "## Where things are").splitlines():
        cells = [cell.strip() for cell in line.split("|")]
        if len(cells) < 4 or set(cells[1]) <= {"-", " "} or cells[1] == "Path":
            continue
        for name in re.findall(r"`([^`]+)`", cells[1]):
            paths[name.rstrip("/")] = cells[2]
    return paths


def test_readme_says_where_every_crate_and_application_is():
    listed = _where_things_are()
    members = [path for folder in ("crates", "apps") for path in sorted((REPO / folder).iterdir())
               if path.is_dir()]
    assert len(members) >= 7, members
    for member in members:
        name = member.relative_to(REPO).as_posix()
        assert name in listed, f'README\'s "Where things are" has no line for {name}'


def test_every_path_readme_lists_exists():
    listed = _where_things_are()
    assert len(listed) >= 12, listed
    for name in listed:
        # A row may name siblings after the first path: `python/hotend_engine`, `hotend_fit`.
        places = [REPO / name, REPO / "python" / name, REPO / "scripts" / name]
        assert any(place.exists() for place in places), f"README lists {name}, which is not there"


def test_readme_states_what_of_the_desktop_application_is_tested_and_what_is_not():
    state = " ".join(_section(README, "## Status").split())
    works, rest = state.split("**Simulated or unproven**")
    unproven, not_built = rest.split("**Not built**")
    assert "`apps/desktop`" in works and "simulated printer" in works
    assert "headless Chromium" in unproven and "development bridge" in unproven
    assert "No automated test opens the desktop application's own window" in unproven
    assert "Windows and macOS" in unproven
    assert "An installer." in not_built


def test_the_scripts_list_every_setting_they_read():
    # A setting is a HOTEND_ variable a script reads with a default: ${NAME:-...}.
    for script in ("test-all.sh", "setup-wsl.sh"):
        text = (REPO / "scripts" / script).read_text(encoding="utf-8")
        header = text[:text.index("\nset -u -o pipefail\n")]
        read = set(re.findall(r"\$\{(HOTEND_[A-Z_]+):-", text))
        assert read, script
        for name in sorted(read):
            assert name in header, f"scripts/{script} reads {name}, which its header does not list"
    assert "HOTEND_REFS_DIR" in (REPO / "scripts" / "test-all.sh").read_text(encoding="utf-8")
    assert "HOTEND_REFS_DIR" in README


def test_the_protocol_names_the_same_deliveries_in_both_places():
    # Section 8's table says which ways `printers_list` reports; section 8.2 said for a
    # while that the `lan` delivery is listed as disabled, which no build does.
    (row,) = [line for line in PROTOCOL.splitlines() if line.startswith("| `printers_list` |")]
    in_table = set(re.findall(r"`(simulated_printer|bambu_connect|export_to_folder|lan)`", row))
    assert in_table == {"simulated_printer", "bambu_connect", "export_to_folder"}, row
    assert "has no entry" in row
    printing = _section(PROTOCOL, "### 8.2 Printing through the core")
    opening = " ".join(printing.split("\n1. **Propose**")[0].split())
    (sentence,) = [part for part in re.split(r"(?<=\.) ", opening) if "`printers_list`" in part]
    in_text = set(re.findall(r"`(simulated_printer|bambu_connect|export_to_folder|lan)`", sentence))
    assert in_text == in_table, sentence
    assert "has no entry" in sentence and "disabled" not in sentence, sentence
