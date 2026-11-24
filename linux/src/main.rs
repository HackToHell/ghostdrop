//! Ghostdrop for Linux: a fast drop-down terminal built on libghostty.
//!
//!   ghostdrop              toggle (starts it if not running, then shows it)
//!   ghostdrop --daemon     start hidden (for autostart)
//!   ghostdrop ctl <cmd>    toggle|show|hide|new-tab [profile]|next-tab|previous-tab|reload|quit [force]
//!   ghostdrop check        validate the config
//!   ghostdrop +<action>    run a Ghostty CLI action (+list-themes, +show-config --docs, ...)
// bindgen emits c_uint or c_int enum constants depending on platform/version,
// so casts that are no-ops here are needed elsewhere.
#![allow(clippy::unnecessary_cast)]
// x11.rs uses an immediately-invoked closure as a try-block.
#![allow(clippy::redundant_closure_call, clippy::too_many_arguments)]
// `as_chunks` needs Rust 1.88; keep building on older toolchains.
#![allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]

mod core;
mod draw;
mod drm;
mod ffi;
mod ghostty;
mod ipc;
mod keys;
mod settings;
mod tab;
mod wayland;
mod x11;
mod xkb;

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static QUIT: AtomicBool = AtomicBool::new(false);
/// Async-signal-safe wakeup fd (an eventfd): the signal handler only calls `libc::write`.
static SIG_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::SeqCst);
    let fd = SIG_FD.load(Ordering::Acquire);
    if fd >= 0 {
        let val: u64 = 1;
        unsafe {
            libc::write(fd, (&val as *const u64).cast(), std::mem::size_of::<u64>());
        }
    }
}

/// Installs SIGINT/SIGTERM/SIGHUP handlers and returns a non-blocking eventfd
/// that becomes readable when a termination signal arrives.
pub fn install_signal_handlers() -> std::io::Result<std::os::fd::OwnedFd> {
    use rustix::event::{eventfd, EventfdFlags};
    let fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).map_err(std::io::Error::from)?;
    SIG_FD.store(fd.as_raw_fd(), Ordering::Release);
    unsafe {
        let h = on_signal as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGHUP, h);
    }
    Ok(fd)
}

pub fn disarm_signal_fd() {
    SIG_FD.store(-1, Ordering::Release);
}

pub fn quit_requested() -> bool {
    QUIT.load(Ordering::SeqCst)
}

/// Point libghostty at our bundled resources (themes, shell integration,
/// terminfo) and keep a parent terminal's variables out of our shells.
fn setup_env() {
    for v in ["GHOSTTY_BIN_DIR", "GHOSTTY_SHELL_FEATURES", "GHOSTTY_QUICK_TERMINAL", "TERMINFO", "VTE_VERSION", "WINDOWID"] {
        std::env::remove_var(v);
    }
    if let Ok(exe) = std::fs::canonicalize("/proc/self/exe") {
        if let Some(dir) = exe.parent() {
            for cand in [dir.join("share/ghostty"), dir.join("../share/ghostdrop/ghostty")] {
                if cand.join("themes").is_dir() {
                    std::env::set_var("GHOSTTY_RESOURCES_DIR", std::fs::canonicalize(&cand).unwrap_or(cand));
                    return;
                }
            }
        }
    }
}

/// Link the bundled AI setup guide next to the config (see AGENTS.md).
fn link_setup_guide() {
    let Ok(exe) = std::fs::canonicalize("/proc/self/exe") else { return };
    let Some(dir) = exe.parent() else { return };
    let guide = [dir.join("AGENTS.md"), dir.join("../share/doc/ghostdrop/AGENTS.md")]
        .into_iter()
        .find_map(|p| std::fs::canonicalize(p).ok());
    let Some(guide) = guide else { return };
    let Some(cfg_dir) = settings::Settings::path().parent().map(|p| p.to_path_buf()) else { return };
    let _ = std::fs::create_dir_all(&cfg_dir);
    // Same names as the macOS app: agents look for either.
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let link = cfg_dir.join(name);
        match std::fs::symlink_metadata(&link) {
            Ok(m) if !m.file_type().is_symlink() => continue, // the user's own file
            Ok(_) => {
                if std::fs::read_link(&link).ok().as_deref() == Some(guide.as_path()) {
                    continue;
                }
                let _ = std::fs::remove_file(&link);
            }
            Err(_) => {}
        }
        let _ = std::os::unix::fs::symlink(&guide, &link);
    }
}

fn check() -> i32 {
    ghostty::global_init(None);
    let mut s = settings::Settings::load();
    let _cfg = ghostty::Runtime::make_config(&mut s);
    println!("config: {}", settings::Settings::path().display());
    println!("hotkey: {}", s.hotkey.as_ref().map_or("none".into(), |h| h.describe()));
    let profiles: Vec<String> = s
        .profile_order
        .iter()
        .map(|n| {
            let p = &s.profiles[n];
            let star = if *n == s.default_profile { "*" } else { "" };
            format!("{n}{star} → {}", p.effective_command().unwrap_or_else(|| "login shell".into()))
        })
        .collect();
    println!("profiles: {}", profiles.join(", "));
    let binds: Vec<String> = s.bindings.iter().map(|(c, a)| format!("{}={}", c.describe(), a.describe())).collect();
    println!("bindings: {}", binds.join(", "));
    if s.errors.is_empty() {
        println!("OK");
        0
    } else {
        for e in &s.errors {
            println!("error: {e}");
        }
        1
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);

    match sub {
        Some(a) if a.starts_with('+') => {
            setup_env();
            ghostty::global_init(Some(&args));
            std::process::exit(0);
        }
        Some("ctl") => {
            let cmd = args[2..].join(" ");
            if cmd.is_empty() {
                eprintln!("usage: ghostdrop ctl toggle|show|hide|new-tab [profile]|next-tab|previous-tab|reload|quit [force]");
                std::process::exit(2);
            }
            match ipc::send(&cmd) {
                Ok(r) => {
                    println!("{r}");
                    std::process::exit(if r.starts_with("error") { 1 } else { 0 });
                }
                Err(e) => {
                    eprintln!("ghostdrop is not running ({e})");
                    std::process::exit(1);
                }
            }
        }
        Some("check") => {
            setup_env();
            std::process::exit(check());
        }
        Some("--version") | Some("-V") => {
            println!("ghostdrop {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help") | Some("-h") => {
            println!("{}", include_str!("main.rs").lines().skip(2).take(5).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n"));
            return;
        }
        _ => {}
    }

    let daemon = matches!(sub, Some("--daemon") | Some("-d"));
    if ipc::is_running() {
        // Already running: launching again toggles (handy for WM key bindings).
        if !daemon {
            let _ = ipc::send("toggle");
        }
        return;
    }

    setup_env();
    if !ghostty::global_init(None) {
        eprintln!("ghostdrop: ghostty_init failed");
        std::process::exit(1);
    }
    link_setup_guide();

    let make_core = || {
        let mut s = settings::Settings::load();
        let rt = ghostty::Runtime::new(&mut s).unwrap_or_else(|| {
            eprintln!("ghostdrop: failed to create libghostty app");
            std::process::exit(1);
        });
        for e in &s.errors {
            eprintln!("ghostdrop config: {e}");
        }
        core::Core::new(s, rt)
    };

    let backend = std::env::var("GHOSTDROP_BACKEND").unwrap_or_default();
    let has_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let has_x11 = std::env::var_os("DISPLAY").is_some();
    let result = match backend.as_str() {
        "x11" => x11::run(make_core(), !daemon),
        "wayland" => wayland::run(make_core, !daemon),
        _ if has_wayland => match wayland::run(make_core, !daemon) {
            Err(e) if e.contains("layer-shell") && has_x11 => {
                eprintln!("ghostdrop: {e}; falling back to X11 (XWayland)");
                x11::run(make_core(), !daemon)
            }
            r => r,
        },
        _ if has_x11 => x11::run(make_core(), !daemon),
        _ => Err("no display: set WAYLAND_DISPLAY or DISPLAY".into()),
    };
    if let Err(e) = result {
        eprintln!("ghostdrop: {e}");
        ipc::cleanup();
        std::process::exit(1);
    }
}

/// glibc heap statistics (debug): in-use vs free-but-retained bytes, and RSS
/// before/after optionally returning free memory to the OS.
pub fn mem_report(trim: bool) -> String {
    fn rss_mib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()))
            .unwrap_or(0)
            / 1024
    }
    let before = rss_mib();
    let mi = unsafe { libc::mallinfo2() };
    let mut out = format!(
        "rss={before}MiB malloc_in_use={}MiB malloc_free={}MiB mmapped={}MiB",
        mi.uordblks >> 20,
        mi.fordblks >> 20,
        mi.hblkhd >> 20
    );
    if trim {
        unsafe { libc::malloc_trim(0) };
        out += &format!(" rss_after_trim={}MiB", rss_mib());
    }
    out
}
