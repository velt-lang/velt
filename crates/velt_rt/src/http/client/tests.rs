//! The client against a scripted HTTP/1.1 server: redirects (modes, method changes, headers
//! dropped across origins, the limit), reason phrases, bodies in several frames, and aborting.

use super::send::{self, Outgoing};
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the test server saw of one request: `"METHOD /path"` and the lowercased header lines.
#[derive(Debug, Clone)]
struct Seen {
    line: String,
    headers: Vec<String>,
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// A server answering each request with `respond(path, port)` (the raw response bytes); one
/// connection per request (`connection: close` in every answer).
async fn server(respond: fn(&str, u16) -> String) -> (u16, Log) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let log: Log = Arc::default();
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![];
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let mut lines = head.split("\r\n");
                let first = lines.next().unwrap_or_default();
                let mut parts = first.split(' ');
                let line = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
                let path = line.split(' ').nth(1).unwrap().to_string();
                let headers = lines
                    .take_while(|l| !l.is_empty())
                    .map(|l| l.to_lowercase())
                    .collect();
                seen.lock().unwrap().push(Seen { line, headers });
                let _ = s.write_all(respond(&path, port).as_bytes()).await;
            });
        }
    });
    (port, log)
}

fn answer(status: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nconnection: close\r\ncontent-length: {}\r\n{extra}\r\n{body}",
        body.len()
    )
}

fn get(url: &str, headers: &[&str]) -> Outgoing {
    let flat: Vec<&[u8]> = headers.iter().map(|h| h.as_bytes()).collect();
    Outgoing {
        method: Method::GET,
        url: send::parse_url(url).unwrap(),
        headers: send::header_map(&flat).unwrap(),
        body: Bytes::new(),
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn redirects(path: &str, port: u16) -> String {
    match path {
        "/a" => answer("302 Found", "location: /b\r\n", ""),
        "/b" => answer("200 OK", "", "at b"),
        "/post" => answer("301 Moved Permanently", "location: /b\r\n", ""),
        "/keep" => answer("307 Temporary Redirect", "location: /b\r\n", ""),
        "/other" => answer(
            "302 Found",
            &format!("location: http://localhost:{port}/b\r\n"),
            "",
        ),
        "/loop" => answer("302 Found", "location: /loop\r\n", ""),
        "/teapot" => answer("418 Teapot Time", "", "short and stout"),
        "/chunked" => "HTTP/1.1 200 OK\r\nconnection: close\r\ntransfer-encoding: chunked\r\n\r\n\
                       3\r\nabc\r\n4\r\ndefg\r\n0\r\n\r\n"
            .into(),
        _ => answer("404 Not Found", "", ""),
    }
}

async fn fetch(o: Outgoing, mode: Redirect) -> Result<FetchResp, VeltErr> {
    run(vec![], Ok(o), mode, None).await
}

fn message(e: VeltErr) -> String {
    let mut m = e.message;
    // SAFETY: an owned message string.
    let s = unsafe { m.to_string_lossy() };
    unsafe { m.release() };
    s
}

#[test]
fn follows_redirects_like_the_fetch_standard() {
    rt().block_on(async {
        let (port, log) = server(redirects).await;
        let base = format!("http://127.0.0.1:{port}");
        let r = fetch(get(&format!("{base}/a#frag"), &[]), Redirect::Follow)
            .await
            .unwrap();
        assert_eq!((r.status, r.redirected), (200, true));
        assert_eq!(r.url, format!("{base}/b"));

        let mut post = get(&format!("{base}/post"), &["content-type", "text/plain"]);
        post.method = Method::POST;
        post.body = Bytes::from_static(b"x");
        fetch(post, Redirect::Follow).await.unwrap();
        let mut keep = get(&format!("{base}/keep"), &[]);
        keep.method = Method::PUT;
        fetch(keep, Redirect::Follow).await.unwrap();
        let lines: Vec<String> = log.lock().unwrap().iter().map(|s| s.line.clone()).collect();
        assert_eq!(
            lines,
            [
                "GET /a",
                "GET /b",
                "POST /post",
                "GET /b",
                "PUT /keep",
                "PUT /b"
            ]
        );
        let seen = log.lock().unwrap().clone();
        assert!(seen[2].headers.contains(&"content-type: text/plain".into()));
        assert!(!seen[3]
            .headers
            .iter()
            .any(|h| h.starts_with("content-type")));
        assert!(seen[0].headers.contains(&"user-agent: velt".into()));
        assert!(seen[0].headers.contains(&"accept: */*".into()));
    });
}

#[test]
fn redirect_modes_and_limit() {
    rt().block_on(async {
        let (port, log) = server(redirects).await;
        let base = format!("http://127.0.0.1:{port}");
        let r = fetch(get(&format!("{base}/a"), &[]), Redirect::Manual)
            .await
            .unwrap();
        assert_eq!((r.status, r.redirected), (302, false));
        let e = fetch(get(&format!("{base}/a"), &[]), Redirect::Error)
            .await
            .err()
            .unwrap();
        assert_eq!(message(e), "fetch failed: unexpected redirect");
        let before = log.lock().unwrap().len();
        let e = fetch(get(&format!("{base}/loop"), &[]), Redirect::Follow)
            .await
            .err()
            .unwrap();
        assert_eq!(message(e), "fetch failed: redirect count exceeded");
        assert_eq!(log.lock().unwrap().len() - before, 21);
    });
}

#[test]
fn credentials_stay_on_their_origin() {
    rt().block_on(async {
        let (port, log) = server(redirects).await;
        let url = format!("http://127.0.0.1:{port}/other");
        let o = get(
            &url,
            &[
                "authorization",
                "Bearer t",
                "x-keep",
                "1",
                "host",
                "app.test",
            ],
        );
        let r = fetch(o, Redirect::Follow).await.unwrap();
        assert_eq!(r.url, format!("http://localhost:{port}/b"));
        let seen = log.lock().unwrap().clone();
        assert!(seen[0].headers.contains(&"authorization: bearer t".into()));
        assert!(!seen[1]
            .headers
            .iter()
            .any(|h| h.starts_with("authorization")));
        assert!(seen[1].headers.contains(&"x-keep: 1".into()));
        // A `host` the request set is the first origin's: the next hop names its own.
        assert!(seen[0].headers.contains(&"host: app.test".into()));
        assert!(seen[1].headers.contains(&format!("host: localhost:{port}")));
    });
}

#[test]
fn reason_phrase_and_bodies() {
    rt().block_on(async {
        let (port, _) = server(redirects).await;
        let base = format!("http://127.0.0.1:{port}");
        let r = fetch(get(&format!("{base}/teapot"), &[]), Redirect::Follow)
            .await
            .unwrap();
        assert_eq!((r.status, &*r.status_text), (418, "Teapot Time"));
        let r = Arc::new(r);
        let w = receive(r.clone()).await.unwrap();
        assert_eq!(w.as_slice(), b"short and stout");
        let again = receive(r).await.err().unwrap();
        assert_eq!(again.code, code::INVALID_INPUT);
        let r = fetch(get(&format!("{base}/chunked"), &[]), Redirect::Follow)
            .await
            .unwrap();
        assert_eq!(receive(Arc::new(r)).await.unwrap().into_vec(), b"abcdefg");
        let r = fetch(get(&format!("{base}/b"), &[]), Redirect::Follow)
            .await
            .unwrap();
        assert_eq!(r.status_text, "OK");
    });
}

#[test]
fn bad_urls_and_headers_fail_before_connecting() {
    let e = send::parse_url("ftp://example.com/").err().unwrap();
    assert_eq!(e.code, code::UNSUPPORTED);
    assert_eq!(
        send::parse_url("not a url").err().unwrap().code,
        code::INVALID_INPUT
    );
    let bad: [&[u8]; 2] = [b"bad name", b"v"];
    assert_eq!(
        send::header_map(&bad).err().unwrap().code,
        code::INVALID_INPUT
    );
    let bad: [&[u8]; 2] = [b"x", b"a\nb"];
    assert_eq!(
        send::header_map(&bad).err().unwrap().code,
        code::INVALID_INPUT
    );
    let set: [&[u8]; 4] = [b"accept", b"text/html", b"user-agent", b"me"];
    let map = send::header_map(&set).unwrap();
    assert_eq!(map.get("accept").unwrap(), "text/html");
    assert_eq!(map.get("user-agent").unwrap(), "me");
}

#[test]
fn an_aborted_signal_drops_the_request() {
    rt().block_on(async {
        // Reads the request, never answers, and reports when the client closes the connection.
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (arrived_tx, arrived) = tokio::sync::oneshot::channel::<()>();
        let (closed_tx, closed) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let mut arrived_tx = Some(arrived_tx);
            loop {
                match s.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Some(tx) = arrived_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                }
            }
            let _ = closed_tx.send(());
        });
        let signal = Arc::new(Signal::default());
        let url = format!("http://127.0.0.1:{port}/");
        let work = tokio::spawn(run(
            vec![],
            Ok(get(&url, &[])),
            Redirect::Follow,
            Some(signal.clone()),
        ));
        // Hang guards, not assertions: each step completes at once.
        let guard = std::time::Duration::from_secs(60);
        tokio::time::timeout(guard, arrived).await.unwrap().unwrap();
        crate::task::abort::abort_for_test(&signal);
        let e = work.await.unwrap().err().unwrap();
        assert_eq!(message(e), "This operation was aborted");
        // The request was dropped, not abandoned: its connection is closed.
        tokio::time::timeout(guard, closed).await.unwrap().unwrap();
    });
}
