"""setup-wsl.sh and test-all.sh, run for real against stand-in tools.

Each test copies the scripts into a throwaway checkout whose path it chooses, puts stand-ins
for cargo, uv, rustc, rustup and dpkg-query first on PATH, and runs the script with bash. The
stand-ins only write down how they were called, so a run takes well under a second, needs no
network and builds nothing. What is checked is what the scripts do with the tools:

- every cargo call that resolves dependencies passes `--locked`, and every uv call that
  touches the environment passes `--frozen`: a lock file that no longer matches its manifest
  fails the run instead of being rewritten (docs/IMPLEMENTATION_BRIEF.md, rule 11);
- test-all.sh counts the Rust tests cargo ignored, and runs the ones that need a real
  PrusaSlicer only when one is on PATH;
- test-all.sh passes on a checkout without local reference files, says which Python
  tests that leaves out, runs exactly those when HOTEND_REFS_DIR names the folder, and
  still fails on any other skipped test;
- setup-wsl.sh asks for what the desktop application needs (the system packages
  docs/DEPENDENCIES.md names, Node, pnpm, the interface's locked packages, a Chromium that
  starts), and test-all.sh asks for Node and pnpm before its first step, not after half an
  hour;
- the commands setup-wsl.sh tells the user to run next still work when pasted into a shell,
  whatever characters the checkout and cache paths contain.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import stat
import subprocess
from dataclasses import dataclass
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1]
REPO = SCRIPTS.parent
BASH = shutil.which("bash")

# Folder names a checkout or a cache really gets: a Windows user name with a space
# (/mnt/c/Users/First Last/...), and one with characters every shell treats specially.
PLAIN = "plain"
AWKWARD_NAMES = ["First Last", "it's $HOME (x) & *more*"]

# cargo subcommands that never read or write Cargo.lock.
CARGO_WITHOUT_LOCK = {"fmt"}


@dataclass
class Run:
    returncode: int
    stdout: str
    checkout: Path
    cache: Path
    home: Path
    logs: Path
    env: dict[str, str]

    def calls(self, tool: str) -> list[list[str]]:
        """Every call of a stand-in, in order, as its argument list."""
        log = self.logs / f"{tool}.log"
        if not log.exists():
            return []
        return [line.split("\t") if line else []
                for line in log.read_text(encoding="utf-8").splitlines()]

    def paste(self, command: str, then: str = "") -> str:
        """What a user's shell prints after the command is pasted into it."""
        done = subprocess.run([BASH, "-c", f"{command}\n{then}"], env=self.env, text=True,
                              capture_output=True, timeout=30, check=False)
        assert done.returncode == 0, f"pasting {command!r} failed: {done.stderr}"
        return done.stdout


def _stand_in(path: Path, body: str) -> None:
    path.write_text("#!/bin/sh\n" + body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _log_call(log: Path) -> str:
    # One call per line, arguments separated by tabs.
    return f"(IFS='\t'; printf '%s\\n' \"$*\") >> {shlex.quote(str(log))}\n"


# What the stand-in cargo prints for `cargo test`: two test binaries, four ignored tests.
CARGO_TEST_OUTPUT = """\
running 3 tests
test one ... ok
test needs_a_slicer ... ignored, needs an installed PrusaSlicer; run with `-- --ignored`
test also_needs_a_slicer ... ignored
test result: ok. 1 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.01s

running 2 tests
test third ... ignored
test fourth ... ignored
test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s
"""


# What pytest prints for a skipped test with `-rs --no-fold-skipped`.
def _skipped(test: str, reason: str) -> str:
    return f"SKIPPED {test} - Skipped: {reason}\n"


LOCAL_FILE_TESTS = [
    "tests/engine/test_engine_real_references.py::test_it_is_measured[soft_figure]",
    "tests/scenarios/test_scenario_dumpling_steamer.py::test_it_fits[soft_figure]",
    "tests/scenarios/test_scenario_dumpling_steamer.py::test_it_fits[figure_b]",
]
PYTEST_WITHOUT_LOCAL_FILES = (
    "....sss....\n=========================== short test summary info ===========\n"
    + "".join(_skipped(test, "HOTEND_REFS_DIR is not set") for test in LOCAL_FILE_TESTS)
    + "8 passed, 3 skipped in 1.00s\n")
PNPM_VERSION = "10.28.0"


def _run_script(tmp_path: Path, script: str, name: str, *, use_cache: bool = True,
                failing_cargo_step: str | None = None,
                real_prusaslicer: str = "0",
                arguments: tuple[str, ...] = (),
                environment: dict[str, str] | None = None,
                packages_installed: bool = True,
                pytest_output: str = "",
                pytest_output_with_local_files: str = "",
                desktop: bool = False,
                node_version: str | None = "v22.23.4",
                pnpm: bool = True,
                chromium: str = "starts",
                user_node_version: str | None = None) -> Run:
    """Runs a copy of the script in `<tmp>/<name>/hotend` with the stand-ins on PATH.

    `pytest_output` is what the stand-in uv prints for the script's pytest step, and
    `pytest_output_with_local_files` what it prints when HOTEND_REFS_DIR is set.

    `desktop` gives the checkout an apps/desktop, as the real one has. Then `node_version`
    is what the stand-in node answers (None: no node), `pnpm` says whether pnpm is on PATH,
    and `chromium` is the state of the browser: "starts", "missing" (it starts once
    `playwright install chromium` ran) or "needs libraries" (it starts only after
    `playwright install-deps chromium` ran as well). `user_node_version` puts a second
    node in ~/.local/bin, where setup-wsl.sh installs one, with that folder last on PATH.
    """
    root = tmp_path / name
    checkout = root / "hotend"
    cache = root / "cache"
    home = root / "home"
    logs = tmp_path / "logs"
    bin_dir = tmp_path / "bin"
    scratch = tmp_path / "tmp"
    for folder in (checkout / "scripts", checkout / "python", home, logs, bin_dir, scratch):
        folder.mkdir(parents=True)
    for source in SCRIPTS.glob("*.sh"):
        shutil.copy(source, checkout / "scripts" / source.name)

    hotend = "#!/bin/sh\n" + _log_call(logs / "hotend.log")
    _stand_in(bin_dir / "cargo", _log_call(logs / "cargo.log") + f"""
if [ "$1" = build ]; then
  out="${{CARGO_TARGET_DIR:-$PWD/target}}/debug"
  mkdir -p "$out" && printf '%s' {shlex.quote(hotend)} > "$out/hotend" && chmod +x "$out/hotend"
fi
[ "$1" = {shlex.quote(failing_cargo_step or "no such step")} ] && exit 101
if [ "$1" = test ] && [ "$2$3" = "--locked--workspace" ]; then
  printf '%s' {shlex.quote(CARGO_TEST_OUTPUT)}
fi
exit 0
""")
    # The stand-in uv prints the given output for the pytest step (the only call with
    # `-rs`), and writes down whether that call saw HOTEND_REFS_DIR.
    (bin_dir / "pytest.out").write_text(pytest_output, encoding="utf-8")
    (bin_dir / "pytest-local-files.out").write_text(pytest_output_with_local_files,
                                                    encoding="utf-8")
    _stand_in(bin_dir / "uv", _log_call(logs / "uv.log") + f"""
[ "$1" = --version ] && echo "uv 0.0.0 (stand-in)"
case " $* " in
  *" pytest -q -rs "*)
    printf '%s\\n' "${{HOTEND_REFS_DIR-<unset>}}" >> {shlex.quote(str(logs / "pytest-refs.log"))}
    if [ -n "${{HOTEND_REFS_DIR:-}}" ]; then
      cat {shlex.quote(str(bin_dir / "pytest-local-files.out"))}
    else
      cat {shlex.quote(str(bin_dir / "pytest.out"))}
    fi
    ;;
esac
exit 0
""")
    _stand_in(bin_dir / "rustc", 'echo "rustc 1.97.0 (stand-in)"\n')
    _stand_in(bin_dir / "rustup", 'printf "rustfmt-x\\nclippy-x\\n"\n')
    _stand_in(bin_dir / "dpkg-query", 'printf "install ok installed"\n' if packages_installed
              else "exit 1\n")
    if desktop:
        _desktop_stand_ins(checkout, bin_dir, logs, home, node_version, pnpm, chromium)

    env = {
        "PATH": f"{bin_dir}:{_system_bin(tmp_path)}",
        "HOME": str(home),
        "TMPDIR": str(scratch),
        "LC_ALL": "C",
    }
    if use_cache:
        env["HOTEND_CACHE_DIR"] = str(cache)
    # The machine running these tests may or may not have a PrusaSlicer in /usr/bin, so the
    # default here is "do not look"; "stand-in" puts one first on PATH and lets the script
    # find it.
    if real_prusaslicer == "stand-in":
        _stand_in(bin_dir / "prusa-slicer", "exit 0\n")
    else:
        env["HOTEND_TEST_REAL_PRUSASLICER"] = real_prusaslicer
    env.update(environment or {})
    if user_node_version is not None:
        user_bin = home / ".local" / "bin"
        user_bin.mkdir(parents=True)
        shutil.copy(bin_dir / "node", user_bin / "node")  # with its mode; then its answer
        (user_bin / "node").write_text((bin_dir / "node").read_text(encoding="utf-8").replace(
            shlex.quote(node_version or ""), shlex.quote(user_node_version)), encoding="utf-8")
        env["PATH"] += f":{user_bin}"
    done = subprocess.run([BASH, str(checkout / "scripts" / script), *arguments], env=env,
                          text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          timeout=60, check=False)
    return Run(done.returncode, done.stdout, checkout, cache, home, logs, env)


# What the stand-ins decide is present or not. A machine running these tests may have its own
# copy in /usr/bin (Ubuntu's nodejs 18, for example), which would answer in their place.
STAND_IN_ONLY = frozenset({"node", "nodejs", "npm", "npx", "corepack", "pnpm"})


def _system_bin(tmp_path: Path) -> Path:
    """/usr/bin and /bin as links in one folder, without the programs of STAND_IN_ONLY."""
    folder = tmp_path / "system-bin"
    if folder.exists():
        return folder
    folder.mkdir()
    for system in (Path("/usr/bin"), Path("/bin")):
        for program in system.iterdir():
            link = folder / program.name
            if program.name not in STAND_IN_ONLY and not link.is_symlink():
                link.symlink_to(program)
    return folder


def _desktop_stand_ins(checkout: Path, bin_dir: Path, logs: Path, home: Path,
                       node_version: str | None, pnpm: bool, chromium: str) -> None:
    """An apps/desktop in the checkout, and stand-ins for what the scripts run for it."""
    desktop = checkout / "apps" / "desktop"
    (desktop / "src-tauri" / "tests").mkdir(parents=True)
    (desktop / "package.json").write_text(json.dumps(
        {"name": "hotend-desktop", "packageManager": f"pnpm@{PNPM_VERSION}",
         "engines": {"node": ">=22"}}, indent=2), encoding="utf-8")
    (desktop / "src-tauri" / "tests" / "confirm_dialog.sh").write_text("exit 0\n",
                                                                       encoding="utf-8")
    # The browser's state is a file both stand-ins read: the stand-in node "starts" it,
    # the stand-in pnpm "installs" it.
    state = bin_dir / "chromium.state"
    state.write_text(chromium, encoding="utf-8")
    quoted = shlex.quote(str(state))
    if node_version is not None:
        _stand_in(bin_dir / "node", f"""
if [ "$1" = --version ]; then echo {shlex.quote(node_version)}; exit 0; fi
{_log_call(logs / "node.log")}
if [ "$(cat {quoted})" = starts ]; then
  echo "Chromium 1.2.3 (${{HOTEND_CHROMIUM:-the stand-in of playwright install}})"
else
  echo "browserType.launch: $(cat {quoted})"; exit 1
fi
""")
    pnpm_stand_in = _log_call(logs / "pnpm.log") + f"""
[ "$1" = --version ] && echo {PNPM_VERSION}
case "$*" in
  "exec playwright install chromium")
    [ "$(cat {quoted})" = missing ] && printf starts > {quoted} ;;
  "exec playwright install-deps chromium")
    printf starts > {quoted} ;;
esac
exit 0
"""
    if pnpm:
        _stand_in(bin_dir / "pnpm", pnpm_stand_in)
    # corepack puts the pnpm launcher where it is told to; curl "downloads" a file that is
    # not the Node archive.
    (bin_dir / "pnpm.stand-in").write_text("#!/bin/sh\n" + pnpm_stand_in, encoding="utf-8")
    _stand_in(bin_dir / "corepack", _log_call(logs / "corepack.log") + f"""
if [ "$1" = enable ]; then
  cp {shlex.quote(str(bin_dir / "pnpm.stand-in"))} "$3/pnpm" && chmod +x "$3/pnpm"
fi
exit 0
""")
    _stand_in(bin_dir / "curl", _log_call(logs / "curl.log") + """
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output ]; then printf 'not the Node archive' > "$2"; fi
  shift
done
exit 0
""")


def _setup(tmp_path: Path, name: str = PLAIN, **options) -> Run:
    run = _run_script(tmp_path, "setup-wsl.sh", name, **options)
    assert run.returncode == 0, run.stdout
    assert "Setup is complete. Next:" in run.stdout, run.stdout
    return run


def _hint(run: Run, starts_with: str) -> str:
    """The one line of the closing hints that starts with the given text."""
    found = [line.strip() for line in run.stdout.splitlines()
             if line.strip().startswith(starts_with)]
    assert len(found) == 1, f"expected one line starting {starts_with!r} in:\n{run.stdout}"
    return found[0]


def _demo_hint(run: Run) -> str:
    lines = run.stdout.splitlines()
    return lines[lines.index("Setup is complete. Next:") + 1].strip()


def _before_separator(arguments: list[str]) -> list[str]:
    """cargo's own arguments: what follows `--` belongs to the program or the tool it runs."""
    return arguments[:arguments.index("--")] if "--" in arguments else arguments


def _assert_cargo_locked(run: Run, expected_subcommands: list[str]) -> None:
    calls = run.calls("cargo")
    assert [call[0] for call in calls] == expected_subcommands, run.stdout
    for call in calls:
        if call[0] not in CARGO_WITHOUT_LOCK:
            assert "--locked" in _before_separator(call), (
                f"`cargo {' '.join(call)}` would rewrite a stale Cargo.lock instead of failing")


# ---- lock files ------------------------------------------------------------------------


def test_setup_builds_with_the_committed_lock_files(tmp_path):
    run = _setup(tmp_path)
    _assert_cargo_locked(run, ["build"])
    assert ["sync", "--frozen"] in run.calls("uv")


def test_test_all_runs_every_cargo_step_with_the_committed_lock_file(tmp_path):
    run = _run_script(tmp_path, "test-all.sh", PLAIN)
    assert run.returncode == 0, run.stdout
    assert re.search(r"RESULT: all \d+ steps passed", run.stdout), run.stdout
    _assert_cargo_locked(run, ["build", "fmt", "clippy", "test", "clippy", "test", "run"])
    environment_calls = [call for call in run.calls("uv") if call[0] in ("sync", "run")]
    assert len(environment_calls) >= 3, run.calls("uv")
    for call in environment_calls:
        assert "--frozen" in call, f"`uv {' '.join(call)}` may change the environment or uv.lock"


def test_test_all_counts_the_ignored_rust_tests_and_names_them(tmp_path):
    run = _run_script(tmp_path, "test-all.sh", PLAIN)
    assert run.returncode == 0, run.stdout
    assert "test-all: cargo test ignored 4 test(s) in this step." in run.stdout, run.stdout
    summary = run.stdout[run.stdout.index("== Summary =="):]
    assert ("Rust tests ignored by cargo test: 4 (the real-PrusaSlicer tests were not run: "
            "HOTEND_TEST_REAL_PRUSASLICER=0)") in summary, summary
    for name in ("needs_a_slicer", "also_needs_a_slicer", "third", "fourth"):
        assert f"  - {name}\n" in summary, summary
    assert re.search(r"skipped\s+real PrusaSlicer tests", summary), summary
    assert re.search(r"RESULT: all \d+ steps passed \(1 skipped", summary), summary


def test_test_all_runs_the_real_prusaslicer_tests_when_one_is_installed(tmp_path):
    run = _run_script(tmp_path, "test-all.sh", PLAIN, real_prusaslicer="stand-in")
    assert run.returncode == 0, run.stdout
    _assert_cargo_locked(
        run, ["build", "fmt", "clippy", "test", "clippy", "test", "test", "run"])
    real = run.calls("cargo")[6]
    assert real == ["test", "--locked", "-p", "hotend-slicer", "--test", "real_prusaslicer",
                    "--", "--ignored"], real
    summary = run.stdout[run.stdout.index("== Summary =="):]
    assert re.search(r"ok\s+real PrusaSlicer tests", summary), summary
    assert "the real-PrusaSlicer tests were then run with" in summary, summary
    assert re.search(r"RESULT: all \d+ steps passed in", summary), summary


def test_a_failing_cargo_test_step_fails_test_all(tmp_path):
    # The test steps run cargo through a pipe to count the ignored tests; the pipe must not
    # swallow cargo's failure.
    run = _run_script(tmp_path, "test-all.sh", PLAIN, failing_cargo_step="test")
    assert run.returncode != 0, run.stdout
    assert 'RESULT: FAILED at "cargo test --workspace"' in run.stdout, run.stdout
    assert [call[0] for call in run.calls("cargo")] == ["build", "fmt", "clippy", "test"]


def test_a_failing_cargo_step_fails_test_all(tmp_path):
    # `--locked` makes cargo fail on a stale lock; that is only a guard if the failure is
    # passed on and nothing runs after it.
    run = _run_script(tmp_path, "test-all.sh", PLAIN, failing_cargo_step="clippy")
    assert run.returncode != 0, run.stdout
    assert 'RESULT: FAILED at "cargo clippy' in run.stdout, run.stdout
    assert [call[0] for call in run.calls("cargo")] == ["build", "fmt", "clippy"]


def test_a_failing_build_fails_setup(tmp_path):
    run = _run_script(tmp_path, "setup-wsl.sh", PLAIN, failing_cargo_step="build")
    assert run.returncode != 0, run.stdout
    assert "the build failed" in run.stdout, run.stdout
    assert "Setup is complete" not in run.stdout


def test_ci_does_not_call_cargo_around_the_scripts_without_the_lock():
    resolving = re.compile(r"\bcargo\s+(build|check|clippy|test|run|bench|doc|install)\b")
    workflows = sorted((REPO / ".github" / "workflows").glob("*.y*ml"))
    assert workflows, "no CI workflow found"
    for workflow in workflows:
        text = workflow.read_text(encoding="utf-8")
        assert "scripts/test-all.sh" in text or "cargo" not in text
        for number, line in enumerate(text.splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            if resolving.search(line):
                assert "--locked" in line, f"{workflow.name}:{number}: {line.strip()}"


# ---- the commands setup-wsl.sh tells the user to run -----------------------------------


@pytest.mark.parametrize("name", [PLAIN, *AWKWARD_NAMES])
def test_the_demo_command_setup_prints_can_be_pasted(tmp_path, name):
    run = _setup(tmp_path, name)
    before = len(run.calls("hotend"))  # setup itself ran `hotend doctor`
    run.paste(_demo_hint(run))
    assert run.calls("hotend")[before:] == [["demo"]]


@pytest.mark.parametrize("name", [PLAIN, *AWKWARD_NAMES])
def test_the_demo_command_can_be_pasted_without_a_cache_dir(tmp_path, name):
    # Then the binary is inside the checkout, whose path the user did not choose either.
    run = _setup(tmp_path, name, use_cache=False)
    before = len(run.calls("hotend"))
    run.paste(_demo_hint(run))
    assert run.calls("hotend")[before:] == [["demo"]]


@pytest.mark.parametrize("name", [PLAIN, *AWKWARD_NAMES])
def test_the_export_line_setup_prints_sets_both_paths(tmp_path, name):
    run = _setup(tmp_path, name)
    printed = run.paste(_hint(run, "export HOTEND_PYTHON="),
                        'printf "%s\\n" "$HOTEND_PYTHON" "$HOTEND_PYTHON_SRC"')
    assert printed.splitlines() == [str(run.cache / "venv" / "bin" / "python"),
                                    str(run.checkout / "python")]


@pytest.mark.parametrize("name", [PLAIN, *AWKWARD_NAMES])
def test_the_bashrc_line_setup_prints_makes_the_cache_dir_permanent(tmp_path, name):
    run = _setup(tmp_path, name)
    run.paste(_hint(run, "echo "))
    bashrc = run.home / ".bashrc"
    assert bashrc.exists(), "the pasted line did not write ~/.bashrc"
    # A new shell reads ~/.bashrc; the value must be the folder that was used, as one word.
    printed = run.paste('. "$HOME/.bashrc"', 'printf "%s" "$HOTEND_CACHE_DIR"')
    assert printed == str(run.cache)


def test_ordinary_paths_are_printed_without_quoting_noise(tmp_path):
    run = _setup(tmp_path)
    hotend = run.cache / "target" / "debug" / "hotend"
    assert _demo_hint(run) == f"{hotend} demo"
    assert _hint(run, "echo ") == f"echo 'export HOTEND_CACHE_DIR={run.cache}' >> ~/.bashrc"
    assert _hint(run, "export HOTEND_PYTHON=") == (
        f"export HOTEND_PYTHON={run.cache}/venv/bin/python "
        f"HOTEND_PYTHON_SRC={run.checkout}/python")


def test_the_stand_ins_were_used_not_the_real_tools(tmp_path):
    run = _setup(tmp_path)
    assert os.path.realpath(run.cache / "target" / "debug" / "hotend").startswith(str(tmp_path))
    assert run.calls("hotend") == [["doctor"]]


# ---- local reference files -------------------------------------------------------------


def _pytest_runs(run: Run) -> list[list[str]]:
    """The arguments after `pytest` of every run of the Python tests, in order."""
    return [call[call.index("pytest") + 1:] for call in run.calls("uv")
            if "pytest" in call and "-rs" in call]


def test_test_all_passes_on_a_checkout_without_local_reference_files(tmp_path):
    # The documented command, on a machine that has only the repository: the tests that
    # read local files skip, and that is reported, not a failure.
    run = _run_script(tmp_path, "test-all.sh", PLAIN, pytest_output=PYTEST_WITHOUT_LOCAL_FILES)
    assert run.returncode == 0, run.stdout
    summary = run.stdout[run.stdout.index("== Summary =="):]
    assert re.search(r"ok\s+pytest\s", summary), summary
    assert re.search(r"skipped\s+pytest on local files", summary), summary
    assert ("Python tests that read local reference files: 3 "
            "(not run: HOTEND_REFS_DIR is not set)") in summary, summary
    for test in LOCAL_FILE_TESTS:
        assert f"  - {test}\n" in summary, summary
    assert re.search(r"RESULT: all \d+ steps passed \(2 skipped", summary), summary
    # Nothing after pytest was left out, and the tests were run once.
    assert [call[0] for call in run.calls("cargo")] == [
        "build", "fmt", "clippy", "test", "clippy", "test", "run"]
    assert _pytest_runs(run) == [["-q", "-rs", "--no-fold-skipped"]]


def test_any_other_skipped_python_test_still_fails_test_all(tmp_path):
    output = PYTEST_WITHOUT_LOCAL_FILES + _skipped(
        "tests/agent/test_agent.py::test_it_runs_in_the_sandbox", "no sandbox launcher")
    run = _run_script(tmp_path, "test-all.sh", PLAIN, pytest_output=output)
    assert run.returncode != 0, run.stdout
    assert "test-all: 1 Python test(s) were skipped" in run.stdout, run.stdout
    assert 'RESULT: FAILED at "pytest"' in run.stdout, run.stdout
    assert [call[0] for call in run.calls("cargo")] == ["build"]


def test_test_all_runs_exactly_the_local_file_tests_when_the_folder_is_named(tmp_path):
    refs = tmp_path / "somebody's files"
    refs.mkdir()
    run = _run_script(tmp_path, "test-all.sh", PLAIN, pytest_output=PYTEST_WITHOUT_LOCAL_FILES,
                      pytest_output_with_local_files="...\n3 passed in 60.00s\n",
                      environment={"HOTEND_REFS_DIR": str(refs)})
    assert run.returncode == 0, run.stdout
    # First every test without the folder, as on any machine; then the ones that asked
    # for it, by name, with it.
    assert _pytest_runs(run) == [["-q", "-rs", "--no-fold-skipped"],
                                 ["-q", "-rs", "--no-fold-skipped", *LOCAL_FILE_TESTS]]
    seen = (run.logs / "pytest-refs.log").read_text(encoding="utf-8").splitlines()
    assert seen == ["<unset>", str(refs)]
    summary = run.stdout[run.stdout.index("== Summary =="):]
    assert re.search(r"ok\s+pytest on local files", summary), summary
    assert f"3 (run with HOTEND_REFS_DIR={refs})" in summary, summary
    assert re.search(r"RESULT: all \d+ steps passed \(1 skipped", summary), summary


def test_a_local_file_test_that_still_skips_fails_test_all(tmp_path):
    # With the folder named, a pass must mean the files were read.
    refs = tmp_path / "refs"
    refs.mkdir()
    still_skipped = "..s\n" + _skipped(LOCAL_FILE_TESTS[2],
                                       "the file of figure_b is not in HOTEND_REFS_DIR")
    run = _run_script(tmp_path, "test-all.sh", PLAIN, pytest_output=PYTEST_WITHOUT_LOCAL_FILES,
                      pytest_output_with_local_files=still_skipped,
                      environment={"HOTEND_REFS_DIR": str(refs)})
    assert run.returncode != 0, run.stdout
    assert "test-all: 1 test(s) still skipped" in run.stdout, run.stdout
    assert 'RESULT: FAILED at "pytest on local files"' in run.stdout, run.stdout


def test_a_reference_folder_that_does_not_exist_fails_test_all(tmp_path):
    run = _run_script(tmp_path, "test-all.sh", PLAIN, pytest_output=PYTEST_WITHOUT_LOCAL_FILES,
                      environment={"HOTEND_REFS_DIR": str(tmp_path / "nowhere")})
    assert run.returncode != 0, run.stdout
    assert "is not a folder" in run.stdout, run.stdout
    assert len(_pytest_runs(run)) == 1


# ---- the desktop application -----------------------------------------------------------


def _system_packages_of_the_desktop() -> list[str]:
    """The Linux system packages docs/DEPENDENCIES.md says the desktop is built against."""
    rows = [line for line in (REPO / "docs" / "DEPENDENCIES.md").read_text(
        encoding="utf-8").splitlines() if re.match(r"\| lib[^|]*\(system, Linux\) \|", line)]
    assert len(rows) == 1, rows
    names = rows[0].split("|")[1].replace("(system, Linux)", "")
    return [name.strip() for name in names.split(",")]


def test_setup_asks_for_the_packages_the_desktop_application_is_built_against(tmp_path):
    # `cargo build --workspace` builds apps/desktop/src-tauri too; without these it stops
    # in glib-sys.
    packages = _system_packages_of_the_desktop()
    assert "libwebkit2gtk-4.1-dev" in packages and len(packages) >= 5, packages
    run = _run_script(tmp_path, "setup-wsl.sh", PLAIN, packages_installed=False)
    assert run.returncode != 0, run.stdout
    (command,) = [line for line in run.stdout.splitlines() if "apt-get install -y" in line]
    asked = command.split("apt-get install -y")[1].split()
    for package in [*packages, "xvfb", "xdotool"]:
        assert package in asked, f"{package} is not in: {command}"
    assert run.calls("cargo") == [], "the build was started without its system packages"


def test_setup_prepares_the_desktop_interface(tmp_path):
    run = _setup(tmp_path, desktop=True)
    assert "present:  node v22.23.4" in run.stdout, run.stdout
    assert f"present:  pnpm {PNPM_VERSION}" in run.stdout, run.stdout
    assert ["install", "--frozen-lockfile"] in run.calls("pnpm"), run.calls("pnpm")
    assert "present:  Chromium 1.2.3" in run.stdout, run.stdout
    # Nothing was downloaded: Node, pnpm and a Chromium that starts were there.
    assert run.calls("curl") == [] and run.calls("corepack") == []
    assert not [call for call in run.calls("pnpm") if "playwright" in call]
    assert _hint(run, "bash scripts/test-all.sh") == "bash scripts/test-all.sh"


@pytest.mark.parametrize("node_version", [None, "v18.19.1"])
def test_setup_does_not_unpack_a_node_download_with_another_hash(tmp_path, node_version):
    # Ubuntu 24.04's own nodejs is 18: Node comes from nodejs.org, and only the archive
    # whose SHA-256 is written in the script is unpacked.
    run = _run_script(tmp_path, "setup-wsl.sh", PLAIN, desktop=True, node_version=node_version)
    assert run.returncode != 0, run.stdout
    (download,) = run.calls("curl")
    (url,) = [argument for argument in download if argument.startswith("https://")]
    assert re.fullmatch(r"https://nodejs\.org/dist/v(22\.[\d.]+)/node-v\1-linux-(x64|arm64)"
                        r"\.tar\.xz", url), url
    assert "Nothing was unpacked" in run.stdout, run.stdout
    assert not (run.home / ".local" / "lib").exists()
    assert not (run.home / ".local" / "bin" / "node").exists()
    assert run.calls("cargo") == [] and run.calls("pnpm") == []


def test_setup_adds_the_pnpm_the_interface_is_locked_with(tmp_path):
    run = _setup(tmp_path, desktop=True, pnpm=False)
    user_bin = str(run.home / ".local" / "bin")
    assert run.calls("corepack") == [["enable", "--install-directory", user_bin, "pnpm"],
                                     ["install", "--global", f"pnpm@{PNPM_VERSION}"]]
    assert f"installed: pnpm {PNPM_VERSION} ({user_bin}/pnpm)" in run.stdout, run.stdout
    assert ["install", "--frozen-lockfile"] in run.calls("pnpm")


def test_setup_downloads_a_chromium_when_none_starts(tmp_path):
    run = _setup(tmp_path, desktop=True, chromium="missing")
    assert ["exec", "playwright", "install", "chromium"] in run.calls("pnpm")
    assert ["exec", "playwright", "install-deps", "chromium"] not in run.calls("pnpm")
    assert "installed: Chromium 1.2.3" in run.stdout, run.stdout


def test_setup_installs_chromiums_system_libraries_only_with_apt(tmp_path):
    install_deps = ["exec", "playwright", "install-deps", "chromium"]
    run = _run_script(tmp_path, "setup-wsl.sh", PLAIN, desktop=True, chromium="needs libraries")
    assert run.returncode != 0, run.stdout
    assert "pnpm exec playwright install-deps chromium)" in run.stdout, run.stdout
    assert install_deps not in run.calls("pnpm")
    assert "Setup is complete" not in run.stdout

    run = _setup(tmp_path / "apt", desktop=True, chromium="needs libraries", arguments=("--apt",))
    assert install_deps in run.calls("pnpm")
    assert "installed: Chromium 1.2.3" in run.stdout, run.stdout


def test_setup_uses_the_chromium_it_is_given_and_says_how_to_pass_it_on(tmp_path):
    browser = tmp_path / "my chromium"
    run = _setup(tmp_path, desktop=True, environment={"HOTEND_CHROMIUM": str(browser)})
    assert f"present:  Chromium 1.2.3 ({browser})" in run.stdout, run.stdout
    assert not [call for call in run.calls("pnpm") if "playwright" in call]
    hint = _hint(run, "HOTEND_CHROMIUM=")
    assert hint == f"HOTEND_CHROMIUM='{browser}' bash scripts/test-all.sh"


def test_setup_does_not_replace_a_named_chromium_that_does_not_start(tmp_path):
    run = _run_script(tmp_path, "setup-wsl.sh", PLAIN, desktop=True, chromium="missing",
                      environment={"HOTEND_CHROMIUM": "/nowhere/chrome"})
    assert run.returncode != 0, run.stdout
    assert "HOTEND_CHROMIUM=/nowhere/chrome did not start" in run.stdout, run.stdout
    assert not [call for call in run.calls("pnpm") if "playwright" in call]


def test_setup_help_is_the_whole_header_and_nothing_else(tmp_path):
    run = _run_script(tmp_path, "setup-wsl.sh", PLAIN, arguments=("--help",))
    assert run.returncode == 0, run.stdout
    lines = run.stdout.splitlines()
    assert lines[0].startswith("Sets up a clean Ubuntu 24.04"), lines[0]
    assert lines[-1].strip() == "the same value", lines[-3:]
    assert run.calls("cargo") == [] and run.calls("uv") == []


DESKTOP_PNPM_STEPS = [["install", "--frozen-lockfile"], ["typecheck"], ["lint"], ["test"],
                      ["build"], ["build:bridge"], ["e2e"]]


def test_test_all_runs_the_desktop_steps_when_the_checkout_has_the_desktop(tmp_path):
    run = _run_script(tmp_path, "test-all.sh", PLAIN, desktop=True)
    assert run.returncode == 0, run.stdout
    _assert_cargo_locked(
        run, ["build", "fmt", "clippy", "test", "clippy", "test", "run", "build"])
    assert run.calls("pnpm") == DESKTOP_PNPM_STEPS
    assert re.search(r"ok\s+desktop: end-to-end", run.stdout), run.stdout


@pytest.mark.parametrize(("options", "says"), [
    ({"pnpm": False}, "'pnpm' is not installed or not on PATH. Run: bash scripts/setup-wsl.sh"),
    ({"node_version": None},
     "'node' is not installed or not on PATH. Run: bash scripts/setup-wsl.sh"),
    ({"node_version": "v18.19.1"}, "needs Node 22 or newer"),
    ({"environment": {"HOTEND_CHROMIUM": "/nowhere/chrome"}},
     "HOTEND_CHROMIUM=/nowhere/chrome is not a program"),
], ids=["no pnpm", "no node", "old node", "no such chromium"])
def test_test_all_asks_for_what_the_desktop_steps_need_before_the_first_step(
        tmp_path, options, says):
    # The desktop steps are the last ones; finding out there costs half an hour.
    run = _run_script(tmp_path, "test-all.sh", PLAIN, desktop=True, **options)
    assert run.returncode == 2, run.stdout
    assert says in run.stdout, run.stdout
    assert run.calls("cargo") == [] and run.calls("uv") == [] and run.calls("pnpm") == []


def test_the_node_setup_installed_wins_over_an_older_one_earlier_on_path(tmp_path):
    # Ubuntu's own nodejs (18) in /usr/bin, ~/.local/bin already on PATH but after it: the
    # scripts must find the Node 22 that setup-wsl.sh put into ~/.local/bin.
    run = _run_script(tmp_path, "test-all.sh", PLAIN, desktop=True, node_version="v18.19.1",
                      user_node_version="v22.23.5")
    assert run.returncode == 0, run.stdout
    assert run.calls("pnpm") == DESKTOP_PNPM_STEPS
    run = _setup(tmp_path / "setup", desktop=True, node_version="v18.19.1",
                 user_node_version="v22.23.5")
    assert f"present:  node v22.23.5 ({run.home}/.local/bin/node)" in run.stdout, run.stdout
    assert run.calls("curl") == []
