# Ghostdrop setup guide

> **For humans:** paste this into an AI coding agent that can run commands on your
> Mac (Claude Code, Codex, Cursor, Pi, …):
>
> *"Help me set up Ghostdrop, the drop-down terminal I just installed. Read
> /Applications/Ghostdrop.app/Contents/Resources/AGENTS.md and follow it."*
>
> The Ghostdrop menu-bar icon → **Set Up with AI…** copies this prompt with the right path.

---

## For AI agents

You are configuring **Ghostdrop**, a native drop-down (Quake-style) terminal for
macOS and Linux, built on libghostty (Ghostty's terminal engine). It has a global show/hide hotkey,
tabs, and profiles. Your job: understand how the user wants it to behave, write
**one config file**, validate it, reload the app, and confirm with the user.

### Ground rules

- Only write `~/.config/ghostdrop/config` (or `$GHOSTDROP_CONFIG` if set). Back up an
  existing file first (`cp config config.bak-$(date +%Y%m%d%H%M%S)`).
- Do **not** modify the user's Ghostty or iTerm2 configuration unless they explicitly
  ask. Ghostty options placed in Ghostdrop's config apply to the drop-down only.
- Keep the config minimal: only lines that differ from defaults, each with a short
  comment on its own line explaining why.
- Syntax: one `key = value` per line. Comments must be on their **own line**
  starting with `#` — inline `# comments` after a value are NOT allowed.
- Never set `control-commands = true` (it enables test-only input injection).
- Ask before anything that changes system state outside the config (login item,
  quarantine removal, installing terminfo on remote hosts).
- Ask questions in one compact batch with proposed defaults the user can accept,
  rather than one at a time.

### On Linux

Everything in this guide applies, except (check `uname` first):

- **Binary**: `G="$(command -v ghostdrop || echo "$HOME/.local/bin/ghostdrop")"`; there
  is no app bundle, menu-bar icon, Dock icon or login item (`status-item`,
  `dock-icon`, `ctl login-item` don't apply). Engine docs: `"$G" +show-config --default --docs`.
- **Start/autostart**: from the window manager config, e.g. sway/i3
  `exec ghostdrop --daemon`. Running `ghostdrop` with no arguments toggles the
  drop-down, starting it first if needed.
- **Hotkey**: on X11 Ghostdrop grabs `hotkey` itself (preferred; the WM only needs
  `exec --no-startup-id ghostdrop --daemon`). If another program already owns that
  key (e.g. tilda or guake on `` ctrl+` ``), pick a different `hotkey`; the daemon
  prints "hotkey … is taken by another client" to stderr when that happens.
  Wayland has no global key grabs, so bind the key **in the compositor**
  to run `ghostdrop`: sway `bindsym ctrl+grave exec ghostdrop`, Hyprland
  `bind = CTRL, grave, exec, ghostdrop`. Edit the WM config only with the user's OK,
  then reload the WM (`swaymsg reload`, `i3-msg reload`).
- **Rendering**: zero-copy GPU presentation is automatic (falls back to CPU frames
  where unsupported); `GHOSTDROP_GPU=0` in the daemon's environment forces CPU frames
  if a driver misbehaves.
- **Tab shortcuts**: Ghostty's Linux defaults: `ctrl+shift+t` new tab, `ctrl+shift+w`
  close, `ctrl+page_up/down` or `ctrl+tab` switch, `alt+1…9` go to tab,
  `ctrl+shift+c/v` copy/paste, `ctrl+shift+f` search, `ctrl+shift+,` reload config.
- **Placement**: Wayland shows it on the focused output; X11 on the monitor under
  the mouse (`quick-terminal-screen = main` for the primary). Wayland only:
  `quick-terminal-keyboard-interactivity = on-demand` (default; Ghostdrop switches to
  `exclusive` itself on compositors such as sway that drop on-demand focus) or
  `exclusive`. `quick-terminal-animation-duration` is ignored (no animation).
- **Ghostty config** that is inherited: `~/.config/ghostty/config` and
  `~/.config/ghostty/config.ghostty`.
- **Errors**: `ghostdrop ctl reload` prints config errors and the drop-down shows them.

### Tools

```sh
# Locate the app: prefer the running copy, then the usual install locations.
G="$(ps -o comm= -p "$(pgrep -x ghostdrop | head -1)" 2>/dev/null)"
[ -x "$G" ] || G=/Applications/Ghostdrop.app/Contents/MacOS/ghostdrop
[ -x "$G" ] || G="$HOME/Applications/Ghostdrop.app/Contents/MacOS/ghostdrop"
APP="${G%/Contents/MacOS/ghostdrop}"

"$G" check                     # validate config; prints hotkey/profiles/bindings; exit 1 + errors on problems
"$G" ctl reload                # apply config to the running app (also re-registers the hotkey)
"$G" ctl show | hide | toggle  # show/hide the drop-down
"$G" ctl new-tab [profile]     # open a tab
"$G" ctl login-item on|off     # launch at login (ask the user first)
open -a Ghostdrop              # start the app if it isn't running (pgrep -x ghostdrop)

# Ghostty engine reference (matches the bundled engine version exactly):
"$G" +show-config --default --docs   # every Ghostty option with docs
"$G" +explain-config <option>        # docs for one option, e.g. font-family
"$G" +list-themes --plain            # available themes
"$G" +list-fonts                     # installed fonts usable as font-family
"$G" +list-keybinds --default        # Ghostty's default key bindings
"$G" +list-actions                   # Ghostty binding actions
"$G" +version
less "$APP/Contents/Resources/ghostty/doc/ghostty.5.md"   # full Ghostty config manual
```

### Workflow

1. **Inspect** (read-only):
   - Is it installed and running? `pgrep -x ghostdrop`. If `$APP` is under
     `/Volumes/` or `AppTranslocation`, it's running from the disk image: ask the user
     to drag Ghostdrop into Applications (Ghostdrop also offers to move itself) and
     relaunch it before continuing.
   - Existing Ghostdrop config: `cat ~/.config/ghostdrop/config` (a file with only
     comments is the untouched first-run template).
   - Ghostty config (inherited automatically): `~/.config/ghostty/config`,
     `~/.config/ghostty/config.ghostty`,
     `~/Library/Application Support/com.mitchellh.ghostty/config.ghostty` (and `config`).
     Note any `quick-terminal-*` and `keybind` lines.
   - iTerm2 hotkey window, if any (see *Migrating from iTerm2*).
   - Login shell `echo $SHELL`; ssh hosts they use: `grep -i '^Host ' ~/.ssh/config`.
2. **Interview** — propose defaults based on what you found:
   1. Hotkey to show/hide (default `` ctrl+` `` = `ctrl+grave_accent`).
   2. Position and size (default: top, full width, 50 % height, screen under the mouse).
   3. What tabs run: login shell, an ssh host, tmux, a project dir…? Extra profiles
      bound to their own keys?
   4. Tab shortcuts: macOS style (`cmd+t`, `cmd+w`, `cmd+1…9` — always on), Linux /
      Guake style (`ctrl+shift+t`, `ctrl+page_up/down`, `ctrl+shift+w`), or iTerm2's.
   5. Look: theme, font, font size, transparency/blur — or keep their Ghostty look.
   6. Behaviour: hide when focus moves away? slide animation or instant? tab bar
      auto/always/never, top/bottom? menu-bar icon? Dock icon?
   7. Launch at login?
3. **Write** the config.
4. **Validate**: `"$G" check` must print `OK`. Fix every reported error.
5. **Apply**: `"$G" ctl reload` (or `open -a Ghostdrop` if not running).
6. **Verify** with the user: press the hotkey; try the tab shortcuts. If the hotkey
   does nothing, see *Troubleshooting*.
7. Mention: the menu-bar icon has Open Config / Reload Config / Launch at Login, and
   `cmd+shift+,` reloads the config from inside the terminal.

### How configuration works

Layers, later wins:
1. Ghostdrop defaults: top, 50 % high, screen under the mouse, no animation, autohide.
2. The user's **Ghostty config** (theme, fonts, keybinds, …). Skip with
   `inherit-ghostty-config = false`.
3. `~/.config/ghostdrop/config`. Ghostdrop's own keys (below) are handled by
   Ghostdrop; **every other line is passed to Ghostty** (any Ghostty option works).

### Ghostdrop keys

| Key | Default | Values / notes |
|---|---|---|
| `hotkey` | `ctrl+grave_accent` | one key combo (see *Key names*); `none` disables. Global, needs no Accessibility permission. Modifier-only / double-tap hotkeys are not supported. |
| `default-profile` | `shell` | profile used for the first tab, `new_tab`, `cmd+t`, the `+` button |
| `profile.<name>.command` | — | run via `/bin/sh -c`; empty = Ghostty `command` or the login shell. The built-in profile `shell` is the login shell. |
| `profile.<name>.term` | — | `$TERM` override for that profile's command, e.g. `xterm-256color` for remote hosts without Ghostty's terminfo |
| `profile.<name>.env` | — | `KEY=VALUE`, repeatable (not for `TERM`; use `.term`) |
| `profile.<name>.cwd` | — | working directory (`~` expanded) |
| `profile.<name>.title` | profile name | tab title until the program sets one |
| `profile.<name>.wait-after-command` | `false` | keep the tab open after the command exits |
| `bind` | see below | `trigger=action`, repeatable; `bind = clear` removes all defaults; `bind = <trigger>=unbind` removes one |
| `tab-bar` | `auto` | `auto` (hidden with 1 tab), `always`, `never` |
| `tab-bar-position` | `top` | `top`, `bottom` |
| `status-item` | `true` | menu-bar icon |
| `dock-icon` | `false` | show in Dock / cmd+tab |
| `prespawn` | `true` | start the first tab at launch so the first hotkey press is instant |
| `inherit-ghostty-config` | `true` | load the user's Ghostty config underneath |

Ghostty options that control the drop-down window (set them in Ghostdrop's config):

| Ghostty key | Ghostdrop default | Values |
|---|---|---|
| `quick-terminal-position` | `top` | `top`, `bottom`, `left`, `right`, `center` |
| `quick-terminal-size` | `50%` | `<primary>[,<secondary>]` each `N%` or `Npx`. Primary = height for top/bottom, width for left/right. E.g. `40%,80%` |
| `quick-terminal-screen` | `mouse` | `mouse`, `main`, `macos-menu-bar` |
| `quick-terminal-animation-duration` | `0` | seconds; `0` = instant, `0.1` = quick slide |
| `quick-terminal-autohide` | `true` | hide when another app gets focus |

Other commonly wanted Ghostty options (check exact values with `+explain-config`):
`theme` (`theme = light:X,dark:Y` follows macOS appearance), `font-family`,
`font-size`, `background-opacity` (0–1), `background-blur` (`true` or radius),
`window-padding-x` / `window-padding-y`, `cursor-style`, `macos-option-as-alt`,
`copy-on-select`, `confirm-close-surface` (`false` = never ask before closing a
tab), `scrollback-limit`, `term`, `command`, `working-directory`.
Ignored by Ghostdrop: window decorations/titlebar options, `window-width/height`,
`quick-terminal-space-behavior` (always on all Spaces), splits, the inspector.

### Key names (for `hotkey` and `bind`)

- Modifiers, joined with `+`: `ctrl`/`control`, `shift`, `alt`/`opt`/`option`,
  `cmd`/`command`/`super`.
- Keys: any single character (`a`–`z`, `0`–`9`, `` ` - = [ ] \ ; ' , . / ``), or
  named: `grave_accent` (aliases `backquote`, `grave`, `backtick`), `space`, `tab`,
  `enter`/`return`, `escape`/`esc`, `backspace`, `delete`, `insert`, `home`, `end`,
  `page_up`, `page_down`, `left`, `right`, `up`, `down` (or `arrow_*`), `f1`–`f20`,
  `minus`, `equal`, `bracket_left`, `bracket_right`, `backslash`, `semicolon`,
  `quote`, `comma`, `period`, `slash`, `digit_0`–`digit_9`.
- Examples: `ctrl+grave_accent`, `f12`, `alt+space`, `ctrl+shift+t`, `cmd+shift+left`.
- Avoid as hotkey: `` cmd+` `` (macOS window cycling), `cmd+space` (Spotlight),
  `ctrl+space` (input source switching on many Macs).

### `bind` actions

`new_tab` · `new_tab:<profile>` · `close_tab` · `next_tab` · `previous_tab` ·
`last_tab` · `goto_tab:<N>` · `move_tab:<±N>` · `toggle` · `hide` · `rename_tab` ·
`reload_config` · `open_config` · `quit` · `unbind` · `ghostty:<ghostty action>`
(e.g. `ghostty:clear_screen`).

`bind` keys are handled **before** the terminal and Ghostty keybinds. Default binds:
`ctrl+shift+t=new_tab`, `ctrl+page_up=previous_tab`, `ctrl+page_down=next_tab`,
`ctrl+shift+page_up=move_tab:-1`, `ctrl+shift+page_down=move_tab:1`,
`cmd+shift+left=move_tab:-1`, `cmd+shift+right=move_tab:1`.

Ghostty's own default keybinds also work (macOS): `cmd+t` new tab, `cmd+w` close,
`cmd+1…8` go to tab, `cmd+9` last tab, `ctrl+tab` / `ctrl+shift+tab` and
`cmd+shift+]` / `cmd+shift+[` next/previous, `cmd+f` search, `cmd+k` clear,
`cmd+shift+,` reload config, `cmd+q` quit. Change them with Ghostty syntax, e.g.
`keybind = cmd+t=unbind`. Mouse: drag the free edge to resize (remembered;
double-click to reset), three-finger swipe to switch tabs, middle-click a tab to close.

### Recipes

Guake / Yakuake style:
```ini
hotkey = f12
quick-terminal-size = 45%
bind = ctrl+shift+w=close_tab
bind = alt+1=goto_tab:1
bind = alt+2=goto_tab:2
bind = alt+3=goto_tab:3
```

Remote dev box as the default, local shell on another key:
```ini
profile.devbox.command = ssh devbox
# host lacks Ghostty's terminfo; or install it (see Troubleshooting)
profile.devbox.term = xterm-256color
profile.devbox.title = devbox
default-profile = devbox
bind = ctrl+shift+t=new_tab:shell
```

Always-attached tmux:
```ini
profile.tmux.command = tmux new -A -s drop
default-profile = tmux
```

Translucent bottom panel, 35 % high, 80 % wide, slide in:
```ini
quick-terminal-position = bottom
quick-terminal-size = 35%,80%
quick-terminal-animation-duration = 0.1
background-opacity = 0.9
background-blur = true
```

### Migrating from iTerm2's hotkey window

Read `~/Library/Preferences/com.googlecode.iterm2.plist` with Python `plistlib`
(don't edit it). In `New Bookmarks`, the profile with `"Has Hotkey": true` is the
hotkey window:
- `HotKey Key Code` is a macOS virtual key code: 50 `` ` ``, 49 space, 48 tab,
  122 f1, 120 f2, 99 f3, 118 f4, 96 f5, 97 f6, 98 f7, 100 f8, 101 f9, 109 f10,
  103 f11, 111 f12; letters/digits per the US layout. `HotKey Characters` shows the
  character.
- `HotKey Modifier Flags` bitmask: 131072 shift, 262144 ctrl, 524288 option,
  1048576 cmd (sum).
- `Custom Command == "Yes"` → `Command` becomes a profile command (and usually
  `default-profile`).
- `HotKey Window AutoHides` → `quick-terminal-autohide`; `HotKey Window Animates` →
  animation duration; `Screen` -2 = screen with cursor → `quick-terminal-screen = mouse`.
- Window placement isn't reliably derivable — ask the user.
- `GlobalKeyMap` / profile `Keyboard Map` entries look like `"0x54-0x60000"`
  (character, modifier mask); `Action` 27 = new tab with profile (`Text` is the
  profile GUID — map it to a Ghostdrop profile), 0/32 next tab, 2/39 previous tab,
  33/34 move tab left/right.
- `Prompt Before Closing 2 == 0/false` → `confirm-close-surface = false`.
- Tell the user to turn off iTerm2's hotkey (iTerm2 → Settings → Profiles → Keys →
  Hotkey Window) so both apps don't fight over the key.

Ghostty quick-terminal users: their `quick-terminal-*` settings are inherited
already. If Ghostty has a `keybind = global:...=toggle_quick_terminal` on the same
key and Ghostty runs too, pick a different Ghostdrop hotkey or remove one.

### Troubleshooting

- **Hotkey does nothing**: another app owns it (iTerm2 hotkey window, Ghostty global
  keybind, Raycast/Alfred). The Ghostdrop menu-bar menu shows a ⚠︎ line when the
  hotkey can't be registered. Pick another key.
- **No window appears at all after launch**: that's expected, Ghostdrop has no Dock
  icon; press the hotkey or use the menu-bar icon.
- **"Ghostdrop can't be opened / unidentified developer"** (downloaded builds are not
  notarized): System Settings → Privacy & Security → *Open Anyway*. Only if the user
  trusts the source and asks: `xattr -dr com.apple.quarantine "$APP"`.
- **ssh/curl to LAN or `.local` hosts fails with "No route to host"**: macOS Local
  Network privacy. Approve the prompt, or System Settings → Privacy & Security →
  Local Network → enable Ghostdrop. Internet and VPN hosts are unaffected.
- **Remote host complains about `xterm-ghostty`**: either set
  `profile.<name>.term = xterm-256color`, or install the terminfo on that host (ask first):
  `TERMINFO="$APP/Contents/Resources/terminfo" infocmp -x xterm-ghostty | ssh HOST -- tic -x -`
- **Config errors**: `"$G" check` lists them with line numbers (Ghostty errors are
  prefixed `ghostty:`).
