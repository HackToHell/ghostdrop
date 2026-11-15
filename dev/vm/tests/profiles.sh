#!/bin/bash
# Inside the VM: profile commands actually run (regression test for
# patches/ghostty/0002: the command string used to dangle) and `term` applies.
source ~/vmenv.sh; sway_env; gd() { ~/bin/ghostdrop "$@"; }; cfg=~/.config/gd-test/config
fail=0
check() { # label, config lines, expected $TERM in the default tab
  printf "control-commands = true\n$2" > $cfg
  gd ctl quit force >/dev/null 2>&1; sleep 0.8; rm -f /tmp/term.txt
  setsid -f ~/bin/ghostdrop --daemon > /tmp/gd-profiles.log 2>&1 </dev/null; sleep 1.2
  gd ctl show >/dev/null; sleep 1; gd ctl binding 'text:echo $TERM > /tmp/term.txt\r' >/dev/null; sleep 0.6
  got=$(cat /tmp/term.txt 2>/dev/null)
  if [ "$got" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1 (TERM='$got', want '$3')"; fail=1; fi
}
check "login shell" "" xterm-ghostty
check "plain command" "profile.p.command = /bin/bash --norc --noprofile\ndefault-profile = p\n" xterm-ghostty
check "term override" "profile.p.command = /bin/bash --norc --noprofile\nprofile.p.term = xterm-256color\ndefault-profile = p\n" xterm-256color
check "env wrapper" "profile.p.command = /usr/bin/env FOO=1 /bin/bash --norc --noprofile\ndefault-profile = p\n" xterm-ghostty
check "sh -c with ;" "profile.p.command = /bin/sh -c \"true; exec /bin/bash --norc\"\ndefault-profile = p\n" xterm-ghostty
printf 'control-commands = true\n' > $cfg; gd ctl quit force >/dev/null 2>&1
exit $fail
