#!/bin/bash
# Run inside the VM once (dev/vm/vm.sh ssh 'bash ~/ghostdrop/dev/vm/tests/setup.sh'):
# installs the helpers the tests use and binds ctrl+` on both desktops to the
# freshly built binary.
#   ~/vmenv.sh          sway_env / x11_env: point a shell at either desktop
#   ~/bin/ghostdrop     dev wrapper: build output + test config + per-desktop socket
set -euo pipefail

cat > ~/vmenv.sh <<'EOF'
# source inside the VM: env for talking to the desktops
export XDG_RUNTIME_DIR=/run/user/$(id -u)
export GHOSTTY_RESOURCES_DIR=$HOME/ghostdrop/build/ghostty-linux/share/ghostty
export LANG=C.UTF-8
sway_env() { export WAYLAND_DISPLAY=wayland-1 SWAYSOCK=$(ls $XDG_RUNTIME_DIR/sway-ipc.*.sock | head -1); unset DISPLAY; }
x11_env() { export DISPLAY=:1; unset WAYLAND_DISPLAY SWAYSOCK; }
EOF

mkdir -p ~/bin ~/.config/gd-test
cat > ~/bin/ghostdrop <<'EOF'
#!/bin/sh
# dev wrapper: run the freshly built binary with the test config, one daemon per desktop
export GHOSTTY_RESOURCES_DIR=$HOME/ghostdrop/build/ghostty-linux/share/ghostty LANG=C.UTF-8 GHOSTDROP_DEBUG=1
export GHOSTDROP_CONFIG=$HOME/.config/gd-test/config
[ -n "$WAYLAND_DISPLAY" ] && export GHOSTDROP_SOCKET=$XDG_RUNTIME_DIR/gd-sway.sock || export GHOSTDROP_SOCKET=$XDG_RUNTIME_DIR/gd-x11.sock
exec $HOME/ghostdrop/linux/target/release/ghostdrop "$@"
EOF
chmod +x ~/bin/ghostdrop
[ -f ~/.config/gd-test/config ] || printf 'control-commands = true\n' > ~/.config/gd-test/config

echo "bindsym ctrl+grave exec $HOME/bin/ghostdrop" > ~/.config/sway/ghostdrop.conf
echo "bindsym ctrl+grave exec --no-startup-id $HOME/bin/ghostdrop" > ~/.config/i3/ghostdrop.conf
grep -q ghostdrop.conf ~/.config/sway/config || echo 'include ~/.config/sway/ghostdrop.conf' >> ~/.config/sway/config
grep -q ghostdrop.conf ~/.config/i3/config || echo 'include ~/.config/i3/ghostdrop.conf' >> ~/.config/i3/config

source ~/vmenv.sh
(sway_env; swaymsg reload >/dev/null 2>&1) || true
(x11_env; i3-msg reload >/dev/null 2>&1) || true
echo "ok: ~/vmenv.sh, ~/bin/ghostdrop, ctrl+\` bound on sway and i3"
