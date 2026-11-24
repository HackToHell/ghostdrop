//! Control socket: single-instance detection + `ghostdrop ctl <command>`.
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

/// Max size of a single IPC command line (prevents a client from stalling or OOMing the UI thread).
const MAX_COMMAND_BYTES: u64 = 64 * 1024;
const SERVER_IO_TIMEOUT: Duration = Duration::from_millis(200);
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(5);

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
    s.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
    s.set_write_timeout(Some(CLIENT_IO_TIMEOUT))?;
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

/// Read one command line from a freshly accepted client (bounded in time and size).
pub fn read_command(stream: &UnixStream) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(SERVER_IO_TIMEOUT)).ok()?;
    let mut line = String::new();
    let n = BufReader::new(stream.take(MAX_COMMAND_BYTES)).read_line(&mut line).ok()?;
    if n == 0 {
        return None;
    }
    Some(line.trim().to_string())
}

/// Send a reply with a bounded write timeout so a stalled/paused client can
/// never block the daemon's UI loop indefinitely.
pub fn reply(mut stream: &UnixStream, msg: &str) {
    let _ = stream.set_write_timeout(Some(SERVER_IO_TIMEOUT));
    let _ = stream.write_all(msg.as_bytes());
    let _ = stream.write_all(b"\n");
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn read_command_times_out_on_silent_client() {
        let (a, _b) = UnixStream::pair().unwrap();
        let t0 = Instant::now();
        assert_eq!(read_command(&a), None);
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn read_command_caps_unterminated_line() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            let chunk = vec![b'x'; 8192];
            for _ in 0..16 {
                if b.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        let got = read_command(&a).unwrap();
        assert_eq!(got.len(), MAX_COMMAND_BYTES as usize);
        let _ = writer.join();
    }

    #[test]
    fn reply_times_out_when_client_never_reads() {
        let (a, _b) = UnixStream::pair().unwrap();
        let big = "A".repeat(4 * 1024 * 1024);
        let t0 = Instant::now();
        reply(&a, &big);
        assert!(t0.elapsed() < Duration::from_secs(2));
    }
}
