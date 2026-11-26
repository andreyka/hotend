"""uv.lock installs on every desktop platform without building anything.

The installer ships the locked environment, and `uv sync --frozen` on a user's or a build
machine must not need a compiler. So every package of the runtime closure (what the engine
and the agent import, not the development tools) has a wheel for CPython 3.13 on Windows
x64, macOS on Apple silicon, macOS on Intel and Linux x64. Read from the lock file only: no
network, no installation.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

import pytest
from packaging.markers import Marker
from packaging.utils import parse_wheel_filename

LOCK = Path(__file__).resolve().parents[1] / "uv.lock"
PYTHON = (3, 13)

# platform -> (marker environment, does a wheel's platform tag run there)
PLATFORMS = {
    "windows-x64": (
        {"sys_platform": "win32", "platform_machine": "AMD64", "os_name": "nt",
         "platform_system": "Windows"},
        lambda tag: tag == "win_amd64"),
    "macos-arm64": (
        {"sys_platform": "darwin", "platform_machine": "arm64", "os_name": "posix",
         "platform_system": "Darwin"},
        lambda tag: tag.startswith("macosx_") and tag.endswith(("_arm64", "_universal2"))),
    "macos-x64": (
        {"sys_platform": "darwin", "platform_machine": "x86_64", "os_name": "posix",
         "platform_system": "Darwin"},
        lambda tag: tag.startswith("macosx_") and tag.endswith(("_x86_64", "_universal2"))),
    "linux-x64": (
        {"sys_platform": "linux", "platform_machine": "x86_64", "os_name": "posix",
         "platform_system": "Linux"},
        lambda tag: tag.startswith("manylinux") and tag.endswith("_x86_64")),
}
COMMON = {"python_version": "3.13", "python_full_version": "3.13.0",
          "implementation_name": "cpython", "platform_python_implementation": "CPython"}


def _installs(filename: str, runs_on) -> bool:
    """Whether CPython 3.13 on the platform installs this wheel."""
    for tag in parse_wheel_filename(filename)[3]:
        if not (tag.platform == "any" or runs_on(tag.platform)):
            continue
        if tag.interpreter.startswith("py3") and tag.abi == "none":
            return True
        if tag.interpreter.startswith("cp3") and tag.interpreter[3:].isdigit():
            minor = int(tag.interpreter[3:])
            if (tag.abi == "abi3" and minor <= PYTHON[1]) or (
                    minor == PYTHON[1] and tag.abi in ("cp313", "none")):
                return True
    return False


def runtime_closure(lock: dict, environment: dict) -> list[dict]:
    """The packages `uv sync --frozen --no-dev` installs in this marker environment."""
    packages = lock["package"]
    root = next(p for p in packages if "editable" in p.get("source", {}))

    def applies(dependency: dict) -> bool:
        marker = dependency.get("marker")
        return marker is None or Marker(marker).evaluate(environment)

    def find(dependency: dict) -> dict:
        found = [p for p in packages if p["name"] == dependency["name"]
                 and dependency.get("version", p["version"]) == p["version"]]
        assert len(found) == 1, f"{dependency} names {len(found)} packages of the lock"
        return found[0]

    closure: dict[tuple[str, str], dict] = {}
    visited: set[tuple[str, str, str]] = set()  # a package once per extra asked of it
    todo = [d for d in root["dependencies"] if applies(d)]
    while todo:
        dependency = todo.pop()
        package = find(dependency)
        closure[(package["name"], package["version"])] = package
        for extra in ("", *dependency.get("extra", [])):
            visit = (package["name"], package["version"], extra)
            if visit in visited:
                continue
            visited.add(visit)
            wanted = (package.get("optional-dependencies", {}).get(extra, []) if extra
                      else package.get("dependencies", []))
            todo += [d for d in wanted if applies(d)]
    return list(closure.values())


def packages_without_a_wheel(lock: dict, platform: str) -> list[str]:
    environment, runs_on = PLATFORMS[platform]
    missing = []
    for package in runtime_closure(lock, {**COMMON, **environment}):
        files = [wheel["url"].rsplit("/", 1)[-1] for wheel in package.get("wheels", [])]
        if not any(_installs(name, runs_on) for name in files):
            missing.append(f"{package['name']} {package['version']}")
    return sorted(missing)


@pytest.mark.parametrize("platform", list(PLATFORMS))
def test_every_runtime_package_has_a_wheel(platform):
    lock = tomllib.loads(LOCK.read_text(encoding="utf-8"))
    closure = runtime_closure(lock, {**COMMON, **PLATFORMS[platform][0]})
    # The walk found the environment, not a corner of it.
    assert {"build123d", "cadquery-ocp-novtk", "pydantic-ai-slim", "numpy"} <= {
        package["name"] for package in closure}
    assert not any(package["name"] in ("pytest", "ruff") for package in closure)
    assert packages_without_a_wheel(lock, platform) == [], (
        f"`uv sync --frozen` would have to build these from source on {platform}")
