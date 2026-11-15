# Ghostdrop

A fast, native **drop-down (Quake-style) terminal for macOS and Linux** built on
[libghostty](https://github.com/ghostty-org/ghostty) — Ghostty's terminal core,
renderer and font stack — with **real tabs** and **global hotkeys**. On Linux it
works on both **X11** (tested on i3) and **Wayland** (tested on sway; any
compositor with wlr-layer-shell).

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

On Linux the shortcuts are Ghostty's Linux defaults (`ctrl+shift+t`,
`ctrl+shift+w`, `ctrl+page_up/down`, `alt+1…9`, `ctrl+shift+c/v`, …); see
[Linux details](#linux-details).

## Install

### macOS

Grab the latest **`Ghostdrop-<version>.dmg`** from
[Releases](https://github.com/HackToHell/ghostdrop/releases/latest), open it and
drag Ghostdrop to Applications (Apple Silicon, macOS 13+). Builds aren't notarized
yet: on first launch use *System Settings → Privacy & Security → Open Anyway*.
To build it yourself, see [Build (macOS)](#build-macos).

### Linux

**Arch Linux** (and derivatives such as Manjaro): build a package from the repo with
`makepkg`, which also installs the build tools it needs:

```sh
git clone --recurse-submodules https://github.com/HackToHell/ghostdrop
cd ghostdrop/packaging/arch
makepkg -si
```

(`make pkg` in the repo root does the same.) The package builds from your clone
(local changes included) with the pinned
Zig 0.17.1 (checksummed) and the crates locked in `linux/Cargo.lock`, and installs
`/usr/bin/ghostdrop` plus its own libghostty under `/usr/lib/ghostdrop`. To update,
`git pull --recurse-submodules` and run `makepkg -si` again.

**Other distributions:** install Rust, clang (for bindgen), pkg-config and the
libxkbcommon, libxkbcommon-x11 and libxcb development packages, then

```sh
scripts/build-linux.sh install   # → ~/.local/bin/ghostdrop (Zig is fetched if needed)
```

Then start it from your window manager. On X11 Ghostdrop grabs the hotkey itself;
Wayland has no global key grabs, so there the compositor runs `ghostdrop`, which
toggles the drop-down (starting it if needed):

```
# i3 (X11)                               # sway
exec --no-startup-id ghostdrop --daemon  exec ghostdrop --daemon
                                         bindsym ctrl+grave exec ghostdrop
# Hyprland
exec-once = ghostdrop --daemon
bind = CTRL, grave, exec, ghostdrop
```

(An i3 `bindsym … exec ghostdrop` works too, but i3 refocuses its own window after
every binding, so the built-in grab is both faster and steadier. If something else
already owns `` ctrl+` ``, set another `hotkey`, e.g. `hotkey = ctrl+alt+grave_accent`.)

## Why

On macOS the only good option was iTerm2's hotkey window. Ghostty has a quick
terminal, but **"Quick terminal tabs are only supported on Linux and not on
macOS"** (Ghostty 1.4 docs). Surveyed alternatives (Nov 2026):

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

On Linux there are classic drop-downs, but they are VTE/Konsole-based and X11-first
(on Wayland they run through XWayland or need desktop-specific integration), while
Ghostty's own quick terminal is Wayland-only:

| Project | Drop-down | Tabs | X11 | Wayland | Engine | Notes |
|---|---|---|---|---|---|---|
| **Ghostdrop** | ✅ | ✅ | ✅ | ✅ layer-shell | libghostty | zero-copy GPU presentation; tested on i3 and sway |
| Ghostty 1.4 quick terminal | ✅ | ✅ | ❌ | ✅ layer-shell | libghostty | GTK app; Wayland only |
| kitty `quick-access-terminal` | ✅ | ✅ | partial | ✅ layer-shell | kitty | not Ghostty |
| Guake | ✅ | ✅ | ✅ | partial | VTE (GTK 3) | Python; on Wayland via XWayland/desktop shortcuts |
| Tilda | ✅ | ✅ | ✅ | partial | VTE (GTK 3) | XWayland on Wayland |
| Yakuake | ✅ | ✅ | ✅ | KDE Plasma | Konsole | KDE only |
| xfce4-terminal `--drop-down` | ✅ | ✅ | ✅ | partial | VTE (GTK 3) | |
| tdrop + any terminal | ✅ | the terminal's | ✅ | ❌ | any | wraps another window; X11 only |
| i3/sway scratchpad + any terminal | ~ | the terminal's | ✅ | ✅ | any | DIY: no drop-down placement or autohide |
| Tabby (quake mode) | ✅ | ✅ | ✅ | partial | Electron | slow |

Ghostdrop is the small, focused version: drop-down + tabs + hotkeys on the Ghostty
engine, nothing else in the hot path, on macOS, X11 and Wayland.

## Speed

Everything performance-critical is libghostty: GPU renderer on its own thread
(Metal on macOS, OpenGL on Linux), SIMD VT parser, native font shaping. The host
apps keep out of the way.

**macOS** — ~4k lines of plain AppKit (no SwiftUI, no Combine, no Electron):

* the first tab is **pre-spawned at launch**, so the first hotkey press is instant;
* tabs are kept alive and switched by toggling visibility (**~0.1 ms**); hidden
  tabs are marked occluded so their renderers idle;
* **show ≈ 5–13 ms** of main-thread work (WindowServer order-front + activate),
  no animation by default (`quick-terminal-animation-duration = 0.1` for a slide);
  **new tab ≈ 2–4 ms** on the main thread;
* libghostty wakeups are coalesced; **0 % CPU** when hidden; App Nap is disabled
  so hidden tabs keep full-speed I/O and the hotkey never wakes a napping process;
* throughput: ~240 MB/s of SGR-heavy output in a test (`cat` of 26 MB).

**Linux** — ~6k lines of Rust talking straight to Wayland/X11 (no GTK, no Qt):

* the first tab is pre-spawned; the X11 hotkey is grabbed in-process (no process
  launch per press); on Wayland the compositor binding's round trip is ~3–4 ms;
* **zero-copy presentation**: frames go from libghostty's GPU buffers straight to
  the display server as DMA-BUFs — X11 via DRI3 + Present, Wayland via
  linux-dmabuf — with no readback or copy. Only the tab bar and prompts are drawn on
  the CPU. It renders on the GPU that drives the display (multi-GPU machines) and
  falls back to CPU frames where that isn't possible (e.g. software rendering);
* **0 % CPU** when hidden; hidden tabs don't render; memory from closed tabs is
  handed back to the OS.

Measured on an NVIDIA RTX 3070 (proprietary driver, Xorg + i3, 3840×1080 drop-down):

| | CPU frames | zero-copy |
|---|---|---|
| show → first frame | 1.2–1.6 ms | **0.1–0.2 ms** |
| `cat` 39 MiB (ghostdrop / Xorg CPU) | 111 MiB/s (890 / 210 ms) | 114 MiB/s (820 / **0** ms) |
| idle, cursor blinking (5 s, ghostdrop + Xorg) | 250 ms | **30 ms** |

Without a GPU (software OpenGL in a VM, i3 and sway): show → first frame 0.6–3 ms,
65–75 MiB/s, 0 % CPU while hidden. How all of this was tested:
[docs/testing.md](docs/testing.md).

## Protocol support

It's the Ghostty engine, so you get what Ghostty supports: xterm/VT520-level VT,
truecolor, **kitty keyboard protocol**, **kitty graphics protocol**, synchronized
output (mode 2026), OSC 8 hyperlinks, OSC 52 clipboard (with confirmation
prompts), OSC 7 cwd, OSC 9;4 progress, focus reporting, bracketed paste, all
mouse modes, curly/colored underlines, ligatures, shell integration,
`xterm-ghostty` terminfo, and IME on macOS. Key events are fed to libghostty the
way Ghostty's own apps do it, so key encoding matches Ghostty byte-for-byte.
Verified in-app on macOS, X11 and Wayland: kitty keyboard (`shift+enter` →
`CSI 13;2u`, `ctrl+i` → `CSI 105;5u`), kitty graphics (`OK`), DECRQM 2026, OSC 52 →
system clipboard, focus in/out reports.

## Build (macOS)

Only the Xcode **Command Line Tools** are required (no full Xcode):

```sh
git clone --recurse-submodules https://github.com/HackToHell/ghostdrop && cd ghostdrop
make            # builds libghostty (downloads Zig 0.17.1 into .tools/ if needed) + the app
make install    # copies Ghostdrop.app to /Applications (PREFIX=~/Applications to change)
open /Applications/Ghostdrop.app
```

Then enable *Launch at Login* from the menu-bar icon.

### Distributing / installing for other people

```sh
make dmg                                   # build/Ghostdrop-<version>.dmg
make dmg SIGN_IDENTITY="Developer ID Application: …" && make notarize   # for public downloads
```

Releases are built by GitHub Actions (`.github/workflows/release.yml`): bump
`CFBundleShortVersionString` in `Resources/Info.plist`, add
`.github/release-notes/v<version>.md`, then push a `v<version>` tag. The workflow
builds on a macOS 26 arm64 runner (with precompiled Metal shaders), verifies the disk
image, and publishes it with a SHA-256 checksum.

The disk image contains `Ghostdrop.app`, an `Applications` shortcut to drag it onto,
and **`Set Up with AI.md`**. If someone launches the app straight from the disk image,
Ghostdrop offers to move itself to Applications. Ad-hoc builds (no Developer ID) need
*System Settings → Privacy & Security → Open Anyway* on other Macs.

## Linux details

The same app, a small Rust frontend in `linux/` on the full libghostty (OpenGL
renderer, FreeType/HarfBuzz/fontconfig, the same VT engine). It reads the same
`~/.config/ghostdrop/config`. What's different from macOS:

* tab shortcuts are Ghostty's Linux defaults — `ctrl+shift+t` new, `ctrl+shift+w`
  close, `ctrl+page_up/down` / `ctrl+tab` switch, `alt+1…9` jump, `ctrl+shift+c/v`
  copy/paste (plus primary selection and middle-click), `ctrl+shift+f` search,
  `ctrl+shift+,` reload;
* it appears on the focused output (Wayland) or the monitor under the mouse (X11),
  sized per monitor, with fractional scaling (`wp_fractional_scale_v1`; `Xft.dpi` on
  X11); clicking another window hides it (`quick-terminal-autohide`);
* on sway, which drops keyboard focus from on-demand layer surfaces, it switches to
  exclusive keyboard focus by itself; `quick-terminal-keyboard-interactivity` is
  honoured elsewhere;
* it won't show while another client holds the keyboard (a screen locker);
* config errors are shown in the drop-down and by `ghostdrop ctl reload`;
* zero-copy needs DRI3 ≥ 1.2 + Present (X11) or linux-dmabuf v4 (Wayland) and a
  driver that exports DMA-BUFs; export buffers are allocated with GBM in a layout
  the display server lists as importable (NVIDIA's X server rejects the compressed
  layouts GL picks on its own). `GHOSTDROP_GPU=0` forces CPU frames;
* not yet: IME (text-input-v3 / XIM), slide animation, drag-to-resize.

`dev/vm/` has an Arch Linux test VM (Apple `container`, i3 and sway desktops over
VNC) and `dev/vm/tests/` the end-to-end tests; see [docs/testing.md](docs/testing.md).

## Set up with AI

Ghostdrop ships an agent-oriented setup guide at a fixed path:

```
/Applications/Ghostdrop.app/Contents/Resources/AGENTS.md   # macOS
/usr/share/doc/ghostdrop/AGENTS.md                         # Linux package (~/.local/share/… for build-linux.sh)
```

It tells an AI coding agent (Claude Code, Codex, Cursor, Pi, …) how to inspect the
user's current setup (Ghostty config, iTerm2 hotkey window), interview them, write
`~/.config/ghostdrop/config`, validate it (`ghostdrop check`) and reload
(`ghostdrop ctl reload`). It includes the full key/action reference, recipes and an
iTerm2 migration map. It also gives the agent the bundled engine's own docs:
`ghostdrop +show-config --default --docs`, `+explain-config <key>`, `+list-themes`,
`+list-fonts`, `+list-keybinds`.

How users get there:
* first launch shows a welcome with **Copy AI Setup Prompt**;
* menu-bar icon → **Set Up with AI…** copies a prompt with the correct path;
* `~/.config/ghostdrop/AGENTS.md` and `CLAUDE.md` are symlinks to the guide, so an
  agent started in that folder picks it up automatically (macOS and Linux);
* the disk image includes it as `Set Up with AI.md`.

Notes:
* Ghostty is vendored as a submodule pinned to **v1.4.0** with two patch files
  (`patches/ghostty/`). On macOS: install a plain `libghostty.a` without an
  xcframework, optionally compile Metal shaders at runtime (CLT has no `metal`
  compiler — the library is compiled once off the main thread and cached by
  Metal; with full Xcode the shaders are precompiled as usual), and don't force
  `wait-after-command` for profile commands (so tabs close when their command
  exits, like iTerm). On Linux: a `linux` embedding platform with a frame API
  (CPU or DMA-BUF frames), render-device selection, GBM export buffers, and a fix
  for profile commands being read after they were freed.
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

On Linux `status-item`, `dock-icon`, `macos-menu-bar`, the slide animation and
drag-to-resize don't apply; Wayland shows the drop-down on the focused output and
honours `quick-terminal-keyboard-interactivity`.

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
ghostdrop ctl toggle|show|hide|next-tab|previous-tab|reload
ghostdrop ctl new-tab [profile]
ghostdrop ctl login-item on|off              # macOS
ghostdrop ctl quit [force]                   # Linux
ghostdrop check
ghostdrop +<ghostty-cli-action> …            # e.g. +list-themes --plain
```

On macOS the binary is `Ghostdrop.app/Contents/MacOS/ghostdrop`, and relaunching
the app (Spotlight, Finder, `open -a Ghostdrop`) also toggles it. On Linux running
`ghostdrop` toggles it, starting the daemon if needed.
`GHOSTDROP_TRACE=1` logs hot-path timings to stderr. `control-commands = true`
enables test-only commands (`dump`, `snapshot`, `send-text`, …) — leave it off
normally.

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
Resources/AGENTS.md     setup guide for AI agents (bundled into the app and the DMG)
patches/ghostty/        patches applied to vendor/ghostty before building
scripts/build-libghostty.sh
linux/src/              the Linux app (Rust)
  core.rs               backend-independent logic: tabs, placement, prompts, search, IPC commands
  wayland.rs            wlr-layer-shell surface, input, clipboard, fractional scale, dmabuf subsurface
  x11.rs                override-redirect window, MIT-SHM, DRI3/Present, XKB, hotkey grab, XInput2
  drm.rs                render node of a DRM device (for picking libghostty's GPU)
  ghostty.rs / tab.rs   libghostty runtime, config layering, one surface per tab
  settings.rs, keys.rs, xkb.rs, draw.rs, ipc.rs
linux/examples/wlshot.rs  wlr-screencopy capture used by the Wayland tests
scripts/build-linux.sh  builds libghostty (shared) + the Linux app, `install` to ~/.local
packaging/arch/PKGBUILD makepkg package built from the checkout
dev/vm/                 Arch Linux test VM (i3 + sway over VNC) and tests/
dev/ci/linux-smoke.sh   CI check of an installed package (renders a frame on Xvfb)
docs/testing.md         what was tested, how, and the results
```

Input handling in `SurfaceView.swift` is adapted from Ghostty's macOS app
(MIT, © Mitchell Hashimoto, Ghostty contributors).
