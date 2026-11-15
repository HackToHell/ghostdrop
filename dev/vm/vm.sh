#!/usr/bin/env bash
# Ghostdrop Linux dev VM (Arch Linux ARM) on Apple's `container machine` (macOS 26+).
#
#   dev/vm/vm.sh up                 build image, create + provision the machine, print access info
#   dev/vm/vm.sh info               IP, ssh command, VNC URLs + password
#   dev/vm/vm.sh ssh [cmd...]       ssh into the machine (as user "dev")
#   dev/vm/vm.sh sync               rsync this repo to ~/ghostdrop in the machine
#   dev/vm/vm.sh vnc x11|wayland    open the desktop in macOS Screen Sharing
#   dev/vm/vm.sh shot x11|wayland [out.png]   screenshot a desktop over VNC (no GUI needed)
#   dev/vm/vm.sh stop | start | destroy
#
# Isolation: the machine is created WITHOUT the default home-directory mount and
# without your ssh-agent, so nothing in the VM can read ~/.ssh or use your keys.
# The repo is copied in with `sync` instead.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STATE="$ROOT/.tools/vm"
NAME="${GHOSTDROP_VM:-ghostdrop-arch}"
IMAGE="ghostdrop-arch-dev:latest"
VMUSER="dev"
KEY="$STATE/id_ed25519"
# The default resolver handed to VMs (the vmnet gateway) doesn't work when the Mac
# uses a VPN/Tailscale resolver, so use public resolvers for this VM only.
DNS="${GHOSTDROP_VM_DNS:-1.1.1.1 8.8.8.8}"
mkdir -p "$STATE"

# Never hand our ssh-agent to anything container-related.
c() { env -u SSH_AUTH_SOCK container "$@"; }

ip_of() {
  c machine inspect "$NAME" 2>/dev/null | python3 -c '
import json, re, sys
txt = sys.stdin.read()
m = re.findall(r"\b(192\.168\.\d+\.\d+|10\.\d+\.\d+\.\d+)(?:/\d+)?\b", txt)
print(m[0] if m else "")'
}

need_ip() {
  local ip
  for _ in $(seq 60); do ip="$(ip_of)"; [ -n "$ip" ] && { echo "$ip"; return; }; sleep 1; done
  echo "could not determine the machine's IP (is it running? try: $0 start)" >&2; exit 1
}

ssh_vm() {
  local ip; ip="$(need_ip)"
  ssh -i "$KEY" -o IdentitiesOnly=yes -o IdentityAgent=none \
      -o UserKnownHostsFile="$STATE/known_hosts" -o StrictHostKeyChecking=accept-new \
      -o LogLevel=ERROR "$VMUSER@$ip" "$@"
}

vncpass() { cat "$STATE/vnc-password"; }

vncdo() {
  if [ ! -x "$STATE/venv/bin/vncdo" ]; then
    python3 -m venv "$STATE/venv" >/dev/null
    "$STATE/venv/bin/pip" -q install vncdotool >/dev/null
  fi
  "$STATE/venv/bin/vncdo" "$@"
}

cmd="${1:-info}"; shift || true
case "$cmd" in
  up)
    command -v container >/dev/null || { echo "Install Apple's container tool first (see dev/vm/README.md)"; exit 1; }
    if ! c image inspect "$IMAGE" >/dev/null 2>&1; then
      echo "==> building $IMAGE"
      dnsflags=(); for d in $DNS; do dnsflags+=(--dns "$d"); done
      c build --arch arm64 "${dnsflags[@]}" -t "$IMAGE" -f "$ROOT/dev/vm/Containerfile" "$ROOT/dev/vm"
    fi
    if ! c machine inspect "$NAME" >/dev/null 2>&1; then
      echo "==> creating machine $NAME"
      c machine create "$IMAGE" --name "$NAME" --cpus "${CPUS:-6}" --memory "${MEMORY:-8G}" \
        --home-mount none --user "$VMUSER"
    fi
    [ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N "" -C "ghostdrop-vm" -f "$KEY"
    [ -s "$STATE/vnc-password" ] || python3 -c 'import secrets,string; print("".join(secrets.choice(string.ascii_letters+string.digits) for _ in range(8)), end="")' > "$STATE/vnc-password"
    echo "==> provisioning"
    # `container machine run` joins argv with spaces and re-parses it with a shell, so
    # ship the current provision script + its arguments as a single base64 token.
    payload="$(python3 - "$ROOT/dev/vm/provision.sh" "$VMUSER" "$(vncpass)" "$DNS" "$(cat "$KEY.pub")" <<'PY'
import base64, shlex, sys
script, *args = sys.argv[1:]
body = "set -- " + " ".join(shlex.quote(a) for a in args) + "\n" + open(script).read()
print(base64.b64encode(body.encode()).decode())
PY
)"
    c machine run -n "$NAME" -- echo "$payload" "|" base64 -d "|" sudo bash
    "$0" sync
    "$0" info
    ;;
  info)
    ip="$(need_ip)"
    cat <<EOF
machine:   $NAME ($IMAGE), user $VMUSER
ssh:       $0 ssh        (= ssh -i $KEY $VMUSER@$ip)
X11 + i3:  vnc://$ip:5901   password: $(vncpass)
sway:      vnc://$ip:5902   (no password; host-only network)
open:      $0 vnc x11   |   $0 vnc wayland
EOF
    ;;
  ssh) ssh_vm "$@" ;;
  sync)
    ip="$(need_ip)"
    rsync -az --delete \
      --exclude /build/ --exclude /.tools/ --exclude .zig-cache/ --exclude zig-out/ \
      --exclude /linux/target/ --exclude .DS_Store \
      -e "ssh -i $KEY -o IdentitiesOnly=yes -o IdentityAgent=none -o UserKnownHostsFile=$STATE/known_hosts -o StrictHostKeyChecking=accept-new -o LogLevel=ERROR" \
      "$ROOT/" "$VMUSER@$ip:ghostdrop/"
    ;;
  vnc)
    ip="$(need_ip)"
    case "${1:-x11}" in
      x11) echo "password: $(vncpass)"; open "vnc://$ip:5901" ;;
      wayland|sway) open "vnc://$ip:5902" ;;
      *) echo "usage: $0 vnc x11|wayland"; exit 2 ;;
    esac
    ;;
  shot)
    ip="$(need_ip)"; kind="${1:-x11}"; out="${2:-$STATE/$kind.png}"
    case "$kind" in
      x11) vncdo -s "$ip::5901" -p "$(vncpass)" pause 1 capture "$out" ;;
      wayland|sway) vncdo -s "$ip::5902" pause 2 capture "$out" ;;
    esac
    echo "$out"
    ;;
  start) c machine run -n "$NAME" -- true ;;
  stop) c machine stop "$NAME" ;;
  destroy) c machine rm "$NAME" ;;
  *) sed -n '2,16p' "$0"; exit 2 ;;
esac
