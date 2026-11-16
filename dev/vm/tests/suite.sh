#!/bin/bash
# End-to-end regression suite, run from the host: dev/vm/tests/suite.sh [x11|wayland]...
# Drives the real desktops over VNC (keyboard/mouse) and asserts via
# `ghostdrop ctl dump`, files written by the shell, and the system clipboard.
# Opens a normal window below the dropdown if needed (foot on sway, xterm on i3).
# Note: every `drive` is a fresh VNC client whose pointer starts at (0,0), so
# always `move` before clicking within the same drive.
set -u
cd "$(dirname "$0")/../../.."
VM=dev/vm/vm.sh
PASS=0 FAIL=0

ok() { PASS=$((PASS + 1)); echo "  ✓ $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  ✗ $1  (got: $2)"; }
expect() { [ "$2" = "$3" ] && ok "$1" || bad "$1" "$2"; }

[ $# -gt 0 ] || set -- x11 wayland
for be in "$@"; do
  if [ "$be" = x11 ]; then envf=x11_env; off=0; paste_cmd="xclip -o -selection clipboard"
    set_clip='setsid -f sh -c "printf %s \"echo pasted-ok > /tmp/suite-paste.txt\" | xclip -selection clipboard" >/dev/null 2>&1 </dev/null'
  else envf=sway_env; off=24; paste_cmd="wl-paste --no-newline"
    set_clip='setsid -f sh -c "printf %s \"echo pasted-ok > /tmp/suite-paste.txt\" | wl-copy" >/dev/null 2>&1 </dev/null'
  fi
  R() { $VM ssh "source ~/vmenv.sh; $envf; $1" 2>/dev/null; }
  dump() { R "~/bin/ghostdrop ctl dump" | python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
  drive() { printf "$1" | $VM drive "$be" >/dev/null 2>&1; }

  echo "== $be"
  win=$([ "$be" = x11 ] && echo xterm || echo foot)
  R "pgrep -x $win >/dev/null || setsid -f $win >/dev/null 2>&1 </dev/null; sleep 0.5"
  R "printf 'control-commands = true\n' > ~/.config/gd-test/config; rm -f /tmp/suite-*.txt; ~/bin/ghostdrop ctl quit force >/dev/null 2>&1; sleep 0.6; GHOSTDROP_TRACE=1 setsid -f ~/bin/ghostdrop --daemon > /tmp/gd-suite-$be.log 2>&1 </dev/null; sleep 1.5"
  # Keep one VNC client attached so sway always has a keyboard (see README).
  [ "$be" = wayland ] && { printf 'sleep 600\n' | $VM drive wayland >/dev/null 2>&1 & HOLD=$!; sleep 1.5; }

  drive 'key ctrl-`\nsleep 1\n'
  expect "hotkey shows" "$(dump 'd["visible"]')" "True"
  drive 'type echo typed-ok > /tmp/suite-type.txt\nkey enter\nsleep 0.6\n'
  expect "typing reaches the shell" "$(R 'cat /tmp/suite-type.txt')" "typed-ok"

  drive "type clear; echo copy-me\nkey enter\nsleep 0.6\nmove 40 $((off + 12))\nclick 1\nclick 1\nsleep 0.3\nkey ctrl-shift-C\nsleep 0.5\n"
  expect "copy to system clipboard" "$(R "timeout 3 $paste_cmd")" "copy-me"
  R "$set_clip"; sleep 0.3
  drive 'key ctrl-shift-V\nkey enter\nsleep 0.6\n'
  expect "paste from system clipboard" "$(R 'cat /tmp/suite-paste.txt')" "pasted-ok"

  drive 'key ctrl-shift-T\nsleep 1\n'
  expect "ctrl+shift+t opens a tab" "$(dump 'len(d["tabs"])')" "2"
  drive "move 1584 $((off + 14))\nclick 1\nsleep 1\n"
  expect "tab bar + opens a tab" "$(dump 'len(d["tabs"])')" "3"
  drive "move 200 $((off + 14))\nclick 1\nsleep 0.5\n"
  expect "clicking a tab selects it" "$(dump 'd["selected"]')" "0"
  drive "move 1300 $((off + 14))\nclick 2\nsleep 1\n"
  expect "middle-click closes a tab" "$(dump 'len(d["tabs"])')" "2"

  drive "move 800 $((off + 300))\nsleep 0.5\nmove 800 850\nsleep 0.8\n"
  expect "hovering a window keeps it open" "$(dump 'd["visible"]')" "True"
  drive 'move 800 850\nclick 1\nsleep 0.8\n'
  expect "clicking a window autohides" "$(dump 'd["visible"]')" "False"
  drive 'key ctrl-`\nsleep 0.8\n'
  expect "hotkey shows again" "$(dump 'd["visible"]')" "True"
  drive 'key ctrl-`\nsleep 0.8\n'
  expect "hotkey hides" "$(dump 'd["visible"]')" "False"
  [ "$be" = wayland ] && kill $HOLD 2>/dev/null
done
echo "passed $PASS, failed $FAIL"
[ $FAIL -eq 0 ]
