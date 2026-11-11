//! The listener of the loopback flow: `127.0.0.1` on a free port, answering only
//! `GET /callback?…&state=<its state>` (compared in constant time) once; every other request
//! gets a 404 and is otherwise ignored, so a stray tab or another page can't end the sign-in.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velt_http::{Request, Response, Server};

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

/// A listener waiting for one callback.
pub struct Listener {
    server: Server,
    registry: String,
    answers: Receiver<Callback>,
}

impl Listener {
    /// Listens on `127.0.0.1`, any free port, for the callback carrying `state`; `registry` is
    /// named on the page the browser shows.
    pub fn bind(state: &str, registry: &str) -> Result<Listener, String> {
        let socket = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("cannot listen on 127.0.0.1 for the browser: {e}"))?;
        let (send, answers) = channel();
        let handler = Handler {
            state: state.to_string(),
            registry: registry.to_string(),
            send: Mutex::new(Some(send)),
        };
        let handler = Arc::new(move |req: Request| handler.answer(&req));
        let server = Server::start(socket, handler, 0)
            .map_err(|e| format!("cannot listen on 127.0.0.1 for the browser: {e}"))?;
        Ok(Listener {
            server,
            registry: registry.to_string(),
            answers,
        })
    }

    /// The port the browser is sent back to.
    pub fn port(&self) -> u16 {
        self.server.addr().port()
    }

    /// The callback, waiting at most `timeout`; stops listening either way.
    pub fn wait(self, timeout: Duration) -> Result<Callback, String> {
        let got = self.answers.recv_timeout(timeout);
        self.server.stop();
        match got {
            Ok(callback) => Ok(callback),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Err(format!(
                "the browser did not come back within {} minutes: run `velt login {} --device` to sign in without a browser on this machine",
                timeout.as_secs().div_ceil(60),
                self.registry
            )),
        }
    }
}

struct Handler {
    state: String,
    registry: String,
    /// Taken by the first matching callback: later ones get a 404.
    send: Mutex<Option<Sender<Callback>>>,
}

impl Handler {
    fn answer(&self, req: &Request) -> Response {
        let not_found =
            || Response::text(404, "not found").with_header("Cache-Control", "no-store");
        if req.method != "GET" || req.path != "/callback" {
            return not_found();
        }
        let state = query_value(&req.query, "state").unwrap_or_default();
        if !same(state.as_bytes(), self.state.as_bytes()) {
            return not_found();
        }
        let callback = match (
            query_value(&req.query, "code"),
            query_value(&req.query, "error"),
        ) {
            (Some(code), _) if !code.is_empty() && code.len() <= MAX_CODE => Callback::Code(code),
            (_, Some(word)) => Callback::Refused(Refusal::from_word(&word)),
            _ => return not_found(),
        };
        let Some(send) = self.send.lock().ok().and_then(|mut s| s.take()) else {
            return not_found();
        };
        let signed_in = matches!(callback, Callback::Code(_));
        let _ = send.send(callback);
        page(signed_in, &self.registry)
    }
}

/// The page the browser shows after the callback.
fn page(signed_in: bool, registry: &str) -> Response {
    let text = if signed_in {
        format!(
            "Signed in to {}. You can close this tab and return to the terminal.",
            escape(registry)
        )
    } else {
        "Signing in failed: see the terminal.".to_string()
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
