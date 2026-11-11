//! The sign-in flows against a stub registry; the tests play the browser (and the clock of
//! the device flow), so nothing waits for real time except the loopback timeout test.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velt_http::{Request, Response, Server};

use super::device;
use super::loopback::{query_value, Callback, Listener, Refusal};
use super::*;

/// The registry's side: codes issued for challenges, and the device flow's scripted answers.
#[derive(Default)]
struct Stub {
    /// `code` → the challenge it was issued for.
    codes: HashMap<String, String>,
    /// What the next device polls answer (`token` = success); `expired_token` when empty.
    polls: VecDeque<&'static str>,
    /// Device polls received.
    polled: usize,
    /// The `error` word the sign-in page sends the browser back with, instead of a code.
    refuse: Option<&'static str>,
    /// The sign-in page issues a code for another challenge (the exchange fails).
    wrong_challenge: bool,
    /// The callback URL the page sent the browser to; replayed while the code is exchanged.
    callback: Option<String>,
    /// The listener's answer to that replay.
    replayed: Option<u16>,
}

fn json(status: u16, value: serde_json::Value) -> Response {
    Response::bytes(status, "application/json", value.to_string().into_bytes())
}

fn token() -> Response {
    json(
        200,
        serde_json::json!({"token": "velt_s3cret", "user": "andii", "expires": "2027-11-11"}),
    )
}

/// A registry at `/` with browser sign-in, its page at `/login/cli`, and one at `/old` without
/// (404 for everything, like `velt registry serve`).
fn registry() -> (Server, String, Arc<Mutex<Stub>>) {
    let stub: Arc<Mutex<Stub>> = Arc::default();
    let s = stub.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let page = format!("{base}/login/cli");
    let here = base.clone();
    let handler = Arc::new(move |req: Request| {
        let mut stub = s.lock().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let field = |k: &str| body[k].as_str().unwrap_or_default().to_string();
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/api/v1/login") => {
                json(200, serde_json::json!({"browser": page, "device": true}))
            }
            // The sign-in page: the user signs in at once, and the browser goes back.
            ("GET", "/login/cli") => {
                let q = |k| query_value(&req.query, k).unwrap();
                let (port, state) = (q("port"), q("state"));
                let outcome = match stub.refuse {
                    Some(word) => format!("error={word}&error_description=EVIL%1b[31m"),
                    None => {
                        let issued = match stub.wrong_challenge {
                            true => challenge("another verifier"),
                            false => q("challenge"),
                        };
                        stub.codes.insert("c1".into(), issued);
                        "code=c1".into()
                    }
                };
                let to = format!("http://127.0.0.1:{port}/callback?{outcome}&state={state}");
                stub.callback = Some(to.clone());
                Response::text(302, "").with_header("Location", &to)
            }
            ("POST", "/api/v1/login/token") => {
                // The tab is still waiting for its page: a reload of the callback is ignored.
                if let Some(callback) = stub.callback.take() {
                    let replay = velt_http::fetch("GET", &callback, &[], b"").unwrap();
                    stub.replayed = Some(replay.status);
                }
                match stub.codes.remove(&field("code")) {
                    Some(c) if c == challenge(&field("verifier")) => token(),
                    _ => json(400, serde_json::json!({"error": "invalid_grant"})),
                }
            }
            ("POST", "/api/v1/login/device") => json(
                200,
                serde_json::json!({
                    "device_code": "d".repeat(64), "user_code": "WDJB-MJHT",
                    "verification_uri": format!("{here}/device"),
                    "verification_uri_complete": format!("{here}/device?code=WDJB-MJHT"),
                    "expires_in": 600, "interval": 5
                }),
            ),
            ("POST", "/api/v1/login/device/token") if field("device_code") == "d".repeat(64) => {
                stub.polled += 1;
                match stub.polls.pop_front().unwrap_or("expired_token") {
                    "token" => token(),
                    word => json(400, serde_json::json!({ "error": word })),
                }
            }
            _ => Response::text(404, "not found"),
        }
    });
    let server = Server::start(listener, handler, 1 << 16).unwrap();
    (server, base, stub)
}

/// Plays the browser: follows the sign-in page's redirect to the listener (after a few requests
/// the listener must ignore), and records what the terminal showed and how long it waited.
#[derive(Default)]
struct Browser {
    said: Vec<String>,
    opened: Vec<String>,
    slept: Vec<u64>,
    /// The listener's answers to the stray requests.
    answers: Vec<u16>,
    /// The callback's tab, waiting for its page.
    tab: Option<std::thread::JoinHandle<Response>>,
    /// Whether `open` follows the redirect (false: a machine without a browser).
    works: bool,
}

impl Ui for Browser {
    fn open(&mut self, url: &str) -> bool {
        self.opened.push(url.to_string());
        if !self.works {
            return false;
        }
        let page = velt_http::fetch("GET", url, &[], b"").unwrap();
        let to = page.header("location").unwrap().to_string();
        let (base, query) = to.split_once('?').unwrap();
        let port = base
            .trim_start_matches("http://127.0.0.1:")
            .split('/')
            .next()
            .unwrap();
        let listener = format!("http://127.0.0.1:{port}");
        let wrong_state = to.replace("&state=", "&state=0");
        for stray in [
            format!("{listener}/"),
            format!("{listener}/callback?{}", query.replace("state=", "x=")),
            wrong_state,
        ] {
            self.answers
                .push(velt_http::fetch("GET", &stray, &[], b"").unwrap().status);
        }
        // The tab gets its page only once the sign-in is over.
        self.tab = Some(std::thread::spawn(move || {
            velt_http::fetch("GET", &to, &[], b"").unwrap()
        }));
        true
    }

    fn say(&mut self, line: &str) {
        self.said.push(line.to_string());
    }

    fn sleep(&mut self, d: Duration) {
        self.slept.push(d.as_secs());
    }
}

impl Browser {
    /// The text of the page the callback's tab ended with.
    fn page(&mut self) -> String {
        let tab = self.tab.take().expect("a callback").join().unwrap();
        assert_eq!(tab.status, 200);
        tab.body_text()
    }
}

fn browser_ui() -> Browser {
    Browser {
        works: true,
        ..Browser::default()
    }
}

#[test]
fn a_registry_without_browser_sign_in_gets_a_pasted_token() {
    let (server, base, _) = registry();
    let found = discover(&base).unwrap().unwrap();
    assert_eq!(found.browser, format!("{base}/login/cli"));
    assert!(found.device);
    // 404 (`velt registry serve`): paste as before.
    assert_eq!(discover(&format!("{base}/old")).unwrap(), None);
    server.stop();
    // Nothing listening: an error, which `velt login` turns into the paste prompt.
    assert!(discover(&base).is_err());
}

#[test]
fn the_loopback_flow_signs_in_with_the_code_and_the_verifier() {
    let (server, base, stub) = registry();
    let found = discover(&base).unwrap().unwrap();
    let mut ui = browser_ui();
    let signed = browser(&base, &found, &mut ui, CALLBACK_TIMEOUT).unwrap();
    assert_eq!(
        (
            signed.token.as_str(),
            signed.user.as_str(),
            signed.expires.as_str()
        ),
        ("velt_s3cret", "andii", "2027-11-11")
    );
    assert!(!format!("{signed:?}").contains("s3cret"));
    // Wrong path, no state, wrong state: 404; the callback's replay: 404.
    assert_eq!(ui.answers, [404, 404, 404]);
    assert_eq!(stub.lock().unwrap().replayed, Some(404));
    let page = ui.page();
    assert!(
        page.contains(&format!(
            "Signed in to {base} as andii. You can close this tab"
        )),
        "{page}"
    );
    let opened = &ui.opened[0];
    assert!(
        opened.starts_with(&format!("{base}/login/cli?port=")),
        "{opened}"
    );
    let state = query_value(opened.split_once('?').unwrap().1, "state").unwrap();
    assert_eq!(state.len(), 32);
    assert!(
        ui.said[0].contains(opened) && ui.said[0].contains("--device"),
        "{:?}",
        ui.said
    );
    // The code was used: exchanging it again is refused.
    assert!(stub.lock().unwrap().codes.is_empty());
    server.stop();
}

#[test]
fn refusals_print_fixed_messages_and_bad_codes_are_refused() {
    let (server, base, stub) = registry();
    let found = discover(&base).unwrap().unwrap();
    for (word, message) in [
        ("access_denied", "cancelled in the browser"),
        ("refused", "refused to sign this account in"),
        ("server_error", "a server error"),
        ("%3Cscript%3E", "did not sign you in"),
    ] {
        stub.lock().unwrap().refuse = Some(word);
        let mut ui = browser_ui();
        let e = browser(&base, &found, &mut ui, CALLBACK_TIMEOUT).unwrap_err();
        assert!(e.contains(message), "{word}: {e}");
        assert!(!e.contains("EVIL") && !e.contains("script"), "{e}");
        assert!(ui.page().contains("Signing in failed: see the terminal."));
    }
    stub.lock().unwrap().refuse = None;
    // A code the exchange refuses (`invalid_grant`): the tab says it failed, not signed in.
    stub.lock().unwrap().wrong_challenge = true;
    let mut ui = browser_ui();
    let e = browser(&base, &found, &mut ui, CALLBACK_TIMEOUT).unwrap_err();
    assert!(e.contains("did not accept the sign-in"), "{e}");
    let page = ui.page();
    assert!(
        page.contains("Signing in failed: see the terminal."),
        "{page}"
    );
    stub.lock().unwrap().wrong_challenge = false;
    // A code with another verifier (or an unknown one) is worth nothing.
    assert!(exchange(&base, "c1", "verifier")
        .unwrap_err()
        .contains("did not accept the sign-in"));
    let pkce = Pkce::new().unwrap();
    stub.lock()
        .unwrap()
        .codes
        .insert("c2".into(), pkce.challenge.clone());
    let e = exchange(&base, "c2", &Pkce::new().unwrap().verifier).unwrap_err();
    assert!(e.contains("did not accept the sign-in"), "{e}");
    server.stop();
}

#[test]
fn the_listener_gives_up_after_its_timeout() {
    let mut listener = Listener::bind("ab", "https://registry.example.com").unwrap();
    // timing: the timeout is what this test checks.
    let e = listener.wait(Duration::from_millis(20)).unwrap_err();
    assert!(
        e.contains("velt login https://registry.example.com --device"),
        "{e}"
    );
    // A refused callback only ends the wait when its state matches; its tab waits for the
    // outcome.
    let mut listener = Listener::bind("ab", "r").unwrap();
    let url = format!("http://127.0.0.1:{}/callback", listener.port());
    let other = velt_http::fetch("GET", &format!("{url}?error=refused&state=a"), &[], b"").unwrap();
    assert_eq!(other.status, 404);
    let ours = format!("{url}?error=refused&state=ab");
    let tab = std::thread::spawn(move || velt_http::fetch("GET", &ours, &[], b"").unwrap());
    assert_eq!(
        listener.wait(CALLBACK_TIMEOUT).unwrap(),
        Callback::Refused(Refusal::Refused)
    );
    listener.finish(None);
    assert!(tab
        .join()
        .unwrap()
        .body_text()
        .contains("Signing in failed"));
}

#[test]
fn the_device_flow_polls_at_the_interval_and_slows_down() {
    let (server, base, stub) = registry();
    stub.lock().unwrap().polls = ["authorization_pending", "slow_down", "token"].into();
    let mut ui = Browser::default();
    let signed = device::sign_in(&base, &mut ui).unwrap();
    assert_eq!(signed.user, "andii");
    assert_eq!(ui.slept, [5, 5, 10]);
    assert_eq!(
        ui.said[0],
        format!("Open {base}/device and enter the code WDJB-MJHT")
    );
    assert_eq!(ui.opened, [format!("{base}/device?code=WDJB-MJHT")]);
    assert_eq!(stub.lock().unwrap().polled, 3);
    server.stop();
}

#[test]
fn the_device_flow_ends_when_the_code_expires_or_is_declined() {
    let (server, base, stub) = registry();
    stub.lock().unwrap().polls = ["authorization_pending"].into();
    let e = device::sign_in(&base, &mut Browser::default()).unwrap_err();
    assert!(
        e.contains("device code expired") && e.contains("--device"),
        "{e}"
    );
    stub.lock().unwrap().polls = ["access_denied"].into();
    let e = device::sign_in(&base, &mut Browser::default()).unwrap_err();
    assert!(e.contains("declined"), "{e}");
    // A registry that keeps answering `authorization_pending` is given up on at `expires_in`.
    stub.lock().unwrap().polls = std::iter::repeat_n("authorization_pending", 1000).collect();
    let mut ui = Browser::default();
    let e = device::sign_in(&base, &mut ui).unwrap_err();
    assert!(e.contains("device code expired"), "{e}");
    assert_eq!(ui.slept.iter().sum::<u64>(), 600);
    server.stop();
}

#[test]
fn pkce_secrets_and_urls() {
    // base64url without padding, and the S256 challenge (checked against Python's hashlib).
    for (bytes, text) in [
        (&b"f"[..], "Zg"),
        (b"fo", "Zm8"),
        (b"foo", "Zm9v"),
        (b"foobar", "Zm9vYmFy"),
        (&[0xfb, 0xff], "-_8"),
    ] {
        assert_eq!(base64url(bytes), text);
    }
    assert_eq!(
        challenge(&"a".repeat(64)),
        "_-BU_nrgy23GXDr5th1SCfQ5hR20PQulmXM33xVGaOs"
    );
    let (a, b) = (Pkce::new().unwrap(), Pkce::new().unwrap());
    assert_eq!(
        (a.verifier.len(), a.state.len(), a.challenge.len()),
        (64, 32, 43)
    );
    assert!(a
        .verifier
        .bytes()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_ne!(a.verifier, b.verifier);
    assert!(!format!("{a:?}").contains(&a.verifier));
    let url = browser_url("https://veltpm.com/login/cli", 4321, &a);
    assert_eq!(
        url,
        format!(
            "https://veltpm.com/login/cli?port=4321&challenge={}&state={}",
            a.challenge, a.state
        )
    );
    assert!(browser_url("https://x.example/l?v=1", 1, &a)
        .starts_with("https://x.example/l?v=1&port=1&"));
    for bad in [
        "http://veltpm.com/login",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://a b",
        "https://x/\u{1b}[31m",
    ] {
        assert!(!is_openable(bad), "{bad}");
    }
    assert!(is_openable("https://veltpm.com/login/cli") && is_openable("http://127.0.0.1:8/x"));
    assert_eq!(
        query_value("a=1&state=x%2By+z&state=2", "state").as_deref(),
        Some("x+y z")
    );
    assert_eq!(query_value("xstate=1", "state"), None);
    assert_eq!(query_value("state=%zz", "state"), None);
    assert!(display_safe("WDJB-MJHT") && !display_safe("a\u{1b}[2J") && !display_safe(""));
}
