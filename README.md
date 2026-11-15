# Ghostdrop

A fast, native **drop-down (Quake-style) terminal for macOS** built on
[libghostty](https://github.com/ghostty-org/ghostty) — Ghostty's terminal core,
renderer and font stack — with **real tabs** and **global hotkeys**.

```
ctrl+`            show / hide (global, no Accessibility permission needed)
ctrl+shift+t      new tab                     cmd+t        new tab
ctrl+tab          next tab                    ctrl+shift+tab   previous tab
ctrl+page_down    next tab                    ctrl+page_up     previous tab
cmd+1 … cmd+9     jump to tab                 cmd+w        close tab
cmd+shift+←/→     move tab                    ctrl+shift+page_up/down  move tab
cmd+f             search                      cmd+shift+,  reload config
3-finger swipe    previous / next tab         middle-click tab  close
drag free edge    resize (remembered)         double-click edge  reset size
```

## Why

On Linux, Guake/Yakuake/Tilda give you a drop-down terminal with tabs. On macOS
the only good option was iTerm2's hotkey window. Ghostty has a quick terminal,
but **"Quick terminal tabs are only supported on Linux and not on macOS"**
(Ghostty 1.4 docs). Surveyed alternatives (Nov 2026):

| Project | Drop-down | Tabs | Engine | Notes |
|---|---|---|---|---|
| Ghostty 1.4 quick terminal | ✅ | ❌ on macOS | libghostty | no tabs on macOS |
| iTerm2 hotkey window | ✅ | ✅ | own | not Ghostty, heavier |
| kitty `quick-access-terminal` | ✅ | ✅ | kitty | not Ghostty |
| cmux | ❌ | ✅ | libghostty | no drop-down |
| QuickTTY (dntsk/quicktty) | ✅ | ✅ | libghostty | very new, agent/Telegram features, needs Xcode to build |
| QuikTerm (hegarty-tech/quik-term) | ✅ | ✅ | libghostty | 0 stars, bundles a web server/remote access |
| Notchshell | ✅ | ✅ | libghostty | non-commercial license |
| Tabby / Hyper | ✅ | ✅ | Electron | slow |

Ghostdrop is the small, focused version: drop-down + tabs + hotkeys, nothing else
in the hot path.

## Speed

Everything performance-critical is libghostty (Metal renderer on its own thread,
SIMD VT parser, CoreText shaping). The host app is ~3.5k lines of plain AppKit —
no SwiftUI, no Combine, no Electron — and keeps out of the way:

* the first tab is **pre-spawned at launch**, so the first hotkey press is instant;
* tabs are kept alive and switched by toggling visibility (**~0.1 ms**); hidden
  tabs are marked occluded so their renderers idle;
* **show ≈ 5–13 ms** of main-thread work (WindowServer order-front + activate),
  no animation by default (`quick-terminal-animation-duration = 0.1` for a slide);
  **new tab ≈ 2–4 ms** on the main thread;
* libghostty wakeups are coalesced; **0 % CPU** when hidden; App Nap is disabled
  so hidden tabs keep full-speed I/O and the hotkey never wakes a napping process;
* throughput: ~240 MB/s of SGR-heavy output in a test (`cat` of 26 MB).

## Protocol support

It's the Ghostty engine, so you get what Ghostty supports: xterm/VT520-level VT,
truecolor, **kitty keyboard protocol**, **kitty graphics protocol**, synchronized
output (mode 2026), OSC 8 hyperlinks, OSC 52 clipboard (with confirmation
prompts), OSC 7 cwd, OSC 9;4 progress, focus reporting, bracketed paste, all
mouse modes, curly/colored underlines, ligatures, shell integration, IME,
`xterm-ghostty` terminfo. Key events are fed to libghostty exactly the way
Ghostty.app does it (same translation-mods / IME / `performKeyEquivalent` logic),
so key encoding matches Ghostty byte-for-byte. Verified in-app: kitty keyboard
(`shift+enter` → `CSI 13;2u`, `ctrl+i` → `CSI 105;5u`), kitty graphics (`OK`),
DECRQM 2026, OSC 52 → macOS pasteboard, focus in/out reports.

## Build

Only the Xcode **Command Line Tools** are required (no full Xcode):

```sh
git clone --recurse-submodules <this repo> && cd ghostdrop
make            # builds libghostty (downloads Zig 0.17.1 into .tools/ if needed) + the app
make install    # copies Ghostdrop.app to /Applications (PREFIX=~/Applications to change)
open /Applications/Ghostdrop.app
```

Then enable *Launch at Login* from the menu-bar icon.

Notes:
* Ghostty is vendored as a submodule pinned to **v1.4.0** with three small patches
  (`patches/ghostty/`): install a plain `libghostty.a` on macOS without an
  xcframework, optionally compile Metal shaders at runtime (CLT has no `metal`
  compiler — the library is compiled once off the main thread and cached by
  Metal; with full Xcode the shaders are precompiled as usual), and don't force
  `wait-after-command` for profile commands (so tabs close when their command
  exits, like iTerm).
* **Local Network privacy (macOS 15+)**: macOS gates LAN / `.local` / Bonjour
  access per app, *including child processes* (ssh, curl in your tabs). A
  terminal never touches the LAN itself, so macOS may never ask and silently
  deny ("No route to host" to LAN devices; internet and VPN hosts are fine).
  Ghostdrop does a tiny Bonjour browse at launch to trigger the system prompt,
  and if access is denied it shows a warning in the menu-bar item that opens
  *Privacy & Security → Local Network*. Run it from the installed copy
  (`make install`): the rule follows the registered app, and ad-hoc rebuilds run
  from `build/` confuse it.
* If **iTerm2's hotkey window** uses the same hotkey, disable it (iTerm2 →
  Settings → Profiles → Keys → Hotkey Window) or quit iTerm2.

## Configuration

`~/.config/ghostdrop/config` (created with comments on first run; `ghostdrop
check` validates it). Same `key = value` syntax as Ghostty, and it's layered:

1. Ghostdrop defaults (top, 50 % high, mouse screen, no animation)
2. **your Ghostty config** (theme, fonts, keybinds, …) — `inherit-ghostty-config = false` to skip
3. this file — any Ghostty option here overrides Ghostty for the drop-down only

| Key | Default | |
|---|---|---|
| `hotkey` | `ctrl+grave_accent` | global show/hide (`none` to disable) |
| `quick-terminal-position` | `top` | `top`, `bottom`, `left`, `right`, `center` |
| `quick-terminal-size` | `50%` | `50%`, `50%,80%`, `600px` (primary,secondary). Dragging an edge overrides it (saved as a screen fraction, so it carries across monitors) until you double-click an edge or change this option |
| `quick-terminal-screen` | `mouse` | `mouse`, `main`, `macos-menu-bar` |
| `quick-terminal-animation-duration` | `0` | seconds; `0` = instant, e.g. `0.1` for a slide |
| `quick-terminal-autohide` | `true` | hide when focus moves elsewhere |
| `profile.<name>.command` | | command for tabs of this profile (`shell` = login shell) |
| `profile.<name>.term` | | `$TERM` override, e.g. `xterm-256color` for remote hosts |
| `profile.<name>.env` | | `KEY=VALUE`, repeatable |
| `profile.<name>.cwd` / `.title` / `.wait-after-command` | | |
| `default-profile` | `shell` | first tab and `new_tab` / `cmd+t` |
| `bind` | see top | `trigger=action`; `bind = clear` drops defaults; `=unbind` removes one |
| `tab-bar` | `auto` | `auto` (hidden with one tab), `always`, `never` |
| `tab-bar-position` | `top` | `top`, `bottom` |
| `status-item` / `dock-icon` / `prespawn` | `true` / `false` / `true` | |

Bind actions: `new_tab[:profile]`, `close_tab`, `next_tab`, `previous_tab`,
`last_tab`, `goto_tab:N`, `move_tab:±N`, `toggle`, `hide`, `rename_tab`,
`reload_config`, `open_config`, `quit`, `ghostty:<any ghostty action>`.
Ghostty `keybind`s keep working (`new_tab`, `goto_tab`, `move_tab`, `close_tab`,
`toggle_quick_terminal`, `start_search`, … are all handled).

Example (an iTerm2 hotkey-window migration):

```ini
hotkey = ctrl+grave_accent
profile.devbox.command = ssh devbox
profile.devbox.term = xterm-256color
# first tab and cmd+t → remote
default-profile = devbox
# ctrl+shift+t → local shell
bind = ctrl+shift+t=new_tab:shell
confirm-close-surface = false
```

## Scripting

```sh
Ghostdrop.app/Contents/MacOS/ghostdrop ctl toggle|show|hide|next-tab|previous-tab|reload
Ghostdrop.app/Contents/MacOS/ghostdrop ctl new-tab [profile]
Ghostdrop.app/Contents/MacOS/ghostdrop check
```

Relaunching the app (Spotlight, Finder, `open -a Ghostdrop`) also toggles it.
`GHOSTDROP_TRACE=1` logs hot-path timings to stderr. `control-commands = true`
enables test-only commands (`send-text`, `key`, `dump`, `snapshot`) — leave it
off normally.

## Layout

```
Sources/Ghostdrop/
  main.swift            entry, ghostty_init, `ctl` / `check` CLI
  AppDelegate.swift     lifecycle, menu-bar item, config reload, control channel
  GhosttyRuntime.swift  libghostty app/config, runtime callbacks (actions, clipboard, close)
  SurfaceView.swift     NSView hosting one surface: keyboard/IME/mouse → libghostty
  DropController.swift  the panel: geometry, slide animation, autohide, tabs, search
  TabBarView.swift      custom-drawn tab bar
  ResizeHandle.swift    drag-to-resize strips on the free edges
  LocalNetworkCheck.swift  triggers/detects macOS Local Network permission
  Hotkey.swift          Carbon global hotkey + IPC
  Settings.swift        config parsing, key combos, profiles
patches/ghostty/        patches applied to vendor/ghostty before building
scripts/build-libghostty.sh
```

Input handling in `SurfaceView.swift` is adapted from Ghostty's macOS app
(MIT, © Mitchell Hashimoto, Ghostty contributors).
