#!/usr/bin/env bash
# Runs every check of the repository, in order, and stops at the first failure:
#
#   uv sync --frozen, the tests of these scripts and of the documents (scripts/tests),
#   ruff, pytest, the Python tests that read local reference files (when
#   HOTEND_REFS_DIR says where they are), cargo fmt --check, clippy -D warnings,
#   cargo test --workspace, the same two
#   for hotend-printer's `lan` feature (off by default; its tests talk to stand-in servers
#   on loopback inside the test process), the tests that need a real PrusaSlicer when one is
#   installed, then the offline end-to-end run (hotend demo) and the agent's long scripted
#   conversation, scored by the agent evaluation harness (python -m hotend_agent.evals
#   --scripted: real core, gateway, engine and the stand-in slicer; no model and no key),
#   and, when the checkout holds apps/desktop, the desktop interface: the development bridge
#   is built, the native print confirmation is shown on a virtual screen and pressed
#   (apps/desktop/src-tauri/tests/confirm_dialog.sh; needs xvfb-run and xdotool, and is
#   reported as skipped without them), then pnpm install --frozen-lockfile, typecheck, lint,
#   unit tests, build and the end-to-end tests in headless Chromium (needs Node 22 or newer,
#   pnpm and a Chromium; scripts/setup-wsl.sh installs all three).
#
# A skipped Python test fails the pytest step, with one exception. A few tests measure
# local reference files: other people's designs, which are never in the repository. They are
# listed, with the sizes expected of them, in reference-models.json in the same folder. Such
# a test skips with a reason that names HOTEND_REFS_DIR. The pytest step runs without that
# variable on every machine and takes those tests for the next step, which runs exactly them
# with the variable when it is set (a test that still skips then fails the step) and is
# reported as skipped, with their names, when it is not.
#
# Rust tests marked `ignore` are not run by `cargo test`; the script counts them, names
# them, and says in the summary which of them it ran afterwards. There are 24 today:
#   4   an installed PrusaSlicer (hotend-slicer real_prusaslicer; Hotend never installs a
#       slicer): with `prusa-slicer` on PATH they are run for real, in the sandbox; without
#       it that step is reported as skipped
#   8   an installed Bambu Studio (hotend-slicer real_bambu_studio); never run here
#   10  local slice files, which are not in the repository: hotend-printer real_slice (5) and
#       hotend-slicer real_proof (2) with HOTEND_REAL_SLICE_DIR, hotend-slicer real_reference
#       (3) with HOTEND_REAL_SLICE_REF; never run here
#   2   paid requests to a provider (hotend-core real_research); run by a person only
# The five of hotend-printer are counted twice in the summary: its tests run a second time
# with the `lan` feature.
#
# No step writes a lock file. Every cargo step passes --locked: if Cargo.lock no longer
# matches the Cargo.toml files the step fails and cargo says so; update the lock on purpose
# (run the cargo command once without --locked) and commit it with the manifest.
#
# Usage:   bash scripts/test-all.sh
# Options (environment):
#   HOTEND_CACHE_DIR=<dir>     keep the cargo target directory and the Python environment
#                               there (recommended when the checkout is on /mnt/<drive>/;
#                               see scripts/lib.sh)
#   HOTEND_TEST_ALLOW_SKIP=1   let tests that need the sandbox skip instead of fail, and
#                               accept skipped Python tests (they then prove nothing)
#   HOTEND_REFS_DIR=<dir>      the folder with local reference files and their list
#                               reference-models.json (not in the repository). Without it
#                               the tests that read them are not run, and the summary says so
#   HOTEND_CHROMIUM=<program>  the Chromium the desktop end-to-end tests run (default: the
#                               one of `playwright install chromium`, which
#                               scripts/setup-wsl.sh downloads)
#   HOTEND_TEST_REAL_PRUSASLICER=0
#                               do not run the real-PrusaSlicer tests even when one is
#                               installed (default: run them when `prusa-slicer` is on PATH)
#
# No test uses the network, a provider key or a printer; the printer is always the simulated
# one. The only real slicer a test runs is an installed PrusaSlicer, in the step named
# above. `uv sync` and cargo download locked dependencies the first time.

set -u -o pipefail

. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
add_user_tool_dirs_to_path
configure_build_dirs

STEPS=()      # name of every step, in order
RESULTS=()    # ok | FAIL | skipped | not run
SECONDS_TAKEN=()
FAILED=""

plan() { STEPS+=("$1"); RESULTS+=("not run"); SECONDS_TAKEN+=(""); }

plan "uv sync --frozen"
plan "scripts and docs tests"
plan "ruff check"
plan "build the sandbox launcher"
plan "pytest"
plan "pytest on local files"
plan "cargo fmt --check"
plan "cargo clippy -D warnings"
plan "cargo test --workspace"
plan "cargo clippy -D warnings (hotend-printer, lan)"
plan "cargo test (hotend-printer, lan)"
plan "real PrusaSlicer tests"
plan "hotend demo"
plan "agent eval self-check"
# The desktop interface (apps/desktop), when the checkout has it: its own type check, lint,
# unit tests and build, then its end-to-end tests in headless Chromium against the
# development bridge (scripted agent, stand-in slicer, simulated printer; loopback only).
DESKTOP="$REPO/apps/desktop"
if [ -f "$DESKTOP/package.json" ]; then
  plan "desktop: build the bridge"
  plan "desktop: confirmation dialog"
  plan "desktop: pnpm install"
  plan "desktop: typecheck"
  plan "desktop: lint"
  plan "desktop: unit tests"
  plan "desktop: build"
  plan "desktop: end-to-end"
fi

# A step that returns this did not apply on this machine and said why; it is not a failure.
SKIP=77
IGNORED_RUST_TESTS=0      # tests `cargo test` reported as ignored, over all cargo test steps
IGNORED_RUST_NAMES=""     # their names, one per line
IGNORED_RUST_RUN=""       # what was done about them, for the summary
LOCAL_FILE_TESTS=()       # the Python tests that skipped for want of HOTEND_REFS_DIR
LOCAL_FILE_RUN=""         # what was done about them, for the summary

CURRENT=-1

# step <command...>: runs the next planned step unless an earlier one failed.
step() {
  CURRENT=$((CURRENT + 1))
  [ -n "$FAILED" ] && return 0
  local name="${STEPS[$CURRENT]}" started=$SECONDS
  printf '\n==> [%d/%d] %s\n' "$((CURRENT + 1))" "${#STEPS[@]}" "$name"
  local status=0
  "$@" || status=$?
  if [ "$status" -eq 0 ]; then
    RESULTS[CURRENT]="ok"
  elif [ "$status" -eq "$SKIP" ]; then
    RESULTS[CURRENT]="skipped"
  else
    RESULTS[CURRENT]="FAIL"
    FAILED="$name"
  fi
  SECONDS_TAKEN[CURRENT]="$((SECONDS - started)) s"
}

summary() {
  local i
  printf '\n== Summary ==\n'
  for i in "${!STEPS[@]}"; do
    printf '  %-8s %-28s %s\n' "${RESULTS[$i]}" "${STEPS[$i]}" "${SECONDS_TAKEN[$i]}"
  done
  if [ -n "$LOCAL_FILE_RUN" ]; then
    printf '\nPython tests that read local reference files: %d (%s)\n' \
      "${#LOCAL_FILE_TESTS[@]}" "$LOCAL_FILE_RUN"
    [ "${#LOCAL_FILE_TESTS[@]}" -gt 0 ] && printf '  - %s\n' "${LOCAL_FILE_TESTS[@]}"
  fi
  printf '\nRust tests ignored by cargo test: %d%s\n' "$IGNORED_RUST_TESTS" \
    "${IGNORED_RUST_RUN:+ ($IGNORED_RUST_RUN)}"
  [ -n "$IGNORED_RUST_NAMES" ] && printf '%s\n' "$IGNORED_RUST_NAMES"
  if [ -n "$FAILED" ]; then
    printf '\nRESULT: FAILED at "%s". Its output is above; later steps were not run.\n' "$FAILED"
  else
    local skipped=0
    for i in "${!RESULTS[@]}"; do [ "${RESULTS[$i]}" = "skipped" ] && skipped=$((skipped + 1)); done
    if [ "$skipped" -gt 0 ]; then
      printf '\nRESULT: all %d steps passed (%d skipped, see above) in %d s.\n' \
        "${#STEPS[@]}" "$skipped" "$SECONDS"
    else
      printf '\nRESULT: all %d steps passed in %d s.\n' "${#STEPS[@]}" "$SECONDS"
    fi
  fi
}

need() {
  command -v "$1" >/dev/null 2>&1 && return 0
  say "test-all: '$1' is not installed or not on PATH. Run: bash scripts/setup-wsl.sh"
  exit 2
}
need uv
need cargo
# The desktop steps come last, after half an hour of other steps: what they cannot run
# without is asked for now.
if [ -f "$DESKTOP/package.json" ]; then
  need node
  need pnpm
  NODE_VERSION="$(node --version 2>/dev/null)"
  if ! version_at_least "${NODE_VERSION#v}" "$MIN_NODE"; then
    say "test-all: the desktop interface needs Node $MIN_NODE or newer; 'node' on PATH is"
    say "          ${NODE_VERSION:-not answering}. Run: bash scripts/setup-wsl.sh"
    exit 2
  fi
  if [ -n "${HOTEND_CHROMIUM:-}" ] && ! { [ -f "$HOTEND_CHROMIUM" ] && [ -x "$HOTEND_CHROMIUM" ]; }; then
    say "test-all: HOTEND_CHROMIUM=$HOTEND_CHROMIUM is not a program. Name a Chromium, or"
    say "          unset it to use the one scripts/setup-wsl.sh downloads."
    exit 2
  fi
fi

report_build_dirs

py() { (cd "$REPO/python" && "$@"); }
cargo_in_repo() { (cd "$REPO" && cargo "$@"); }

# pytest_listing_skips <log> <pytest arguments...>: runs pytest in python/, with every
# skipped test on a line of its own (`SKIPPED <test> - <reason>`), and keeps the output.
pytest_listing_skips() {
  local log="$1"
  shift
  # The agent tests run the agent inside the real sandbox; tell them where the launcher is.
  (cd "$REPO/python" && HOTEND_SANDBOX_BIN="$TARGET_DIR/debug/hotend-sandbox" \
    uv run --frozen pytest -q -rs --no-fold-skipped "$@") 2>&1 | tee "$log"
  return "${PIPESTATUS[0]}"
}

# Every Python test, without local reference files on any machine: a test that asks
# for them skips with a reason naming HOTEND_REFS_DIR and is kept for the next step. Any
# other skipped test fails this step.
run_pytest() {
  local log status=0 line name other=0
  log="$(mktemp "${TMPDIR:-/tmp}/hotend-pytest.XXXXXX")" || return 1
  (unset HOTEND_REFS_DIR && pytest_listing_skips "$log") || status=$?
  LOCAL_FILE_TESTS=()
  while IFS= read -r line; do
    case "$line" in
      "SKIPPED "*" - "*HOTEND_REFS_DIR*)
        name="${line#SKIPPED }"
        LOCAL_FILE_TESTS+=("${name%% - *}")
        ;;
      "SKIPPED "*) other=$((other + 1)) ;;
    esac
  done <"$log"
  rm -f "$log"
  [ "$status" -ne 0 ] && return "$status"
  if [ "$other" -gt 0 ] && [ "${HOTEND_TEST_ALLOW_SKIP:-}" != "1" ]; then
    say "test-all: $other Python test(s) were skipped (reasons above). A skipped test proves"
    say "          nothing; fix the cause, or set HOTEND_TEST_ALLOW_SKIP=1 to accept it."
    return 1
  fi
}

# The tests the pytest step kept for here, run with the local reference files. Those are
# other people's designs and are never in the repository, so on most machines this step
# is skipped and says which tests that leaves unproven. With HOTEND_REFS_DIR set a pass
# always means the files were read: a test that still skips fails the step.
run_local_file_tests() {
  local count=${#LOCAL_FILE_TESTS[@]} refs log status=0 skipped
  if [ "$count" -eq 0 ]; then
    say "test-all: no Python test asked for local reference files."
    return 0
  fi
  if [ -z "${HOTEND_REFS_DIR:-}" ]; then
    LOCAL_FILE_RUN="not run: HOTEND_REFS_DIR is not set"
    say "test-all: $count Python test(s) read local reference files, which are not in"
    say "          the repository, and were not run:"
    printf '            %s\n' "${LOCAL_FILE_TESTS[@]}"
    say "          To run them: HOTEND_REFS_DIR=<the folder with those files> bash scripts/test-all.sh"
    return "$SKIP"
  fi
  refs="$(absolute_path "$HOTEND_REFS_DIR")"
  if [ ! -d "$refs" ]; then
    LOCAL_FILE_RUN="not run: HOTEND_REFS_DIR is not a folder"
    say "test-all: HOTEND_REFS_DIR=$HOTEND_REFS_DIR is not a folder."
    return 1
  fi
  say "test-all: running $count test(s) on the files in $refs"
  log="$(mktemp "${TMPDIR:-/tmp}/hotend-pytest.XXXXXX")" || return 1
  HOTEND_REFS_DIR="$refs" pytest_listing_skips "$log" "${LOCAL_FILE_TESTS[@]}" || status=$?
  skipped="$(grep -c '^SKIPPED ' "$log" || true)"
  rm -f "$log"
  if [ "$status" -ne 0 ]; then
    LOCAL_FILE_RUN="run with HOTEND_REFS_DIR=$refs and failed"
    return "$status"
  fi
  if [ "$skipped" -gt 0 ] && [ "${HOTEND_TEST_ALLOW_SKIP:-}" != "1" ]; then
    LOCAL_FILE_RUN="$skipped of them still skipped with HOTEND_REFS_DIR=$refs"
    say "test-all: $skipped test(s) still skipped (reasons above): a file they read, or its"
    say "          entry in reference-models.json, is not in"
    say "          $refs. Put it there, or unset HOTEND_REFS_DIR to leave these tests out."
    return 1
  fi
  LOCAL_FILE_RUN="run with HOTEND_REFS_DIR=$refs"
}

# cargo_test <cargo test arguments...>: runs the tests and adds the ones cargo reports as
# ignored to the count the summary prints.
cargo_test() {
  local log status ignored names
  log="$(mktemp "${TMPDIR:-/tmp}/hotend-cargo-test.XXXXXX")" || return 1
  (cd "$REPO" && cargo test "$@") 2>&1 | tee "$log"
  status=${PIPESTATUS[0]}
  ignored="$(awk '/^test result:/ { for (i = 2; i <= NF; i++) if ($i ~ /^ignored;?$/) n += $(i - 1) }
                  END { print n + 0 }' "$log")"
  names="$(sed -n 's/^test \(.*\) \.\.\. ignored.*$/  - \1/p' "$log")"
  rm -f "$log"
  IGNORED_RUST_TESTS=$((IGNORED_RUST_TESTS + ignored))
  if [ -n "$names" ]; then
    IGNORED_RUST_NAMES="${IGNORED_RUST_NAMES:+$IGNORED_RUST_NAMES
}$names"
  fi
  say ""
  say "test-all: cargo test ignored $ignored test(s) in this step."
  return "$status"
}

# The tests that are ignored because they need an installed PrusaSlicer, run for real when
# there is one. Run like this, a test that does not find the program fails: a pass always
# means the real program ran (in the sandbox, like every slicer run).
run_real_prusaslicer_tests() {
  if [ "${HOTEND_TEST_REAL_PRUSASLICER:-}" = "0" ]; then
    IGNORED_RUST_RUN="the real-PrusaSlicer tests were not run: HOTEND_TEST_REAL_PRUSASLICER=0"
    say "test-all: $IGNORED_RUST_RUN"
    return "$SKIP"
  fi
  local program
  if ! program="$(command -v prusa-slicer 2>/dev/null)"; then
    IGNORED_RUST_RUN="the real-PrusaSlicer tests were not run: no prusa-slicer on PATH"
    say "test-all: $IGNORED_RUST_RUN."
    say "          Hotend never installs a slicer; on Ubuntu: sudo apt-get install -y prusa-slicer"
    return "$SKIP"
  fi
  say "test-all: found $program; running the tests that need it"
  if (cd "$REPO" && cargo test --locked -p hotend-slicer --test real_prusaslicer -- --ignored); then
    IGNORED_RUST_RUN="the real-PrusaSlicer tests were then run with $program"
  else
    IGNORED_RUST_RUN="the real-PrusaSlicer tests were run with $program and failed"
    return 1
  fi
}

run_demo() {
  local out
  out="$(mktemp -d "${TMPDIR:-/tmp}/hotend-demo.XXXXXX")" || return 1
  if cargo_in_repo run --locked --quiet -p hotend-cli -- demo --out "$out"; then
    say ""
    say "test-all: the demo's artefacts were kept in $out"
  else
    say "test-all: the demo failed; what it wrote is in $out"
    return 1
  fi
}

# The evaluation harness of the agent (python/hotend_agent/EVALS.md) on the shipped scripted
# conversation, through the `hotend` and `hotend-fake-slicer` binaries the cargo steps
# built. It asks no model and reads no key; without --scripted it would need a provider.
run_agent_eval_selfcheck() {
  (cd "$REPO/python" && HOTEND_EVAL_HOTEND="$TARGET_DIR/debug/hotend" \
    uv run --frozen python -m hotend_agent.evals --scripted)
}

desktop() { (cd "$DESKTOP" && "$@"); }

# The native print confirmation, opened by the hotend-confirm-dialog the step before built
# and answered with real key presses and clicks on a virtual screen. The script returns 77
# (skipped) when the machine has no Xvfb or xdotool, and says so.
run_confirm_dialog() {
  bash "$DESKTOP/src-tauri/tests/confirm_dialog.sh" "$TARGET_DIR/debug/hotend-confirm-dialog"
}

# The interface's end-to-end tests: the bridge build of the interface in headless Chromium,
# against the hotend-bridge the step before built. HOTEND_CHROMIUM names the Chromium to
# use; without it Playwright looks for the browser of its own `playwright install chromium`.
run_desktop_e2e() {
  desktop pnpm build:bridge || return 1
  (cd "$DESKTOP" && HOTEND_BRIDGE_BIN="$TARGET_DIR/debug/hotend-bridge" pnpm e2e)
}

# The tests of the scripts themselves (run against stand-in tools) and of what the documents
# promise. Kept out of python/: they belong to scripts/ and docs/, not to a Python package.
run_script_tests() {
  py uv run --frozen ruff check "$REPO/scripts/tests" \
    && py uv run --frozen pytest -q -p no:cacheprovider "$REPO/scripts/tests"
}

step py uv sync --frozen
step run_script_tests
step py uv run --frozen ruff check .
step cargo_in_repo build --locked -p hotend-sandbox
step run_pytest
step run_local_file_tests
step cargo_in_repo fmt --all -- --check
step cargo_in_repo clippy --locked --workspace --all-targets -- -D warnings
step cargo_test --locked --workspace
step cargo_in_repo clippy --locked -p hotend-printer --all-targets --features lan -- -D warnings
step cargo_test --locked -p hotend-printer --features lan
step run_real_prusaslicer_tests
step run_demo
step run_agent_eval_selfcheck
if [ -f "$DESKTOP/package.json" ]; then
  step cargo_in_repo build --locked -p hotend-desktop --features hotend-desktop/dev-bridge \
    -p hotend-sandbox -p hotend-slicer
  step run_confirm_dialog
  step desktop pnpm install --frozen-lockfile
  step desktop pnpm typecheck
  step desktop pnpm lint
  step desktop pnpm test
  step desktop pnpm build
  step run_desktop_e2e
fi

summary
[ -z "$FAILED" ]
