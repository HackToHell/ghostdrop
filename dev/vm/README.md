# Linux dev VM

An Arch Linux ARM machine on Apple's [`container`](https://github.com/apple/container)
(`container machine`, macOS 26+) with two real desktops you can open over VNC:

| Desktop | Port | Notes |
|---|---|---|
| X11 + i3 | `5901` | TigerVNC `Xvnc`, VNC password in `.tools/vm/vnc-password` |
| Wayland + sway | `5902` | sway + `wayvnc`, no password (host-only network) |

`$mod` is **Alt** in both (Cmd/Super is taken by macOS over VNC). There's no GPU:
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

## Isolation

`container machine` normally mounts your whole home directory read-write and
forwards your ssh-agent into the VM. `vm.sh` turns both off (`--home-mount none`,
no `SSH_AUTH_SOCK`), uses its own ssh key (`.tools/vm/id_ed25519`), and copies the
repo in with `sync`.
