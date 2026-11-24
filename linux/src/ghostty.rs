//! Single FFI boundary to libghostty (app, config, surface, frames, and C callbacks).
//!
//! No other module imports `crate::ffi`, and this module does not import `core`,
//! `x11`, `wayland`, `ipc`, or `draw`. Callbacks can fire on any thread (wakeups)
//! or synchronously inside calls like `ghostty_surface_key` / `ghostty_surface_mouse_button`
//! while libghostty holds non-recursive internal locks. Every C callback only copies
//! C data into a `GEvent` value and pushes it into `QUEUE`, and every FFI entry point
//! traps immediately if called re-entrantly from inside a callback or another FFI call.
use crate::ffi::*;
use crate::settings::{config_dir, Settings};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub type SurfaceId = u64;

/// Modifier bitmask matching `ghostty_input_mods_e`.
pub mod mods {
    use crate::ffi::*;

    pub const SHIFT: u32 = 1 << 0;
    pub const CTRL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    pub const SUPER: u32 = 1 << 3;
    pub const CAPS: u32 = 1 << 4;
    pub const NUM: u32 = 1 << 5;
    pub const SHIFT_RIGHT: u32 = 1 << 6;
    pub const CTRL_RIGHT: u32 = 1 << 7;
    pub const ALT_RIGHT: u32 = 1 << 8;
    pub const SUPER_RIGHT: u32 = 1 << 9;

    const _: () = {
        assert!(SHIFT == GHOSTTY_MODS_SHIFT as u32);
        assert!(CTRL == GHOSTTY_MODS_CTRL as u32);
        assert!(ALT == GHOSTTY_MODS_ALT as u32);
        assert!(SUPER == GHOSTTY_MODS_SUPER as u32);
        assert!(CAPS == GHOSTTY_MODS_CAPS as u32);
        assert!(NUM == GHOSTTY_MODS_NUM as u32);
        assert!(SHIFT_RIGHT == GHOSTTY_MODS_SHIFT_RIGHT as u32);
        assert!(CTRL_RIGHT == GHOSTTY_MODS_CTRL_RIGHT as u32);
        assert!(ALT_RIGHT == GHOSTTY_MODS_ALT_RIGHT as u32);
        assert!(SUPER_RIGHT == GHOSTTY_MODS_SUPER_RIGHT as u32);
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clip {
    Standard,
    Primary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipContent {
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipRequestKind {
    Paste,
    Osc52Read,
    Osc52Write,
    KittyRead,
    KittyWrite,
    List,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTabMode {
    This,
    Other,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseShape {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Four,
    Five,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeDim {
    Percentage(f64),
    Pixels(f64),
}

impl SizeDim {
    pub fn resolve(self, parent: f64) -> f64 {
        match self {
            SizeDim::Percentage(p) => parent * p / 100.0,
            SizeDim::Pixels(px) => px,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QuickTerminalSize {
    pub primary: Option<SizeDim>,
    pub secondary: Option<SizeDim>,
}

#[derive(Debug)]
pub enum GEvent {
    Render(SurfaceId),
    NewTab(Option<SurfaceId>),
    CloseTab(SurfaceId, CloseTabMode),
    CloseWindow,
    GotoTab(i32),
    MoveTab(isize),
    Toggle,
    ToggleFullscreen,
    Quit,
    ReloadConfig { soft: bool },
    OpenConfig,
    SetTitle(SurfaceId, String),
    Pwd(SurfaceId, String),
    MouseShape(SurfaceId, MouseShape),
    MouseVisible(bool),
    OpenUrl { url: String, trusted: bool },
    Bell(SurfaceId),
    Notify { title: String, body: String },
    StartSearch(SurfaceId, String),
    EndSearch(SurfaceId),
    SearchTotal(SurfaceId, isize),
    SearchSelected(SurfaceId, isize),
    Background(SurfaceId, [u8; 3]),
    CloseSurface(SurfaceId, bool),
    ClipRead { sid: SurfaceId, clip: Clip, state: usize, mimes: Vec<String>, list: bool },
    ClipConfirm {
        sid: SurfaceId,
        contents: Vec<ClipContent>,
        available: Vec<String>,
        state: usize,
        kind: ClipRequestKind,
    },
    ClipConfirmNil { sid: SurfaceId, state: usize },
    ClipWrite { clip: Clip, text: String, confirm: bool },
    PromptTitle(SurfaceId),
    CopyTitle(SurfaceId),
    ResetSize,
    ConfigChanged,
}

// ---------------------------------------------------------------- re-entrancy trap

thread_local! {
    static CALLBACK_DEPTH: Cell<usize> = const { Cell::new(0) };
    static FFI_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct DepthGuard(&'static std::thread::LocalKey<Cell<usize>>);

impl DepthGuard {
    fn enter(key: &'static std::thread::LocalKey<Cell<usize>>) -> Self {
        key.with(|c| c.set(c.get() + 1));
        Self(key)
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        self.0.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Panics immediately if called from inside a libghostty C callback or while
/// another libghostty FFI call is active on this thread. Active in release builds.
pub fn assert_not_in_call(context: &str) {
    let cb = CALLBACK_DEPTH.with(|c| c.get());
    let ffi = FFI_DEPTH.with(|c| c.get());
    assert!(cb == 0, "re-entrant call ({context}) from inside a libghostty callback");
    assert!(ffi == 0, "re-entrant call ({context}) while a libghostty FFI call is active");
}

#[inline(always)]
fn with_ffi<R>(context: &str, f: impl FnOnce() -> R) -> R {
    assert_not_in_call(context);
    let _guard = DepthGuard::enter(&FFI_DEPTH);
    f()
}

#[inline(always)]
fn with_callback<R>(f: impl FnOnce() -> R) -> R {
    let _guard = DepthGuard::enter(&CALLBACK_DEPTH);
    f()
}

// ---------------------------------------------------------------- queue & clipboard state tracking

static QUEUE: Mutex<Vec<GEvent>> = Mutex::new(Vec::new());
static WAKER: Mutex<Option<calloop::ping::Ping>> = Mutex::new(None);
static PENDING_CLIPBOARD: OnceLock<Mutex<HashMap<SurfaceId, HashSet<usize>>>> = OnceLock::new();

fn pending_clipboard() -> &'static Mutex<HashMap<SurfaceId, HashSet<usize>>> {
    PENDING_CLIPBOARD.get_or_init(|| Mutex::new(HashMap::new()))
}

fn track_clipboard_state(sid: SurfaceId, state: usize) {
    if sid != 0 && state != 0 {
        pending_clipboard().lock().unwrap().entry(sid).or_default().insert(state);
    }
}

fn consume_clipboard_state(sid: SurfaceId, state: usize) -> bool {
    if sid == 0 || state == 0 {
        return false;
    }
    let mut map = pending_clipboard().lock().unwrap();
    let Some(set) = map.get_mut(&sid) else { return false };
    let removed = set.remove(&state);
    if set.is_empty() {
        map.remove(&sid);
    }
    removed
}

fn take_pending_clipboard_states(sid: SurfaceId) -> Vec<usize> {
    pending_clipboard().lock().unwrap().remove(&sid).map(|s| s.into_iter().collect()).unwrap_or_default()
}

/// Render on the GPU behind this DRM node (before the first surface exists).
pub fn set_render_device(path: &str) {
    if let Ok(c) = CString::new(path) {
        with_ffi("ghostty_set_render_device", || unsafe { ghostty_set_render_device(c.as_ptr()) });
    }
}

/// DRM modifiers the presenter imports (export buffers are allocated in one).
pub fn set_present_modifiers(mods: &[u64]) {
    with_ffi("ghostty_set_present_modifiers", || unsafe {
        ghostty_set_present_modifiers(mods.as_ptr(), mods.len())
    });
}

pub fn set_waker(p: calloop::ping::Ping) {
    *WAKER.lock().unwrap() = Some(p);
}

pub fn wake() {
    if let Some(w) = WAKER.lock().unwrap().as_ref() {
        w.ping();
    }
}

pub fn push(ev: GEvent) {
    QUEUE.lock().unwrap().push(ev);
    wake();
}

pub fn drain() -> Vec<GEvent> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}

pub fn has_pending() -> bool {
    !QUEUE.lock().unwrap().is_empty()
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn surface_id(s: ghostty_surface_t) -> Option<SurfaceId> {
    if s.is_null() {
        return None;
    }
    // Unlocked pointer read allowed inside C callbacks.
    let ud = unsafe { ghostty_surface_userdata(s) } as usize as u64;
    (ud != 0).then_some(ud)
}

fn clip_of(loc: ghostty_clipboard_e) -> Option<Clip> {
    match loc {
        GHOSTTY_CLIPBOARD_STANDARD => Some(Clip::Standard),
        GHOSTTY_CLIPBOARD_SELECTION | GHOSTTY_CLIPBOARD_PRIMARY => Some(Clip::Primary),
        _ => None,
    }
}

fn mouse_shape_of(shape: ghostty_action_mouse_shape_e) -> MouseShape {
    match shape {
        GHOSTTY_MOUSE_SHAPE_DEFAULT => MouseShape::Default,
        GHOSTTY_MOUSE_SHAPE_POINTER => MouseShape::Pointer,
        GHOSTTY_MOUSE_SHAPE_CROSSHAIR | GHOSTTY_MOUSE_SHAPE_CELL => MouseShape::Crosshair,
        GHOSTTY_MOUSE_SHAPE_GRAB => MouseShape::Grab,
        GHOSTTY_MOUSE_SHAPE_GRABBING => MouseShape::Grabbing,
        GHOSTTY_MOUSE_SHAPE_NOT_ALLOWED | GHOSTTY_MOUSE_SHAPE_NO_DROP => MouseShape::NotAllowed,
        GHOSTTY_MOUSE_SHAPE_WAIT | GHOSTTY_MOUSE_SHAPE_PROGRESS => MouseShape::Wait,
        GHOSTTY_MOUSE_SHAPE_EW_RESIZE | GHOSTTY_MOUSE_SHAPE_COL_RESIZE => MouseShape::ResizeH,
        GHOSTTY_MOUSE_SHAPE_NS_RESIZE | GHOSTTY_MOUSE_SHAPE_ROW_RESIZE => MouseShape::ResizeV,
        _ => MouseShape::Text,
    }
}

fn c_mouse_button(b: MouseButton) -> ghostty_input_mouse_button_e {
    match b {
        MouseButton::Left => GHOSTTY_MOUSE_LEFT,
        MouseButton::Right => GHOSTTY_MOUSE_RIGHT,
        MouseButton::Middle => GHOSTTY_MOUSE_MIDDLE,
        MouseButton::Four => GHOSTTY_MOUSE_FOUR,
        MouseButton::Five => GHOSTTY_MOUSE_FIVE,
        MouseButton::Unknown => GHOSTTY_MOUSE_UNKNOWN,
    }
}

// ---------------------------------------------------------------- pure-data C callbacks

unsafe extern "C" fn cb_wakeup(_: *mut c_void) {
    with_callback(wake);
}

unsafe extern "C" fn cb_action(_app: ghostty_app_t, target: ghostty_target_s, action: ghostty_action_s) -> bool {
    with_callback(|| {
        let sid = if target.tag == GHOSTTY_TARGET_SURFACE { surface_id(target.target.surface) } else { None };
        let a = action.action;
        let ev = match action.tag {
            GHOSTTY_ACTION_RENDER => match sid {
                Some(id) => GEvent::Render(id),
                None => return true,
            },
            GHOSTTY_ACTION_NEW_TAB | GHOSTTY_ACTION_NEW_WINDOW => GEvent::NewTab(sid),
            GHOSTTY_ACTION_CLOSE_TAB => match sid {
                Some(id) => {
                    let mode = match a.close_tab_mode {
                        GHOSTTY_ACTION_CLOSE_TAB_MODE_OTHER => CloseTabMode::Other,
                        GHOSTTY_ACTION_CLOSE_TAB_MODE_RIGHT => CloseTabMode::Right,
                        _ => CloseTabMode::This,
                    };
                    GEvent::CloseTab(id, mode)
                }
                None => return false,
            },
            GHOSTTY_ACTION_CLOSE_WINDOW | GHOSTTY_ACTION_CLOSE_ALL_WINDOWS => GEvent::CloseWindow,
            GHOSTTY_ACTION_GOTO_TAB => GEvent::GotoTab(a.goto_tab as i32),
            GHOSTTY_ACTION_MOVE_TAB => GEvent::MoveTab(a.move_tab.amount as isize),
            GHOSTTY_ACTION_TOGGLE_QUICK_TERMINAL | GHOSTTY_ACTION_TOGGLE_VISIBILITY => GEvent::Toggle,
            GHOSTTY_ACTION_TOGGLE_FULLSCREEN | GHOSTTY_ACTION_TOGGLE_MAXIMIZE => GEvent::ToggleFullscreen,
            GHOSTTY_ACTION_RESET_WINDOW_SIZE => GEvent::ResetSize,
            GHOSTTY_ACTION_QUIT => GEvent::Quit,
            GHOSTTY_ACTION_RELOAD_CONFIG => GEvent::ReloadConfig { soft: a.reload_config.soft },
            GHOSTTY_ACTION_OPEN_CONFIG => GEvent::OpenConfig,
            GHOSTTY_ACTION_SET_TITLE | GHOSTTY_ACTION_SET_TAB_TITLE => {
                let p = if action.tag == GHOSTTY_ACTION_SET_TITLE { a.set_title.title } else { a.set_tab_title.title };
                GEvent::SetTitle(sid.unwrap_or(0), cstr(p))
            }
            GHOSTTY_ACTION_PWD => GEvent::Pwd(sid.unwrap_or(0), cstr(a.pwd.pwd)),
            GHOSTTY_ACTION_MOUSE_SHAPE => GEvent::MouseShape(sid.unwrap_or(0), mouse_shape_of(a.mouse_shape)),
            GHOSTTY_ACTION_MOUSE_VISIBILITY => GEvent::MouseVisible(a.mouse_visibility == GHOSTTY_MOUSE_VISIBLE),
            GHOSTTY_ACTION_OPEN_URL => {
                let v = a.open_url;
                if v.url.is_null() {
                    return false;
                }
                let bytes = std::slice::from_raw_parts(v.url as *const u8, v.len as usize);
                let trusted = v.kind == GHOSTTY_ACTION_OPEN_URL_KIND_TEXT || v.kind == GHOSTTY_ACTION_OPEN_URL_KIND_HTML;
                GEvent::OpenUrl { url: String::from_utf8_lossy(bytes).into_owned(), trusted }
            }
            GHOSTTY_ACTION_RING_BELL => GEvent::Bell(sid.unwrap_or(0)),
            GHOSTTY_ACTION_DESKTOP_NOTIFICATION => GEvent::Notify {
                title: cstr(a.desktop_notification.title),
                body: cstr(a.desktop_notification.body),
            },
            GHOSTTY_ACTION_START_SEARCH => GEvent::StartSearch(sid.unwrap_or(0), cstr(a.start_search.needle)),
            GHOSTTY_ACTION_END_SEARCH => GEvent::EndSearch(sid.unwrap_or(0)),
            GHOSTTY_ACTION_SEARCH_TOTAL => GEvent::SearchTotal(sid.unwrap_or(0), a.search_total.total as isize),
            GHOSTTY_ACTION_SEARCH_SELECTED => GEvent::SearchSelected(sid.unwrap_or(0), a.search_selected.selected as isize),
            GHOSTTY_ACTION_COLOR_CHANGE => {
                let c = a.color_change;
                if c.kind != GHOSTTY_ACTION_COLOR_KIND_BACKGROUND {
                    return true;
                }
                GEvent::Background(sid.unwrap_or(0), [c.r, c.g, c.b])
            }
            GHOSTTY_ACTION_PROMPT_TITLE => GEvent::PromptTitle(sid.unwrap_or(0)),
            GHOSTTY_ACTION_COPY_TITLE_TO_CLIPBOARD => GEvent::CopyTitle(sid.unwrap_or(0)),
            GHOSTTY_ACTION_CONFIG_CHANGE => GEvent::ConfigChanged,
            GHOSTTY_ACTION_SHOW_CHILD_EXITED => return false,
            GHOSTTY_ACTION_CELL_SIZE
            | GHOSTTY_ACTION_INITIAL_SIZE
            | GHOSTTY_ACTION_SIZE_LIMIT
            | GHOSTTY_ACTION_SCROLLBAR
            | GHOSTTY_ACTION_KEY_SEQUENCE
            | GHOSTTY_ACTION_KEY_TABLE
            | GHOSTTY_ACTION_SELECTION_CHANGED
            | GHOSTTY_ACTION_QUIT_TIMER
            | GHOSTTY_ACTION_READONLY
            | GHOSTTY_ACTION_PROGRESS_REPORT
            | GHOSTTY_ACTION_COMMAND_FINISHED
            | GHOSTTY_ACTION_RENDERER_HEALTH
            | GHOSTTY_ACTION_SECURE_INPUT => return true,
            _ => return false,
        };
        push(ev);
        true
    })
}

unsafe extern "C" fn cb_read_clipboard(
    ud: *mut c_void,
    loc: ghostty_clipboard_e,
    state: *mut c_void,
    mimes: *const *const c_char,
    mimes_len: usize,
    list: bool,
) -> ghostty_clipboard_read_result_e {
    with_callback(|| {
        let Some(clip) = clip_of(loc) else { return GHOSTTY_CLIPBOARD_READ_UNSUPPORTED };
        let mut ms = Vec::new();
        if !mimes.is_null() {
            for i in 0..mimes_len {
                ms.push(cstr(*mimes.add(i)));
            }
        }
        if !list && !ms.iter().any(|m| m == "text/plain") {
            return GHOSTTY_CLIPBOARD_READ_UNAVAILABLE;
        }
        let sid = ud as usize as u64;
        let state = state as usize;
        track_clipboard_state(sid, state);
        push(GEvent::ClipRead { sid, clip, state, mimes: ms, list });
        GHOSTTY_CLIPBOARD_READ_STARTED
    })
}

unsafe extern "C" fn cb_confirm_read_clipboard(
    ud: *mut c_void,
    confirm: *const ghostty_clipboard_confirm_s,
    state: *mut c_void,
    request: ghostty_clipboard_request_e,
) {
    with_callback(|| {
        let sid = ud as usize as u64;
        let state = state as usize;
        track_clipboard_state(sid, state);
        let Some(c) = confirm.as_ref() else {
            push(GEvent::ClipConfirmNil { sid, state });
            return;
        };
        let mut contents = Vec::new();
        if !c.contents.is_null() {
            for i in 0..c.contents_len {
                let item = &*c.contents.add(i);
                if item.mime.is_null() {
                    continue;
                }
                let data = if item.len > 0 && !item.data.is_null() {
                    std::slice::from_raw_parts(item.data as *const u8, item.len).to_vec()
                } else {
                    Vec::new()
                };
                contents.push(ClipContent { mime: cstr(item.mime), data });
            }
        }
        let mut available = Vec::new();
        if !c.available.is_null() {
            for i in 0..c.available_len {
                let p = *c.available.add(i);
                if !p.is_null() {
                    available.push(cstr(p));
                }
            }
        }
        let kind = match request {
            GHOSTTY_CLIPBOARD_REQUEST_PASTE => ClipRequestKind::Paste,
            GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ => ClipRequestKind::Osc52Read,
            GHOSTTY_CLIPBOARD_REQUEST_OSC_52_WRITE => ClipRequestKind::Osc52Write,
            GHOSTTY_CLIPBOARD_REQUEST_KITTY_READ => ClipRequestKind::KittyRead,
            GHOSTTY_CLIPBOARD_REQUEST_KITTY_WRITE => ClipRequestKind::KittyWrite,
            GHOSTTY_CLIPBOARD_REQUEST_LIST => ClipRequestKind::List,
            _ => ClipRequestKind::Other,
        };
        push(GEvent::ClipConfirm { sid, contents, available, state, kind });
    });
}

unsafe extern "C" fn cb_write_clipboard(
    _ud: *mut c_void,
    loc: ghostty_clipboard_e,
    content: *const ghostty_clipboard_content_s,
    len: usize,
    confirm: bool,
) {
    with_callback(|| {
        let Some(clip) = clip_of(loc) else { return };
        if content.is_null() {
            return;
        }
        for i in 0..len {
            let c = &*content.add(i);
            if cstr(c.mime) == "text/plain" && !c.data.is_null() {
                let b = std::slice::from_raw_parts(c.data as *const u8, c.len);
                push(GEvent::ClipWrite { clip, text: String::from_utf8_lossy(b).into_owned(), confirm });
                return;
            }
        }
    });
}

unsafe extern "C" fn cb_close_surface(ud: *mut c_void, process_alive: bool) {
    with_callback(|| {
        push(GEvent::CloseSurface(ud as usize as u64, process_alive));
    });
}

// ---------------------------------------------------------------- global init & config

pub const DEFAULT_GHOSTTY_CONFIG: &str = "\
quick-terminal-position = top
quick-terminal-size = 50%
quick-terminal-screen = mouse
quick-terminal-animation-duration = 0
quick-terminal-autohide = true
";

/// Initialize libghostty's global state. With `cli_args`, runs a Ghostty CLI
/// action (`+list-themes` etc.) and exits.
pub fn global_init(cli_args: Option<&[String]>) -> bool {
    let args: Vec<CString> = match cli_args {
        Some(a) => a.iter().map(|s| CString::new(s.as_str()).unwrap_or_default()).collect(),
        None => vec![CString::new("ghostdrop").unwrap()],
    };
    let mut argv: Vec<*mut c_char> = args.iter().map(|s| s.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let argc = args.len();
    let argv_ptr = argv.as_mut_ptr();
    // Retain for process lifetime: `ghostty_init` stores `argv` inside global state.
    std::mem::forget(args);
    std::mem::forget(argv);
    let ok = with_ffi("ghostty_init", || unsafe { ghostty_init(argc, argv_ptr) == 0 });
    if ok && cli_args.is_some() {
        with_ffi("ghostty_cli_try_action", || unsafe { ghostty_cli_try_action() });
    }
    ok
}

fn has_ghostty_config() -> bool {
    let d = config_dir().join("ghostty");
    ["config", "config.ghostty"].iter().any(|f| d.join(f).exists())
}

fn scratch_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("ghostdrop-{}", unsafe { libc::getuid() }));
    let _ = std::fs::create_dir_all(&d);
    d
}

pub struct Config {
    raw: ghostty_config_t,
}

impl Config {
    /// Layering: Ghostdrop defaults < the user's Ghostty config < Ghostdrop config.
    pub fn load(settings: &mut Settings) -> Config {
        let raw = with_ffi("ghostty_config_load", || unsafe {
            let cfg = ghostty_config_new();
            let dir = scratch_dir();
            let defaults = dir.join("defaults.conf");
            let _ = std::fs::write(&defaults, DEFAULT_GHOSTTY_CONFIG);
            let p = CString::new(defaults.to_string_lossy().as_bytes()).unwrap();
            ghostty_config_load_file(cfg, p.as_ptr());
            // Only if present: libghostty would otherwise write a template file.
            if settings.inherit_ghostty_config && has_ghostty_config() {
                ghostty_config_load_default_files(cfg);
            }
            if !settings.ghostty_lines.is_empty() {
                let over = dir.join("overrides.conf");
                let _ = std::fs::write(&over, settings.ghostty_lines.join("\n") + "\n");
                let p = CString::new(over.to_string_lossy().as_bytes()).unwrap();
                ghostty_config_load_file(cfg, p.as_ptr());
            }
            ghostty_config_load_recursive_files(cfg);
            ghostty_config_finalize(cfg);
            let n = ghostty_config_diagnostics_count(cfg);
            // Point at the user's file/line, not our scratch overrides file.
            let over = format!("{}:", dir.join("overrides.conf").display());
            for i in 0..n {
                let d = ghostty_config_get_diagnostic(cfg, i);
                let msg = cstr(d.message);
                let mapped = msg.strip_prefix(&over).and_then(|rest| {
                    let (num, tail) = rest.split_once(':')?;
                    let src = settings.ghostty_line_src.get(num.parse::<usize>().ok()?.checked_sub(1)?)?;
                    Some(format!("{} line {src}: {}", Settings::path().display(), tail.trim_start()))
                });
                settings.errors.push(mapped.unwrap_or_else(|| format!("ghostty: {msg}")));
            }
            cfg
        });
        Config { raw }
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            with_ffi("ghostty_config_free", || unsafe { ghostty_config_free(self.raw) });
            self.raw = std::ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------- runtime

pub struct Runtime {
    app: ghostty_app_t,
    config: Config,
}

impl Runtime {
    pub fn make_config(settings: &mut Settings) -> Config {
        Config::load(settings)
    }

    pub fn new(settings: &mut Settings) -> Option<Runtime> {
        let config = Config::load(settings);
        let rt = ghostty_runtime_config_s {
            userdata: std::ptr::null_mut(),
            supports_selection_clipboard: true,
            wakeup_cb: Some(cb_wakeup),
            action_cb: Some(cb_action),
            read_clipboard_cb: Some(cb_read_clipboard),
            confirm_read_clipboard_cb: Some(cb_confirm_read_clipboard),
            write_clipboard_cb: Some(cb_write_clipboard),
            close_surface_cb: Some(cb_close_surface),
        };
        let app = with_ffi("ghostty_app_new", || unsafe { ghostty_app_new(&rt, config.raw) });
        if app.is_null() {
            return None;
        }
        Some(Runtime { app, config })
    }

    pub fn reload(&mut self, settings: &mut Settings) {
        let cfg = Config::load(settings);
        with_ffi("ghostty_app_update_config", || unsafe {
            ghostty_app_update_config(self.app, cfg.raw);
        });
        self.config = cfg;
    }

    pub fn soft_reload(&self) {
        with_ffi("ghostty_app_update_config", || unsafe {
            ghostty_app_update_config(self.app, self.config.raw);
        });
    }

    pub fn tick(&self) {
        with_ffi("ghostty_app_tick", || unsafe { ghostty_app_tick(self.app) });
    }

    pub fn set_focus(&self, focused: bool) {
        with_ffi("ghostty_app_set_focus", || unsafe { ghostty_app_set_focus(self.app, focused) });
    }

    fn get<T>(&self, key: &str, out: &mut T) -> bool {
        with_ffi("ghostty_config_get", || unsafe {
            ghostty_config_get(self.config.raw, out as *mut T as *mut c_void, key.as_ptr() as *const c_char, key.len())
        })
    }

    pub fn string(&self, key: &str) -> Option<String> {
        let mut p: *const c_char = std::ptr::null();
        (self.get(key, &mut p) && !p.is_null()).then(|| cstr(p))
    }

    pub fn f64(&self, key: &str, def: f64) -> f64 {
        let mut v = def;
        self.get(key, &mut v);
        v
    }

    pub fn bool(&self, key: &str, def: bool) -> bool {
        let mut v = def;
        self.get(key, &mut v);
        v
    }

    pub fn color(&self, key: &str) -> Option<[u8; 3]> {
        let mut c = ghostty_config_color_s::default();
        self.get(key, &mut c).then_some([c.r, c.g, c.b])
    }

    pub fn quick_terminal_size(&self) -> QuickTerminalSize {
        let mut v = ghostty_config_quick_terminal_size_s::default();
        self.get("quick-terminal-size", &mut v);
        fn dim(s: &ghostty_quick_terminal_size_s) -> Option<SizeDim> {
            unsafe {
                match s.tag {
                    GHOSTTY_QUICK_TERMINAL_SIZE_PERCENTAGE => Some(SizeDim::Percentage(s.value.percentage as f64)),
                    GHOSTTY_QUICK_TERMINAL_SIZE_PIXELS => Some(SizeDim::Pixels(s.value.pixels as f64)),
                    _ => None,
                }
            }
        }
        QuickTerminalSize { primary: dim(&v.primary), secondary: dim(&v.secondary) }
    }

    pub fn needs_confirm_quit(&self) -> bool {
        with_ffi("ghostty_app_needs_confirm_quit", || unsafe { ghostty_app_needs_confirm_quit(self.app) })
    }

    pub fn new_surface(
        &self,
        id: SurfaceId,
        command: Option<&str>,
        working_directory: Option<&str>,
        env: &[(String, String)],
        font_size: f32,
        scale: f64,
        wait_after_command: bool,
    ) -> Option<Surface> {
        let cmd_c = command.map(|c| CString::new(c).unwrap_or_default());
        let wd_c = working_directory.map(|c| CString::new(c).unwrap_or_default());
        let env_k: Vec<CString> = env.iter().map(|(k, _)| CString::new(k.as_str()).unwrap_or_default()).collect();
        let env_v: Vec<CString> = env.iter().map(|(_, v)| CString::new(v.as_str()).unwrap_or_default()).collect();
        let mut env_c: Vec<ghostty_env_var_s> =
            env_k.iter().zip(&env_v).map(|(k, v)| ghostty_env_var_s { key: k.as_ptr(), value: v.as_ptr() }).collect();

        let raw = with_ffi("ghostty_surface_new", || unsafe {
            let mut cfg = ghostty_surface_config_new();
            cfg.platform_tag = GHOSTTY_PLATFORM_LINUX as _;
            cfg.userdata = id as usize as *mut c_void;
            cfg.scale_factor = scale;
            cfg.font_size = font_size;
            cfg.wait_after_command = wait_after_command;
            cfg.context = GHOSTTY_SURFACE_CONTEXT_TAB;
            cfg.command = cmd_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            cfg.working_directory = wd_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            cfg.env_vars = if env_c.is_empty() { std::ptr::null_mut() } else { env_c.as_mut_ptr() };
            cfg.env_var_count = env_c.len();
            let s = ghostty_surface_new(self.app, &cfg);
            if !s.is_null() {
                // CPU frames until the core enables GPU frames (backend permitting).
                ghostty_surface_set_gpu_frames(s, false);
            }
            s
        });
        if raw.is_null() {
            return None;
        }
        Some(Surface { id, raw })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.app.is_null() {
            with_ffi("ghostty_app_free", || unsafe { ghostty_app_free(self.app) });
            self.app = std::ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------- frames & surfaces

/// A frame from libghostty: CPU pixels (BGRA8, premultiplied) or a DMA-BUF
/// (top-down DRM ARGB8888, see patches/ghostty). Freed on drop, which also
/// closes the DMA-BUF fds; presenters dup what they keep.
pub struct Frame {
    handle: *mut c_void,
    pub width: u32,
    pub height: u32,
    pub data: FrameData,
}

pub enum FrameData {
    Memory { pixels: *const u8, stride: usize, flip_y: bool },
    Dmabuf(Dmabuf),
}

#[derive(Clone, Copy, Debug)]
pub struct Dmabuf {
    pub fourcc: u32,
    pub modifier: u64,
    pub n_planes: usize,
    pub fds: [i32; 4],
    pub offsets: [u32; 4],
    pub strides: [u32; 4],
    /// Identity of the underlying buffer (inode of plane 0's dma_buf): the
    /// renderer cycles through a few, so presenters cache imports by this.
    pub ino: u64,
}

impl Frame {
    pub fn dmabuf(&self) -> Option<&Dmabuf> {
        match &self.data {
            FrameData::Dmabuf(d) => Some(d),
            FrameData::Memory { .. } => None,
        }
    }

    /// Row `y` in top-down order (CPU frames only).
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        let FrameData::Memory { pixels, stride, flip_y } = self.data else { return None };
        let src_y = if flip_y { self.height - 1 - y } else { y };
        Some(unsafe { std::slice::from_raw_parts(pixels.add(src_y as usize * stride), self.width as usize * 4) })
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            with_ffi("ghostty_frame_free", || unsafe { ghostty_frame_free(self.handle) });
            self.handle = std::ptr::null_mut();
        }
    }
}

pub struct Surface {
    pub id: SurfaceId,
    raw: ghostty_surface_t,
}

impl Surface {
    pub fn take_frame(&self) -> Option<Frame> {
        with_ffi("ghostty_surface_take_frame", || {
            let mut f = ghostty_frame_s::default();
            if !unsafe { ghostty_surface_take_frame(self.raw, &mut f) } {
                return None;
            }
            let data = if f.kind == GHOSTTY_FRAME_MEMORY as i32 && !f.pixels.is_null() {
                FrameData::Memory { pixels: f.pixels, stride: f.stride, flip_y: f.flip_y }
            } else if f.kind == GHOSTTY_FRAME_DMABUF as i32 && (1..=4).contains(&f.n_planes) && f.fds[0] >= 0 {
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                let ino = if unsafe { libc::fstat(f.fds[0], &mut st) } == 0 { st.st_ino as u64 } else { 0 };
                FrameData::Dmabuf(Dmabuf {
                    fourcc: f.fourcc,
                    modifier: f.modifier,
                    n_planes: f.n_planes as usize,
                    fds: f.fds,
                    offsets: f.offsets,
                    strides: f.strides,
                    ino,
                })
            } else {
                unsafe { ghostty_frame_free(f.handle) };
                return None;
            };
            Some(Frame { handle: f.handle, width: f.width, height: f.height, data })
        })
    }

    pub fn set_size(&self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            with_ffi("ghostty_surface_set_size", || unsafe { ghostty_surface_set_size(self.raw, w, h) });
        }
    }

    pub fn set_scale(&self, scale: f64) {
        with_ffi("ghostty_surface_set_content_scale", || unsafe {
            ghostty_surface_set_content_scale(self.raw, scale, scale)
        });
    }

    pub fn set_focus(&self, focused: bool) {
        with_ffi("ghostty_surface_set_focus", || unsafe { ghostty_surface_set_focus(self.raw, focused) });
    }

    pub fn set_gpu_frames(&self, on: bool) {
        with_ffi("ghostty_surface_set_gpu_frames", || unsafe {
            ghostty_surface_set_gpu_frames(self.raw, on);
            ghostty_surface_refresh(self.raw);
        });
    }

    pub fn set_visible(&self, visible: bool) {
        with_ffi("ghostty_surface_set_occlusion", || unsafe {
            ghostty_surface_set_occlusion(self.raw, visible)
        });
    }

    pub fn binding_action(&self, action: &str) -> bool {
        with_ffi("ghostty_surface_binding_action", || unsafe {
            ghostty_surface_binding_action(self.raw, action.as_ptr() as *const _, action.len())
        })
    }

    pub fn request_close(&self) {
        with_ffi("ghostty_surface_request_close", || unsafe { ghostty_surface_request_close(self.raw) });
    }

    pub fn process_exited(&self) -> bool {
        with_ffi("ghostty_surface_process_exited", || unsafe { ghostty_surface_process_exited(self.raw) })
    }

    /// Working directory and font size to inherit for a new local tab.
    pub fn inherited_tab_config(&self) -> (Option<String>, f32) {
        with_ffi("ghostty_surface_inherited_config", || unsafe {
            let c = ghostty_surface_inherited_config(self.raw, GHOSTTY_SURFACE_CONTEXT_TAB);
            let cwd = (!c.working_directory.is_null()).then(|| cstr(c.working_directory));
            (cwd, c.font_size)
        })
    }

    pub fn grid(&self) -> (u16, u16) {
        let s = with_ffi("ghostty_surface_size", || unsafe { ghostty_surface_size(self.raw) });
        (s.columns, s.rows)
    }

    pub fn visible_text(&self) -> String {
        with_ffi("ghostty_surface_read_text", || unsafe {
            let mut t = ghostty_text_s::default();
            let sel = ghostty_selection_s {
                top_left: ghostty_point_s { tag: GHOSTTY_POINT_VIEWPORT, coord: GHOSTTY_POINT_COORD_TOP_LEFT, x: 0, y: 0 },
                bottom_right: ghostty_point_s {
                    tag: GHOSTTY_POINT_VIEWPORT,
                    coord: GHOSTTY_POINT_COORD_BOTTOM_RIGHT,
                    x: 0,
                    y: 0,
                },
                rectangle: false,
            };
            if !ghostty_surface_read_text(self.raw, sel, &mut t) {
                return String::new();
            }
            let s = cstr(t.text);
            ghostty_surface_free_text(self.raw, &mut t);
            s
        })
    }

    pub fn key(
        &self,
        action: KeyAction,
        mods: u32,
        consumed_mods: u32,
        keycode: u32,
        text: Option<&str>,
        unshifted_codepoint: u32,
        composing: bool,
    ) {
        let c_action = match action {
            KeyAction::Press => GHOSTTY_ACTION_PRESS,
            KeyAction::Repeat => GHOSTTY_ACTION_REPEAT,
            KeyAction::Release => GHOSTTY_ACTION_RELEASE,
        };
        let text_c = text.and_then(|s| CString::new(s).ok());
        let ev = ghostty_input_key_s {
            action: c_action,
            mods: mods as _,
            consumed_mods: consumed_mods as _,
            keycode,
            text: text_c.as_ref().map_or(std::ptr::null(), |t| t.as_ptr()),
            unshifted_codepoint,
            composing,
        };
        with_ffi("ghostty_surface_key", || unsafe { ghostty_surface_key(self.raw, ev) });
    }

    pub fn mouse_pos(&self, x: f64, y: f64, mods: u32) {
        with_ffi("ghostty_surface_mouse_pos", || unsafe {
            ghostty_surface_mouse_pos(self.raw, x, y, mods as _)
        });
    }

    pub fn mouse_button(&self, pressed: bool, button: MouseButton, mods: u32) {
        let state = if pressed { GHOSTTY_MOUSE_PRESS } else { GHOSTTY_MOUSE_RELEASE };
        let btn = c_mouse_button(button);
        with_ffi("ghostty_surface_mouse_button", || unsafe {
            ghostty_surface_mouse_button(self.raw, state, btn, mods as _)
        });
    }

    pub fn mouse_scroll(&self, dx: f64, dy: f64, precise: bool) {
        with_ffi("ghostty_surface_mouse_scroll", || unsafe {
            ghostty_surface_mouse_scroll(self.raw, dx, dy, precise as i32)
        });
    }

    /// Completes a tracked clipboard request at most once.
    pub fn complete_clipboard(
        &self,
        contents: &[ClipContent],
        available: &[String],
        state: usize,
        confirmed: bool,
    ) {
        if !consume_clipboard_state(self.id, state) {
            return;
        }
        let mime_c: Vec<CString> = contents.iter().map(|c| CString::new(c.mime.as_str()).unwrap_or_default()).collect();
        let c_contents: Vec<ghostty_clipboard_content_s> = contents
            .iter()
            .zip(&mime_c)
            .map(|(c, m)| ghostty_clipboard_content_s {
                mime: m.as_ptr(),
                data: c.data.as_ptr() as *const c_char,
                len: c.data.len(),
            })
            .collect();
        let avail_c: Vec<CString> = available.iter().map(|s| CString::new(s.as_str()).unwrap_or_default()).collect();
        let avail_p: Vec<*const c_char> = avail_c.iter().map(|s| s.as_ptr()).collect();
        let complete = ghostty_clipboard_complete_s {
            contents: if c_contents.is_empty() { std::ptr::null() } else { c_contents.as_ptr() },
            contents_len: c_contents.len(),
            available: if avail_p.is_empty() { std::ptr::null() } else { avail_p.as_ptr() },
            available_len: avail_p.len(),
            confirmed,
            remember: false,
        };
        with_ffi("ghostty_surface_complete_clipboard_request", || unsafe {
            ghostty_surface_complete_clipboard_request(self.raw, &complete, state as *mut c_void)
        });
    }

    /// Denies a tracked clipboard request at most once.
    pub fn deny_clipboard(&self, state: usize) {
        if !consume_clipboard_state(self.id, state) {
            return;
        }
        with_ffi("ghostty_surface_deny_clipboard_request", || unsafe {
            ghostty_surface_deny_clipboard_request(self.raw, state as *mut c_void)
        });
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // Deny any in-flight clipboard requests before freeing so libghostty
            // releases its heap-allocated request state cleanly.
            for state in take_pending_clipboard_states(self.id) {
                with_ffi("ghostty_surface_deny_clipboard_request", || unsafe {
                    ghostty_surface_deny_clipboard_request(self.raw, state as *mut c_void)
                });
            }
            with_ffi("ghostty_surface_free", || unsafe { ghostty_surface_free(self.raw) });
            self.raw = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "from inside a libghostty callback")]
    fn reentrancy_traps_inside_callback() {
        with_callback(|| {
            assert_not_in_call("test_reenter");
        });
    }

    #[test]
    #[should_panic(expected = "while a libghostty FFI call is active")]
    fn reentrancy_traps_inside_ffi() {
        with_ffi("outer", || {
            with_ffi("inner", || {});
        });
    }

    #[test]
    fn clipboard_state_consumed_once_and_cleaned_up() {
        let sid: SurfaceId = 999_001;
        let st1: usize = 0x1000;
        let st2: usize = 0x2000;
        track_clipboard_state(sid, st1);
        track_clipboard_state(sid, st2);
        assert!(consume_clipboard_state(sid, st1));
        assert!(!consume_clipboard_state(sid, st1));
        let rest = take_pending_clipboard_states(sid);
        assert_eq!(rest, vec![st2]);
        assert!(take_pending_clipboard_states(sid).is_empty());
    }
}
