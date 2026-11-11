//! The device flow (`velt login --device`, RFC 8628 field names): the registry hands out a short
//! code, the user enters it on the registry's page from any browser, and the CLI polls until the
//! sign-in is done, declined or expired.

use std::time::Duration;

use serde::Deserialize;

use super::{display_safe, is_openable, post, shown, Answer, SignedIn, Ui};

/// The interval when the registry names none (RFC 8628 §3.2).
const DEFAULT_INTERVAL: u64 = 5;
/// What `slow_down` adds to the interval.
const SLOW_DOWN: u64 = 5;
/// Bounds on what the registry asks for: at least a second between polls, at most an hour in all.
const MIN_INTERVAL: u64 = 1;
const MAX_EXPIRES: u64 = 3600;

/// A started device sign-in. `Debug` never prints the device code.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Started {
    /// The secret the CLI polls with.
    pub device_code: String,
    /// What the user enters (`XXXX-XXXX`).
    pub user_code: String,
    /// The page to enter it on.
    pub verification_uri: String,
    /// The page with the code filled in.
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// Seconds until the code expires.
    pub expires_in: u64,
    /// Seconds between polls.
    #[serde(default)]
    pub interval: Option<u64>,
}

impl std::fmt::Debug for Started {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Started")
            .field("device_code", &"…")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .finish_non_exhaustive()
    }
}

/// `POST <api>/api/v1/login/device`: a new device sign-in.
pub fn start(api: &str) -> Result<Started, String> {
    crate::credentials::check_transport(api, "the sign-in")?;
    let resp = velt_http::fetch_within(
        "POST",
        &super::endpoint(api, "/device"),
        &[("Accept", "application/json")],
        b"",
        super::small(),
    )?;
    if resp.status != 200 {
        return Err(format!(
            "registry {api} did not start a device sign-in (HTTP {})",
            resp.status
        ));
    }
    let invalid = || format!("registry {api} answered the device sign-in with an invalid start");
    let mut started: Started = serde_json::from_slice(&resp.body).map_err(|_| invalid())?;
    if !display_safe(&started.user_code)
        || !is_openable(&started.verification_uri)
        || started.device_code.is_empty()
    {
        return Err(invalid());
    }
    if started
        .verification_uri_complete
        .as_deref()
        .is_some_and(|u| !is_openable(u))
    {
        started.verification_uri_complete = None;
    }
    Ok(started)
}

/// One poll's answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    Done(SignedIn),
    /// `authorization_pending`.
    Pending,
    /// `slow_down`.
    SlowDown,
}

/// `POST <api>/api/v1/login/device/token`; declined and expired sign-ins are errors.
pub fn poll(api: &str, device_code: &str) -> Result<Poll, String> {
    let body = serde_json::json!({ "device_code": device_code }).to_string();
    match post(api, "/device/token", &body)? {
        Answer::Token(signed) => Ok(Poll::Done(signed)),
        Answer::Error(e) => match e.as_str() {
            "authorization_pending" => Ok(Poll::Pending),
            "slow_down" => Ok(Poll::SlowDown),
            "access_denied" => Err(format!(
                "the sign-in was declined, or registry {api} refused this account"
            )),
            "expired_token" => Err(expired(api)),
            _ => Err(format!(
                "registry {api} ended the device sign-in ({})",
                shown(&e)
            )),
        },
    }
}

fn expired(api: &str) -> String {
    format!("the device code expired before the sign-in was done: run `velt login {api} --device` again")
}

/// The device flow: start, show the code (and try to open the page), then poll every
/// `interval` seconds (`slow_down` adds 5) until done or `expires_in` seconds have been waited.
pub fn sign_in(api: &str, ui: &mut dyn Ui) -> Result<SignedIn, String> {
    let started = start(api)?;
    ui.say(&format!(
        "Open {} and enter the code {}",
        started.verification_uri, started.user_code
    ));
    if let Some(complete) = &started.verification_uri_complete {
        // A machine without a browser is why the device flow exists: failing to open is fine.
        ui.open(complete);
    }
    let mut interval = started
        .interval
        .unwrap_or(DEFAULT_INTERVAL)
        .max(MIN_INTERVAL);
    let expires_in = started.expires_in.min(MAX_EXPIRES);
    let mut waited = 0;
    loop {
        if waited >= expires_in {
            return Err(expired(api));
        }
        ui.sleep(Duration::from_secs(interval));
        waited += interval;
        match poll(api, &started.device_code)? {
            Poll::Done(signed) => return Ok(signed),
            Poll::Pending => {}
            Poll::SlowDown => interval += SLOW_DOWN,
        }
    }
}
