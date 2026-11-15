# Testing

What was tested on macOS and Linux, how, and what it found. The Linux scripts live
in [`dev/vm/tests/`](../dev/vm/tests); see [`dev/vm/README.md`](../dev/vm/README.md)
for running them.

## Environments

| | Display | GPU | Used for |
|---|---|---|---|
| macOS (Apple Silicon, macOS 26) | WindowServer | Apple GPU (Metal) | the macOS app |
| Linux VM: Arch Linux ARM (aarch64) in an Apple `container` machine | X11 (Xvnc + **i3**) and Wayland (**sway** 1.12 + wayvnc), both over VNC | none: Mesa llvmpipe (software OpenGL) | features, bug bash, end-to-end suite, packaging |
| Linux desktop (x86_64, Arch-based) | Xorg + i3, 4K monitors; a private headless sway for Wayland | NVIDIA RTX 30-series, proprietary driver 610, two GPUs | zero-copy presentation, performance on real hardware |

On the desktop, which was in normal use, tests never showed anything or injected
input: the window was redirected offscreen with Composite, made click-through
(empty input shape) and never took focus (`GHOSTDROP_X11_OFFSCREEN=1`). Its contents
were read back with `ghostdrop ctl snapshot`. Wayland ran in a separate headless
sway with its own runtime directory, captured with `examples/wlshot`
(wlr-screencopy). Input went through `ghostdrop ctl` debug commands, never through
the real keyboard.

## Linux end-to-end suite

`dev/vm/tests/suite.sh` drives the real desktops over VNC (keyboard and mouse) and
checks the outcome through `ghostdrop ctl dump`, files written by the shell and the
system clipboard (`wl-paste` / `xclip`). With a normal window (foot / xterm) open
below the drop-down:

| Check | i3 (X11) | sway (Wayland) |
|---|---|---|
| hotkey shows | ✅ | ✅ |
| typing reaches the shell | ✅ | ✅ |
| copy to the system clipboard | ✅ | ✅ |
| paste from the system clipboard | ✅ | ✅ |
| `ctrl+shift+t` opens a tab | ✅ | ✅ |
| tab bar `+` opens a tab | ✅ | ✅ |
| clicking a tab selects it | ✅ | ✅ |
| middle-click closes a tab | ✅ | ✅ |
| hovering another window keeps it open | ✅ | ✅ |
| clicking another window autohides | ✅ | ✅ |
| hotkey shows again | ✅ | ✅ |
| hotkey hides | ✅ | ✅ |

`dev/vm/tests/profiles.sh` checks that profile commands start and get their `TERM`:
login shell, a plain command, a `term` override, an `/usr/bin/env` wrapper, and
`sh -c` with `;`. All pass.

## Feature and bug-bash tests (Linux)

Each row was tested on i3 and sway unless noted. "Found" lists bugs that the test
caught and that were fixed.

| Area | What was tested | Found |
|---|---|---|
| Focus, autohide | Hover in and out, click another window, click empty desktop, keyboard re-creation, 10 hotkey toggles in a row | Autohide fired on transient focus flicker (now debounced). sway drops focus from on-demand layer surfaces (now detected; exclusive is used). Click-outside never fired on i3 (now XInput2 raw events). i3 refocuses its own window after every binding, racing the toggle (toggle now tolerates a fresh focus loss). |
| Clipboard | `ctrl+shift+c`, copy-on-select (primary), paste from other apps, middle-click paste, OSC 52 | The Wayland clipboard did nothing: data devices weren't created for seats that existed at startup. |
| Protocols | In-terminal probe: DA1, XTVERSION, DECRQM 2026, kitty keyboard (`shift+enter` → `CSI 13;2u`, `ctrl+i` → `CSI 105;5u`, `esc` → `CSI 27u`, modifier presses), kitty graphics, OSC 11, OSC 52, focus reports | X11 reported modifier-key presses without their own modifier (`CSI 57441u` instead of `CSI 57441;2u`). |
| Tabs and UI | `+` button, tab click, middle-click close, close confirmation, rename prompt, search box, scrollback | The search/prompt cursor was a glyph many UI fonts lack (now drawn as a bar). |
| Profiles | Commands, `TERM` override, failing and missing commands, unknown profile | Any profile command other than a bare shell failed to start: libghostty kept a pointer to the command string after it was freed (`patches/ghostty/0002`). `ctl new-tab <unknown>` silently opened the default profile (now an error). |
| HiDPI | sway at scale 2 and 1.5, pointer selection at 1.5 | Fractional scales rendered at 2× and were downscaled (now `wp_fractional_scale_v1` + viewporter: exact device pixels). |
| Fonts | CJK fallback, emoji, box drawing | — |
| Multi-monitor | Wayland outputs of different sizes, unplugging the output it's on; X11 RandR monitors | The drop-down was sized for the wrong output (Wayland: now probes the chosen output; X11: terminals weren't resized when moving between monitors). |
| Keyboard | Key repeat (Wayland: 26 characters in 1.5 s at 600 ms / 25 Hz), X11 hotkey with NumLock and CapsLock on, hotkey owned by another client | — |
| Config | Live reload (font size), invalid option | Errors pointed at an internal scratch file and reload said "ok" (now the user's file and line, shown in the drop-down and returned by `ctl reload`). |
| Robustness | X server and Wayland compositor killed under a running daemon | The X11 daemon spun at 100 % CPU on a dead connection (now exits). |
| Memory | 3 × 25 tabs opened and closed | In-use memory stayed flat (80 MiB), but glibc kept ~1 GiB of freed GL memory (now returned with `malloc_trim` off the UI thread). |
| Lifetime | 20 hide/show cycles plus 20,000 lines of output | File descriptors stable (87 → 87). |
| Screen lock | Show requested while a screen locker holds the keyboard | It would have appeared above the lock screen (now refuses to show). |
| Sessions | Two desktops for one user at the same time | Both daemons shared one control socket (now one per display). |
| Packaging | `makepkg -si` from the checkout (aarch64), build on x86_64 | — |

## Zero-copy presentation (NVIDIA desktop)

| Step | Result |
|---|---|
| Render device | The X server's GPU (from DRI3) differs from EGL's default device on this two-GPU machine; libghostty now renders on the X server's GPU. |
| X11 import | DRI3 `PixmapFromBuffers` failed with `BadAlloc`: GL had allocated a compressed block-linear layout the X server doesn't list. Export buffers are now allocated with GBM using the server's modifiers, and import succeeds. |
| Colours | The first GPU frames were too dark: the driver applied sRGB decoding on fetch. The export is now a raw blit with flipped rows; GPU frames are pixel-identical to CPU frames (background `(40,44,52)`, red text `(87,56,61)` in both). |
| UI composition | The tab bar is drawn on the CPU around the GPU-presented terminal. Prompts switch the tab to CPU frames and back. |
| Stress | 20 hide/show cycles and heavy output: no X errors, no fallbacks, file descriptors stable. |
| Wayland | Headless sway on the same GPU: dmabuf feedback gives the device and 13 ARGB8888 modifiers, the compositor accepts the buffers, and a screenshot shows correct output. |
| Fallback | Without DRI3 (Xvnc) or linux-dmabuf (software-rendered sway), Ghostdrop uses CPU frames from the start; after an import error it switches to CPU frames for good. |

## Performance

Linux, NVIDIA RTX 3070, Xorg + i3, 3840×1080 drop-down:

| | CPU frames | zero-copy |
|---|---|---|
| show → first frame | 1.2–1.6 ms | **0.1–0.2 ms** |
| `cat` of 39 MiB of coloured text | 111 MiB/s | 114 MiB/s |
| CPU during that `cat` (ghostdrop / Xorg) | 890 / 210 ms | 820 / **0** ms |
| idle with a blinking cursor, 5 s (ghostdrop + Xorg) | 250 ms | **30 ms** |

Throughput is limited by terminal parsing, not presentation; the gain is in what
presenting a frame costs.

Linux VM, software rendering (llvmpipe), CPU frames:

| | sway | i3 |
|---|---|---|
| show → first frame | 0.6–1.2 ms | 0.7–1.7 ms |
| `ghostdrop` toggle round trip (process + IPC) | 4.2 ms | 2.9 ms |
| `cat` of 39 MiB | 75 MiB/s | 65 MiB/s |
| CPU while hidden | 0 % | 0 % |

macOS (Apple Silicon): show 5–13 ms of main-thread work, new tab 2–4 ms, tab switch
~0.1 ms, ~240 MB/s of SGR-heavy output, 0 % CPU while hidden.

## macOS

Tested in-app with `control-commands = true` (`ghostdrop ctl send-text`, `key`,
`dump`, `snapshot`) and `GHOSTDROP_TRACE=1` timings:

- show/hide via the global hotkey, prespawned first tab, tab switching and
  reordering, close confirmation, search, profiles, config reload and `check`;
- protocol probe: kitty keyboard, kitty graphics, DECRQM 2026, OSC 52 to the
  pasteboard, focus reports;
- mouse resize (remembered per screen fraction), the Local Network permission
  prompt and warning, the AI setup flow, and running from the disk image;
- release builds on GitHub Actions (macOS 26 runner) with the disk image verified
  before publishing.

## Not covered yet

- A real (non-headless) Wayland session on a GPU, and Mesa drivers (AMD/Intel) on
  hardware; the zero-copy path is the same code, but untested there.
- Input methods (IME) on Linux, which aren't implemented.
- Desktops without wlr-layer-shell (GNOME on Wayland).
