#!/bin/bash
# Kill the display server under a running daemon; it must exit, not spin.
export XDG_RUNTIME_DIR=/run/user/$(id -u)
BIN=$HOME/ghostdrop/linux/target/release/ghostdrop
check() { # name pid
  for i in $(seq 20); do kill -0 $2 2>/dev/null || { echo "$1: daemon exited cleanly"; return; }; sleep 0.25; done
  echo "$1: daemon STILL RUNNING, cpu=$(ps -o %cpu= -p $2)"; kill $2
}
# Wayland: private headless sway
printf 'output * bg #000000 solid_color\n' > /tmp/min-sway.conf
before=$(ls $XDG_RUNTIME_DIR | grep -E '^wayland-[0-9]+$' | sort)
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 setsid sway -c /tmp/min-sway.conf >/tmp/min-sway.log 2>&1 </dev/null & SW=$!
sleep 1.5; WD=$(comm -13 <(echo "$before") <(ls $XDG_RUNTIME_DIR | grep -E '^wayland-[0-9]+$' | sort) | head -1)
WAYLAND_DISPLAY=$WD GHOSTDROP_SOCKET=$XDG_RUNTIME_DIR/gd-death-wl.sock GHOSTDROP_BACKEND=wayland setsid $BIN --daemon >/tmp/gd-death-wl.log 2>&1 </dev/null & P=$!
sleep 1.5; WAYLAND_DISPLAY=$WD GHOSTDROP_SOCKET=$XDG_RUNTIME_DIR/gd-death-wl.sock $BIN ctl show >/dev/null; sleep 0.5
kill $SW; check "wayland ($WD)" $P; tail -1 /tmp/gd-death-wl.log
# X11: private Xvnc :7
setsid Xvnc :7 -geometry 1024x768 -SecurityTypes None -rfbport 5977 >/tmp/xvnc7.log 2>&1 </dev/null & XS=$!
sleep 1.5
DISPLAY=:7 GHOSTDROP_SOCKET=$XDG_RUNTIME_DIR/gd-death-x.sock GHOSTDROP_BACKEND=x11 setsid $BIN --daemon >/tmp/gd-death-x.log 2>&1 </dev/null & P=$!
sleep 1.5; DISPLAY=:7 GHOSTDROP_SOCKET=$XDG_RUNTIME_DIR/gd-death-x.sock $BIN ctl show >/dev/null; sleep 0.5
kill $XS; check "x11 (:7)" $P; tail -1 /tmp/gd-death-x.log
