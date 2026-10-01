//! The `velt dev` protocol between the supervisor and the program (rt_abi_async.md §13): one
//! request per connection to the supervisor's dev channel (`VELT_DEV_SOCKET`). Both sides live
//! here so they cannot drift apart; the transport is per platform:
//! - `unix`: a Unix socket; listeners travel as file descriptors (`SCM_RIGHTS`);
//! - `windows`: a named pipe; listeners travel as `WSAPROTOCOL_INFOW` records
//!   (`WSADuplicateSocketW` for the requesting process, `WSASocketW` on its side).
//!
//! Requests (one line each, then the reply):
//! - **Listener handover**: `listen <addr>\n` → `ok…\n` with the listening socket, or
//!   `err <message>\n`. The supervisor binds an address on its first request and hands the same
//!   socket to every later program, so the port (even one picked with port 0) stays the same
//!   across reloads.
//! - **Build report** (JIT host): the host compiles while the previous version still runs, then
//!   sends `built ok\n` or `built failed\n`, one line per source file it read, and an empty
//!   line. After `ok` it waits for `go\n`, which the supervisor sends once the previous version
//!   has stopped.
//! - **Reload channel** (hot swap): after `go` the host keeps the connection, and the supervisor
//!   sends `reload` there for every later version ([`reload`]).
//! - **Stop channel** (Windows, which has no SIGTERM): `watch-stop\n` → `ok\n`; the connection
//!   stays open and the supervisor later writes `stop\n` to ask the program to drain and exit.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

pub mod reload;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix::receive_listener;
#[cfg(unix)]
pub use unix::{connect, peer_pid, reply_ok, Server, Stream};
#[cfg(windows)]
use windows::receive_listener;
#[cfg(windows)]
pub use windows::{connect, peer_pid, reply_ok, Server, Stream};

/// Longest reply line accepted.
const MAX_LINE: usize = 4096;
/// Longest build report accepted (a few hundred paths).
const MAX_REPORT: u64 = 1 << 20;

/// Program side: ask the supervisor at `socket` for a listener bound to `addr`.
pub fn request(socket: &OsStr, addr: &str) -> io::Result<TcpListener> {
    let stream = connect(socket).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("velt dev: cannot reach the supervisor: {e}"),
        )
    })?;
    (&stream).write_all(format!("listen {addr}\n").as_bytes())?;
    receive_listener(&stream)
}

/// A request the supervisor received.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// `listen <addr>`: hand over a listener for this address.
    Listen(String),
    /// `built ok|failed` with the files the build read.
    Built {
        /// Whether the build succeeded (the host then waits for [`reply_go`]).
        ok: bool,
        /// Every source file the build read.
        files: Vec<PathBuf>,
    },
    /// `watch-stop`: keep this connection to send [`reply_stop`] later.
    WatchStop,
}

/// Supervisor side: read one request.
pub fn read_request(stream: &Stream) -> io::Result<Request> {
    let mut reader = BufReader::new(stream.take(MAX_REPORT));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let first = line.trim_end();
    if let Some(addr) = first.strip_prefix("listen ") {
        return Ok(Request::Listen(addr.to_string()));
    }
    let ok = match first {
        "built ok" => true,
        "built failed" => false,
        "watch-stop" => return Ok(Request::WatchStop),
        other => return Err(io::Error::other(format!("unexpected request `{other}`"))),
    };
    let files = read_files(&mut reader)?;
    Ok(Request::Built { ok, files })
}

/// File lines up to an empty line (or the end).
fn read_files(reader: &mut impl BufRead) -> io::Result<Vec<PathBuf>> {
    let mut files = vec![];
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            return Ok(files);
        }
        files.push(PathBuf::from(line.trim_end_matches(['\r', '\n'])));
    }
}

/// Append one line per file, then the empty line that ends the list.
fn push_files(text: &mut String, files: &[PathBuf]) {
    for f in files {
        let f = f.to_string_lossy();
        // A path with a line break cannot be sent; it is only not watched.
        if !f.contains('\n') {
            text.push_str(&f);
            text.push('\n');
        }
    }
    text.push('\n');
}

/// Host side: report a build to the supervisor at `socket`. After a good build this blocks
/// until the supervisor says `go` (the previous version has stopped), and returns the
/// connection: the host's reload channel ([`reload`]).
pub fn report_build(socket: &OsStr, ok: bool, files: &[PathBuf]) -> io::Result<Option<Stream>> {
    let stream = connect(socket)?;
    let mut report = format!("built {}\n", if ok { "ok" } else { "failed" });
    push_files(&mut report, files);
    (&stream).write_all(report.as_bytes())?;
    if !ok {
        return Ok(None);
    }
    expect_line(&stream, "go")?;
    Ok(Some(stream))
}

/// Supervisor side: let a reported host start its program.
pub fn reply_go(stream: &Stream) -> io::Result<()> {
    (&*stream).write_all(b"go\n")
}

/// Supervisor side: reply with an error message (no listener).
pub fn reply_err(stream: &Stream, message: &str) -> io::Result<()> {
    let line = format!("err {}\n", message.replace('\n', " "));
    (&*stream).write_all(line.as_bytes())
}

/// Program side: open the stop channel (a `watch-stop` request the supervisor acknowledged).
pub fn watch_stop(socket: &OsStr) -> io::Result<Stream> {
    let stream = connect(socket)?;
    (&stream).write_all(b"watch-stop\n")?;
    expect_line(&stream, "ok")?;
    Ok(stream)
}

/// Program side: block until the supervisor asks this program to stop (or goes away).
pub fn wait_stop(stream: &Stream) {
    let _ = read_line_unbuffered(stream);
}

/// Supervisor side: acknowledge a `watch-stop` request (keep `stream` for [`reply_stop`]).
pub fn reply_watching(stream: &Stream) -> io::Result<()> {
    (&*stream).write_all(b"ok\n")
}

/// Supervisor side: ask the program on a `watch-stop` connection to stop.
pub fn reply_stop(stream: &Stream) -> io::Result<()> {
    (&*stream).write_all(b"stop\n")
}

/// A dev channel name unique to this process and server (`velt-dev-<pid>-<n>`).
fn channel_name() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("velt-dev-{}-{n}", std::process::id())
}

/// Read one line and check it is `want`.
fn expect_line(stream: &Stream, want: &str) -> io::Result<()> {
    match read_line_unbuffered(stream)?.as_str() {
        line if line == want => Ok(()),
        other => Err(io::Error::other(format!("unexpected reply `{other}`"))),
    }
}

/// One line, read byte by byte so nothing after it is consumed (the connection stays in use).
fn read_line_unbuffered(mut stream: impl Read) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8];
    while line.len() < MAX_LINE {
        if stream.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&line).trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole protocol through a real dev channel: build reports, listener handover
    /// (the same socket every time), errors and the stop channel.
    #[test]
    fn protocol_round_trip() {
        let mut server = Server::bind().unwrap();
        let name = server.name().to_os_string();
        let bound = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = bound.local_addr().unwrap().port();
        let files = vec![PathBuf::from("/a/main.vlt"), PathBuf::from("/std/http.vlt")];
        let (socket, sent) = (name.clone(), files.clone());
        let program = std::thread::spawn(move || {
            report_build(&socket, false, &sent[..1]).unwrap();
            report_build(&socket, true, &sent).unwrap();
            let listener = request(&socket, "127.0.0.1:0").unwrap();
            assert_eq!(listener.local_addr().unwrap().port(), port);
            let err = request(&socket, "bad").unwrap_err();
            assert_eq!(err.to_string(), "no such host");
            let stop = watch_stop(&socket).unwrap();
            wait_stop(&stop);
        });
        let s = server.accept().unwrap();
        let failed = Request::Built {
            ok: false,
            files: files[..1].to_vec(),
        };
        assert_eq!(read_request(&s).unwrap(), failed);
        let s = server.accept().unwrap();
        #[cfg(any(target_os = "linux", target_vendor = "apple", windows))]
        assert_eq!(peer_pid(&s).unwrap(), std::process::id());
        assert_eq!(
            read_request(&s).unwrap(),
            Request::Built { ok: true, files }
        );
        reply_go(&s).unwrap();
        let s = server.accept().unwrap();
        let listen = |a: &str| Request::Listen(a.to_string());
        assert_eq!(read_request(&s).unwrap(), listen("127.0.0.1:0"));
        reply_ok(&s, &bound).unwrap();
        let s = server.accept().unwrap();
        assert_eq!(read_request(&s).unwrap(), listen("bad"));
        reply_err(&s, "no such\nhost").unwrap();
        let s = server.accept().unwrap();
        assert_eq!(read_request(&s).unwrap(), Request::WatchStop);
        reply_watching(&s).unwrap();
        reply_stop(&s).unwrap();
        program.join().unwrap();
    }
}
