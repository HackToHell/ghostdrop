//! libghostty app/config + runtime callbacks.
//!
//! Callbacks can fire on any thread (wakeups) or synchronously inside calls
//! like `ghostty_surface_key` (actions, clipboard requests). They copy what
//! they need into a `GEvent` queue that the main loop drains.
use crate::ffi::*;
use crate::settings::{config_dir, Settings};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub type SurfaceId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clip {
    Standard,
    Primary,
}

#[derive(Debug)]
pub enum GEvent {
    Render(SurfaceId),
    NewTab(Option<SurfaceId>),
    CloseTab(SurfaceId, u32),
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
    MouseShape(SurfaceId, u32),
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
    ClipConfirm { sid: SurfaceId, text: String, state: usize, request: u32 },
    ClipWrite { clip: Clip, text: String, confirm: bool },
    PromptTitle(SurfaceId),
    CopyTitle(SurfaceId),
    ResetSize,
    ConfigChanged,
}

static QUEUE: Mutex<Vec<GEvent>> = Mutex::new(Vec::new());
static WAKER: OnceLock<calloop::ping::Ping> = OnceLock::new();

pub fn set_waker(p: calloop::ping::Ping) {
    let _ = WAKER.set(p);
}

pub fn push(ev: GEvent) {
    QUEUE.lock().unwrap().push(ev);
    if let Some(w) = WAKER.get() {
        w.ping();
    }
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

// ---------------------------------------------------------------- callbacks

unsafe extern "C" fn cb_wakeup(_: *mut c_void) {
    if let Some(w) = WAKER.get() {
        w.ping();
    }
}

unsafe extern "C" fn cb_action(_app: ghostty_app_t, target: ghostty_target_s, action: ghostty_action_s) -> bool {
    let sid = if target.tag == GHOSTTY_TARGET_SURFACE { surface_id(target.target.surface) } else { None };
    let a = action.action;
    let ev = match action.tag {
        GHOSTTY_ACTION_RENDER => match sid {
            Some(id) => GEvent::Render(id),
            None => return true,
        },
        GHOSTTY_ACTION_NEW_TAB | GHOSTTY_ACTION_NEW_WINDOW => GEvent::NewTab(sid),
        GHOSTTY_ACTION_CLOSE_TAB => match sid {
            Some(id) => GEvent::CloseTab(id, a.close_tab_mode as u32),
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
        GHOSTTY_ACTION_MOUSE_SHAPE => GEvent::MouseShape(sid.unwrap_or(0), a.mouse_shape as u32),
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
}

unsafe extern "C" fn cb_read_clipboard(
    ud: *mut c_void,
    loc: ghostty_clipboard_e,
    state: *mut c_void,
    mimes: *const *const c_char,
    mimes_len: usize,
    list: bool,
) -> ghostty_clipboard_read_result_e {
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
    push(GEvent::ClipRead { sid: ud as usize as u64, clip, state: state as usize, mimes: ms, list });
    GHOSTTY_CLIPBOARD_READ_STARTED
}

unsafe extern "C" fn cb_confirm_read_clipboard(
    ud: *mut c_void,
    confirm: *const ghostty_clipboard_confirm_s,
    state: *mut c_void,
    request: ghostty_clipboard_request_e,
) {
    let mut text = String::new();
    if let Some(c) = confirm.as_ref() {
        if !c.contents.is_null() {
            for i in 0..c.contents_len {
                let item = &*c.contents.add(i);
                if cstr(item.mime) == "text/plain" && !item.data.is_null() {
                    let b = std::slice::from_raw_parts(item.data as *const u8, item.len);
                    text = String::from_utf8_lossy(b).into_owned();
                }
            }
        }
    }
    push(GEvent::ClipConfirm { sid: ud as usize as u64, text, state: state as usize, request: request as u32 });
}

unsafe extern "C" fn cb_write_clipboard(
    _ud: *mut c_void,
    loc: ghostty_clipboard_e,
    content: *const ghostty_clipboard_content_s,
    len: usize,
    confirm: bool,
) {
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
}

unsafe extern "C" fn cb_close_surface(ud: *mut c_void, process_alive: bool) {
    push(GEvent::CloseSurface(ud as usize as u64, process_alive));
}

// ---------------------------------------------------------------- runtime

pub struct Runtime {
    pub app: ghostty_app_t,
    pub config: ghostty_config_t,
}

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
    let ok = unsafe { ghostty_init(args.len(), argv.as_mut_ptr()) } == 0;
    if ok && cli_args.is_some() {
        unsafe { ghostty_cli_try_action() };
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

impl Runtime {
    /// Layering: Ghostdrop defaults < the user's Ghostty config < Ghostdrop config.
    pub fn make_config(settings: &mut Settings) -> ghostty_config_t {
        unsafe {
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
            for i in 0..n {
                let d = ghostty_config_get_diagnostic(cfg, i);
                settings.errors.push(format!("ghostty: {}", cstr(d.message)));
            }
            cfg
        }
    }

    pub fn new(settings: &mut Settings) -> Option<Runtime> {
        let config = Self::make_config(settings);
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
        let app = unsafe { ghostty_app_new(&rt, config) };
        if app.is_null() {
            return None;
        }
        Some(Runtime { app, config })
    }

    pub fn reload(&mut self, settings: &mut Settings) {
        let cfg = Self::make_config(settings);
        unsafe {
            ghostty_app_update_config(self.app, cfg);
            ghostty_config_free(self.config);
        }
        self.config = cfg;
    }

    pub fn tick(&self) {
        unsafe { ghostty_app_tick(self.app) }
    }

    pub fn set_focus(&self, focused: bool) {
        unsafe { ghostty_app_set_focus(self.app, focused) }
    }

    fn get<T>(&self, key: &str, out: &mut T) -> bool {
        unsafe { ghostty_config_get(self.config, out as *mut T as *mut c_void, key.as_ptr() as *const c_char, key.len()) }
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

    pub fn quick_terminal_size(&self) -> ghostty_config_quick_terminal_size_s {
        let mut v = ghostty_config_quick_terminal_size_s::default();
        self.get("quick-terminal-size", &mut v);
        v
    }

    pub fn needs_confirm_quit(&self) -> bool {
        unsafe { ghostty_app_needs_confirm_quit(self.app) }
    }
}

/// Size spec → pixels against a parent dimension.
pub fn size_dim(s: &ghostty_quick_terminal_size_s, parent: f64) -> Option<f64> {
    unsafe {
        match s.tag {
            GHOSTTY_QUICK_TERMINAL_SIZE_PERCENTAGE => Some(parent * s.value.percentage as f64 / 100.0),
            GHOSTTY_QUICK_TERMINAL_SIZE_PIXELS => Some(s.value.pixels as f64),
            _ => None,
        }
    }
}

/// Completes a pending clipboard read with optional text.
pub fn complete_clipboard(surface: ghostty_surface_t, text: Option<&str>, state: usize, available: &[&str], confirmed: bool) {
    let mime = CString::new("text/plain").unwrap();
    let avail_c: Vec<CString> = available.iter().map(|s| CString::new(*s).unwrap()).collect();
    let avail_p: Vec<*const c_char> = avail_c.iter().map(|s| s.as_ptr()).collect();
    let content = text.map(|t| ghostty_clipboard_content_s {
        mime: mime.as_ptr(),
        data: t.as_ptr() as *const c_char,
        len: t.len(),
    });
    let complete = ghostty_clipboard_complete_s {
        contents: content.as_ref().map_or(std::ptr::null(), |c| c as *const _),
        contents_len: content.is_some() as usize,
        available: if avail_p.is_empty() { std::ptr::null() } else { avail_p.as_ptr() },
        available_len: avail_p.len(),
        confirmed,
        remember: false,
    };
    unsafe { ghostty_surface_complete_clipboard_request(surface, &complete, state as *mut c_void) }
}

pub fn deny_clipboard(surface: ghostty_surface_t, state: usize) {
    unsafe { ghostty_surface_deny_clipboard_request(surface, state as *mut c_void) }
}
