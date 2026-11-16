# Linux dev VM

An Arch Linux ARM machine on Apple's [`container`](https://github.com/apple/container)
(`container machine`, macOS 26+) with two real desktops you can open over VNC:

| Desktop | Port | Notes |
|---|---|---|
| X11 + i3 | `5901` | TigerVNC `Xvnc`, VNC password in `.tools/vm/vnc-password` |
| Wayland + sway | `5902` | sway + `wayvnc`, no password (host-only network) |

`$mod` is **Alt** in both (⌥ Option on the Mac keyboard; Cmd/Super is taken by
macOS over VNC). `Alt+Enter` opens a terminal, `Alt+d` a launcher. There's no GPU:
OpenGL/EGL is Mesa llvmpipe, so it's functional, not representative of real
performance.

## One-time setup

```sh
# Install Apple's container tool (admin password required), then start it:
sudo installer -pkg container-1.7.1-installer-signed.pkg -target /
container system start --enable-kernel-install

dev/vm/vm.sh up      # build the image, create + provision the machine
```

## Daily use

```sh
dev/vm/vm.sh info            # IP, ssh, VNC URLs and password
dev/vm/vm.sh vnc x11         # open X11/i3 in Screen Sharing
dev/vm/vm.sh vnc wayland     # open sway in Screen Sharing
dev/vm/vm.sh ssh             # shell in the VM
dev/vm/vm.sh sync            # copy this repo to ~/ghostdrop in the VM
dev/vm/vm.sh shot wayland    # screenshot a desktop (used for automated checks)
dev/vm/vm.sh stop | start | destroy
```

If macOS Screen Sharing won't connect to the sway desktop (it doesn't speak every
VNC auth scheme), use TigerVNC Viewer: `brew install --cask tigervnc-viewer`.

## Tests

Build in the VM, then run the suites (from the host unless noted):

```sh
dev/vm/vm.sh sync && dev/vm/vm.sh ssh 'cd ~/ghostdrop && scripts/build-linux.sh'
dev/vm/vm.sh ssh 'bash ~/ghostdrop/dev/vm/tests/setup.sh'   # once: helpers + ctrl+` bindings
dev/vm/tests/suite.sh                  # end-to-end on i3 and sway over VNC (hotkey, typing,
                                       # clipboard both ways, tabs + tab bar, click-outside)
dev/vm/tests/states.sh                 # input/prompt/focus state: drags and releases over the tab
                                       # bar, queued prompts, unfocused hotkey, dead clipboard owner
dev/vm/vm.sh ssh 'bash ~/ghostdrop/dev/vm/tests/profiles.sh'   # profile commands, TERM override
dev/vm/vm.sh ssh 'bash ~/ghostdrop/dev/vm/tests/perf.sh sway'  # latency/throughput/CPU (or x11)
dev/vm/vm.sh ssh 'bash ~/ghostdrop/dev/vm/tests/leak.sh'       # open/close 75 tabs, RSS + malloc in-use
dev/vm/vm.sh ssh 'bash ~/ghostdrop/dev/vm/tests/death.sh'      # display server dies → daemon exits
dev/vm/vm.sh drive x11 < dev/vm/tests/ui-x11.txt   # screenshot tour → /tmp/gd-test/ui-*.png
```

`tests/probe.py` checks terminal protocols from inside the drop-down (kitty keyboard
and graphics, DECRQM 2026, OSC 52, focus reports); run it in a tab, press
`shift+enter`, `ctrl+i`, `esc`, and read `/tmp/probe.json`.

Testing on a real desktop someone is using (e.g. a GPU box for zero-copy):
`GHOSTDROP_X11_OFFSCREEN=1` keeps the X11 window offscreen (Composite redirect),
click-through and focus-less, and `ghostdrop ctl snapshot out.ppm` (with
`control-commands = true`) reads back what it shows, including DRI3/Present frames.
For Wayland, run a private headless sway (`WLR_BACKENDS=headless`, its own
`XDG_RUNTIME_DIR`) and capture it with `cargo run --release --example wlshot -- out.ppm`.

Gotchas when driving the desktops:

* Each `vm.sh drive` is a new VNC client whose pointer starts at (0,0): `move`
  before `click` in the same script.
* On sway the virtual keyboard exists only while a VNC client is connected, so
  connecting/disconnecting looks like keyboard focus loss (autohide). `suite.sh`
  keeps one client attached; do the same for manual runs.
* TigerVNC doesn't auto-repeat held keys (real viewers send repeats themselves);
  test X11 repeat with `xdotool keydown`.
* Daemons that run non-shell commands ask before quitting: scripts use
  `ghostdrop ctl quit force`.
* `wl-copy` / `xclip` stay running to serve the selection: detach them
  (`setsid -f sh -c '… | wl-copy' </dev/null >/dev/null 2>&1`) or ssh never returns.

## Isolation

`container machine` normally mounts your whole home directory read-write and
forwards your ssh-agent into the VM. `vm.sh` turns both off (`--home-mount none`,
no `SSH_AUTH_SOCK`), uses its own ssh key (`.tools/vm/id_ed25519`), and copies the
repo in with `sync`.

Enter the VM through `dev/vm/vm.sh` (it strips `SSH_AUTH_SOCK`). A plain
`container machine run` from a shell that has an ssh-agent forwards that agent into
the VM for the duration of the session.
