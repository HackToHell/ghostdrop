#!/bin/bash
# Usage: leak.sh [env assignments...] — open/close 25 tabs twice, print RSS.
source ~/vmenv.sh; sway_env; gd() { ~/bin/ghostdrop "$@"; }
gd ctl quit force >/dev/null 2>&1; sleep 0.8
env "$@" setsid -f ~/bin/ghostdrop --daemon > /tmp/gd-sway.log 2>&1 </dev/null; sleep 1.5
pid=$(pgrep -n -x ghostdrop); gd ctl show >/dev/null; sleep 0.5
m() { echo "RSS $(( $(awk '/VmRSS/{print $2}' /proc/$pid/status) / 1024 )) MiB"; }
echo "baseline: $(m)"
for r in 1 2 3; do
  for i in $(seq 25); do gd ctl new-tab >/dev/null; sleep 0.05; done; sleep 1; a=$(m)
  for i in $(seq 25); do gd ctl binding close_surface >/dev/null; sleep 0.05; done; sleep 1.5
  echo "cycle $r: 26 tabs $a -> 1 tab $(m)  [$(gd ctl mem | grep -o 'malloc_in_use=[0-9]*MiB')]"
done
