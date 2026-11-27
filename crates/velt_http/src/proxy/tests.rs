//! Which proxy the environment picks, and requests through a local proxy: a `CONNECT` tunnel to
//! an HTTPS server, a plain request in absolute form, and a proxy that refuses.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver};

use super::*;
use crate::client::{fetch_via, https_via, Limits};
use crate::tls::config_with;
use crate::tls::tests::{serve_once, TEST_CA};
use crate::Response;

/// `choose` with the variables of `vars`.
fn pick(url: &str, vars: &[(&str, &str)]) -> Option<String> {
    let var = |name: &str| {
        vars.iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.to_string())
    };
    choose(url, var).unwrap().map(|p| p.authority)
}

#[test]
fn the_scheme_picks_the_variable() {
    let all = [
        ("https_proxy", "http://s:1"),
        ("http_proxy", "http://p:2"),
        ("ALL_PROXY", "http://a:3"),
    ];
    assert_eq!(pick("https://example.com/", &all).as_deref(), Some("s:1"));
    assert_eq!(pick("http://example.com/", &all).as_deref(), Some("p:2"));
    // Uppercase `HTTPS_PROXY` counts; lowercase wins over it.
    let upper = [("HTTPS_PROXY", "u:4")];
    assert_eq!(pick("https://example.com/", &upper).as_deref(), Some("u:4"));
    let both = [("HTTPS_PROXY", "u:4"), ("https_proxy", "l:5")];
    assert_eq!(pick("https://example.com/", &both).as_deref(), Some("l:5"));
    // `ALL_PROXY` when nothing more specific is set.
    let fallback = [("ALL_PROXY", "a:3")];
    assert_eq!(
        pick("http://example.com/", &fallback).as_deref(),
        Some("a:3")
    );
    assert_eq!(
        pick("https://example.com/", &fallback).as_deref(),
        Some("a:3")
    );
    assert_eq!(pick("https://example.com/", &[]), None);
}

/// A CGI request's `Proxy:` header sets `HTTP_PROXY` ("httpoxy"): only `http_proxy` counts.
#[test]
fn uppercase_http_proxy_is_ignored() {
    let vars = [("HTTP_PROXY", "http://evil:80")];
    assert_eq!(pick("http://example.com/", &vars), None);
    let vars = [("HTTP_PROXY", "http://evil:80"), ("ALL_PROXY", "a:3")];
    assert_eq!(pick("http://example.com/", &vars).as_deref(), Some("a:3"));
}

#[test]
fn this_machine_is_reached_directly() {
    let vars = [("https_proxy", "p:1"), ("http_proxy", "p:1")];
    for url in [
        "http://localhost:8080/",
        "https://127.0.0.1/",
        "https://[::1]:8443/",
        "http://127.3.4.5/",
    ] {
        assert_eq!(pick(url, &vars), None, "{url}");
    }
}

#[test]
fn no_proxy_entries() {
    let list = "localhost,127.0.0.0/8,::1,10.0.0.0/8,.svc.cluster.local,*.corp.example,\
                example.org, [fd00::1]:8443 , internal:8080";
    let direct = [
        ("example.org", None),
        ("api.example.org", None),
        ("x.svc.cluster.local", None),
        ("svc.cluster.local", None),
        ("a.b.corp.example", None),
        ("10.1.2.3", None),
        ("EXAMPLE.ORG.", None),
        ("fd00::1", Some(8443)),
        ("internal", Some(8080)),
    ];
    for (host, port) in direct {
        assert!(bypasses(list, host, port.or(Some(443))), "{host}");
    }
    let proxied = [
        ("notexample.org", None),
        ("example.org.evil", None),
        ("11.1.2.3", None),
        ("fd00::1", Some(443)),
        ("internal", Some(443)),
        ("cluster.local", None),
    ];
    for (host, port) in proxied {
        assert!(!bypasses(list, host, port.or(Some(443))), "{host}");
    }
    assert!(bypasses("*", "anything.example", Some(443)));
    assert!(bypasses("fd00::/8", "fd12::5", Some(443)));
    assert!(!bypasses("", "example.org", Some(443)));
    let vars = [("https_proxy", "p:1"), ("NO_PROXY", "example.org")];
    assert_eq!(pick("https://www.example.org/", &vars), None);
    assert_eq!(pick("https://example.com/", &vars).as_deref(), Some("p:1"));
    // `no_proxy` wins over `NO_PROXY`.
    let vars = [
        ("https_proxy", "p:1"),
        ("no_proxy", "example.com"),
        ("NO_PROXY", "example.org"),
    ];
    assert_eq!(pick("https://example.com/", &vars), None);
    assert_eq!(pick("https://example.org/", &vars).as_deref(), Some("p:1"));
}

#[test]
fn proxy_urls() {
    let p = Proxy::parse("http://proxy.example:3128/", "$X").unwrap();
    assert_eq!(p.authority, "proxy.example:3128");
    assert_eq!(p.authorization, None);
    assert_eq!(
        Proxy::parse("proxy.example", "$X").unwrap().authority,
        "proxy.example:80"
    );
    assert_eq!(
        Proxy::parse("HTTP://[::1]:9", "$X").unwrap().authority,
        "[::1]:9"
    );
    assert_eq!(
        Proxy::parse("[fd00::2]", "$X").unwrap().authority,
        "[fd00::2]:80"
    );
    let p = Proxy::parse("http://al%40ice:p%3Aw@proxy:8080", "$X").unwrap();
    assert_eq!(p.authority, "proxy:8080");
    // base64("al@ice:p:w")
    assert_eq!(p.authorization.as_deref(), Some("Basic YWxAaWNlOnA6dw=="));
    let p = Proxy::parse("http://user@proxy:8080", "$X").unwrap();
    assert_eq!(p.authorization.as_deref(), Some("Basic dXNlcjo="));
    for (bad, why) in [
        ("socks5://proxy:1080", "not supported"),
        ("https://proxy:443", "not supported"),
        ("http://proxy:99999", "bad port"),
        ("http://user:secret@", "no host"),
        ("http://user:sec%0Aret@proxy:1", "control character"),
    ] {
        let err = Proxy::parse(bad, "$HTTPS_PROXY").unwrap_err();
        assert!(
            err.starts_with("invalid proxy in $HTTPS_PROXY"),
            "{bad}: {err}"
        );
        assert!(err.contains(why), "{bad}: {err}");
        assert!(!err.contains("secret"), "the password stays out: {err}");
    }
    let err = choose("https://example.com/", |n| {
        (n == "ALL_PROXY").then(|| "socks5h://p:1".to_string())
    })
    .unwrap_err();
    assert!(err.contains("$ALL_PROXY"), "{err}");
}

#[test]
fn base64_vectors() {
    for (plain, coded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
    ] {
        assert_eq!(base64(plain.as_bytes()), coded);
    }
}

/// The head `proxy_once` got. The proxy sends it before it forwards or answers anything, so it is
/// there once a response through the proxy has arrived (and missing if the request went around it).
fn head_of(heads: &Receiver<String>) -> String {
    heads
        .try_recv()
        .expect("the request did not go through the proxy")
}

/// A proxy for one connection: it sends the request head it got on the channel. A `CONNECT` is
/// answered with `answer` and, for a 200, tunneled to its target; any other request is answered
/// by the proxy itself with its request line as the body.
fn proxy_once(answer: &'static str) -> (Proxy, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let authority = listener.local_addr().unwrap().to_string();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && client.read(&mut byte).unwrap_or(0) == 1 {
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let line = head.lines().next().unwrap_or("").to_string();
        tx.send(head).unwrap();
        let Some(target) = line.strip_prefix("CONNECT ") else {
            let body = line.as_bytes();
            let _ = write!(
                client,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = client.write_all(body);
            return;
        };
        let _ = client.write_all(answer.as_bytes());
        if !answer.starts_with("HTTP/1.1 200") {
            return;
        }
        let target = target.split(' ').next().unwrap();
        let mut origin = TcpStream::connect(target).unwrap();
        let (mut c2, mut o2) = (client.try_clone().unwrap(), origin.try_clone().unwrap());
        let up = std::thread::spawn(move || {
            let _ = std::io::copy(&mut c2, &mut o2);
            let _ = o2.shutdown(std::net::Shutdown::Write);
        });
        let _ = std::io::copy(&mut origin, &mut client);
        let _ = client.shutdown(std::net::Shutdown::Write);
        let _ = up.join();
    });
    (
        Proxy {
            authority,
            authorization: Some("Basic dTpw".into()),
        },
        rx,
    )
}

#[test]
fn https_goes_through_a_connect_tunnel() {
    let (proxy, heads) = proxy_once("HTTP/1.1 200 Connection established\r\n\r\n");
    let origin = serve_once(Response::text(200, "tunneled"));
    let port = origin.rsplit_once(':').unwrap().1;
    let rest = format!("localhost:{port}/api?q=1");
    let config = config_with(TEST_CA.as_bytes()).unwrap();
    let resp = https_via("GET", &rest, &[], b"", config, Some(&proxy)).unwrap();
    // TLS ran to the origin, which checked its certificate for `localhost`.
    assert_eq!(resp.body_text(), "tunneled GET /api");
    let head = head_of(&heads);
    assert!(
        head.starts_with(&format!(
            "CONNECT localhost:{port} HTTP/1.1\r\nHost: localhost:{port}\r\n"
        )),
        "{head}"
    );
    assert!(
        head.contains("Proxy-Authorization: Basic dTpw\r\n"),
        "{head}"
    );
}

#[test]
fn http_is_sent_to_the_proxy_whole() {
    let (proxy, heads) = proxy_once("");
    let url = "http://origin.invalid:8080/a/b?c=d";
    let resp = fetch_via(
        "GET",
        url,
        &[("X-A", "1")],
        b"",
        Limits::DEFAULT,
        Some(&proxy),
    )
    .unwrap();
    assert_eq!(
        resp.body_text(),
        "GET http://origin.invalid:8080/a/b?c=d HTTP/1.1"
    );
    let head = head_of(&heads);
    assert!(head.contains("\r\nHost: origin.invalid:8080\r\n"), "{head}");
    assert!(
        head.contains("\r\nProxy-Authorization: Basic dTpw\r\n"),
        "{head}"
    );
    assert!(head.contains("\r\nX-A: 1\r\n"), "{head}");
}

#[test]
fn a_refused_tunnel_is_an_error() {
    let (proxy, _heads) =
        proxy_once("HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n");
    let url = "https://origin.invalid/";
    let err = fetch_via("GET", url, &[], b"", Limits::DEFAULT, Some(&proxy)).unwrap_err();
    assert!(
        err.contains(
            "refused the tunnel to origin.invalid:443: HTTP/1.1 407 Proxy Authentication Required"
        ),
        "{err}"
    );
    let gone = Proxy {
        authority: "127.0.0.1:1".into(),
        authorization: None,
    };
    let err = fetch_via("GET", url, &[], b"", Limits::DEFAULT, Some(&gone)).unwrap_err();
    assert!(err.contains("proxy 127.0.0.1:1: cannot connect"), "{err}");
}
