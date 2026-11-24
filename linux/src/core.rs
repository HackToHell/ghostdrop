//! Window-system independent logic: tabs, show/hide, key routing, tab bar,
//! prompts, search, clipboard, IPC. Backends implement `Window` and feed
//! normalized input into `Core`.
use crate::draw::{luminance, mix, Canvas, Font, Rgb};
use crate::ghostty::{
    self, Clip, ClipContent, ClipRequestKind, CloseTabMode, GEvent, KeyAction, MouseButton, MouseShape, QuickTerminalSize, Runtime,
    SurfaceId, mods as gmods,
};
use crate::keys::Mods;
use crate::settings::{Action, Settings, TabBarMode};
use crate::tab::{Frame, Tab};
use crate::xkb::{KeyInput, KeyState};
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// Cap clipboard transfers on both X11 (INCR) and Wayland (pipe) so a huge or
/// runaway selection owner cannot stall or OOM the daemon.
pub const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;

/// `GHOSTDROP_TRACE=1` logs lifecycle/input events to stderr.
pub fn trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("GHOSTDROP_TRACE").is_some())
}

macro_rules! trace {
    ($($t:tt)*) => { if $crate::core::trace_on() { eprintln!("[trace {}] {}", $crate::core::trace_clock(), format!($($t)*)); } };
}

/// Wall-clock time of day (HH:MM:SS.mmm, UTC) for correlating traces with test scripts.
pub fn trace_clock() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let s = d.as_secs() % 86400;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, s / 60 % 60, s % 60, d.subsec_millis())
}
pub(crate) use trace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cursor {
    Default,
    Text,
    Pointer,
    Crosshair,
    ResizeH,
    ResizeV,
    Grab,
    Grabbing,
    NotAllowed,
    Wait,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pos {
    Top,
    Bottom,
    Left,
    Right,
    Center,
}

/// Where/how big the drop-down should be. Sizes are resolved by the backend
/// against the screen it actually uses.
#[derive(Clone, Copy)]
pub struct Placement {
    pub pos: Pos,
    pub size: QuickTerminalSize,
    pub fullscreen: bool,
    /// "mouse" | "main" | "macos-menu-bar"
    pub screen_mouse: bool,
    pub overlay_layer: bool,
    pub exclusive_keyboard: bool,
}

impl Placement {
    /// Logical (w, h) on a screen of `sw` × `sh`; None = span the full axis.
    pub fn resolve(&self, sw: f64, sh: f64) -> (Option<f64>, Option<f64>) {
        if self.fullscreen {
            return (None, None);
        }
        let p = self.size.primary.map(|d| {
            d.resolve(match self.pos {
                Pos::Left | Pos::Right => sw,
                Pos::Center if sw < sh => sw,
                _ => sh,
            })
        });
        let s = self.size.secondary.map(|d| {
            d.resolve(match self.pos {
                Pos::Left | Pos::Right => sh,
                Pos::Center if sw < sh => sh,
                _ => sw,
            })
        });
        let clamp = |v: f64, max: f64| v.clamp(100.0, max);
        match self.pos {
            Pos::Top | Pos::Bottom => (s.map(|v| clamp(v, sw)), Some(clamp(p.unwrap_or(400.0), sh))),
            Pos::Left | Pos::Right => (Some(clamp(p.unwrap_or(400.0), sw)), s.map(|v| clamp(v, sh))),
            Pos::Center => {
                if sw >= sh {
                    (Some(clamp(p.unwrap_or(800.0), sw)), Some(clamp(s.unwrap_or(400.0), sh)))
                } else {
                    (Some(clamp(s.unwrap_or(400.0), sw)), Some(clamp(p.unwrap_or(800.0), sh)))
                }
            }
        }
    }
}

/// x, y, width, height in physical pixels.
pub type Rect = (u32, u32, u32, u32);

/// What the core needs from a window-system backend.
pub trait Window {
    fn show(&mut self, p: &Placement);
    fn hide(&mut self);
    fn is_visible(&self) -> bool;
    fn is_focused(&self) -> bool;
    /// Take keyboard focus while visible (`toggle` on a visible but
    /// unfocused window relies on this, so every backend must implement it).
    fn focus(&mut self);
    fn scale(&self) -> f64;
    /// Physical size of the mapped window.
    fn size(&self) -> (u32, u32);
    fn request_redraw(&mut self);
    fn set_cursor(&mut self, c: Cursor);
    fn set_clipboard(&mut self, clip: Clip, text: String);
    /// Asynchronous; the result comes back via `Core::clipboard_result`.
    fn read_clipboard(&mut self, clip: Clip, token: u64);
    fn backend_name(&self) -> &'static str;
    fn hotkey_status(&self) -> String;
    /// Only a new GPU (DMA-BUF) frame for the terminal area: backends that
    /// present those directly skip recomposing the window.
    fn request_present(&mut self) {
        self.request_redraw();
    }
    /// Stop presenting DMA-BUF frames directly (CPU frames from now on).
    /// Called through `Core::gpu_failed`, which also switches the tabs.
    fn disable_gpu(&mut self, why: &str);
    /// Debug: write what the window shows to a PPM file.
    fn snapshot(&mut self, _path: &str) -> Result<(), String> {
        Err("snapshot is not supported by this backend".into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    Other(u32),
}

#[derive(Clone, Copy, Debug)]
pub enum PointerInput {
    Motion { x: f64, y: f64 },
    Leave,
    Button { button: Button, pressed: bool },
    /// dx/dy in "lines" (discrete) or pixels (precise); positive = down/right.
    Scroll { dx: f64, dy: f64, precise: bool },
}

enum PromptKind {
    CloseTab(SurfaceId),
    Quit,
    ClipConfirm { sid: SurfaceId, contents: Vec<ClipContent>, available: Vec<String>, state: usize },
    ClipWrite { clip: Clip, text: String },
    OpenUrl(String),
    Rename(SurfaceId),
    /// Informational (config errors); any answer dismisses.
    Notice,
}

struct Prompt {
    kind: PromptKind,
    title: String,
    detail: String,
    input: Option<String>,
}

struct Search {
    sid: SurfaceId,
    needle: String,
    total: isize,
    selected: isize,
}

struct ClipRead {
    sid: SurfaceId,
    state: usize,
    want_text: bool,
    list: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    Tab(usize, bool),
    Plus,
}

/// Who owns the pointer between a button press and its release. Drags and
/// the release go to whoever got the press, wherever the pointer is by then
/// (the display server keeps sending us the pointer while a button is held).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Capture {
    None,
    /// `button` went down on the tab bar. `drag` = the tab being dragged
    /// (left button on a tab) and the x where it last moved.
    Bar { button: Button, drag: Option<(SurfaceId, f64)> },
    /// Buttons held in this tab's terminal (bit mask over `button_index`).
    Terminal { id: SurfaceId, held: u8 },
}

fn button_index(b: Button) -> u8 {
    match b {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::Other(8) => 3,
        Button::Other(9) => 4,
        Button::Other(_) => 5,
    }
}

fn button_bit(b: Button) -> u8 {
    1u8 << button_index(b)
}

fn button_from_index(i: u8) -> MouseButton {
    match i {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::Four,
        4 => MouseButton::Five,
        _ => MouseButton::Unknown,
    }
}

/// Where a key press went; its repeats and its release follow it there, even
/// if a prompt opened or closed in between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyTarget {
    /// Ghostdrop used it: a binding, the prompt or the search field.
    Ui,
    /// Sent to this tab's terminal.
    Terminal(SurfaceId),
}

/// Pending autohide. Each variant carries the time it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Autohide {
    Idle,
    /// Keyboard focus went away; hide unless it comes back by then
    /// (compositors and devices bounce focus briefly).
    FocusLost(Instant),
    /// The user clicked outside; hide even if we still hold focus.
    OutsideClick(Instant),
}

impl Autohide {
    fn deadline(self) -> Option<Instant> {
        match self {
            Autohide::Idle => None,
            Autohide::FocusLost(t) | Autohide::OutsideClick(t) => Some(t),
        }
    }
}

fn prompt_about(p: &Prompt, id: SurfaceId) -> bool {
    match p.kind {
        PromptKind::CloseTab(x) | PromptKind::Rename(x) => x == id,
        PromptKind::ClipConfirm { sid, .. } => sid == id,
        _ => false,
    }
}

/// The same question asked twice (e.g. close requested again) is asked once.
fn same_question(a: &PromptKind, b: &PromptKind) -> bool {
    match (a, b) {
        (PromptKind::CloseTab(x), PromptKind::CloseTab(y)) | (PromptKind::Rename(x), PromptKind::Rename(y)) => x == y,
        (PromptKind::Quit, PromptKind::Quit) => true,
        _ => false,
    }
}

pub struct Core {
    pub settings: Settings,
    pub rt: Runtime,
    pub tabs: Vec<Tab>,
    pub selected: usize,
    next_id: u64,
    fullscreen: bool,
    font: Font,
    hover: Option<Hover>,
    mouse: (f64, f64),
    capture: Capture,
    last_bar_click: Option<Instant>,
    term_cursor: Cursor,
    cursor_shown: Option<Cursor>,
    /// The prompt on screen; `prompts` wait behind it (never set without it).
    prompt: Option<Prompt>,
    prompts: VecDeque<Prompt>,
    search: Option<Search>,
    clip_reads: HashMap<u64, ClipRead>,
    next_token: u64,
    keys_down: HashMap<u32, KeyTarget>,
    autohide: Autohide,
    /// For tracing show -> first presented frame latency.
    shown_at: Option<Instant>,
    /// Config errors found at startup are shown on the first show.
    notice_pending: bool,
    /// Pending malloc_trim after tabs were closed.
    trim_at: Option<Instant>,
    /// The backend presents DMA-BUF frames directly (zero copy).
    gpu_backend: bool,
    /// CPU frames in a row while GPU frames were wanted (export failing).
    gpu_misses: u32,
    /// When keyboard focus was last lost (see `toggle`).
    focus_lost_at: Option<Instant>,
    pub quit: bool,
    pub last_error: Option<String>,
}

const TAB_BAR_HEIGHT: f64 = 28.0;

fn mouse_button(b: Button) -> MouseButton {
    match b {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
        Button::Other(8) => MouseButton::Four,
        Button::Other(9) => MouseButton::Five,
        Button::Other(_) => MouseButton::Unknown,
    }
}

/// Run a helper (xdg-open, notify-send) fully detached.
pub fn spawn_detached(program: &str, args: &[&str]) {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

impl Core {
    pub fn new(settings: Settings, rt: Runtime) -> Core {
        let notice_pending = !settings.errors.is_empty();
        Core {
            settings,
            rt,
            tabs: Vec::new(),
            selected: 0,
            next_id: 1,
            fullscreen: false,
            font: Font::load(),
            hover: None,
            mouse: (-1.0, -1.0),
            capture: Capture::None,
            last_bar_click: None,
            term_cursor: Cursor::Text,
            cursor_shown: None,
            prompt: None,
            prompts: VecDeque::new(),
            search: None,
            clip_reads: HashMap::new(),
            next_token: 1,
            keys_down: HashMap::new(),
            autohide: Autohide::Idle,
            shown_at: None,
            notice_pending,
            trim_at: None,
            gpu_backend: false,
            gpu_misses: 0,
            focus_lost_at: None,
            quit: false,
            last_error: None,
        }
    }

    // ------------------------------------------------------------ geometry

    pub fn placement(&self) -> Placement {
        let pos = match self.rt.string("quick-terminal-position").as_deref() {
            Some("bottom") => Pos::Bottom,
            Some("left") => Pos::Left,
            Some("right") => Pos::Right,
            Some("center") => Pos::Center,
            _ => Pos::Top,
        };
        Placement {
            pos,
            size: self.rt.quick_terminal_size(),
            fullscreen: self.fullscreen,
            screen_mouse: self.rt.string("quick-terminal-screen").as_deref() != Some("main"),
            overlay_layer: self.rt.string("gtk-quick-terminal-layer").as_deref() == Some("overlay"),
            exclusive_keyboard: self.rt.string("quick-terminal-keyboard-interactivity").as_deref() == Some("exclusive"),
        }
    }

    fn tab_bar_visible(&self) -> bool {
        match self.settings.tab_bar {
            TabBarMode::Always => true,
            TabBarMode::Never => false,
            TabBarMode::Auto => self.tabs.len() > 1,
        }
    }

    fn bar_height(&self, scale: f64) -> u32 {
        if self.tab_bar_visible() {
            (TAB_BAR_HEIGHT * scale).round() as u32
        } else {
            0
        }
    }

    /// Terminal area in physical pixels: (x, y, w, h).
    fn term_rect(&self, w: &dyn Window) -> (u32, u32, u32, u32) {
        self.term_rect_at(w.size(), w.scale())
    }

    pub fn term_rect_at(&self, (ww, wh): (u32, u32), scale: f64) -> (u32, u32, u32, u32) {
        let bh = self.bar_height(scale).min(wh);
        let y = if self.settings.tab_bar_top { bh } else { 0 };
        (0, y, ww, wh - bh)
    }

    /// Push size/scale to every surface (after resize, scale or tab-bar change).
    pub fn relayout(&mut self, w: &mut dyn Window) {
        let (ww, wh) = w.size();
        if ww == 0 || wh == 0 {
            return; // not configured yet (Wayland); the configure will relayout
        }
        let (_, _, tw, th) = self.term_rect(w);
        let scale = w.scale();
        for t in &self.tabs {
            t.set_scale(scale);
            t.set_size(tw, th);
        }
        w.request_redraw();
    }

    fn colors(&self) -> (Rgb, Rgb) {
        let bg = self.selected_tab().and_then(|t| t.background).or_else(|| self.rt.color("background")).unwrap_or([0x1d, 0x1f, 0x21]);
        let fg = self.rt.color("foreground").unwrap_or([0xc5, 0xc8, 0xc6]);
        (bg, fg)
    }

    // ------------------------------------------------------------ tabs

    pub fn selected_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.selected)
    }

    fn tab_index(&self, id: SurfaceId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn new_tab(&mut self, w: &mut dyn Window, profile: Option<&str>) {
        self.new_tab_from(w, profile, None)
    }

    /// New tab; cwd/font size are inherited from `source` (or the selected tab).
    pub fn new_tab_from(&mut self, w: &mut dyn Window, profile: Option<&str>, source: Option<SurfaceId>) {
        let profile = self.settings.profile(profile);
        let (mut cwd, mut font_size) = (None, 0.0f32);
        let src = source.and_then(|id| self.tab_index(id)).or((!self.tabs.is_empty()).then_some(self.selected));
        if let Some(cur) = src.and_then(|i| self.tabs.get(i)) {
            let (inherited_cwd, inherited_font) = cur.inherited_tab_config();
            font_size = inherited_font;
            if profile.effective_command().is_none() && cur.profile.effective_command().is_none() {
                cwd = inherited_cwd;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let scale = w.scale();
        match Tab::new(&self.rt, id, profile, cwd, font_size, scale) {
            Some(tab) => {
                let had_bar = self.tab_bar_visible();
                self.tabs.push(tab);
                let (_, _, tw, th) = self.term_rect(w);
                let (tw, th) = if tw == 0 || th == 0 { (800, 400) } else { (tw, th) };
                let t = self.tabs.last().unwrap();
                t.set_scale(scale);
                t.set_size(tw, th);
                let idx = self.tabs.len() - 1;
                self.select(w, idx);
                if had_bar != self.tab_bar_visible() {
                    self.relayout(w);
                }
            }
            None => self.last_error = Some("failed to create terminal surface".into()),
        }
    }

    pub fn select(&mut self, w: &mut dyn Window, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if let Some(s) = self.search.take() {
            if let Some(i) = self.tab_index(s.sid) {
                self.tabs[i].binding_action("end_search");
            }
        }
        let visible = w.is_visible();
        let focused = w.is_focused();
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let on = i == idx;
            t.set_visible(on && visible);
            t.set_focus(on && focused);
        }
        self.selected = idx;
        w.request_redraw();
    }

    fn goto(&mut self, w: &mut dyn Window, n: i32) {
        let len = self.tabs.len();
        if len == 0 {
            return;
        }
        let i = match n {
            -1 => (self.selected + len - 1) % len,
            -2 => (self.selected + 1) % len,
            -3 => len - 1,
            n if n >= 1 => (n as usize - 1).min(len - 1),
            _ => return,
        };
        self.select(w, i);
    }

    fn move_tab(&mut self, w: &mut dyn Window, delta: isize) {
        let len = self.tabs.len() as isize;
        if len < 2 || delta == 0 {
            return;
        }
        let to = ((self.selected as isize + delta) % len + len) % len;
        self.move_tab_to(w, self.selected, to as usize);
    }

    fn move_tab_to(&mut self, w: &mut dyn Window, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        if self.selected == from {
            self.selected = to;
        } else if from < self.selected && to >= self.selected {
            self.selected -= 1;
        } else if from > self.selected && to <= self.selected {
            self.selected += 1;
        }
        w.request_redraw();
    }

    fn close_tab_now(&mut self, w: &mut dyn Window, id: SurfaceId) {
        let Some(idx) = self.tab_index(id) else { return };
        let had_bar = self.tab_bar_visible();
        // Deny in-flight clipboard requests belonging to this tab before freeing its surface.
        let mut stale_states = Vec::new();
        self.clip_reads.retain(|_, r| {
            if r.sid == id {
                stale_states.push(r.state);
                false
            } else {
                true
            }
        });
        if self.search.as_ref().is_some_and(|s| s.sid == id) {
            self.search = None;
        }
        // Questions about this tab are moot now (its surface, and any
        // clipboard request it made, goes away with it).
        self.prompts.retain(|p| {
            if prompt_about(p, id) {
                if let PromptKind::ClipConfirm { state, .. } = p.kind {
                    stale_states.push(state);
                }
                false
            } else {
                true
            }
        });
        let prompt_gone = self.prompt.as_ref().is_some_and(|p| prompt_about(p, id));
        if prompt_gone {
            if let Some(Prompt { kind: PromptKind::ClipConfirm { state, .. }, .. }) = self.prompt.take() {
                stale_states.push(state);
            }
            self.prompt = self.prompts.pop_front();
            w.request_redraw();
        }
        for state in stale_states {
            self.tabs[idx].deny_clipboard(state);
        }
        if matches!(self.capture, Capture::Terminal { id: c, .. } if c == id) {
            self.capture = Capture::None;
        }
        if let Capture::Bar { drag, .. } = &mut self.capture {
            if drag.is_some_and(|(d, _)| d == id) {
                *drag = None;
            }
        }
        self.keys_down.retain(|_, t| *t != KeyTarget::Terminal(id));
        self.tabs.remove(idx); // drop frees the surface (and denies any states still in QUEUE)
        // A surface's GL context and buffers free into glibc's per-thread
        // arenas, which keep it (hundreds of MiB after many tabs). Hand it
        // back to the OS once things settle.
        self.trim_at = Some(Instant::now() + std::time::Duration::from_millis(1500));
        if self.tabs.is_empty() {
            self.selected = 0;
            self.hide(w);
            return;
        }
        if idx < self.selected || self.selected >= self.tabs.len() {
            self.selected = self.selected.saturating_sub(1);
        }
        let sel = self.selected.min(self.tabs.len() - 1);
        self.select(w, sel);
        if had_bar != self.tab_bar_visible() {
            self.relayout(w);
        }
        self.sync_gpu();
        if prompt_gone {
            self.show_prompt_tab(w);
        }
    }

    // ------------------------------------------------------------ show/hide

    pub fn prespawn(&mut self, w: &mut dyn Window) {
        if self.settings.prespawn && self.tabs.is_empty() {
            self.new_tab(w, None);
            for t in &self.tabs {
                t.set_visible(false);
            }
        }
    }

    pub fn show(&mut self, w: &mut dyn Window) {
        trace!("show");
        if trace_on() {
            self.shown_at = Some(Instant::now());
        }
        if self.tabs.is_empty() {
            self.new_tab(w, None);
        }
        let p = self.placement();
        w.show(&p);
        if !w.is_visible() {
            trace!("show refused by the backend");
            return;
        }
        if std::mem::take(&mut self.notice_pending) {
            self.config_notice(w);
        }
        // The window may have moved to a monitor of a different size (X11
        // sizes synchronously; Wayland relayouts again on configure).
        self.relayout(w);
        let sel = self.selected;
        if let Some(t) = self.tabs.get(sel) {
            t.set_visible(true);
        }
        w.request_redraw();
    }

    /// Cancel all interactive prompts (keeping any non-interactive config notice),
    /// denying pending clipboard requests so terminal apps don't hang waiting.
    fn cancel_interactive_prompts(&mut self) {
        let mut notice: Option<Prompt> = None;
        let mut deny: Vec<(SurfaceId, usize)> = Vec::new();
        for p in self.prompt.take().into_iter().chain(self.prompts.drain(..)) {
            match p.kind {
                PromptKind::Notice => {
                    if notice.is_none() {
                        notice = Some(p);
                    }
                }
                PromptKind::ClipConfirm { sid, state, .. } => deny.push((sid, state)),
                _ => {}
            }
        }
        self.prompt = notice;
        for (sid, state) in deny {
            if let Some(i) = self.tab_index(sid) {
                self.tabs[i].deny_clipboard(state);
            }
        }
    }

    pub fn hide(&mut self, w: &mut dyn Window) {
        trace!("hide");
        self.autohide = Autohide::Idle;
        self.cancel_interactive_prompts();
        // The release of a held button won't reach us once we're gone.
        self.release_capture();
        self.keys_down.clear();
        w.hide();
        for t in self.tabs.iter_mut() {
            t.set_visible(false);
            t.set_focus(false);
        }
        self.rt.set_focus(false);
        self.sync_gpu();
    }

    pub fn toggle(&mut self, w: &mut dyn Window) {
        if w.is_visible() {
            // Window managers running a key binding (e.g. i3's `exec
            // ghostdrop`) refocus their own window right as the toggle
            // arrives; a focus loss that recent still counts as focused.
            let just_lost = self.focus_lost_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300));
            if w.is_focused() || just_lost {
                self.hide(w);
            } else {
                w.focus();
            }
        } else {
            self.show(w);
        }
    }

    /// Backend reports keyboard focus changes.
    pub fn focus_changed(&mut self, w: &mut dyn Window, focused: bool) {
        trace!("focus_changed {focused} visible={}", w.is_visible());
        self.focus_lost_at = (!focused).then(Instant::now);
        self.rt.set_focus(focused);
        let sel = self.selected;
        if let Some(t) = self.tabs.get_mut(sel) {
            t.set_focus(focused);
        }
        if focused {
            // A click outside still hides even though focus is back.
            if matches!(self.autohide, Autohide::FocusLost(_)) {
                self.autohide = Autohide::Idle;
            }
        } else {
            self.keys_down.clear();
            // Debounced: compositors/devices can bounce focus briefly
            // (keyboard re-creation, layout switches). Only hide if it stays away.
            // A pending outside-click hide keeps its (earlier) deadline.
            if self.autohide == Autohide::Idle
                && w.is_visible()
                && self.prompt.is_none()
                && self.rt.bool("quick-terminal-autohide", true)
            {
                self.autohide = Autohide::FocusLost(Instant::now() + std::time::Duration::from_millis(250));
            }
        }
        w.request_redraw();
    }

    /// Backends report whether they can present DMA-BUF frames directly.
    pub fn set_gpu_backend(&mut self, on: bool) {
        if self.gpu_backend != on {
            trace!("gpu presentation {}", if on { "on" } else { "off" });
        }
        self.gpu_backend = on;
        self.gpu_misses = 0;
        self.sync_gpu();
    }

    /// Zero-copy presentation failed (import, present or export): the backend
    /// and the tabs both go back to CPU frames, together.
    pub fn gpu_failed(&mut self, w: &mut dyn Window, why: &str) {
        w.disable_gpu(why);
        self.set_gpu_backend(false);
    }

    /// GPU frames when the backend can present them and nothing is drawn on
    /// top of the terminal (prompts and the search box need CPU composition).
    fn sync_gpu(&mut self) {
        let want = self.gpu_backend && self.prompt.is_none() && self.search.is_none();
        for t in &self.tabs {
            t.set_gpu(want);
        }
    }

    /// The selected tab's frame if it is a DMA-BUF to present directly, with
    /// the terminal rectangle (physical px) for a window of `size`/`scale`.
    pub fn gpu_frame(&self, size: (u32, u32), scale: f64) -> Option<(&Frame, Rect)> {
        if !self.gpu_backend || self.prompt.is_some() || self.search.is_some() {
            return None;
        }
        let f = self.tabs.get(self.selected)?.frame.as_ref().filter(|f| f.dmabuf().is_some())?;
        Some((f, self.term_rect_at(size, scale)))
    }

    /// The user clicked outside the dropdown (backends detect this their own way).
    pub fn outside_click(&mut self, w: &mut dyn Window) {
        if w.is_visible() && self.prompt.is_none() && self.rt.bool("quick-terminal-autohide", true) {
            trace!("outside click -> autohide");
            // Short delay so the WM's own focus change (if any) lands first;
            // hide() then only restores focus if we still hold it.
            self.autohide = Autohide::OutsideClick(Instant::now() + std::time::Duration::from_millis(60));
        }
    }

    /// When the event loop should wake up for our timers.
    pub fn next_deadline(&self) -> Option<Instant> {
        match (self.autohide.deadline(), self.trim_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Fire due timers (called by the backends after each dispatch).
    pub fn poll_timers(&mut self, w: &mut dyn Window) {
        if self.trim_at.is_some_and(|t| Instant::now() >= t) {
            self.trim_at = None;
            // Takes tens of ms on a big heap; keep it off the UI thread.
            let _ = std::thread::Builder::new().name("trim".into()).spawn(|| {
                let t0 = Instant::now();
                unsafe { libc::malloc_trim(0) };
                trace!("malloc_trim {:.1} ms (background)", t0.elapsed().as_secs_f64() * 1e3);
            });
        }
        if self.autohide.deadline().is_some_and(|at| Instant::now() >= at) {
            let forced = matches!(self.autohide, Autohide::OutsideClick(_));
            self.autohide = Autohide::Idle;
            if w.is_visible() && (forced || !w.is_focused()) && self.prompt.is_none() {
                trace!("autohide");
                self.hide(w);
            }
        }
    }

    /// Backend reports the window was resized / rescaled.
    pub fn resized(&mut self, w: &mut dyn Window) {
        self.relayout(w);
    }

    // ------------------------------------------------------------ actions

    pub fn perform(&mut self, w: &mut dyn Window, a: &Action) {
        match a {
            Action::NewTab(p) => {
                self.new_tab(w, p.as_deref());
                if !w.is_visible() {
                    self.show(w);
                }
            }
            Action::CloseTab => {
                if let Some(t) = self.selected_tab() {
                    t.request_close();
                }
            }
            Action::NextTab => self.goto(w, -2),
            Action::PreviousTab => self.goto(w, -1),
            Action::LastTab => self.goto(w, -3),
            Action::GotoTab(n) => self.goto(w, *n),
            Action::MoveTab(n) => self.move_tab(w, *n as isize),
            Action::Toggle => self.toggle(w),
            Action::Hide => self.hide(w),
            Action::RenameTab => {
                if let Some(id) = self.selected_tab().map(|t| t.id) {
                    self.prompt_rename(w, id);
                }
            }
            Action::ReloadConfig => self.reload(w),
            Action::OpenConfig => spawn_detached("xdg-open", &[&Settings::path().to_string_lossy()]),
            Action::Quit => self.request_quit(w),
            Action::Unbind => {}
            Action::Ghostty(act) => {
                if let Some(t) = self.selected_tab() {
                    t.binding_action(act);
                }
            }
        }
        self.process(w);
    }

    pub fn reload(&mut self, w: &mut dyn Window) {
        let mut s = Settings::load();
        self.rt.reload(&mut s);
        for e in &s.errors {
            eprintln!("ghostdrop config: {e}");
        }
        self.settings = s;
        if w.is_visible() {
            let p = self.placement();
            w.show(&p);
        }
        self.relayout(w);
        self.config_notice(w);
    }

    /// Surface config problems in the dropdown (macOS shows them in its menu).
    pub fn config_notice(&mut self, w: &mut dyn Window) {
        let is_notice = |p: &Prompt| matches!(p.kind, PromptKind::Notice);
        let errs = &self.settings.errors;
        if errs.is_empty() {
            self.prompts.retain(|p| !is_notice(p));
            if self.prompt.as_ref().is_some_and(is_notice) {
                self.prompt = self.prompts.pop_front();
            }
            w.request_redraw();
            return;
        }
        let mut detail: Vec<String> = errs.iter().take(3).cloned().collect();
        if errs.len() > 3 {
            detail.push(format!("… and {} more", errs.len() - 3));
        }
        detail.push("Run `ghostdrop check` for details; fix and reload.".into());
        let notice = Prompt {
            kind: PromptKind::Notice,
            title: format!("Config: {} issue{}", errs.len(), if errs.len() == 1 { "" } else { "s" }),
            detail: detail.join("\n"),
            input: None,
        };
        // Update the notice in place, else show it, else queue it behind
        // the questions (never replace one).
        if let Some(slot) = self.prompt.iter_mut().chain(self.prompts.iter_mut()).find(|p| is_notice(p)) {
            *slot = notice;
        } else if self.prompt.is_none() {
            self.prompt = Some(notice);
        } else {
            self.prompts.push_back(notice);
        }
        w.request_redraw();
    }

    /// Ask a question. Questions are answered one at a time, in order:
    /// replacing one would drop it unanswered (a clipboard request would
    /// never be completed, a tab never closed). A config notice on screen
    /// steps back behind it.
    fn ask(&mut self, w: &mut dyn Window, kind: PromptKind, title: String, detail: String, input: Option<String>) {
        self.ensure_visible(w);
        if self.prompt.iter().chain(self.prompts.iter()).any(|p| same_question(&p.kind, &kind)) {
            return;
        }
        let p = Prompt { kind, title, detail, input };
        match self.prompt.take() {
            None => self.prompt = Some(p),
            Some(n) if matches!(n.kind, PromptKind::Notice) => {
                self.prompts.push_front(n);
                self.prompt = Some(p);
            }
            Some(cur) => {
                self.prompt = Some(cur);
                self.prompts.push_back(p);
            }
        }
        self.show_prompt_tab(w);
        self.sync_gpu();
        w.request_redraw();
    }

    /// A question about a tab is asked with that tab in front (when it comes
    /// on screen, not when it was queued).
    fn show_prompt_tab(&mut self, w: &mut dyn Window) {
        let id = match self.prompt.as_ref().map(|p| &p.kind) {
            Some(PromptKind::CloseTab(id) | PromptKind::Rename(id)) => *id,
            _ => return,
        };
        if let Some(i) = self.tab_index(id).filter(|i| *i != self.selected) {
            self.select(w, i);
        }
    }

    fn request_quit(&mut self, w: &mut dyn Window) {
        if self.rt.needs_confirm_quit() {
            self.ask(w, PromptKind::Quit, "Quit Ghostdrop?".into(), "Processes are still running in some tabs.".into(), None);
        } else {
            self.quit = true;
        }
    }

    fn ensure_visible(&mut self, w: &mut dyn Window) {
        if !w.is_visible() {
            self.show(w);
        }
    }

    fn prompt_rename(&mut self, w: &mut dyn Window, id: SurfaceId) {
        let cur = self.tab_index(id).and_then(|i| self.tabs[i].title_override.clone()).unwrap_or_default();
        self.ask(w, PromptKind::Rename(id), "Rename tab".into(), "Empty = use the title set by the terminal.".into(), Some(cur));
    }

    fn open_url(&mut self, w: &mut dyn Window, url: String, trusted: bool) {
        let lower = url.to_lowercase();
        let safe = ["http://", "https://", "mailto:", "ftp://", "sftp://", "ssh://"].iter().any(|p| lower.starts_with(p));
        if trusted || safe {
            spawn_detached("xdg-open", &[&url]);
        } else {
            self.ask(w, PromptKind::OpenUrl(url.clone()), "Open this link?".into(), url, None);
        }
    }

    // ------------------------------------------------------------ libghostty events

    pub fn tick(&mut self, w: &mut dyn Window) {
        self.rt.tick();
        self.process(w);
    }

    /// Handle everything libghostty queued (possibly synchronously during
    /// an input call).
    pub fn process(&mut self, w: &mut dyn Window) {
        ghostty::assert_not_in_call("Core::process");
        for _ in 0..8 {
            let evs = ghostty::drain();
            if evs.is_empty() {
                return;
            }
            for ev in evs {
                self.handle(w, ev);
            }
        }
        if ghostty::has_pending() {
            ghostty::wake();
        }
    }

    fn handle(&mut self, w: &mut dyn Window, ev: GEvent) {
        ghostty::assert_not_in_call("Core::handle");
        if !matches!(ev, GEvent::Render(_)) {
            trace!("event {ev:?}");
        }
        match ev {
            GEvent::Render(id) => {
                if let Some(i) = self.tab_index(id) {
                    if let Some(f) = self.tabs[i].take_frame() {
                        let gpu = f.dmabuf().is_some();
                        // This tab was asked for GPU frames but CPU frames
                        // keep coming: the driver can't export (e.g. software
                        // rendering). Judged by what the tab was actually
                        // asked for: modes switch lazily (at the next draw),
                        // so CPU frames right after a switch aren't failures.
                        let asked = self.gpu_backend && self.tabs[i].gpu();
                        self.gpu_misses = if asked && !gpu { self.gpu_misses + 1 } else { 0 };
                        if self.gpu_misses >= 8 {
                            self.gpu_failed(w, "DMA-BUF export failed");
                        }
                        let was_gpu = self.tabs[i].frame.as_ref().is_some_and(|p| p.dmabuf().is_some());
                        self.tabs[i].frame = Some(f);
                        if i == self.selected && w.is_visible() {
                            // Same kind as before: just the terminal area changed.
                            if gpu && was_gpu {
                                w.request_present();
                            } else {
                                w.request_redraw();
                            }
                        }
                    }
                }
            }
            GEvent::NewTab(source) => {
                self.new_tab_from(w, None, source);
                self.ensure_visible(w);
            }
            GEvent::CloseTab(id, mode) => {
                let ids: Vec<SurfaceId> = match mode {
                    CloseTabMode::Other => self.tabs.iter().map(|t| t.id).filter(|x| *x != id).collect(),
                    CloseTabMode::Right => match self.tab_index(id) {
                        Some(i) => self.tabs[i + 1..].iter().map(|t| t.id).collect(),
                        None => vec![],
                    },
                    CloseTabMode::This => vec![id],
                };
                for x in ids {
                    if let Some(i) = self.tab_index(x) {
                        self.tabs[i].request_close();
                    }
                }
            }
            GEvent::CloseWindow => self.hide(w),
            GEvent::GotoTab(n) => self.goto(w, n),
            GEvent::MoveTab(n) => self.move_tab(w, n),
            GEvent::Toggle => self.toggle(w),
            GEvent::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                if w.is_visible() {
                    let p = self.placement();
                    w.show(&p);
                    self.relayout(w);
                }
            }
            GEvent::ResetSize => {
                self.fullscreen = false;
                if w.is_visible() {
                    let p = self.placement();
                    w.show(&p);
                    self.relayout(w);
                }
            }
            GEvent::Quit => self.request_quit(w),
            GEvent::ReloadConfig { soft } => {
                if soft {
                    self.rt.soft_reload();
                } else {
                    self.reload(w);
                }
            }
            GEvent::OpenConfig => spawn_detached("xdg-open", &[&Settings::path().to_string_lossy()]),
            GEvent::SetTitle(id, t) => {
                if let Some(i) = self.tab_index(id) {
                    if self.tabs[i].term_title != t {
                        self.tabs[i].term_title = t;
                        w.request_redraw();
                    }
                }
            }
            GEvent::Pwd(id, p) => {
                if let Some(i) = self.tab_index(id) {
                    self.tabs[i].pwd = Some(p);
                }
            }
            GEvent::MouseShape(id, shape) => {
                if self.selected_tab().is_some_and(|t| t.id != id) {
                    return;
                }
                self.term_cursor = match shape {
                    MouseShape::Default => Cursor::Default,
                    MouseShape::Pointer => Cursor::Pointer,
                    MouseShape::Crosshair => Cursor::Crosshair,
                    MouseShape::Grab => Cursor::Grab,
                    MouseShape::Grabbing => Cursor::Grabbing,
                    MouseShape::NotAllowed => Cursor::NotAllowed,
                    MouseShape::Wait => Cursor::Wait,
                    MouseShape::ResizeH => Cursor::ResizeH,
                    MouseShape::ResizeV => Cursor::ResizeV,
                    MouseShape::Text => Cursor::Text,
                };
                self.update_cursor(w);
            }
            GEvent::MouseVisible(visible) => {
                if !visible {
                    self.cursor_shown = Some(Cursor::Hidden);
                    w.set_cursor(Cursor::Hidden);
                } else {
                    self.update_cursor(w);
                }
            }
            GEvent::OpenUrl { url, trusted } => self.open_url(w, url, trusted),
            GEvent::Bell(id) => {
                if let Some(i) = self.tab_index(id) {
                    if i != self.selected || !w.is_visible() {
                        self.tabs[i].bell = true;
                        w.request_redraw();
                    }
                }
            }
            GEvent::Notify { title, body } => {
                let title = if title.is_empty() { "Ghostdrop".to_string() } else { title };
                spawn_detached("notify-send", &["-a", "Ghostdrop", &title, &body]);
            }
            GEvent::StartSearch(id, needle) => {
                let Some(i) = self.tab_index(id) else { return };
                if let Some(prev) = self.search.as_ref().filter(|s| s.sid != id).map(|s| s.sid) {
                    if let Some(pi) = self.tab_index(prev) {
                        self.tabs[pi].binding_action("end_search");
                    }
                }
                self.search = Some(Search { sid: id, needle: needle.clone(), total: -1, selected: -1 });
                self.tabs[i].binding_action(&format!("search:{needle}"));
                self.sync_gpu();
                w.request_redraw();
            }
            GEvent::EndSearch(id) => {
                if self.search.as_ref().is_some_and(|s| s.sid == id) {
                    self.search = None;
                    self.sync_gpu();
                    w.request_redraw();
                }
            }
            GEvent::SearchTotal(id, n) => {
                if let Some(s) = self.search.as_mut().filter(|s| s.sid == id) {
                    s.total = n;
                    w.request_redraw();
                }
            }
            GEvent::SearchSelected(id, n) => {
                if let Some(s) = self.search.as_mut().filter(|s| s.sid == id) {
                    s.selected = n;
                    w.request_redraw();
                }
            }
            GEvent::Background(id, rgb) => {
                if let Some(i) = self.tab_index(id) {
                    self.tabs[i].background = Some(rgb);
                    w.request_redraw();
                }
            }
            GEvent::CloseSurface(id, needs_confirm) => {
                if needs_confirm && self.tab_index(id).is_some() {
                    let detail = "A process is still running in this tab.".to_string();
                    self.ask(w, PromptKind::CloseTab(id), "Close this tab?".into(), detail, None);
                } else {
                    self.close_tab_now(w, id);
                }
            }
            GEvent::ClipRead { sid, clip, state, mimes, list } => {
                if self.tab_index(sid).is_none() {
                    return;
                }
                let token = self.next_token;
                self.next_token += 1;
                let want_text = mimes.iter().any(|m| m == "text/plain");
                self.clip_reads.insert(token, ClipRead { sid, state, want_text, list });
                w.read_clipboard(clip, token);
            }
            GEvent::ClipConfirmNil { sid, state } => {
                if let Some(i) = self.tab_index(sid) {
                    self.tabs[i].deny_clipboard(state);
                }
            }
            GEvent::ClipConfirm { sid, contents, available, state, kind } => {
                if self.tab_index(sid).is_none() {
                    return;
                }
                let title = match kind {
                    ClipRequestKind::Paste => "Paste this text?",
                    ClipRequestKind::Osc52Read | ClipRequestKind::KittyRead | ClipRequestKind::List => {
                        "Allow a program to read your clipboard?"
                    }
                    _ => "Allow clipboard access?",
                };
                let preview = contents
                    .iter()
                    .find(|c| c.mime == "text/plain")
                    .map(|c| String::from_utf8_lossy(&c.data).into_owned())
                    .unwrap_or_else(|| {
                        contents.iter().map(|c| format!("{} ({} bytes)", c.mime, c.data.len())).collect::<Vec<_>>().join("\n")
                    });
                self.ask(w, PromptKind::ClipConfirm { sid, contents, available, state }, title.into(), preview, None);
            }
            GEvent::ClipWrite { clip, text, confirm } => {
                if confirm {
                    let title = "Allow a program to set your clipboard?".to_string();
                    self.ask(w, PromptKind::ClipWrite { clip, text: text.clone() }, title, text, None);
                } else {
                    w.set_clipboard(clip, text);
                }
            }
            GEvent::PromptTitle(id) => self.prompt_rename(w, id),
            GEvent::CopyTitle(id) => {
                if let Some(i) = self.tab_index(id) {
                    w.set_clipboard(Clip::Standard, self.tabs[i].title());
                }
            }
            GEvent::ConfigChanged => w.request_redraw(),
        }
    }

    /// Backend delivers clipboard contents for a `read_clipboard` request.
    pub fn clipboard_result(&mut self, w: &mut dyn Window, token: u64, text: Option<String>) {
        let Some(req) = self.clip_reads.remove(&token) else { return };
        let Some(i) = self.tab_index(req.sid) else { return };
        let available: Vec<String> = if req.list && text.is_some() { vec!["text/plain".into()] } else { vec![] };
        match text {
            Some(t) if req.want_text => {
                let contents = [ClipContent { mime: "text/plain".into(), data: t.into_bytes() }];
                self.tabs[i].complete_clipboard(&contents, &available, req.state, false);
            }
            _ if req.list => self.tabs[i].complete_clipboard(&[], &available, req.state, false),
            _ => self.tabs[i].deny_clipboard(req.state),
        }
        self.process(w);
    }

    // ------------------------------------------------------------ keyboard

    pub fn key(&mut self, w: &mut dyn Window, k: KeyInput) {
        if std::env::var_os("GHOSTDROP_TRACE_KEYS").is_some() {
            eprintln!("key {:?} kc={} sym={:#x} unshifted={:#x} text={:?} mods={:#x} consumed={:#x} focused={}", k.state, k.keycode, k.sym, k.unshifted, k.text, k.ghostty_mods, k.consumed, w.is_focused());
        }
        // Repeats and releases go wherever the press went.
        let pressed_to = match k.state {
            KeyState::Press => None,
            KeyState::Repeat => self.keys_down.get(&k.keycode).copied(),
            KeyState::Release => self.keys_down.remove(&k.keycode),
        };
        match (k.state, pressed_to) {
            (KeyState::Release, Some(KeyTarget::Ui)) => {}
            // Even if a prompt opened since: the terminal saw the press.
            (KeyState::Release, Some(KeyTarget::Terminal(id))) => self.send_key(w, id, &k),
            // Held keys keep editing the prompt or search field, nothing else.
            (KeyState::Repeat, Some(KeyTarget::Ui)) => {
                if self.prompt.is_some() {
                    self.prompt_key(w, &k);
                } else if self.search.is_some() {
                    self.search_key(w, &k);
                }
            }
            (KeyState::Repeat, Some(KeyTarget::Terminal(id))) => {
                if self.prompt.is_none() {
                    self.send_key(w, id, &k);
                }
            }
            // Pressed before we had focus: the terminal tracks modifiers
            // from releases too, unless a prompt is up.
            (KeyState::Release, None) => {
                if let Some(id) = self.selected_tab().map(|t| t.id).filter(|_| self.prompt.is_none()) {
                    self.send_key(w, id, &k);
                }
            }
            // A press (or a repeat whose press we never saw): route it.
            (KeyState::Press, _) | (KeyState::Repeat, None) => {
                let to = self.route_key(w, &k);
                self.keys_down.insert(k.keycode, to);
            }
        }
    }

    /// Decide who gets a new key press and deliver it.
    fn route_key(&mut self, w: &mut dyn Window, k: &KeyInput) -> KeyTarget {
        if self.prompt.is_some() {
            self.prompt_key(w, k);
            return KeyTarget::Ui;
        }
        if self.search.is_some() && self.search_key(w, k) {
            return KeyTarget::Ui;
        }
        if k.state == KeyState::Press {
            let action = self.settings.bindings.iter().find(|(c, _)| c.matches(k.mods, k.unshifted)).map(|(_, a)| a.clone());
            if let Some(a) = action {
                self.perform(w, &a);
                return KeyTarget::Ui;
            }
        }
        match self.selected_tab().map(|t| t.id) {
            Some(id) => {
                self.send_key(w, id, k);
                KeyTarget::Terminal(id)
            }
            None => KeyTarget::Ui,
        }
    }

    fn send_key(&mut self, w: &mut dyn Window, id: SurfaceId, k: &KeyInput) {
        let Some(i) = self.tab_index(id) else { return };
        let action = match k.state {
            KeyState::Press => KeyAction::Press,
            KeyState::Repeat => KeyAction::Repeat,
            KeyState::Release => KeyAction::Release,
        };
        let unshifted = xkbcommon::xkb::keysym_to_utf32(xkbcommon::xkb::Keysym::new(k.unshifted));
        let text = (!k.text.is_empty() && k.text.chars().next().is_some_and(|c| c as u32 >= 0x20 && c as u32 != 0x7f))
            .then_some(k.text.as_str());
        self.tabs[i].key(action, k.ghostty_mods, k.consumed, k.keycode, text, unshifted, k.composing);
        self.process(w);
    }

    fn prompt_key(&mut self, w: &mut dyn Window, k: &KeyInput) {
        use xkbcommon::xkb::keysyms as ks;
        let editing = self.prompt.as_ref().is_some_and(|p| p.input.is_some());
        // Only a fresh press answers: a held Enter must not also answer the
        // question queued behind this one.
        let answer = k.state == KeyState::Press;
        match k.sym {
            ks::KEY_Return | ks::KEY_KP_Enter if answer => self.prompt_answer(w, true),
            ks::KEY_Escape if answer => self.prompt_answer(w, false),
            ks::KEY_Return | ks::KEY_KP_Enter | ks::KEY_Escape => {}
            ks::KEY_BackSpace if editing => {
                if let Some(s) = self.prompt.as_mut().and_then(|p| p.input.as_mut()) {
                    s.pop();
                }
            }
            _ if editing => {
                if !k.mods.ctrl && !k.mods.alt && !k.text.is_empty() && !k.text.chars().any(|c| c.is_control()) {
                    if let Some(s) = self.prompt.as_mut().and_then(|p| p.input.as_mut()) {
                        s.push_str(&k.text);
                    }
                }
            }
            _ if !answer => {}
            _ => match k.text.as_str() {
                "y" | "Y" => self.prompt_answer(w, true),
                "n" | "N" => self.prompt_answer(w, false),
                _ => {}
            },
        }
        w.request_redraw();
    }

    fn prompt_answer(&mut self, w: &mut dyn Window, yes: bool) {
        let Some(p) = self.prompt.take() else { return };
        self.prompt = self.prompts.pop_front();
        match p.kind {
            PromptKind::CloseTab(id) => {
                if yes {
                    self.close_tab_now(w, id);
                }
            }
            PromptKind::Quit => {
                if yes {
                    self.quit = true;
                }
            }
            PromptKind::Notice => {}
            PromptKind::ClipConfirm { sid, contents, available, state } => {
                if let Some(i) = self.tab_index(sid) {
                    if yes {
                        self.tabs[i].complete_clipboard(&contents, &available, state, true);
                    } else {
                        self.tabs[i].deny_clipboard(state);
                    }
                }
            }
            PromptKind::ClipWrite { clip, text } => {
                if yes {
                    w.set_clipboard(clip, text);
                }
            }
            PromptKind::OpenUrl(url) => {
                if yes {
                    spawn_detached("xdg-open", &[&url]);
                }
            }
            PromptKind::Rename(id) => {
                if yes {
                    if let Some(i) = self.tab_index(id) {
                        let v = p.input.unwrap_or_default();
                        self.tabs[i].title_override = (!v.trim().is_empty()).then_some(v);
                    }
                }
            }
        }
        self.show_prompt_tab(w);
        self.sync_gpu();
        self.process(w);
        w.request_redraw();
    }

    /// Returns true if the key was used by the search field.
    fn search_key(&mut self, w: &mut dyn Window, k: &KeyInput) -> bool {
        use xkbcommon::xkb::keysyms as ks;
        let Some(s) = self.search.as_mut() else { return false };
        let sid = s.sid;
        let action: Option<String> = match k.sym {
            ks::KEY_Escape => {
                self.search = None;
                Some("end_search".into())
            }
            ks::KEY_Return | ks::KEY_KP_Enter => {
                Some(if k.mods.shift { "navigate_search:previous" } else { "navigate_search:next" }.into())
            }
            ks::KEY_BackSpace => {
                s.needle.pop();
                Some(format!("search:{}", s.needle))
            }
            _ if !k.mods.ctrl && !k.mods.alt && !k.mods.sup && !k.text.is_empty() && !k.text.chars().any(|c| c.is_control()) => {
                s.needle.push_str(&k.text);
                Some(format!("search:{}", s.needle))
            }
            _ => None,
        };
        match action {
            Some(a) => {
                if let Some(i) = self.tab_index(sid) {
                    self.tabs[i].binding_action(&a);
                }
                self.process(w);
                w.request_redraw();
                true
            }
            None => false,
        }
    }

    // ------------------------------------------------------------ mouse

    fn update_cursor(&mut self, w: &mut dyn Window) {
        let (x, y) = self.mouse;
        let terminal_drag = matches!(self.capture, Capture::Terminal { .. });
        let want = if self.in_tab_bar(w, x, y) && !terminal_drag { Cursor::Default } else { self.term_cursor };
        if self.cursor_shown != Some(want) {
            self.cursor_shown = Some(want);
            w.set_cursor(want);
        }
    }

    fn in_tab_bar(&self, w: &dyn Window, x: f64, y: f64) -> bool {
        let bh = self.bar_height(w.scale()) as f64 / w.scale();
        if bh == 0.0 || x < 0.0 {
            return false;
        }
        let (_, wh) = w.size();
        let wh = wh as f64 / w.scale();
        if self.settings.tab_bar_top { y < bh } else { y >= wh - bh }
    }

    /// Tab rectangles in logical px: (x, width).
    fn tab_slots(&self, w: &dyn Window) -> (Vec<(f64, f64)>, (f64, f64)) {
        let ww = w.size().0 as f64 / w.scale();
        let plus = 32.0;
        let n = self.tabs.len().max(1) as f64;
        let tw = ((ww - plus) / n).max(60.0);
        let slots = (0..self.tabs.len()).map(|i| (i as f64 * tw, tw)).collect();
        (slots, (ww - plus, plus))
    }

    fn bar_hit(&self, w: &dyn Window, x: f64) -> Option<Hover> {
        let (slots, plus) = self.tab_slots(w);
        if x >= plus.0 {
            return Some(Hover::Plus);
        }
        slots.iter().position(|(sx, sw)| x >= *sx && x < sx + sw).map(|i| {
            let (sx, sw) = slots[i];
            Hover::Tab(i, x >= sx + sw - 24.0 && x < sx + sw - 6.0)
        })
    }

    fn term_pos(&self, w: &dyn Window, x: f64, y: f64) -> (f64, f64) {
        let top = if self.settings.tab_bar_top { self.bar_height(w.scale()) as f64 / w.scale() } else { 0.0 };
        (x, y - top)
    }

    /// End a capture without its release (we're hiding, so it won't come):
    /// move pointer off-surface first so a held click on a link cannot open it,
    /// then tell the terminal the buttons went up so it stops selecting.
    fn release_capture(&mut self) {
        if let Capture::Terminal { id, held } = std::mem::replace(&mut self.capture, Capture::None) {
            if let Some(i) = self.tab_index(id) {
                self.tabs[i].mouse_pos(-1.0, -1.0, 0);
                for code in (0..6u8).filter(|c| held & (1 << c) != 0) {
                    self.tabs[i].mouse_button(false, button_from_index(code), 0);
                }
            }
        }
    }

    pub fn pointer(&mut self, w: &mut dyn Window, ev: PointerInput, mods: Mods) {
        if !matches!(ev, PointerInput::Motion { .. }) {
            trace!("pointer {ev:?} capture={:?}", self.capture);
        }
        let gm = (if mods.shift { gmods::SHIFT } else { 0 })
            | (if mods.ctrl { gmods::CTRL } else { 0 })
            | (if mods.alt { gmods::ALT } else { 0 })
            | (if mods.sup { gmods::SUPER } else { 0 });
        let selected_idx = (self.selected < self.tabs.len()).then_some(self.selected);
        match ev {
            PointerInput::Motion { x, y } => {
                self.mouse = (x, y);
                let in_bar = self.in_tab_bar(w, x, y);
                match self.capture {
                    // Selecting/dragging in a terminal: it follows the pointer
                    // anywhere, tab bar and outside the window included.
                    Capture::Terminal { id, .. } => {
                        if self.hover.take().is_some() {
                            w.request_redraw();
                        }
                        if let Some(i) = self.tab_index(id) {
                            let (tx, ty) = self.term_pos(w, x, y);
                            self.tabs[i].mouse_pos(tx, ty, gm);
                        }
                    }
                    _ if in_bar => {
                        let h = self.bar_hit(w, x);
                        if h != self.hover {
                            self.hover = h;
                            w.request_redraw();
                        }
                        if let (Capture::Bar { drag: Some((id, sx)), .. }, Some(Hover::Tab(to, _))) = (self.capture, h) {
                            if let Some(from) = self.tab_index(id).filter(|from| *from != to && (x - sx).abs() > 8.0) {
                                self.move_tab_to(w, from, to);
                                if let Capture::Bar { drag, .. } = &mut self.capture {
                                    *drag = Some((id, x));
                                }
                            }
                        }
                    }
                    // A tab-bar button is held: the terminal isn't involved.
                    Capture::Bar { .. } => {
                        if self.hover.take().is_some() {
                            w.request_redraw();
                        }
                    }
                    Capture::None => {
                        if self.hover.take().is_some() {
                            w.request_redraw();
                        }
                        if let Some(i) = selected_idx {
                            let (tx, ty) = self.term_pos(w, x, y);
                            self.tabs[i].mouse_pos(tx, ty, gm);
                        }
                    }
                }
                self.update_cursor(w);
            }
            PointerInput::Leave => {
                self.mouse = (-1.0, -1.0);
                if self.hover.take().is_some() {
                    w.request_redraw();
                }
                // While a button is held the pointer is still ours (the
                // release comes to us); don't cut a selection drag short.
                if !matches!(self.capture, Capture::Terminal { .. }) {
                    if let Some(i) = selected_idx {
                        self.tabs[i].mouse_pos(-1.0, -1.0, gm);
                    }
                }
            }
            PointerInput::Button { button, pressed: false } => match self.capture {
                Capture::Terminal { id, held } if held & button_bit(button) != 0 => {
                    if let Some(i) = self.tab_index(id) {
                        self.tabs[i].mouse_button(false, mouse_button(button), gm);
                    }
                    let held = held & !button_bit(button);
                    self.capture = if held == 0 { Capture::None } else { Capture::Terminal { id, held } };
                    self.update_cursor(w);
                }
                Capture::Bar { button: b, .. } if b == button => self.capture = Capture::None,
                // We never saw (or forwarded) the press.
                _ => {}
            },
            PointerInput::Button { button, pressed: true } => {
                if self.prompt.is_some() {
                    return;
                }
                let (x, y) = self.mouse;
                match self.capture {
                    // More buttons during a terminal drag go to that terminal.
                    Capture::Terminal { id, held } => {
                        if let Some(i) = self.tab_index(id) {
                            self.tabs[i].mouse_button(true, mouse_button(button), gm);
                        }
                        self.capture = Capture::Terminal { id, held: held | button_bit(button) };
                    }
                    Capture::Bar { .. } => {}
                    Capture::None if self.in_tab_bar(w, x, y) => {
                        self.capture = Capture::Bar { button, drag: None };
                        self.bar_click(w, button, x);
                    }
                    Capture::None => {
                        if let Some(i) = selected_idx {
                            let id = self.tabs[i].id;
                            self.capture = Capture::Terminal { id, held: button_bit(button) };
                            self.tabs[i].mouse_button(true, mouse_button(button), gm);
                        }
                    }
                }
            }
            PointerInput::Scroll { dx, dy, precise } => {
                let (x, y) = self.mouse;
                let target = match self.capture {
                    Capture::Terminal { id, .. } => self.tab_index(id),
                    Capture::Bar { .. } => None,
                    Capture::None => selected_idx.filter(|_| !self.in_tab_bar(w, x, y)),
                };
                if let Some(i) = target {
                    // Wayland/X11 report positive = down/right; Ghostty wants
                    // positive = up/left (same inversion as its GTK app).
                    self.tabs[i].mouse_scroll(-dx, -dy, precise);
                }
            }
        }
        self.process(w);
    }

    /// A button went down on the tab bar (the capture is already `Bar`).
    fn bar_click(&mut self, w: &mut dyn Window, button: Button, x: f64) {
        match (self.bar_hit(w, x), button) {
            (Some(Hover::Plus), Button::Left) => self.perform(w, &Action::NewTab(None)),
            (Some(Hover::Tab(i, true)), Button::Left) | (Some(Hover::Tab(i, _)), Button::Middle) => {
                if let Some(t) = self.tabs.get(i) {
                    t.request_close();
                }
                self.process(w);
            }
            (Some(Hover::Tab(i, false)), Button::Left) => {
                self.select(w, i);
                let id = self.tabs.get(i).map(|t| t.id);
                if let (Capture::Bar { drag, .. }, Some(id)) = (&mut self.capture, id) {
                    *drag = Some((id, x));
                }
            }
            (None, Button::Left) => {
                let now = Instant::now();
                if self.last_bar_click.is_some_and(|t| now.duration_since(t).as_millis() < 400) {
                    self.perform(w, &Action::NewTab(None));
                }
                self.last_bar_click = Some(now);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ drawing

    /// Compose a full frame. `size`/`scale` describe the window being drawn.
    pub fn draw(&mut self, size: (u32, u32), scale64: f64, c: &mut Canvas) {
        self.sync_gpu();
        let t0 = Instant::now();
        self.draw_inner(size, scale64, c);
        if let Some(at) = self.shown_at.take() {
            trace!("first frame {:.1} ms after show (draw {:.2} ms)", at.elapsed().as_secs_f64() * 1e3, t0.elapsed().as_secs_f64() * 1e3);
        } else if std::env::var_os("GHOSTDROP_TRACE_FRAMES").is_some() {
            trace!("frame {}x{} draw {:.2} ms", size.0, size.1, t0.elapsed().as_secs_f64() * 1e3);
        }
    }

    fn draw_inner(&mut self, size: (u32, u32), scale64: f64, c: &mut Canvas) {
        let scale = scale64 as f32;
        let (bg, fg) = self.colors();
        let (tx, ty, tw, th) = self.term_rect_at(size, scale64);
        let frame = self.tabs.get(self.selected).and_then(|t| t.frame.as_ref());
        c.blit_frame(frame, tx as usize, ty as usize, tw as usize, th as usize, bg);

        let bh = self.bar_height(scale64);
        if bh > 0 {
            let by = if self.settings.tab_bar_top { 0 } else { th as i32 };
            self.draw_tab_bar(c, by, bh as i32, scale, bg, fg);
        }
        if self.search.is_some() {
            self.draw_search(c, ty as i32, scale, bg, fg);
        }
        if self.prompt.is_some() {
            self.draw_prompt(c, scale, bg, fg);
        }
    }

    fn draw_tab_bar(&mut self, c: &mut Canvas, by: i32, bh: i32, scale: f32, bg: Rgb, fg: Rgb) {
        let bar_bg = mix(bg, fg, 0.08);
        c.fill(0, by, c.width as i32, bh, bar_bg);
        let size = 12.5 * scale;
        let (asc, desc) = self.font.line_metrics(size);
        let baseline = by as f32 + (bh as f32 + asc - desc) / 2.0;
        let n = self.tabs.len();
        let plus_w = (32.0 * scale) as i32;
        let tw = (((c.width as i32 - plus_w) as f32) / n.max(1) as f32).max(60.0 * scale) as i32;
        let titles: Vec<(String, bool)> = self.tabs.iter().map(|t| (t.title(), t.bell)).collect();
        for (i, (title, bell)) in titles.into_iter().enumerate() {
            let x = i as i32 * tw;
            let sel = i == self.selected;
            if sel {
                c.fill(x, by, tw, bh, bg);
                c.fill(x, if by == 0 { 0 } else { by + bh - (2.0 * scale) as i32 }, tw, (2.0 * scale) as i32, mix(bg, fg, 0.6));
            } else if matches!(self.hover, Some(Hover::Tab(h, _)) if h == i) {
                c.fill(x, by, tw, bh, mix(bg, fg, 0.13));
            }
            if i > 0 && !sel && i - 1 != self.selected {
                c.fill(x, by + (6.0 * scale) as i32, scale.max(1.0) as i32, bh - (12.0 * scale) as i32, mix(bar_bg, fg, 0.15));
            }
            let color = if sel { mix(bg, fg, 0.95) } else { mix(bar_bg, fg, 0.6) };
            let mut label = if i < 9 { format!("{}  {}", i + 1, title) } else { title };
            if bell {
                label = format!("• {label}");
            }
            let pad = 26.0 * scale;
            let label = self.font.fit(&label, size, tw as f32 - 2.0 * pad);
            let lw = self.font.measure(&label, size);
            let lx = x as f32 + (tw as f32 - lw) / 2.0;
            self.font.draw(c, lx, baseline, &label, size, color, x + tw - (8.0 * scale) as i32);
            if let Some(Hover::Tab(h, on_close)) = self.hover {
                if h == i {
                    let cx = x + tw - (20.0 * scale) as i32;
                    let cy = by + bh / 2 - (7.0 * scale) as i32;
                    if on_close {
                        c.fill(cx - 2, cy - 1, (16.0 * scale) as i32, (16.0 * scale) as i32, mix(bg, fg, 0.2));
                    }
                    self.font.draw(c, cx as f32 + 2.0 * scale, baseline, "×", size * 1.1, mix(bar_bg, fg, 0.8), c.width as i32);
                }
            }
        }
        let px = c.width as i32 - plus_w;
        let pc = if self.hover == Some(Hover::Plus) { mix(bar_bg, fg, 0.95) } else { mix(bar_bg, fg, 0.55) };
        let pw = self.font.measure("+", size * 1.3);
        self.font.draw(c, px as f32 + (plus_w as f32 - pw) / 2.0, baseline + scale, "+", size * 1.3, pc, c.width as i32);
    }

    fn draw_search(&mut self, c: &mut Canvas, top: i32, scale: f32, bg: Rgb, fg: Rgb) {
        let Some(s) = self.search.as_ref() else { return };
        let size = 13.0 * scale;
        let count = if s.total < 0 {
            String::new()
        } else if s.selected >= 0 {
            format!("{}/{}", s.selected + 1, s.total)
        } else {
            format!("-/{}", s.total)
        };
        let text = format!("Search: {}", s.needle);
        let bw = (360.0 * scale) as i32;
        let bh = (32.0 * scale) as i32;
        let x = c.width as i32 - bw - (12.0 * scale) as i32;
        let y = top + (10.0 * scale) as i32;
        let panel = if luminance(bg) < 0.5 { mix(bg, fg, 0.15) } else { mix(bg, fg, 0.08) };
        c.fill(x - 1, y - 1, bw + 2, bh + 2, mix(bg, fg, 0.35));
        c.fill(x, y, bw, bh, panel);
        let (asc, desc) = self.font.line_metrics(size);
        let base = y as f32 + (bh as f32 + asc - desc) / 2.0;
        let cw = self.font.measure(&count, size);
        let fit = self.font.fit_tail(&text, size, bw as f32 - cw - 34.0 * scale);
        let adv = self.font.draw(c, x as f32 + 10.0 * scale, base, &fit, size, fg, x + bw);
        // Text cursor: a thin bar (box-drawing glyphs aren't in every UI font).
        let cx = (x as f32 + 10.0 * scale + adv + 1.0 * scale) as i32;
        c.fill(cx, (base - asc) as i32, (1.0 * scale).max(1.0) as i32, (asc + desc) as i32, fg);
        self.font.draw(c, (x + bw) as f32 - cw - 10.0 * scale, base, &count, size, mix(panel, fg, 0.6), x + bw);
    }

    fn draw_prompt(&mut self, c: &mut Canvas, scale: f32, bg: Rgb, fg: Rgb) {
        let Some(p) = self.prompt.as_ref() else { return };
        let title = p.title.clone();
        let notice = matches!(p.kind, PromptKind::Notice);
        let input = p.input.clone();
        let detail: Vec<String> = p.detail.lines().take(5).map(|l| l.to_string()).collect();
        let size = 14.0 * scale;
        let small = 12.5 * scale;
        let bw = ((560.0 * scale) as i32).min(c.width as i32 - (40.0 * scale) as i32).max(100);
        let lines = 3 + detail.len() + input.is_some() as usize;
        let lh = (22.0 * scale) as i32;
        let bh = lines as i32 * lh + (24.0 * scale) as i32;
        let x = (c.width as i32 - bw) / 2;
        let y = ((c.height as i32 - bh) / 2).max(0);
        let panel = mix(bg, fg, 0.1);
        c.fill(x - 1, y - 1, bw + 2, bh + 2, mix(bg, fg, 0.45));
        c.fill(x, y, bw, bh, panel);
        let pad = 16.0 * scale;
        let mut base = y as f32 + pad + size;
        let t = self.font.fit(&title, size, bw as f32 - 2.0 * pad);
        self.font.draw(c, x as f32 + pad, base, &t, size, fg, x + bw);
        for d in &detail {
            base += lh as f32;
            let d = self.font.fit(d, small, bw as f32 - 2.0 * pad);
            self.font.draw(c, x as f32 + pad, base, &d, small, mix(panel, fg, 0.75), x + bw);
        }
        if let Some(inp) = input {
            base += lh as f32 + 4.0 * scale;
            let fx = x + pad as i32;
            c.fill(fx - 4, (base - size) as i32 - 4, bw - 2 * pad as i32 + 8, lh + 4, bg);
            let shown = self.font.fit_tail(&inp, size, bw as f32 - 2.0 * pad - 4.0 * scale);
            let adv = self.font.draw(c, fx as f32, base, &shown, size, fg, x + bw);
            let (asc, desc) = self.font.line_metrics(size);
            let cx = (fx as f32 + adv + 1.0 * scale) as i32;
            c.fill(cx, (base - asc) as i32, (1.0 * scale).max(1.0) as i32, (asc + desc) as i32, fg);
        }
        base += lh as f32 * 1.4;
        let hint = if notice { "Enter / Esc: Dismiss" } else { "Enter: OK      Esc: Cancel" };
        self.font.draw(c, x as f32 + pad, base, hint, small, mix(panel, fg, 0.55), x + bw);
    }

    // ------------------------------------------------------------ IPC

    pub fn command(&mut self, w: &mut dyn Window, line: &str) -> String {
        let (cmd, arg) = match line.trim().split_once(' ') {
            Some((c, a)) => (c, Some(a.trim().to_string())),
            None => (line.trim(), None),
        };
        let reply = match cmd {
            "toggle" => {
                self.toggle(w);
                "ok".into()
            }
            "show" => {
                self.show(w);
                "ok".into()
            }
            "hide" => {
                self.hide(w);
                "ok".into()
            }
            "new-tab" => {
                let name = arg.filter(|a| !a.is_empty());
                if let Some(n) = name.as_deref().filter(|n| !self.settings.profiles.contains_key(*n)) {
                    let mut have: Vec<&str> = self.settings.profiles.keys().map(|k| k.as_str()).collect();
                    have.sort();
                    return format!("error: no profile '{n}' (have: {})", have.join(", "));
                }
                self.perform(w, &Action::NewTab(name));
                "ok".into()
            }
            "next-tab" => {
                self.goto(w, -2);
                "ok".into()
            }
            "previous-tab" => {
                self.goto(w, -1);
                "ok".into()
            }
            "reload" => {
                self.reload(w);
                if self.settings.errors.is_empty() {
                    "ok".into()
                } else {
                    format!("error: {}", self.settings.errors.join("\nerror: "))
                }
            }
            "quit" => {
                if arg.as_deref() == Some("force") {
                    self.quit = true; // scripts/upgrades: no confirmation
                } else {
                    self.request_quit(w);
                }
                "ok".into()
            }
            "ping" => "pong".into(),
            _ if self.settings.control_commands || std::env::var("GHOSTDROP_DEBUG").as_deref() == Ok("1") => {
                self.debug_command(w, cmd, arg)
            }
            _ => format!("error: unknown command '{cmd}'"),
        };
        self.process(w);
        reply
    }

    fn debug_command(&mut self, w: &mut dyn Window, cmd: &str, arg: Option<String>) -> String {
        match cmd {
            "dump" => self.dump(w),
            "mem" => crate::mem_report(arg.as_deref() == Some("trim")),
            "prompt-answer" => {
                self.prompt_answer(w, arg.as_deref() == Some("yes"));
                w.request_redraw();
                "ok".into()
            }
            "snapshot" => match w.snapshot(arg.as_deref().unwrap_or("/tmp/ghostdrop-snapshot.ppm")) {
                Ok(()) => "ok".into(),
                Err(e) => format!("error: {e}"),
            },
            "binding" => {
                let ok = self.selected_tab().is_some_and(|t| t.binding_action(&arg.unwrap_or_default()));
                format!("{ok}")
            }
            "goto-tab" => {
                self.goto(w, arg.and_then(|a| a.parse().ok()).unwrap_or(0));
                "ok".into()
            }
            "close-tab" => {
                self.perform(w, &Action::CloseTab);
                "ok".into()
            }
            _ => format!("error: unknown command '{cmd}'"),
        }
    }

    pub fn dump(&self, w: &dyn Window) -> String {
        fn esc(s: &str) -> String {
            let mut o = String::new();
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                    c => o.push(c),
                }
            }
            o
        }
        let tabs: Vec<String> = self
            .tabs
            .iter()
            .map(|t| {
                let (cols, rows) = t.grid();
                format!(
                    "{{\"title\":\"{}\",\"profile\":\"{}\",\"cols\":{},\"rows\":{},\"focused\":{},\"exited\":{},\"frame\":\"{}\"}}",
                    esc(&t.title()),
                    esc(&t.profile.name),
                    cols,
                    rows,
                    t.focused,
                    t.process_exited(),
                    t.frame.as_ref().map_or("none".into(), |f| format!(
                        "{}x{}{}",
                        f.width,
                        f.height,
                        if f.dmabuf().is_some() { " dmabuf" } else { "" }
                    ))
                )
            })
            .collect();
        let (ww, wh) = w.size();
        format!(
            "{{\"backend\":\"{}\",\"gpu\":{},\"visible\":{},\"focused\":{},\"size\":[{},{}],\"scale\":{},\"selected\":{},\"tabBar\":{},\"hotkey\":\"{}\",\"prompt\":\"{}\",\"promptsQueued\":{},\"search\":\"{}\",\"errors\":[{}],\"tabs\":[{}],\"screen\":\"{}\"}}",
            w.backend_name(),
            self.gpu_backend,
            w.is_visible(),
            w.is_focused(),
            ww,
            wh,
            w.scale(),
            self.selected,
            self.tab_bar_visible(),
            esc(&w.hotkey_status()),
            esc(&self.prompt.as_ref().map_or(String::new(), |p| p.title.clone())),
            self.prompts.len(),
            esc(&self.search.as_ref().map_or(String::new(), |s| format!("{} [{}/{}]", s.needle, s.selected, s.total))),
            self.settings.errors.iter().map(|e| format!("\"{}\"", esc(e))).collect::<Vec<_>>().join(","),
            tabs.join(","),
            esc(&self.selected_tab().map_or(String::new(), |t| t.visible_text()))
        )
    }
}
