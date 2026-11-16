#!/bin/bash
# Regression tests for the pointer/prompt/keyboard/focus/clipboard state
# machines, run from the host like suite.sh: dev/vm/tests/states.sh [x11|wayland]...
# Drives the real desktops over VNC and asserts via `ghostdrop ctl dump`.
set -u
cd "$(dirname "$0")/../../.."
VM=dev/vm/vm.sh
PASS=0 FAIL=0 SKIP=0

ok() { PASS=$((PASS + 1)); echo "  ✓ $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  ✗ $1  (got: $2)"; }
skip() { SKIP=$((SKIP + 1)); echo "  - $1 (skipped: $2)"; }
expect() { [ "$2" = "$3" ] && ok "$1" || bad "$1" "$2"; }

[ $# -gt 0 ] || set -- x11 wayland
for be in "$@"; do
  if [ "$be" = x11 ]; then envf=x11_env; off=0; other=xterm
    primary_set='setsid -f sh -c "printf none | xclip -selection primary" >/dev/null 2>&1 </dev/null'
    primary_get='timeout 3 xclip -o -selection primary'
  else envf=sway_env; off=24; other=foot
    primary_set='setsid -f sh -c "printf none | wl-copy --primary" >/dev/null 2>&1 </dev/null'
    primary_get='timeout 3 wl-paste --primary --no-newline'
  fi
  R() { $VM ssh "source ~/vmenv.sh; $envf; $1" 2>/dev/null; }
  dump() { R "~/bin/ghostdrop ctl dump" | python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
  ctl() { R "~/bin/ghostdrop ctl $1" >/dev/null; }
  drive() { printf "$1" | $VM drive "$be" >/dev/null 2>&1; }
  bar=$((off + 14))
  titles='[t["title"] for t in d["tabs"]]'

  echo "== $be"
  R "pgrep -x $other >/dev/null || setsid -f $other >/dev/null 2>&1 </dev/null; sleep 0.5"
  R "printf 'control-commands = true\n' > ~/.config/gd-test/config; ~/bin/ghostdrop ctl quit force >/dev/null 2>&1; sleep 0.6; GHOSTDROP_TRACE=1 setsid -f ~/bin/ghostdrop --daemon > /tmp/gd-states-$be.log 2>&1 </dev/null; sleep 1.5"
  [ "$be" = wayland ] && { printf 'sleep 900\n' | $VM drive wayland >/dev/null 2>&1 & HOLD=$!; sleep 1.5; }

  # Three tabs told apart by their titles (the shell sets the cwd).
  drive 'key ctrl-`\nsleep 1\ntype cd /tmp\nkey enter\nsleep 0.4\nkey ctrl-shift-T\nsleep 1\ntype cd /usr\nkey enter\nsleep 0.4\nkey ctrl-shift-T\nsleep 1\ntype cd /etc\nkey enter\nsleep 0.8\n'
  expect "three tabs" "$(dump "$titles")" "['/tmp', '/usr', '/etc']"

  # Press on a tab, release over the terminal, then just hover the tab bar:
  # the drag ended with the release, so nothing moves.
  drive "move 200 $bar\ndown 1\nmove 200 $((off + 150))\nmove 200 $((off + 300))\nup 1\nsleep 0.3\nmove 700 $bar\nsleep 0.3\nmove 1300 $bar\nsleep 0.5\n"
  expect "tab-bar press released over the terminal ends the drag" "$(dump "$titles")" "['/tmp', '/usr', '/etc']"

  # A real drag still reorders.
  drive "move 200 $bar\ndown 1\nmove 400 $bar\nmove 700 $bar\nmove 1000 $bar\nmove 1300 $bar\nsleep 0.2\nup 1\nsleep 0.5\n"
  expect "dragging a tab reorders" "$(dump "$titles + [d['selected']]")" "['/usr', '/etc', '/tmp', 2]"

  # Select in the terminal and release over the tab bar: the terminal gets
  # the release (copy-on-select fills the primary selection).
  drive 'type clear; echo SELECTME-LINE\nkey enter\nsleep 0.6\n'
  R "$primary_set"; sleep 0.5
  drive "move 300 $((off + 160))\ndown 1\nmove 200 $((off + 100))\nmove 100 $((off + 40))\nmove 4 $((off + 8))\nsleep 0.2\nup 1\nsleep 0.4\nmove 300 $((off + 300))\nsleep 0.5\n"
  case "$(R "$primary_get")" in
    *SELECTME-LINE*) ok "selection released over the tab bar completes" ;;
    *) bad "selection released over the tab bar completes" "$(R "$primary_get" | head -c 60)" ;;
  esac

  # Two busy tabs closed at once: both questions are asked, one at a time,
  # each with its own tab in front; a held Enter answers only one.
  ctl "goto-tab 1"; drive 'type sleep 1000\nkey enter\nsleep 0.3\n'
  ctl "goto-tab 2"; drive 'type sleep 1000\nkey enter\nsleep 0.3\n'
  ctl "goto-tab 3"; sleep 0.3
  ctl "binding close_tab:other"; sleep 0.8
  expect "close others asks twice (one queued)" "$(dump '[d["prompt"], d["promptsQueued"], d["selected"]]')" "['Close this tab?', 1, 0]"
  drive 'keydown enter\nsleep 1.3\nkeyup enter\nsleep 0.6\n'
  expect "held Enter answers one question" "$(dump '[d["prompt"], d["promptsQueued"], len(d["tabs"]), d["selected"]]')" "['Close this tab?', 0, 2, 0]"
  ctl "prompt-answer yes"; sleep 0.6
  expect "second answer closes the second tab" "$(dump '[d["prompt"], len(d["tabs"])]')" "['', 1]"

  # Visible but unfocused (autohide off): the hotkey focuses it, a reload
  # doesn't steal focus.
  R "printf 'control-commands = true\nquick-terminal-autohide = false\n' > ~/.config/gd-test/config"
  ctl reload; sleep 0.5
  # Move keyboard focus to a fresh window on the current workspace.
  if [ "$be" = x11 ]; then
    R 'setsid -f xterm -class GdStatesOther >/dev/null 2>&1 </dev/null'; sleep 1.2
    R 'xdotool windowfocus "$(xdotool search --onlyvisible --class GdStatesOther | head -1)"'; sleep 0.8
  else
    R 'setsid -f foot --app-id gd-states-other >/dev/null 2>&1 </dev/null'; sleep 1.2
    R "swaymsg '[app_id=gd-states-other] focus' >/dev/null"; sleep 0.8
  fi
  if [ "$(dump '[d["visible"], d["focused"]]')" = "[True, False]" ]; then
    ctl reload; sleep 0.8
    expect "reload doesn't take focus" "$(dump '[d["visible"], d["focused"]]')" "[True, False]"
    drive 'key ctrl-`\nsleep 0.8\n'
    expect "hotkey focuses a visible, unfocused drop-down" "$(dump '[d["visible"], d["focused"]]')" "[True, True]"
    drive 'key ctrl-`\nsleep 0.8\n'
    expect "hotkey then hides it" "$(dump 'd["visible"]')" "False"
  else
    skip "hotkey focuses a visible, unfocused drop-down" "the compositor kept focus on the drop-down"
  fi
  R "pkill -f 'GdStatesOther|gd-states-other'"
  R "printf 'control-commands = true\n' > ~/.config/gd-test/config"
  ctl reload

  # X11: the clipboard owner dies mid-read; later pastes still work.
  if [ "$be" = x11 ]; then
    R 'pkill -x xclip; setsid -f sh -c "printf %s \"echo first > /tmp/states-paste1.txt\" | xclip -selection clipboard" >/dev/null 2>&1 </dev/null; sleep 0.4; pkill -STOP -x xclip; rm -f /tmp/states-paste*.txt'
    ctl hide; sleep 0.3; drive 'key ctrl-`\nsleep 0.8\n'
    drive 'key ctrl-shift-V\nsleep 0.5\n'
    R 'pkill -KILL -x xclip'; sleep 3
    R 'setsid -f sh -c "printf %s \"echo second > /tmp/states-paste2.txt\" | xclip -selection clipboard" >/dev/null 2>&1 </dev/null'; sleep 0.4
    drive 'key ctrl-shift-V\nkey enter\nsleep 0.8\n'
    expect "paste works after the clipboard owner died mid-read" "$(R 'cat /tmp/states-paste2.txt')" "second"
  fi

  R "~/bin/ghostdrop ctl quit force >/dev/null 2>&1"
  [ "$be" = wayland ] && kill $HOLD 2>/dev/null
done
echo "passed $PASS, failed $FAIL, skipped $SKIP"
[ $FAIL -eq 0 ]
