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
    let uid = unsafe { libc::getuid() };
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("ghostdrop.sock"),
        _ => std::env::temp_dir().join(format!("ghostdrop-{uid}.sock")),
    }
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
