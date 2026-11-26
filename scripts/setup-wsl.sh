#!/usr/bin/env bash
# Sets up a clean Ubuntu 24.04 (WSL2 or native) to build, run and test Hotend (the
# `hotend` command and the desktop application), then runs `hotend doctor`. Safe to run
# again: every step first checks what is already there.
#
# Usage:   bash scripts/setup-wsl.sh [--apt]
#
#   --apt   install missing system packages with apt (uses sudo unless you are root).
#           Without it the script only prints the exact command and stops if a package
#           it cannot work without is missing.
#
# What it does, in order:
#   1. looks at the system: distribution, WSL, kernel (the sandbox needs Linux 6.2+);
#   2. checks the system packages; installs them only with --apt;
#   3. installs rustup and uv for your user if they are missing. Each installer is
#      downloaded to a file first and named before it runs; nothing is piped into a shell.
#      For the desktop interface it also needs Node 22 or newer and pnpm. Ubuntu 24.04's
#      own nodejs package is version 18, so a missing or older Node is installed for your
#      user from the official archive (nodejs.org), which is unpacked only if its SHA-256
#      is the one written in this script; pnpm, in the version apps/desktop/package.json
#      names, comes through corepack, which is part of Node;
#   4. creates the locked Python environment (uv sync --frozen), builds the workspace
#      (cargo build --locked) and installs the interface's locked packages (pnpm install
#      --frozen-lockfile). None of them writes a lock file; a Cargo.lock that no longer
#      matches the Cargo.toml files fails the build instead of being updated;
#   5. makes sure a Chromium starts for the desktop end-to-end tests: the one named by
#      HOTEND_CHROMIUM, else the one Playwright downloads for your user
#      (`playwright install chromium`). The system libraries that one may need are
#      installed only with --apt;
#   6. runs `hotend doctor`.
#
# It never installs a slicer, never asks for an API key and changes nothing outside your
# home directory unless you pass --apt.
#
# Environment:
#   HOTEND_CACHE_DIR=<dir>     keeps the cargo target directory and the Python environment
#                               on the Linux filesystem (recommended when the checkout is on
#                               /mnt/<drive>/; see scripts/lib.sh)
#   HOTEND_CHROMIUM=<program>  a Chromium you already have, for the desktop end-to-end
#                               tests; nothing is downloaded then. Give scripts/test-all.sh
#                               the same value

set -u -o pipefail

. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

USE_APT=0
for arg in "$@"; do
  case "$arg" in
    --apt) USE_APT=1 ;;
    -h | --help)
      sed -n '2,41p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      say "setup-wsl: unknown option '$arg' (the only option is --apt)"
      exit 2
      ;;
  esac
done

# System packages: what each is for.
#   build-essential  C compiler and linker for the Rust crates that build C code
#                    (bundled SQLite, the TLS library)
#   curl, ca-certificates   downloading the rustup and uv installers
#   git              working with the repository
#   xz-utils         unpacking the Node archive
#   libgl1           libGL and libX11, which the OpenCascade CAD kernel links against
#                    (no display is used)
#   libwebkit2gtk-4.1-dev, libgtk-3-dev, libsoup-3.0-dev, libjavascriptcoregtk-4.1-dev,
#   librsvg2-dev     what the desktop application (Tauri on WebKitGTK) is built against;
#                    without them `cargo build --workspace` stops in glib-sys. They bring
#                    pkg-config and the D-Bus headers with them
#   xdg-utils        xdg-open, with which the application opens a link in your browser
#   xvfb, xdotool    tests only: the virtual screen and the key presses with which
#                    scripts/test-all.sh holds the native print confirmation to its rules
#                    (without them that step is reported as skipped)
APT_PACKAGES=(build-essential curl ca-certificates git xz-utils libgl1
  libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev
  librsvg2-dev xdg-utils xvfb xdotool)
RUSTUP_INSTALLER_URL="https://sh.rustup.rs"
UV_INSTALLER_URL="https://astral.sh/uv/install.sh"
MIN_RUST="1.90"
# The Node this script installs when the machine has none of version MIN_NODE (lib.sh) or
# newer: the official Linux archive, and its SHA-256 as published in
# https://nodejs.org/dist/v<version>/SHASUMS256.txt. Change all three together.
NODE_VERSION="22.23.5"
NODE_SHA256_X64="cdeb8670a0c81f51b4dc4ac61919f9b1ad917b304bf8221eb939570663d240f8"
NODE_SHA256_ARM64="2babb9eb5264860d540849f4b064a6fbdebcdc323657d9d382ce23d313fd5e09"
NODE_DIST_URL="https://nodejs.org/dist"
DESKTOP="$REPO/apps/desktop"

section() { printf '\n== %s ==\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }
die() {
  say ""
  say "setup-wsl: $*"
  exit 1
}

# ---- 1. the system ---------------------------------------------------------------------

section "System"
if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  say "Distribution:  ${PRETTY_NAME:-unknown}"
  if [ "${ID:-}" != "ubuntu" ] || [ "${VERSION_ID:-}" != "24.04" ]; then
    note "This script is written for Ubuntu 24.04. It will try anyway; package names may differ."
  fi
else
  say "Distribution:  unknown (no /etc/os-release)"
fi
KERNEL="$(uname -r)"
if grep -qi microsoft /proc/version 2>/dev/null || [ -n "${WSL_DISTRO_NAME:-}" ]; then
  say "WSL:           yes (${WSL_DISTRO_NAME:-distribution name unknown})"
  IS_WSL=1
else
  say "WSL:           no"
  IS_WSL=0
fi
say "Kernel:        $KERNEL"
if ! version_at_least "${KERNEL%%-*}" "6.2"; then
  note "The sandbox needs Linux 6.2 or newer (Landlock ABI 3). On this kernel Hotend"
  note "refuses to run model code, and 'hotend doctor' below will say so."
  if [ "$IS_WSL" = 1 ]; then
    note "Fix: in Windows PowerShell run 'wsl --update', then 'wsl --shutdown', and start"
    note "this distribution again."
  fi
fi

add_user_tool_dirs_to_path
configure_build_dirs
report_build_dirs

# ---- 2. system packages ----------------------------------------------------------------

section "System packages"
MISSING=()
if have dpkg-query; then
  for package in "${APT_PACKAGES[@]}"; do
    if dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q "install ok installed"; then
      say "present:  $package"
    else
      say "MISSING:  $package"
      MISSING+=("$package")
    fi
  done
else
  note "dpkg is not available, so system packages were not checked."
  note "Needed: ${APT_PACKAGES[*]} (or what your distribution calls them)."
fi

if [ "${#MISSING[@]}" -gt 0 ]; then
  SUDO=(sudo)
  [ "$(id -u)" = 0 ] && SUDO=()
  APT_COMMAND="${SUDO[*]:+${SUDO[*]} }apt-get update && ${SUDO[*]:+${SUDO[*]} }apt-get install -y ${MISSING[*]}"
  say ""
  say "To install what is missing:"
  say "    $APT_COMMAND"
  if [ "$USE_APT" = 1 ]; then
    say "Running it now (--apt was given)."
    "${SUDO[@]}" apt-get update || die "apt-get update failed."
    "${SUDO[@]}" apt-get install -y "${MISSING[@]}" || die "apt-get install failed."
  else
    die "Run the command above (it needs sudo), or run this script again with --apt. Then run this script again."
  fi
fi

# ---- 3. user-level tools ---------------------------------------------------------------

# download <url> <what it is>: downloads to a file, says what it is, prints its SHA-256 and
# sets DOWNLOADED to the file and DOWNLOADED_SHA256 to its hash. Nothing is executed here.
download() {
  DOWNLOADED="$(mktemp "${TMPDIR:-/tmp}/hotend-download.XXXXXX")" || die "cannot create a temporary file."
  say "Downloading $2"
  say "    from $1"
  say "    to   $DOWNLOADED"
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location "$1" --output "$DOWNLOADED" \
    || die "the download failed. Check the network connection and run this script again."
  DOWNLOADED_SHA256="$(sha256sum "$DOWNLOADED" | cut -d' ' -f1)"
  say "    sha256 $DOWNLOADED_SHA256"
}

# fetch_installer <url> <what it is>: downloads an installer script and sets INSTALLER to
# the file. Nothing is executed here.
fetch_installer() {
  download "$@"
  INSTALLER="$DOWNLOADED"
  say "    You can read the file before it runs; it is a shell script."
}

section "Rust"
if have cargo && have rustc; then
  RUST_VERSION="$(rustc --version | awk '{print $2}')"
  say "present:  rustc $RUST_VERSION ($(command -v rustc))"
  if ! version_at_least "$RUST_VERSION" "$MIN_RUST"; then
    if have rustup; then
      say "Hotend needs Rust $MIN_RUST or newer; updating the stable toolchain with rustup."
      rustup update stable || die "rustup update failed."
    else
      die "Hotend needs Rust $MIN_RUST or newer and this Rust was not installed with rustup. Update it, or remove it and run this script again to get rustup."
    fi
  fi
else
  fetch_installer "$RUSTUP_INSTALLER_URL" "the rustup installer (the official Rust toolchain installer, from rust-lang.org)"
  say "Running it: installs rustup, cargo and rustc for your user under ~/.rustup and ~/.cargo."
  sh "$INSTALLER" -y --profile minimal --default-toolchain stable --no-modify-path \
    || die "the rustup installer failed."
  rm -f "$INSTALLER"
  add_user_tool_dirs_to_path
  have cargo || die "cargo is still not on PATH after installing rustup; open a new shell and run this script again."
  note "Add ~/.cargo/bin to your PATH for new shells:  echo '. \"\$HOME/.cargo/env\"' >> ~/.bashrc"
fi
if have rustup; then
  for component in rustfmt clippy; do
    if rustup component list --installed 2>/dev/null | grep -q "^$component"; then
      say "present:  $component"
    else
      say "Adding the $component component."
      rustup component add "$component" || die "cannot add $component."
    fi
  done
else
  have rustfmt || note "rustfmt was not found; scripts/test-all.sh needs it (cargo fmt)."
  have cargo-clippy || note "clippy was not found; scripts/test-all.sh needs it (cargo clippy)."
fi

section "uv (Python environments)"
if have uv; then
  say "present:  $(uv --version) ($(command -v uv))"
else
  fetch_installer "$UV_INSTALLER_URL" "the uv installer (Astral's Python package and environment manager)"
  say "Running it: installs the uv binary for your user under ~/.local/bin."
  UV_NO_MODIFY_PATH=1 sh "$INSTALLER" || die "the uv installer failed."
  rm -f "$INSTALLER"
  add_user_tool_dirs_to_path
  have uv || die "uv is still not on PATH after installing it; open a new shell and run this script again."
  note "Add ~/.local/bin to your PATH for new shells:  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc"
fi

# Node and pnpm, for the desktop interface (apps/desktop).
node_is_new_enough() {
  local version
  have node || return 1
  version="$(node --version 2>/dev/null)" || return 1
  version_at_least "${version#v}" "$MIN_NODE"
}

# Installs the official Node archive under ~/.local/lib and links node, npm, npx and
# corepack into ~/.local/bin. The archive is unpacked only if its hash is the pinned one.
install_node() {
  local arch sha name
  case "$(uname -m)" in
    x86_64) arch="x64" sha="$NODE_SHA256_X64" ;;
    aarch64 | arm64) arch="arm64" sha="$NODE_SHA256_ARM64" ;;
    *) die "this script installs Node only for x86_64 and arm64 Linux, and this machine is $(uname -m). Install Node $MIN_NODE or newer yourself and run this script again." ;;
  esac
  name="node-v$NODE_VERSION-linux-$arch"
  download "$NODE_DIST_URL/v$NODE_VERSION/$name.tar.xz" "Node $NODE_VERSION (the official build, from nodejs.org)"
  if [ "$DOWNLOADED_SHA256" != "$sha" ]; then
    rm -f "$DOWNLOADED"
    die "that file is not the Node $NODE_VERSION this script knows (expected sha256 $sha). Nothing was unpacked. Run this script again; if it happens again, do not use the download."
  fi
  say "The hash is the expected one. Unpacking into ~/.local/lib/$name and linking"
  say "node, npm, npx and corepack into ~/.local/bin."
  mkdir -p "$HOME/.local/lib" "$HOME/.local/bin" || die "cannot create ~/.local/lib and ~/.local/bin."
  tar -xJf "$DOWNLOADED" -C "$HOME/.local/lib" || die "cannot unpack the Node archive."
  rm -f "$DOWNLOADED"
  local tool
  for tool in node npm npx corepack; do
    ln -sfn "$HOME/.local/lib/$name/bin/$tool" "$HOME/.local/bin/$tool" || die "cannot link $tool into ~/.local/bin."
  done
  add_user_tool_dirs_to_path
  hash -r
}

if [ -f "$DESKTOP/package.json" ]; then
  section "Node (the desktop interface)"
  if node_is_new_enough; then
    say "present:  node $(node --version) ($(command -v node))"
  else
    if have node; then
      say "found:    node $(node --version 2>/dev/null) ($(command -v node)), but the interface needs $MIN_NODE or newer."
    fi
    note "Ubuntu 24.04's own nodejs package is version 18, so Node comes from nodejs.org."
    install_node
    node_is_new_enough || die "node on PATH is still $(node --version 2>/dev/null || echo missing) ($(command -v node || echo 'not found')). Put ~/.local/bin first on PATH and run this script again."
    say "installed: node $(node --version) ($(command -v node))"
    note "Add ~/.local/bin to your PATH for new shells:  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc"
  fi

  section "pnpm"
  # The version the interface is locked with: "packageManager": "pnpm@<version>".
  PNPM_VERSION="$(sed -n 's/.*"packageManager": *"pnpm@\([0-9][0-9.]*\)".*/\1/p' "$DESKTOP/package.json")"
  [ -n "$PNPM_VERSION" ] || die "apps/desktop/package.json does not name a pnpm version (\"packageManager\")."
  if have pnpm; then
    say "present:  pnpm $(cd "$DESKTOP" && pnpm --version) ($(command -v pnpm)); the interface is locked with $PNPM_VERSION"
  else
    have corepack || die "corepack was not found (it is part of Node $MIN_NODE). Install pnpm $PNPM_VERSION yourself and run this script again."
    say "Adding pnpm $PNPM_VERSION with corepack (part of Node): its launcher goes to ~/.local/bin,"
    say "pnpm itself is downloaded from the npm registry."
    mkdir -p "$HOME/.local/bin" || die "cannot create ~/.local/bin."
    corepack enable --install-directory "$HOME/.local/bin" pnpm || die "corepack enable failed."
    COREPACK_ENABLE_DOWNLOAD_PROMPT=0 corepack install --global "pnpm@$PNPM_VERSION" \
      || die "corepack could not download pnpm $PNPM_VERSION."
    add_user_tool_dirs_to_path
    hash -r
    have pnpm || die "pnpm is still not on PATH after adding it; open a new shell and run this script again."
    say "installed: pnpm $(cd "$DESKTOP" && pnpm --version) ($(command -v pnpm))"
  fi
fi

# ---- 4. the Python environment and the build -------------------------------------------

section "Python environment (uv sync --frozen)"
note "Installs exactly the versions in python/uv.lock into $VENV_DIR."
note "uv downloads Python 3.13 itself if the system has none; the first run takes a few minutes."
(cd "$REPO/python" && uv sync --frozen) || die "uv sync failed. The output above says why."

section "Build (cargo build --workspace --locked)"
note "The first build compiles every dependency and takes several minutes."
(cd "$REPO" && cargo build --workspace --locked) || die "the build failed. The output above says why."

CHROMIUM_NOTE=""
if [ -f "$DESKTOP/package.json" ]; then
  section "Desktop interface packages (pnpm install --frozen-lockfile)"
  note "Installs exactly the versions in apps/desktop/pnpm-lock.yaml into apps/desktop/node_modules."
  (cd "$DESKTOP" && pnpm install --frozen-lockfile) || die "pnpm install failed. The output above says why."

  # ---- 5. a Chromium for the desktop end-to-end tests ----------------------------------

  section "Chromium (the desktop end-to-end tests)"
  if CHROMIUM="$(chromium_launches 2>&1)"; then
    say "present:  $CHROMIUM"
  elif [ -n "${HOTEND_CHROMIUM:-}" ]; then
    say "$CHROMIUM"
    die "HOTEND_CHROMIUM=$HOTEND_CHROMIUM did not start (see above). Name a Chromium that does, or unset it and this script downloads one."
  else
    say "$CHROMIUM"
    say "No Chromium starts yet. Downloading the one this version of Playwright tests with,"
    say "for your user (into ~/.cache/ms-playwright unless PLAYWRIGHT_BROWSERS_PATH says otherwise)."
    (cd "$DESKTOP" && pnpm exec playwright install chromium) \
      || die "the download failed. If you have a Chromium, name it: HOTEND_CHROMIUM=<program> bash scripts/setup-wsl.sh"
    if ! CHROMIUM="$(chromium_launches 2>&1)"; then
      say "$CHROMIUM"
      say ""
      say "Chromium was downloaded but does not start: it needs system libraries. To install them:"
      say "    (cd $(shell_quote "$DESKTOP") && pnpm exec playwright install-deps chromium)"
      if [ "$USE_APT" = 1 ]; then
        say "Running it now (--apt was given)."
        (cd "$DESKTOP" && pnpm exec playwright install-deps chromium) || die "installing Chromium's system libraries failed."
        CHROMIUM="$(chromium_launches 2>&1)" || {
          say "$CHROMIUM"
          die "Chromium still does not start. The output above says why."
        }
      else
        die "Run the command above (it uses apt, with sudo unless you are root), or run this script again with --apt. Then run this script again."
      fi
    fi
    say "installed: $CHROMIUM"
  fi
  if [ -n "${HOTEND_CHROMIUM:-}" ]; then
    CHROMIUM_NOTE="HOTEND_CHROMIUM=$(shell_quote "$HOTEND_CHROMIUM") "
  fi
fi

# ---- 6. hotend doctor -----------------------------------------------------------------

section "hotend doctor"
HOTEND="$TARGET_DIR/debug/hotend"
[ -x "$HOTEND" ] || die "the build finished but $HOTEND is missing."
"$HOTEND" doctor
DOCTOR=$?

say ""
if [ "$DOCTOR" -eq 0 ]; then
  say "Setup is complete. Next:"
  say "    $(shell_quote "$HOTEND") demo"
  say "        the whole loop offline, about half a minute"
  say "    ${CHROMIUM_NOTE}bash scripts/test-all.sh"
  say "        every test, then the demo (about half an hour)"
else
  say "Setup finished, but 'hotend doctor' reports a missing hard requirement (see above)."
  say "Fix it and run this script again."
fi
if [ -n "${HOTEND_CACHE_DIR:-}" ]; then
  say ""
  say "You used HOTEND_CACHE_DIR=$HOTEND_CACHE_DIR. Use the same value with"
  say "scripts/test-all.sh, or make it permanent:"
  say "    echo $(shell_quote "export HOTEND_CACHE_DIR=$(shell_quote "$HOTEND_CACHE_DIR")") >> ~/.bashrc"
  say "To run hotend from another shell without it, also set:"
  say "    export HOTEND_PYTHON=$(shell_quote "$VENV_DIR/bin/python") HOTEND_PYTHON_SRC=$(shell_quote "$REPO/python")"
fi
exit "$DOCTOR"
