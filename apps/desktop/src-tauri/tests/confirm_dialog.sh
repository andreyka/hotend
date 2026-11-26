#!/usr/bin/env bash
# The native print confirmation, held to what src/dialog.rs promises, with the real dialog
# on a virtual screen (Xvfb) and real key presses and clicks (xdotool):
#
#   1. Return on a freshly opened dialog cancels; the job stays `pending`.
#   2. Space on a freshly opened dialog cancels; the job stays `pending`.
#   3. Return still cancels after the confirming button woke up: Cancel is the default
#      button, not only the first second.
#   4. A click on the confirming button in the dialog's first second does nothing; the
#      same click after that second confirms.
#
# The project is named `50% scale %s%n`: the dialog must open with a name that would crash
# the process if any part of the text were read as a printf format.
#
# Usage: confirm_dialog.sh <hotend-confirm-dialog program> [<screenshot.png>]
# The program is built by: cargo build -p hotend-desktop --features hotend-desktop/dev-bridge
# Exit status: 0 passed, 1 failed, 77 not run (no Xvfb or xdotool on this machine; says so).
# Nothing can be printed: the program uses the simulated printer and the stand-in slicer.

set -u -o pipefail

TOOL="${1:?usage: confirm_dialog.sh <hotend-confirm-dialog program> [<screenshot.png>]}"
SHOT="${2:-}"
NAME='50% scale %s%n'
TITLE='^Start this print\?$'

say() { printf '%s\n' "$*" >&2; }

if [ -z "${HOTEND_DIALOG_ON_SCREEN:-}" ]; then
  for program in xvfb-run xdotool; do
    if ! command -v "$program" >/dev/null 2>&1; then
      say "confirm_dialog: '$program' is not installed, so the dialog cannot be shown and"
      say "                pressed here. On Ubuntu: sudo apt-get install -y xvfb xdotool"
      exit 77
    fi
  done
  [ -x "$TOOL" ] || { say "confirm_dialog: $TOOL is not a program"; exit 1; }
  # The dialog must open on the virtual screen and nowhere else. Under WSLg or a Wayland
  # desktop GTK would prefer the session's Wayland display and put a real "Start this
  # print?" dialog on the person's own screen, where xdotool cannot reach it.
  HOTEND_DIALOG_ON_SCREEN=1 exec env -u WAYLAND_DISPLAY GDK_BACKEND=x11 \
    xvfb-run -a -s "-screen 0 1280x800x24" bash "${BASH_SOURCE[0]}" "$TOOL" "$SHOT"
fi

# Second line of defence: whatever started this part, GTK talks to X11 only.
unset WAYLAND_DISPLAY
export GDK_BACKEND=x11

WORK="$(mktemp -d "${TMPDIR:-/tmp}/hotend-confirm-dialog.XXXXXX")" || exit 1
ANSWERS="$WORK/answers"
LOG="$WORK/log"
: >"$ANSWERS"
"$TOOL" --name "$NAME" --asks 4 >"$ANSWERS" 2>"$LOG" &
TOOL_PID=$!

finish() {
  if [ -n "$TOOL_PID" ]; then
    kill "$TOOL_PID" 2>/dev/null
    wait "$TOOL_PID" 2>/dev/null
  fi
  rm -rf "$WORK"
}
trap finish EXIT

fail() {
  say "confirm_dialog: FAILED: $*"
  say "--- answers so far"
  cat "$ANSWERS" >&2
  say "--- the program's messages"
  tail -n 20 "$LOG" >&2
  exit 1
}

# dialog <step> <id of the dialog before>: the window id of the dialog that is open now,
# which is not the one before (an answered dialog is still on screen for a moment). Polls
# quickly: step 4 clicks in the dialog's first second.
dialog() {
  local id tries=0
  while [ "$tries" -lt 3600 ]; do
    kill -0 "$TOOL_PID" 2>/dev/null || fail "the program ended before a dialog opened ($1)"
    id="$(xdotool search --onlyvisible --name "$TITLE" 2>/dev/null | grep -vx "${2:-none}" \
      | head -n 1)"
    if [ -n "$id" ]; then
      printf '%s\n' "$id"
      return 0
    fi
    sleep 0.05
    tries=$((tries + 1))
  done
  fail "no dialog opened within three minutes ($1)"
}

answers() { wc -l <"$ANSWERS" | tr -d ' '; }

# expect <number> <text>: the program's answer number <number> arrives and is <text>.
expect() {
  local tries=0 line
  while [ "$(answers)" -lt "$1" ]; do
    [ "$tries" -ge 300 ] && fail "answer $1 did not arrive within 30 s"
    sleep 0.1
    tries=$((tries + 1))
  done
  line="$(sed -n "${1}p" "$ANSWERS")"
  [ "$line" = "$2" ] || fail "answer $1 is $line, expected $2"
}

PENDING='{"confirmed":false,"status":"pending"}'

# 1. Return on a fresh dialog.
id="$(dialog "step 1")" || exit 1
xdotool windowfocus --sync "$id"
if [ -n "$SHOT" ]; then
  sleep 0.3
  import -window root "$SHOT" || fail "the screenshot could not be taken"
fi
xdotool key Return
expect 1 "$PENDING"
say "confirm_dialog: 1/4 Return on a fresh dialog cancelled"

# 2. Space on a fresh dialog.
id="$(dialog "step 2" "$id")" || exit 1
xdotool windowfocus --sync "$id"
xdotool key space
expect 2 "$PENDING"
say "confirm_dialog: 2/4 Space on a fresh dialog cancelled"

# 3. Return after the first second.
id="$(dialog "step 3" "$id")" || exit 1
xdotool windowfocus --sync "$id"
sleep 1.5
xdotool key Return
expect 3 "$PENDING"
say "confirm_dialog: 3/4 Return after the first second cancelled"

# 4. The confirming button: the right-hand one of the two along the bottom edge.
id="$(dialog "step 4" "$id")" || exit 1
eval "$(xdotool getwindowgeometry --shell "$id")"
at_x=$((X + WIDTH * 3 / 4))
at_y=$((Y + HEIGHT - 18))
xdotool mousemove "$at_x" "$at_y" click 1
sleep 1.6
[ "$(answers)" -eq 3 ] || fail "a click in the dialog's first second was taken as an answer"
[ -n "$(xdotool search --onlyvisible --name "$TITLE" 2>/dev/null)" ] \
  || fail "the dialog closed on a click in its first second"
xdotool mousemove "$at_x" "$at_y" click 1
expect 4 '{"confirmed":true,"status":"starting"}'
say "confirm_dialog: 4/4 the confirming button was dead for its first second, then confirmed"

wait "$TOOL_PID"
status=$?
TOOL_PID=""
[ "$status" -eq 0 ] || fail "the program ended with status $status"
say "confirm_dialog: passed (project name: $NAME)"
