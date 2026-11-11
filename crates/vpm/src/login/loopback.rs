//! The listener of the loopback flow: `127.0.0.1` on a free port, answering only
//! `GET /callback?…&state=<its state>` (compared in constant time) once; every other request
//! gets a 404 and is otherwise ignored, so a stray tab or another page can't end the sign-in.
//! The callback's tab is answered only when the code has been exchanged, with the real outcome
//! ([`Listener::finish`]).

use std::io::{BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use velt_http::message::{read_request, write_response};
use velt_http::{Request, Response};

/// What the browser brought back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callback {
    /// `code=…`: exchange it for the token.
    Code(String),
    /// `error=…`: the sign-in did not happen.
    Refused(Refusal),
}

/// The `error` words of a callback. Only these fixed messages reach the terminal, never text
/// from the URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `access_denied`: the user cancelled.
    AccessDenied,
    /// `refused`: the registry won't sign this account in (its page said why).
    Refused,
    /// `server_error`.
    ServerError,
    /// Any other word.
    Unknown,
}

impl Refusal {
    fn from_word(word: &str) -> Refusal {
        match word {
            "access_denied" => Refusal::AccessDenied,
            "refused" => Refusal::Refused,
            "server_error" => Refusal::ServerError,
            _ => Refusal::Unknown,
        }
    }

    /// The terminal's message for a sign-in at the registry `api`.
    pub fn message(self, api: &str) -> String {
        match self {
            Refusal::AccessDenied => "the sign-in was cancelled in the browser".to_string(),
            Refusal::Refused => format!(
                "registry {api} refused to sign this account in; its page in the browser says why"
            ),
            Refusal::ServerError => {
                format!("registry {api} failed to sign you in (a server error): try again later")
            }
            Refusal::Unknown => format!("registry {api} did not sign you in"),
        }
    }
}

/// The longest `code` accepted.
const MAX_CODE: usize = 512;
/// How long a connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the callback's tab waits for the outcome (the exchange takes at most a minute).
const OUTCOME_TIMEOUT: Duration = Duration::from_secs(120);
/// How long [`Listener::finish`] waits for the tab's page to be written.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Connections served at once; more are closed unanswered (only this machine can connect).
pub(super) const MAX_CONNECTIONS: usize = 32;

/// The matching callback's connection, waiting for the outcome: the signed-in user (`None`:
/// failed), then reporting that its page was written.
struct Waiting {
    outcome: Sender<Option<String>>,
    written: Receiver<()>,
}

/// A listener waiting for one callback. It answers the callback's tab only when
/// [`Listener::finish`] gives the outcome, and keeps answering 404 to other requests until then.
pub struct Listener {
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    registry: String,
    arrived: Receiver<(Callback, Waiting)>,
    waiting: Option<Waiting>,
}

impl Listener {
    /// Listens on `127.0.0.1`, any free port, for the callback carrying `state`; `registry` is
    /// named on the page the browser shows.
    pub fn bind(state: &str, registry: &str) -> Result<Listener, String> {
        let failed = |e: std::io::Error| format!("cannot listen on 127.0.0.1 for the browser: {e}");
        let socket = TcpListener::bind("127.0.0.1:0").map_err(failed)?;
        let addr = socket.local_addr().map_err(failed)?;
        let (send, arrived) = channel();
        let handler = Arc::new(Handler {
            state: state.to_string(),
            registry: registry.to_string(),
            send: Mutex::new(Some(send)),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let open = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::Builder::new()
            .name("velt-login-listener".into())
            .spawn(move || {
                for conn in socket.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    let (Ok(stream), handler) = (conn, handler.clone()) else {
                        continue;
                    };
                    if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        open.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    let served = open.clone();
                    let spawned = std::thread::Builder::new()
                        .name("velt-login-conn".into())
                        .spawn(move || {
                            handler.serve(stream);
                            served.fetch_sub(1, Ordering::SeqCst);
                        });
                    if spawned.is_err() {
                        open.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })
            .map_err(failed)?;
        Ok(Listener {
            addr,
            stop,
            thread: Some(thread),
            registry: registry.to_string(),
            arrived,
            waiting: None,
        })
    }

    /// The port the browser is sent back to.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The callback, waiting at most `timeout`. Its tab waits for [`Listener::finish`].
    pub fn wait(&mut self, timeout: Duration) -> Result<Callback, String> {
        match self.arrived.recv_timeout(timeout) {
            Ok((callback, waiting)) => {
                self.waiting = Some(waiting);
                Ok(callback)
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Err(format!(
                "the browser did not come back within {} minutes: run `velt login {} --device` to sign in without a browser on this machine",
                timeout.as_secs().div_ceil(60),
                self.registry
            )),
        }
    }

    /// Answers the callback's tab with the outcome (`Some(user)`: signed in), waits until its
    /// page is written, and stops listening.
    pub fn finish(mut self, user: Option<&str>) {
        if let Some(waiting) = self.waiting.take() {
            if waiting.outcome.send(user.map(str::to_string)).is_ok() {
                let _ = waiting.written.recv_timeout(WRITE_TIMEOUT);
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept with a connection of our own.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Handler {
    state: String,
    registry: String,
    /// Taken by the first matching callback: later ones get a 404.
    send: Mutex<Option<Sender<(Callback, Waiting)>>>,
}

impl Handler {
    /// Reads one request and answers it: the matching callback once the outcome is known.
    fn serve(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let Ok(mut writer) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let Ok(req) = read_request(&mut reader, 0) else {
            return;
        };
        let (response, written) = match self.callback(&req) {
            None => (not_found(), None),
            Some(callback) => {
                let (outcome, told) = channel();
                let (wrote, written) = channel();
                let waiting = Waiting { outcome, written };
                match self.send.lock().ok().and_then(|mut s| s.take()) {
                    Some(send) if send.send((callback, waiting)).is_ok() => {
                        let user = told.recv_timeout(OUTCOME_TIMEOUT).ok().flatten();
                        (page(user.as_deref(), &self.registry), Some(wrote))
                    }
                    _ => (not_found(), None),
                }
            }
        };
        let _ = write_response(&mut writer, &response);
        let _ = writer.flush();
        let _ = writer.shutdown(Shutdown::Write);
        if let Some(wrote) = written {
            let _ = wrote.send(());
        }
    }

    /// The request's callback when it is `GET /callback` with this state and a code or an error.
    fn callback(&self, req: &Request) -> Option<Callback> {
        if req.method != "GET" || req.path != "/callback" {
            return None;
        }
        let state = query_value(&req.query, "state").unwrap_or_default();
        if !same(state.as_bytes(), self.state.as_bytes()) {
            return None;
        }
        match (
            query_value(&req.query, "code"),
            query_value(&req.query, "error"),
        ) {
            (Some(code), _) if !code.is_empty() && code.len() <= MAX_CODE => {
                Some(Callback::Code(code))
            }
            (_, Some(word)) => Some(Callback::Refused(Refusal::from_word(&word))),
            _ => None,
        }
    }
}

fn not_found() -> Response {
    Response::text(404, "not found").with_header("Cache-Control", "no-store")
}

/// The page the callback's tab shows: signed in as `user`, or failed (`None`).
fn page(user: Option<&str>, registry: &str) -> Response {
    let text = match user {
        Some(user) => format!(
            "Signed in to {} as {}. You can close this tab and return to the terminal.",
            escape(registry),
            escape(user)
        ),
        None => "Signing in failed: see the terminal.".to_string(),
    };
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>velt login</title></head><body style=\"font-family:system-ui,sans-serif;margin:3em\"><p>{text}</p></body></html>"
    );
    Response::bytes(200, "text/html; charset=utf-8", html.into_bytes())
        .with_header("Cache-Control", "no-store")
        .with_header("Referrer-Policy", "no-referrer")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Equal bytes, compared without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && std::hint::black_box(a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y))) == 0
}

/// The first value of `name` in a query string, percent-decoded (`+` is a space); `None` when
/// absent or not UTF-8.
pub(super) fn query_value(query: &str, name: &str) -> Option<String> {
    let raw = query
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))?;
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let digit = |b: u8| (b as char).to_digit(16);
                let (hi, lo) = (digit(bytes.next()?)?, digit(bytes.next()?)?);
                out.push((hi * 16 + lo) as u8);
            }
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}
