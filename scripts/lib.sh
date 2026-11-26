# shellcheck shell=bash
# Shared by setup-wsl.sh and test-all.sh. Source it; do not run it.
#
# Sets REPO (the repository root) and decides where build outputs go.
#
# Build outputs and the Windows drive
#   A checkout under /mnt/<drive>/ works, but the cargo target directory and the Python
#   environment are several times slower there, and Windows tools may lock their files.
#   Set HOTEND_CACHE_DIR to a folder on the Linux filesystem to move both:
#
#       HOTEND_CACHE_DIR=~/.cache/hotend bash scripts/test-all.sh
#
#   That sets CARGO_TARGET_DIR=$HOTEND_CACHE_DIR/target and
#   UV_PROJECT_ENVIRONMENT=$HOTEND_CACHE_DIR/venv unless you set those yourself, and tells
#   Hotend and its tests where the Python environment is (HOTEND_PYTHON,
#   HOTEND_PYTHON_SRC, HOTEND_TEST_PYTHON).

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

# The desktop interface (apps/desktop/package.json, `engines`) needs this Node or newer.
MIN_NODE="22"

say() { printf '%s\n' "$*"; }
note() { printf '    %s\n' "$*"; }

# version_at_least <have> <need>: compares dotted versions.
version_at_least() {
  [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n 1)" = "$2" ]
}

# Prints $1 as one shell word, for a command the user is told to paste: unchanged when it
# holds only characters no shell treats specially, in single quotes otherwise.
shell_quote() {
  local escaped_quote="'\\''"
  case "$1" in
    "" | *[!A-Za-z0-9_@%+=:,./-]*) printf "'%s'" "${1//\'/$escaped_quote}" ;;
    *) printf '%s' "$1" ;;
  esac
}

# True when the checkout is on a Windows drive mounted into WSL.
on_windows_drive() {
  case "$REPO" in
    /mnt/[a-zA-Z]/* | /mnt/[a-zA-Z]) return 0 ;;
    *) return 1 ;;
  esac
}

# The folder a relative path resolves to, made absolute without requiring it to exist.
absolute_path() {
  # The quoted tildes match a tilde that reached us unexpanded (from a quoted value).
  # shellcheck disable=SC2088
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    "~") printf '%s\n' "$HOME" ;;
    "~/"*) printf '%s\n' "$HOME/${1#"~/"}" ;;
    *) printf '%s\n' "$PWD/$1" ;;
  esac
}

configure_build_dirs() {
  if [ -n "${HOTEND_CACHE_DIR:-}" ]; then
    HOTEND_CACHE_DIR="$(absolute_path "$HOTEND_CACHE_DIR")"
    export HOTEND_CACHE_DIR
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOTEND_CACHE_DIR/target}"
    export UV_PROJECT_ENVIRONMENT="${UV_PROJECT_ENVIRONMENT:-$HOTEND_CACHE_DIR/venv}"
    mkdir -p "$HOTEND_CACHE_DIR"
  fi
  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    CARGO_TARGET_DIR="$(absolute_path "$CARGO_TARGET_DIR")"
    export CARGO_TARGET_DIR
  fi
  TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"

  if [ -n "${UV_PROJECT_ENVIRONMENT:-}" ]; then
    UV_PROJECT_ENVIRONMENT="$(absolute_path "$UV_PROJECT_ENVIRONMENT")"
    export UV_PROJECT_ENVIRONMENT
    # Hotend looks for python/.venv by default; say where the environment really is.
    export HOTEND_PYTHON="${HOTEND_PYTHON:-$UV_PROJECT_ENVIRONMENT/bin/python}"
    export HOTEND_PYTHON_SRC="${HOTEND_PYTHON_SRC:-$REPO/python}"
    # The sandbox crate's tests run their children with this interpreter.
    export HOTEND_TEST_PYTHON="${HOTEND_TEST_PYTHON:-$UV_PROJECT_ENVIRONMENT/bin/python}"
  fi
  VENV_DIR="${UV_PROJECT_ENVIRONMENT:-$REPO/python/.venv}"

  if on_windows_drive; then
    # uv links packages from its cache; across filesystems it can only copy.
    export UV_LINK_MODE="${UV_LINK_MODE:-copy}"
  fi
}

# Says where things go, and recommends the Linux filesystem when on a Windows drive.
report_build_dirs() {
  say "Repository:    $REPO"
  say "Cargo target:  $TARGET_DIR"
  say "Python env:    $VENV_DIR"
  if on_windows_drive; then
    case "$TARGET_DIR$VENV_DIR" in
      */mnt/[a-zA-Z]/*)
        say ""
        say "NOTE: this checkout is on a Windows drive, and so are the build outputs above."
        say "      It works, but builds and Python start-up are several times slower there."
        say "      Recommended: keep them on the Linux filesystem by running"
        say "          HOTEND_CACHE_DIR=~/.cache/hotend bash scripts/$(basename "$0")"
        say "      (or put 'export HOTEND_CACHE_DIR=~/.cache/hotend' in ~/.bashrc)."
        ;;
    esac
  fi
  warn_if_root_outputs_are_private
}

# Prints the nearest existing folder at or above $1 that other users cannot enter, if any.
closed_to_other_users() {
  local dir="$1" mode
  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    if [ -d "$dir" ]; then
      mode="$(stat -c '%a' "$dir" 2>/dev/null)" || return 1
      case "$mode" in
        *[1357]) ;;
        *)
          printf '%s\n' "$dir"
          return 0
          ;;
      esac
    fi
    dir="$(dirname "$dir")"
  done
  return 1
}

# One sandbox test repeats its checks as the user 'nobody' when the tests run as root. That
# user must be able to reach the launcher and the Python environment.
warn_if_root_outputs_are_private() {
  [ "$(id -u)" = 0 ] || return 0
  local closed
  closed="$(closed_to_other_users "$TARGET_DIR")" || closed="$(closed_to_other_users "$VENV_DIR")" || return 0
  say ""
  say "NOTE: you are root and the build outputs are below $closed, which other users"
  say "      cannot enter. One sandbox test runs as the user 'nobody' and will fail there"
  say "      ('cannot start the launcher: Permission denied'). Run as an ordinary user, or"
  say "      use a folder everyone can enter, for example HOTEND_CACHE_DIR=/var/cache/hotend."
}

# Tools installed for the user live here; a fresh shell may not have them on PATH yet.
add_user_tool_dirs_to_path() {
  local dir version
  for dir in "$HOME/.cargo/bin" "$HOME/.local/bin"; do
    case ":$PATH:" in
      *":$dir:"*) ;;
      *) [ -d "$dir" ] && PATH="$dir:$PATH" ;;
    esac
  done
  # The Node setup-wsl.sh installed for the user wins over an older one that comes earlier
  # on PATH (Ubuntu's own nodejs package, or the one a CI image ships).
  if [ -x "$HOME/.local/bin/node" ]; then
    version="$(node --version 2>/dev/null)" || version=""
    if ! version_at_least "${version#v}" "$MIN_NODE"; then
      PATH="$HOME/.local/bin:$PATH"
      hash -r
    fi
  fi
  export PATH
}

# True when the Chromium the desktop end-to-end tests use can be started: the program named
# by HOTEND_CHROMIUM, else the browser of Playwright's own `playwright install chromium`
# (apps/desktop/playwright.config.ts makes the same choice). Prints its version and which
# of the two it is, or the first line of why it did not start. Needs
# apps/desktop/node_modules.
chromium_launches() {
  (cd "$REPO/apps/desktop" && node --input-type=module -e '
    const named = process.env.HOTEND_CHROMIUM;
    try {
      const { chromium } = await import("@playwright/test");
      const browser = await chromium.launch(named ? { executablePath: named } : {});
      console.log(`Chromium ${browser.version()} (${named || "the one Playwright downloaded"})`);
      await browser.close();
    } catch (error) {
      console.log(String(error.message ?? error).split("\n")[0]);
      process.exit(1);
    }')
}
