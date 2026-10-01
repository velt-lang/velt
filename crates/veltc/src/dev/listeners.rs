//! Supervisor side of the dev channel (protocol and transports in `velt_rt_host::dev::handover`):
//! - owns the listening sockets for the whole `velt dev` session and hands them to each program
//!   version. Because the supervisor keeps every socket open, connections that arrive while one
//!   version stops and the next starts wait in the kernel backlog instead of being refused;
//! - forwards the JIT hosts' build reports to the supervisor loop;
//! - (Windows) keeps each program's stop channel, the graceful-stop path where Unix uses SIGTERM.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use velt_rt_host::dev::handover::{self, Request, Server, Stream};

/// A JIT host's build report; after a good build, `stream` is where to send `go`.
pub struct Built {
    /// Whether the build succeeded.
    pub ok: bool,
    /// Every source file the build read.
    pub files: Vec<PathBuf>,
    /// The host's connection, waiting for `go`.
    pub stream: Stream,
}

/// The dev channel server, running on a background thread for the session.
pub struct Handover {
    name: OsString,
    /// Open stop channels by program process id (Windows).
    #[cfg(windows)]
    stop_channels: StopChannels,
}

/// Listening sockets by requested address (`host:port` as the program spelled it).
type Sockets = Arc<Mutex<HashMap<String, TcpListener>>>;
/// Programs' stop channels by process id.
type StopChannels = Arc<Mutex<HashMap<u32, Stream>>>;

/// What the serving thread shares with the supervisor.
struct State {
    sockets: Sockets,
    stop_channels: StopChannels,
    reports: Sender<Built>,
}

impl Handover {
    /// Create the dev channel and serve requests on a background thread; build reports go to
    /// `reports`.
    pub fn start(reports: Sender<Built>) -> Result<Handover, String> {
        let mut server = Server::bind().map_err(|e| e.to_string())?;
        let name = server.name().to_os_string();
        let state = State {
            sockets: Sockets::default(),
            stop_channels: StopChannels::default(),
            reports,
        };
        #[cfg(windows)]
        let stop_channels = state.stop_channels.clone();
        std::thread::Builder::new()
            .name("velt-dev-handover".into())
            .spawn(move || loop {
                match server.accept() {
                    Ok(stream) => serve(stream, &state),
                    // Out of handles or the like: try again shortly.
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            })
            .map_err(|e| format!("cannot start the handover thread: {e}"))?;
        Ok(Handover {
            name,
            #[cfg(windows)]
            stop_channels,
        })
    }

    /// Name to pass in `VELT_DEV_SOCKET`.
    pub fn name(&self) -> &OsStr {
        &self.name
    }

    /// Ask the program with process id `pid` to stop over its stop channel; whether it has one
    /// (a program that never listened has none and is killed instead, like SIGTERM's default).
    #[cfg(windows)]
    pub fn request_stop(&self, pid: u32) -> bool {
        let channel = self
            .stop_channels
            .lock()
            .ok()
            .and_then(|mut c| c.remove(&pid));
        channel.is_some_and(|stream| handover::reply_stop(&stream).is_ok())
    }

    /// Drop the stop channel of a program that exited by itself.
    #[cfg(windows)]
    pub fn forget(&self, pid: u32) {
        if let Ok(mut channels) = self.stop_channels.lock() {
            channels.remove(&pid);
        }
    }
}

/// Handle one request.
fn serve(stream: Stream, state: &State) {
    match handover::read_request(&stream) {
        Ok(Request::Listen(addr)) => hand_over(&stream, &addr, &state.sockets),
        Ok(Request::Built { ok, files }) => {
            let _ = state.reports.send(Built { ok, files, stream });
        }
        Ok(Request::WatchStop) => keep_stop_channel(stream, &state.stop_channels),
        Err(e) => {
            let _ = handover::reply_err(&stream, &e.to_string());
        }
    }
}

/// Answer a listen request: the socket bound earlier for this address, or a new one.
fn hand_over(stream: &Stream, addr: &str, sockets: &Sockets) {
    let Ok(mut sockets) = sockets.lock() else {
        return;
    };
    if !sockets.contains_key(addr) {
        match TcpListener::bind(addr) {
            Ok(listener) => {
                sockets.insert(addr.to_string(), listener);
            }
            Err(e) => {
                let _ = handover::reply_err(stream, &e.to_string());
                return;
            }
        }
    }
    let _ = handover::reply_ok(stream, &sockets[addr]);
}

/// Keep a program's stop channel until the supervisor stops that program.
#[cfg(windows)]
fn keep_stop_channel(stream: Stream, channels: &StopChannels) {
    let Ok(pid) = handover::peer_pid(&stream) else {
        return;
    };
    if handover::reply_watching(&stream).is_ok() {
        if let Ok(mut channels) = channels.lock() {
            channels.insert(pid, stream);
        }
    }
}

/// Unix stops programs with SIGTERM; a stop channel is refused.
#[cfg(unix)]
fn keep_stop_channel(stream: Stream, _: &StopChannels) {
    let _ = handover::reply_err(&stream, "stop channels are for Windows (SIGTERM here)");
}
