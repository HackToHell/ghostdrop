#!/usr/bin/env bash
# Runs as root inside the machine: ghostdrop-provision <user> <vnc-password> < authorized_key.pub
set -euo pipefail
user="$1"; vncpass="$2"
uid="$(id -u "$user")"; gid="$(id -g "$user")"; home="$(getent passwd "$user" | cut -d: -f6)"
as_user() { sudo -u "$user" -H "$@"; }

# --- ssh access (key from the Mac side) ---
install -d -m 700 -o "$user" -g "$gid" "$home/.ssh"
cat > "$home/.ssh/authorized_keys"
chown "$user:$gid" "$home/.ssh/authorized_keys"; chmod 600 "$home/.ssh/authorized_keys"
systemctl enable --now sshd

# --- VNC password for the X11 desktop (Xvnc, classic VNC auth) ---
as_user install -d -m 700 "$home/.config/tigervnc"
printf '%s\n' "$vncpass" | vncpasswd -f > "$home/.config/tigervnc/passwd"
chown "$user:$gid" "$home/.config/tigervnc/passwd"; chmod 600 "$home/.config/tigervnc/passwd"

# --- i3: default config with Alt as $mod (Super is taken by macOS over VNC) ---
as_user install -d "$home/.config/i3"
sed -e 's/^set \$mod Mod4/set $mod Mod1/' /etc/i3/config > "$home/.config/i3/config"
cat >> "$home/.config/i3/config" <<'EOF'

# --- ghostdrop dev VM ---
font pango:JetBrainsMono Nerd Font 10
bindsym $mod+Return exec xterm
include ~/.config/i3/ghostdrop.conf
EOF
touch "$home/.config/i3/ghostdrop.conf"

# --- sway: default config, Alt as $mod, a 1600x1000 virtual output, wayvnc ---
as_user install -d "$home/.config/sway" "$home/.config/wayvnc"
sed -e 's/^set \$mod Mod4/set $mod Mod1/' /etc/sway/config > "$home/.config/sway/config"
cat >> "$home/.config/sway/config" <<'EOF'

# --- ghostdrop dev VM ---
output HEADLESS-1 resolution 1600x1000 position 0,0
font pango:JetBrainsMono Nerd Font 10
exec wayvnc --config ~/.config/wayvnc/config
include ~/.config/sway/ghostdrop.conf
EOF
touch "$home/.config/sway/ghostdrop.conf"
cat > "$home/.config/wayvnc/config" <<'EOF'
address=0.0.0.0
port=5902
EOF
chown -R "$user:$gid" "$home/.config"

# --- the two desktops as services ---
for kind in x11 wayland; do
  cat > "/etc/systemd/system/ghostdrop-vnc-$kind.service" <<EOF
[Unit]
Description=Ghostdrop dev desktop ($kind) over VNC
After=network-online.target

[Service]
User=$user
Group=$gid
WorkingDirectory=$home
Environment=HOME=$home
ExecStartPre=+/usr/bin/install -d -m 0700 -o $uid -g $gid /run/user/$uid
ExecStart=/usr/local/bin/ghostdrop-$kind-session
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
done
systemctl daemon-reload
systemctl enable --now ghostdrop-vnc-x11.service ghostdrop-vnc-wayland.service
echo "provisioned for $user (uid $uid)"
