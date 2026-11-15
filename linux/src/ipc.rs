//! Control socket: single-instance detection + `ghostdrop ctl <command>`.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("GHOSTDROP_SOCKET") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    // One instance per display, so two sessions (or a nested compositor)
    // don't fight over one socket; `ghostdrop ctl` inside a session reaches
    // that session's instance.
    let display = display_tag();
    let uid = unsafe { libc::getuid() };
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join(format!("ghostdrop-{display}.sock")),
        _ => std::env::temp_dir().join(format!("ghostdrop-{uid}-{display}.sock")),
    }
}

/// `wayland-1`, `x11-0`, … from the environment (`default` without a display).
fn display_tag() -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect::<String>();
    if let Some(w) = std::env::var("WAYLAND_DISPLAY").ok().filter(|w| !w.is_empty()) {
        // May be an absolute path; keep the socket name only.
        return clean(w.rsplit('/').next().unwrap_or(&w));
    }
    if let Some(d) = std::env::var("DISPLAY").ok().filter(|d| !d.is_empty()) {
        // ":1", ":1.0", "host:1" → "x11-1", "x11-1.0", "x11-host_1"
        return format!("x11-{}", clean(d.trim_start_matches(':')));
    }
    "default".into()
}

/// Send one command to the running instance. Returns its reply.
pub fn send(cmd: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(socket_path())?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(cmd.as_bytes())?;
    s.write_all(b"\n")?;
    s.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    Ok(reply.trim_end().to_string())
}

pub fn is_running() -> bool {
    send("ping").map(|r| r == "pong").unwrap_or(false)
}

/// Bind the control socket (removing a stale one).
pub fn listen() -> std::io::Result<UnixListener> {
    let path = socket_path();
    if path.exists() && !is_running() {
        let _ = std::fs::remove_file(&path);
    }
    let l = UnixListener::bind(&path)?;
    l.set_nonblocking(true)?;
    Ok(l)
}

/// Read one command line from a freshly accepted client.
pub fn read_command(stream: &UnixStream) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    Some(line.trim().to_string())
}

pub fn reply(mut stream: &UnixStream, msg: &str) {
    let _ = stream.write_all(msg.as_bytes());
    let _ = stream.write_all(b"\n");
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}
