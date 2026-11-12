//! Signing in to a registry through the browser (`velt login`, registry protocol v1), so the
//! user never copies a token by hand:
//!
//! | Request | Answer |
//! |---|---|
//! | `GET  <api>/api/v1/login` | 200 `{"browser": "<page>", "device": true}` when the registry signs in through the browser ([`discover`]); anything else: paste a token as before |
//! | `POST <api>/api/v1/login/token` | `{"code", "verifier"}` → 200 `{"token", "user", "expires"}`, or 400 `{"error": "invalid_grant"}` ([`exchange`]) |
//! | `POST <api>/api/v1/login/device` | starts the device flow ([`device`], RFC 8628 field names) |
//! | `POST <api>/api/v1/login/device/token` | `{"device_code"}` → the token, or 400 `authorization_pending` / `slow_down` / `access_denied` / `expired_token` |
//!
//! The default, [`browser`], is the loopback flow: the CLI listens on `127.0.0.1` (any free port,
//! [`loopback`]), opens `<page>?port=…&challenge=…&state=…`, and the registry sends the browser
//! back to `http://127.0.0.1:<port>/callback?code=…&state=…`. The code is worth a token only
//! together with the [`Pkce`] verifier, which never leaves this process; the `state` keeps other
//! pages from feeding the listener a code of their own. [`device`] (`velt login --device`) signs
//! in from a browser on another machine with a short code.
//!
//! Nothing the registry or the browser sends reaches the terminal except a user name, a date
//! and a device code that pass a strict character check ([`display_safe`]), and the URLs to
//! open ([`is_openable`]); the token is stored by [`crate::credentials`], never printed.

pub mod device;
pub mod loopback;

#[cfg(test)]
mod tests;

use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// How long the loopback flow waits for the browser before suggesting `--device`.
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What the registry's `GET /api/v1/login` offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    /// The page the loopback flow opens (`https://`, or `http://` on this machine).
    pub browser: String,
    /// Whether the device flow is offered.
    pub device: bool,
}

/// A finished sign-in. `Debug` never prints the token.
#[derive(Clone, PartialEq, Eq)]
pub struct SignedIn {
    /// The registry token, for [`crate::credentials::login`].
    pub token: String,
    /// The user it belongs to.
    pub user: String,
    /// The day it stops working (`YYYY-MM-DD`).
    pub expires: String,
}

impl std::fmt::Debug for SignedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedIn")
            .field("token", &"…")
            .field("user", &self.user)
            .field("expires", &self.expires)
            .finish()
    }
}

/// What a sign-in shows and opens: the terminal and the browser, or a test playing both.
pub trait Ui {
    /// Opens `url` in a browser; whether one was started.
    fn open(&mut self, url: &str) -> bool;
    /// Shows a line to the user.
    fn say(&mut self, line: &str);
    /// Waits `d` between two polls of the device flow.
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

fn endpoint(api: &str, rest: &str) -> String {
    format!("{}/api/v1/login{rest}", api.trim_end_matches('/'))
}

/// The registry's browser sign-in; `Ok(None)` when it has none (any answer but a 200 with a
/// `browser` page, e.g. `velt registry serve`): the token is pasted then. `Err` only when the
/// registry can't be reached.
pub fn discover(api: &str) -> Result<Option<Discovery>, String> {
    #[derive(Deserialize)]
    struct Answer {
        browser: Option<String>,
        #[serde(default)]
        device: bool,
    }
    let resp = crate::remote::fetch_within(
        "GET",
        &endpoint(api, ""),
        &[("Accept", "application/json")],
        b"",
        small(),
    )?;
    if resp.status != 200 {
        return Ok(None);
    }
    let Ok(Answer {
        browser: Some(browser),
        device,
    }) = serde_json::from_slice(&resp.body)
    else {
        return Ok(None);
    };
    if !is_openable(&browser) || browser.contains('#') {
        return Err(format!(
            "registry {api} names a sign-in page that can't be opened (it must be an https:// URL without `#`); ask its administrator, or paste a token with `velt login {api} --token-stdin`"
        ));
    }
    Ok(Some(Discovery { browser, device }))
}

/// Answers of the login requests are small JSON documents.
fn small() -> velt_http::Limits {
    velt_http::Limits {
        total: Duration::from_secs(60),
        max_body: 64 << 10,
        ..velt_http::Limits::DEFAULT
    }
}

/// The secrets of one loopback sign-in (PKCE S256, RFC 7636, and the `state`). `Debug` never
/// prints the verifier.
#[derive(Clone)]
pub struct Pkce {
    /// 32 random bytes in lowercase hex; sent only with the code, to the registry.
    pub verifier: String,
    /// base64url (no padding) of the verifier's SHA-256: goes into the browser's URL.
    pub challenge: String,
    /// 16 random bytes in lowercase hex: the callback must carry it back.
    pub state: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"…")
            .field("challenge", &self.challenge)
            .field("state", &self.state)
            .finish()
    }
}

impl Pkce {
    /// Fresh random secrets.
    pub fn new() -> Result<Pkce, String> {
        let verifier = random_hex::<32>()?;
        Ok(Pkce {
            challenge: challenge(&verifier),
            verifier,
            state: random_hex::<16>()?,
        })
    }
}

fn random_hex<const N: usize>() -> Result<String, String> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("cannot make a random secret: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The S256 challenge of `verifier`: base64url without padding of its SHA-256.
pub fn challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// The page the loopback flow opens: `<browser>?port=…&challenge=…&state=…` (`&` when the page
/// already has a query).
pub fn browser_url(browser: &str, port: u16, pkce: &Pkce) -> String {
    let join = if browser.contains('?') { '&' } else { '?' };
    format!(
        "{browser}{join}port={port}&challenge={}&state={}",
        pkce.challenge, pkce.state
    )
}

/// Whether `url` may be handed to a browser and shown: `https://`, or `http://` on this
/// machine, without spaces, quotes or control characters.
pub fn is_openable(url: &str) -> bool {
    url.len() <= 2048
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`'))
        && velt_http::is_tls_or_loopback(url)
        && velt_http::url_host(url).is_ok()
}

/// Whether text from the registry may be printed as is: 1 to 64 ASCII letters, digits, `-`,
/// `_` and `.` (no escape sequences, nothing that looks like a message).
pub fn display_safe(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The loopback flow: listen on `127.0.0.1`, open the browser at the registry's page, wait up to
/// `timeout` for its callback, then [`exchange`] the code.
pub fn browser(
    api: &str,
    found: &Discovery,
    ui: &mut dyn Ui,
    timeout: Duration,
) -> Result<SignedIn, String> {
    let pkce = Pkce::new()?;
    let mut listener = loopback::Listener::bind(&pkce.state, api)?;
    let url = browser_url(&found.browser, listener.port(), &pkce);
    ui.say(&format!(
        "Opening {url} to sign in (or run `velt login {api} --device` to sign in without a browser on this machine)"
    ));
    if !ui.open(&url) {
        ui.say("Could not start a browser: open the URL above yourself.");
    }
    let signed = match listener.wait(timeout)? {
        loopback::Callback::Code(code) => exchange(api, &code, &pkce.verifier),
        loopback::Callback::Refused(word) => Err(word.message(api)),
    };
    // The tab shows the real outcome: signed in only once the token is here.
    listener.finish(signed.as_ref().ok().map(|s| s.user.as_str()));
    signed
}

/// `POST <api>/api/v1/login/token`: the token for a callback's `code`.
pub fn exchange(api: &str, code: &str, verifier: &str) -> Result<SignedIn, String> {
    let body = serde_json::json!({ "code": code, "verifier": verifier }).to_string();
    match post(api, "/token", &body)? {
        Answer::Token(signed) => Ok(signed),
        Answer::Error(e) if e == "invalid_grant" => Err(format!(
            "registry {api} did not accept the sign-in (it expired, was used already, or came from another `velt login`): run `velt login {api}` again"
        )),
        Answer::Error(e) => Err(format!(
            "registry {api} refused the sign-in ({}): run `velt login {api}` again",
            shown(&e)
        )),
    }
}

/// An answer of the token requests.
enum Answer {
    Token(SignedIn),
    /// The `error` word of a 400 (or `HTTP <status>` for other answers).
    Error(String),
}

/// POSTs a JSON `body` to `<api>/api/v1/login<rest>`. The answer's text is never quoted (it may
/// carry a token); only its `error` word, through [`shown`].
fn post(api: &str, rest: &str, body: &str) -> Result<Answer, String> {
    crate::credentials::check_transport(api, "the sign-in")?;
    let resp = crate::remote::fetch_within(
        "POST",
        &endpoint(api, rest),
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
        ],
        body.as_bytes(),
        small(),
    )?;
    #[derive(Deserialize)]
    struct Token {
        token: String,
        user: String,
        expires: String,
    }
    #[derive(Deserialize)]
    struct Error {
        error: String,
    }
    match resp.status {
        200 => {
            let Ok(t) = serde_json::from_slice::<Token>(&resp.body) else {
                return Err(format!(
                    "registry {api} answered the sign-in with something other than a token"
                ));
            };
            // Checked here so the message names the registry's answer (credentials::login's
            // speaks of a pasted token); the token itself is never quoted.
            if t.token.is_empty()
                || t.token.len() > 4096
                || t.token.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(format!(
                    "registry {api} answered the sign-in with a token that is empty, too long or contains spaces or control characters; nothing was stored"
                ));
            }
            if !display_safe(&t.user) || !display_safe(&t.expires) {
                return Err(format!(
                    "registry {api} answered the sign-in with an invalid user name or date"
                ));
            }
            Ok(Answer::Token(SignedIn {
                token: t.token,
                user: t.user,
                expires: t.expires,
            }))
        }
        400 => match serde_json::from_slice::<Error>(&resp.body) {
            Ok(e) => Ok(Answer::Error(e.error)),
            Err(_) => Ok(Answer::Error("HTTP 400".into())),
        },
        s => Ok(Answer::Error(format!("HTTP {s}"))),
    }
}

/// An `error` word for the terminal: as is when [`display_safe`], else a placeholder.
fn shown(word: &str) -> &str {
    if display_safe(word) || word.starts_with("HTTP ") && display_safe(&word[5..]) {
        word
    } else {
        "an unknown error"
    }
}
