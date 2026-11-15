//! Window-system independent logic: tabs, show/hide, key routing, tab bar,
//! prompts, search, clipboard, IPC. Backends implement `Window` and feed
//! normalized input into `Core`.
use crate::draw::{luminance, mix, Canvas, Font, Rgb};
use crate::ffi::*;
use crate::ghostty::{self, complete_clipboard, deny_clipboard, Clip, GEvent, Runtime, SurfaceId};
use crate::keys::Mods;
use crate::settings::{Action, Settings, TabBarMode};
use crate::tab::{Frame, Tab};
use crate::xkb::{KeyInput, KeyState};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::time::Instant;

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
    pub size: ghostty_config_quick_terminal_size_s,
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
        let p = ghostty::size_dim(&self.size.primary, match self.pos {
            Pos::Left | Pos::Right => sw,
            Pos::Center if sw < sh => sw,
            _ => sh,
        });
        let s = ghostty::size_dim(&self.size.secondary, match self.pos {
            Pos::Left | Pos::Right => sh,
            Pos::Center if sw < sh => sh,
            _ => sw,
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
    /// Try to take keyboard focus while visible.
    fn focus(&mut self) {}
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
    ClipConfirm { sid: SurfaceId, text: String, state: usize },
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
    pressed_in_bar: Option<(usize, f64)>,
    last_bar_click: Option<Instant>,
    term_cursor: Cursor,
    cursor_shown: Option<Cursor>,
    prompt: Option<Prompt>,
    search: Option<Search>,
    clip_reads: HashMap<u64, ClipRead>,
    next_token: u64,
    swallow: HashSet<u32>,
    /// Focus was lost at this time; autohide fires if it hasn't come back.
    autohide_at: Option<Instant>,
    /// Hide at `autohide_at` even if we still have focus (click outside).
    autohide_force: bool,
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

fn ghostty_button(b: Button) -> ghostty_input_mouse_button_e {
    match b {
        Button::Left => GHOSTTY_MOUSE_LEFT,
        Button::Right => GHOSTTY_MOUSE_RIGHT,
        Button::Middle => GHOSTTY_MOUSE_MIDDLE,
        Button::Other(8) => GHOSTTY_MOUSE_FOUR,
        Button::Other(9) => GHOSTTY_MOUSE_FIVE,
        Button::Other(_) => GHOSTTY_MOUSE_UNKNOWN,
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
            pressed_in_bar: None,
            last_bar_click: None,
            term_cursor: Cursor::Text,
            cursor_shown: None,
            prompt: None,
            search: None,
            clip_reads: HashMap::new(),
            next_token: 1,
            swallow: HashSet::new(),
            autohide_at: None,
            autohide_force: false,
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
            font_size = cur.font_size();
            if profile.effective_command().is_none() && cur.profile.effective_command().is_none() {
                cwd = cur.inherited_cwd();
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let scale = w.scale();
        match Tab::new(self.rt.app, id, profile, cwd, font_size, scale) {
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
        self.clip_reads.retain(|_, r| r.sid != id);
        if self.search.as_ref().is_some_and(|s| s.sid == id) {
            self.search = None;
        }
        self.tabs.remove(idx); // drop frees the surface
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

    pub fn hide(&mut self, w: &mut dyn Window) {
        trace!("hide");
        self.autohide_at = None;
        self.autohide_force = false;
        w.hide();
        for t in self.tabs.iter_mut() {
            t.set_visible(false);
            t.set_focus(false);
        }
        self.rt.set_focus(false);
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
            if !self.autohide_force {
                self.autohide_at = None;
            }
        } else {
            self.swallow.clear();
            // Debounced: compositors/devices can bounce focus briefly
            // (keyboard re-creation, layout switches). Only hide if it stays away.
            if w.is_visible() && self.prompt.is_none() && self.rt.bool("quick-terminal-autohide", true) {
                self.autohide_at = Some(Instant::now() + std::time::Duration::from_millis(250));
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
        self.sync_gpu();
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
            self.autohide_at = Some(Instant::now() + std::time::Duration::from_millis(60));
            self.autohide_force = true;
        }
    }

    /// When the event loop should wake up for our timers.
    pub fn next_deadline(&self) -> Option<Instant> {
        match (self.autohide_at, self.trim_at) {
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
        if let Some(at) = self.autohide_at {
            if Instant::now() >= at {
                self.autohide_at = None;
                let force = std::mem::take(&mut self.autohide_force);
                if w.is_visible() && (force || !w.is_focused()) && self.prompt.is_none() {
                    trace!("autohide");
                    self.hide(w);
                }
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
        let errs = &self.settings.errors;
        if errs.is_empty() {
            if self.prompt.as_ref().is_some_and(|p| matches!(p.kind, PromptKind::Notice)) {
                self.prompt = None;
            }
            return;
        }
        if self.prompt.as_ref().is_some_and(|p| !matches!(p.kind, PromptKind::Notice)) {
            return; // don't clobber a question
        }
        let mut detail: Vec<String> = errs.iter().take(3).cloned().collect();
        if errs.len() > 3 {
            detail.push(format!("… and {} more", errs.len() - 3));
        }
        detail.push("Run `ghostdrop check` for details; fix and reload.".into());
        self.prompt = Some(Prompt {
            kind: PromptKind::Notice,
            title: format!("Config: {} issue{}", errs.len(), if errs.len() == 1 { "" } else { "s" }),
            detail: detail.join("\n"),
            input: None,
        });
        w.request_redraw();
    }

    fn request_quit(&mut self, w: &mut dyn Window) {
        if self.rt.needs_confirm_quit() {
            self.ensure_visible(w);
            self.prompt = Some(Prompt {
                kind: PromptKind::Quit,
                title: "Quit Ghostdrop?".into(),
                detail: "Processes are still running in some tabs.".into(),
                input: None,
            });
            w.request_redraw();
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
        self.ensure_visible(w);
        self.prompt = Some(Prompt {
            kind: PromptKind::Rename(id),
            title: "Rename tab".into(),
            detail: "Empty = use the title set by the terminal.".into(),
            input: Some(cur),
        });
        w.request_redraw();
    }

    fn open_url(&mut self, w: &mut dyn Window, url: String, trusted: bool) {
        let lower = url.to_lowercase();
        let safe = ["http://", "https://", "mailto:", "ftp://", "sftp://", "ssh://"].iter().any(|p| lower.starts_with(p));
        if trusted || safe {
            spawn_detached("xdg-open", &[&url]);
        } else {
            self.ensure_visible(w);
            self.prompt = Some(Prompt {
                kind: PromptKind::OpenUrl(url.clone()),
                title: "Open this link?".into(),
                detail: url,
                input: None,
            });
            w.request_redraw();
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
        for _ in 0..8 {
            let evs = ghostty::drain();
            if evs.is_empty() {
                break;
            }
            for ev in evs {
                self.handle(w, ev);
            }
        }
    }

    fn handle(&mut self, w: &mut dyn Window, ev: GEvent) {
        if !matches!(ev, GEvent::Render(_)) {
            trace!("event {ev:?}");
        }
        match ev {
            GEvent::Render(id) => {
                if let Some(i) = self.tab_index(id) {
                    if let Some(f) = Frame::take(self.tabs[i].surface) {
                        let gpu = f.dmabuf().is_some();
                        // GPU frames wanted but CPU frames keep coming: the
                        // driver can't export (e.g. software rendering).
                        let wanted = self.gpu_backend && self.prompt.is_none() && self.search.is_none();
                        self.gpu_misses = if wanted && !gpu { self.gpu_misses + 1 } else { 0 };
                        if self.gpu_misses >= 8 {
                            eprintln!("ghostdrop: GPU frames unavailable (DMA-BUF export failed); using CPU frames");
                            self.gpu_misses = 0;
                            self.set_gpu_backend(false);
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
                    m if m == GHOSTTY_ACTION_CLOSE_TAB_MODE_OTHER as u32 => self.tabs.iter().map(|t| t.id).filter(|x| *x != id).collect(),
                    m if m == GHOSTTY_ACTION_CLOSE_TAB_MODE_RIGHT as u32 => match self.tab_index(id) {
                        Some(i) => self.tabs[i + 1..].iter().map(|t| t.id).collect(),
                        None => vec![],
                    },
                    _ => vec![id],
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
                }
            }
            GEvent::ResetSize => {
                self.fullscreen = false;
                if w.is_visible() {
                    let p = self.placement();
                    w.show(&p);
                }
            }
            GEvent::Quit => self.request_quit(w),
            GEvent::ReloadConfig { soft } => {
                if soft {
                    unsafe { ghostty_app_update_config(self.rt.app, self.rt.config) };
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
                    s if s == GHOSTTY_MOUSE_SHAPE_DEFAULT as u32 => Cursor::Default,
                    s if s == GHOSTTY_MOUSE_SHAPE_POINTER as u32 => Cursor::Pointer,
                    s if s == GHOSTTY_MOUSE_SHAPE_CROSSHAIR as u32 || s == GHOSTTY_MOUSE_SHAPE_CELL as u32 => Cursor::Crosshair,
                    s if s == GHOSTTY_MOUSE_SHAPE_GRAB as u32 => Cursor::Grab,
                    s if s == GHOSTTY_MOUSE_SHAPE_GRABBING as u32 => Cursor::Grabbing,
                    s if s == GHOSTTY_MOUSE_SHAPE_NOT_ALLOWED as u32 => Cursor::NotAllowed,
                    s if s == GHOSTTY_MOUSE_SHAPE_WAIT as u32 || s == GHOSTTY_MOUSE_SHAPE_PROGRESS as u32 => Cursor::Wait,
                    s if s == GHOSTTY_MOUSE_SHAPE_EW_RESIZE as u32 || s == GHOSTTY_MOUSE_SHAPE_COL_RESIZE as u32 => Cursor::ResizeH,
                    s if s == GHOSTTY_MOUSE_SHAPE_NS_RESIZE as u32 || s == GHOSTTY_MOUSE_SHAPE_ROW_RESIZE as u32 => Cursor::ResizeV,
                    _ => Cursor::Text,
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
                self.search = Some(Search { sid: id, needle: needle.clone(), total: -1, selected: -1 });
                if let Some(i) = self.tab_index(id) {
                    self.tabs[i].binding_action(&format!("search:{needle}"));
                }
                w.request_redraw();
            }
            GEvent::EndSearch(id) => {
                if self.search.as_ref().is_some_and(|s| s.sid == id) {
                    self.search = None;
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
                    self.ensure_visible(w);
                    if let Some(i) = self.tab_index(id) {
                        self.select(w, i);
                    }
                    self.prompt = Some(Prompt {
                        kind: PromptKind::CloseTab(id),
                        title: "Close this tab?".into(),
                        detail: "A process is still running in this tab.".into(),
                        input: None,
                    });
                    w.request_redraw();
                } else {
                    self.close_tab_now(w, id);
                }
            }
            GEvent::ClipRead { sid, clip, state, mimes, list } => {
                let token = self.next_token;
                self.next_token += 1;
                let want_text = mimes.iter().any(|m| m == "text/plain");
                self.clip_reads.insert(token, ClipRead { sid, state, want_text, list });
                w.read_clipboard(clip, token);
            }
            GEvent::ClipConfirm { sid, text, state, request } => {
                let title = if request == GHOSTTY_CLIPBOARD_REQUEST_PASTE as u32 {
                    "Paste this text?"
                } else {
                    "Allow a program to read your clipboard?"
                };
                self.ensure_visible(w);
                self.prompt = Some(Prompt {
                    kind: PromptKind::ClipConfirm { sid, text: text.clone(), state },
                    title: title.into(),
                    detail: text,
                    input: None,
                });
                w.request_redraw();
            }
            GEvent::ClipWrite { clip, text, confirm } => {
                if confirm {
                    self.ensure_visible(w);
                    self.prompt = Some(Prompt {
                        kind: PromptKind::ClipWrite { clip, text: text.clone() },
                        title: "Allow a program to set your clipboard?".into(),
                        detail: text,
                        input: None,
                    });
                    w.request_redraw();
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
        let surface = self.tabs[i].surface;
        let available: Vec<&str> = if req.list && text.is_some() { vec!["text/plain"] } else { vec![] };
        match text {
            Some(t) if req.want_text => complete_clipboard(surface, Some(&t), req.state, &available, false),
            _ if req.list => complete_clipboard(surface, None, req.state, &available, false),
            _ => deny_clipboard(surface, req.state),
        }
        self.process(w);
    }

    // ------------------------------------------------------------ keyboard

    pub fn key(&mut self, w: &mut dyn Window, k: KeyInput) {
        if std::env::var_os("GHOSTDROP_TRACE_KEYS").is_some() {
            eprintln!("key {:?} kc={} sym={:#x} unshifted={:#x} text={:?} mods={:#x} consumed={:#x} focused={}", k.state, k.keycode, k.sym, k.unshifted, k.text, k.ghostty_mods, k.consumed, w.is_focused());
        }
        if k.state == KeyState::Release && self.swallow.remove(&k.keycode) {
            return;
        }
        if self.prompt.is_some() {
            if k.state != KeyState::Release {
                self.prompt_key(w, &k);
            }
            return;
        }
        if self.search.is_some() && k.state != KeyState::Release && self.search_key(w, &k) {
            return;
        }
        if k.state == KeyState::Press {
            let action = self.settings.bindings.iter().find(|(c, _)| c.matches(k.mods, k.unshifted)).map(|(_, a)| a.clone());
            if let Some(a) = action {
                self.swallow.insert(k.keycode);
                self.perform(w, &a);
                return;
            }
        }
        let Some(tab) = self.selected_tab() else { return };
        let action = match k.state {
            KeyState::Press => GHOSTTY_ACTION_PRESS,
            KeyState::Repeat => GHOSTTY_ACTION_REPEAT,
            KeyState::Release => GHOSTTY_ACTION_RELEASE,
        };
        let unshifted = xkbcommon::xkb::keysym_to_utf32(xkbcommon::xkb::Keysym::new(k.unshifted));
        let text = (!k.text.is_empty() && k.text.chars().next().is_some_and(|c| c as u32 >= 0x20 && c as u32 != 0x7f))
            .then(|| CString::new(k.text.as_str()).ok())
            .flatten();
        let ev = ghostty_input_key_s {
            action,
            mods: k.ghostty_mods as _,
            consumed_mods: k.consumed as _,
            keycode: k.keycode,
            text: text.as_ref().map_or(std::ptr::null(), |t| t.as_ptr()),
            unshifted_codepoint: unshifted,
            composing: k.composing,
        };
        unsafe { ghostty_surface_key(tab.surface, ev) };
        self.process(w);
    }

    fn prompt_key(&mut self, w: &mut dyn Window, k: &KeyInput) {
        use xkbcommon::xkb::keysyms as ks;
        let editing = self.prompt.as_ref().is_some_and(|p| p.input.is_some());
        match k.sym {
            ks::KEY_Return | ks::KEY_KP_Enter => self.prompt_answer(w, true),
            ks::KEY_Escape => self.prompt_answer(w, false),
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
            PromptKind::ClipConfirm { sid, text, state } => {
                if let Some(i) = self.tab_index(sid) {
                    let s = self.tabs[i].surface;
                    if yes {
                        complete_clipboard(s, Some(&text), state, &["text/plain"], true);
                    } else {
                        deny_clipboard(s, state);
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
        let want = if self.in_tab_bar(w, x, y) { Cursor::Default } else { self.term_cursor };
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

    pub fn pointer(&mut self, w: &mut dyn Window, ev: PointerInput, mods: Mods) {
        if !matches!(ev, PointerInput::Motion { .. }) {
            trace!("pointer {ev:?}");
        }
        let gm = (if mods.shift { GHOSTTY_MODS_SHIFT as u32 } else { 0 })
            | (if mods.ctrl { GHOSTTY_MODS_CTRL as u32 } else { 0 })
            | (if mods.alt { GHOSTTY_MODS_ALT as u32 } else { 0 })
            | (if mods.sup { GHOSTTY_MODS_SUPER as u32 } else { 0 });
        let surface = self.selected_tab().map(|t| t.surface);
        match ev {
            PointerInput::Motion { x, y } => {
                self.mouse = (x, y);
                if self.in_tab_bar(w, x, y) {
                    let h = self.bar_hit(w, x);
                    if h != self.hover {
                        self.hover = h;
                        w.request_redraw();
                    }
                    if let (Some((from, sx)), Some(Hover::Tab(to, _))) = (self.pressed_in_bar, h) {
                        if (x - sx).abs() > 8.0 && to != from {
                            self.move_tab_to(w, from, to);
                            self.pressed_in_bar = Some((to, x));
                        }
                    }
                } else {
                    if self.hover.take().is_some() {
                        w.request_redraw();
                    }
                    if let Some(s) = surface {
                        let (tx, ty) = self.term_pos(w, x, y);
                        unsafe { ghostty_surface_mouse_pos(s, tx, ty, gm as _) };
                    }
                }
                self.update_cursor(w);
            }
            PointerInput::Leave => {
                self.mouse = (-1.0, -1.0);
                if self.hover.take().is_some() {
                    w.request_redraw();
                }
                if let Some(s) = surface {
                    unsafe { ghostty_surface_mouse_pos(s, -1.0, -1.0, gm as _) };
                }
            }
            PointerInput::Button { button, pressed } => {
                let (x, y) = self.mouse;
                if self.prompt.is_some() {
                    return;
                }
                if self.in_tab_bar(w, x, y) {
                    if pressed {
                        self.bar_click(w, button, x);
                    } else {
                        self.pressed_in_bar = None;
                    }
                } else if let Some(s) = surface {
                    let state = if pressed { GHOSTTY_MOUSE_PRESS } else { GHOSTTY_MOUSE_RELEASE };
                    unsafe { ghostty_surface_mouse_button(s, state, ghostty_button(button), gm as _) };
                }
            }
            PointerInput::Scroll { dx, dy, precise } => {
                if let Some(s) = surface {
                    let (x, y) = self.mouse;
                    if !self.in_tab_bar(w, x, y) {
                        // Wayland/X11 report positive = down/right; Ghostty wants
                        // positive = up/left (same inversion as its GTK app).
                        unsafe { ghostty_surface_mouse_scroll(s, -dx, -dy, precise as i32) };
                    }
                }
            }
        }
        self.process(w);
    }

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
                self.pressed_in_bar = Some((i, x));
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
            "{{\"backend\":\"{}\",\"gpu\":{},\"visible\":{},\"focused\":{},\"size\":[{},{}],\"scale\":{},\"selected\":{},\"tabBar\":{},\"hotkey\":\"{}\",\"prompt\":\"{}\",\"search\":\"{}\",\"errors\":[{}],\"tabs\":[{}],\"screen\":\"{}\"}}",
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
            esc(&self.search.as_ref().map_or(String::new(), |s| format!("{} [{}/{}]", s.needle, s.selected, s.total))),
            self.settings.errors.iter().map(|e| format!("\"{}\"", esc(e))).collect::<Vec<_>>().join(","),
            tabs.join(","),
            esc(&self.selected_tab().map_or(String::new(), |t| t.visible_text()))
        )
    }
}
