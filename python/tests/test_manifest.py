"""pyproject.toml, the code and docs/DEPENDENCIES.md say the same thing: every direct
dependency is used, and every one has its line in the licence record."""

from __future__ import annotations

import re
import tomllib
from pathlib import Path

PYTHON = Path(__file__).resolve().parents[1]
REPO = PYTHON.parent
PACKAGES = ("hotend_engine", "hotend_fit", "hotend_agent", "tests")

# Distribution name -> how its use shows in the sources.
IMPORT_NAMES = {
    "build123d": "build123d",
    "ocp-tessellate": "ocp_tessellate",
    "bd-warehouse": "bd_warehouse",
    "manifold3d": "manifold3d",
    "numpy": "numpy",
    "pydantic": "pydantic",
    "pydantic-ai-slim": "pydantic_ai",
    "pillow": "PIL",
    "httpx2": "httpx2",
    "pytest": "pytest",
}
# Used without an import statement in Hotend code: (file, text that shows the use).
USED_OTHERWISE = {
    # build123d's Mesher writes 3MF with lib3mf; hotend_engine.export calls the Mesher.
    "lib3mf": (PYTHON / "hotend_engine" / "export.py", "Mesher"),
    "pytest-timeout": (PYTHON / "tests" / "agent" / "test_agent_process.py",
                       "pytest.mark.timeout"),
    "ruff": (REPO / "scripts" / "test-all.sh", "ruff check"),
}


def _name(requirement: str) -> str:
    return re.split(r"[\[<>=!~; ]", requirement, maxsplit=1)[0].lower()


def _direct_dependencies() -> list[str]:
    project = tomllib.loads((PYTHON / "pyproject.toml").read_text(encoding="utf-8"))
    declared = list(project["project"]["dependencies"])
    for group in project.get("dependency-groups", {}).values():
        declared += group
    return [_name(requirement) for requirement in declared]


def _imported_modules() -> set[str]:
    found = set()
    pattern = re.compile(r"^\s*(?:from|import)\s+([A-Za-z_][A-Za-z0-9_]*)", re.MULTILINE)
    for package in PACKAGES:
        for path in (PYTHON / package).rglob("*.py"):
            found.update(pattern.findall(path.read_text(encoding="utf-8")))
    return found


def test_every_direct_dependency_is_used():
    imported = _imported_modules()
    for name in _direct_dependencies():
        if name in USED_OTHERWISE:
            path, needle = USED_OTHERWISE[name]
            assert needle in path.read_text(encoding="utf-8"), f"{name}: {needle} not in {path}"
        else:
            assert name in IMPORT_NAMES, f"{name} is declared but this test does not know its use"
            assert IMPORT_NAMES[name] in imported, f"{name} is declared but nothing imports it"


def test_every_direct_dependency_has_its_line_in_the_licence_record():
    rows = [line.split("|")[1].strip().lower()
            for line in (REPO / "docs" / "DEPENDENCIES.md").read_text(encoding="utf-8").splitlines()
            if line.startswith("| ") and not line.startswith("| ---")]
    python_rows = [row for row in rows if "(rust" not in row]
    declared = _direct_dependencies()
    for name in declared:
        assert any(name in re.split(r"[ ,]+", row) for row in python_rows), (
            f"{name} has no line in docs/DEPENDENCIES.md")
    # And no line describes a Python package that is not there any more.
    for gone in ("trimesh", "scipy", "hypothesis", "pyright"):
        assert gone not in declared
        assert not any(gone in re.split(r"[ ,]+", row) for row in python_rows), gone
