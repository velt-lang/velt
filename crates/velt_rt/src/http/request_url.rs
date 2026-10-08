//! A server request's absolute URL (`req.url`): `http(s)://`, the host, then the path and query,
//! as Deno and Bun give it.
//!
//! Building it allocates (a URL is usually longer than an inline string), and a handler reads it
//! on nearly every request. Clients repeat themselves: a keep-alive connection, or a load
//! balancer's health check, asks for the same few URLs over and over. So each worker thread
//! remembers the last [`RECENT`] URLs it built (shared heap strings), and a request whose scheme,
//! host and target match one of them gets another reference to it: a comparison and a count
//! increment instead of an allocation and a copy.

use super::client::text_of;
use super::request::Conn;
use crate::str::VeltStr;
use hyper::header::HOST;
use std::cell::RefCell;

/// URLs remembered per worker thread.
const RECENT: usize = 4;
/// Longest URL worth remembering (and built on the stack).
const MAX_LEN: usize = 256;

/// A worker's recent URLs, replaced round-robin; released when the thread ends.
struct Recent {
    urls: [VeltStr; RECENT],
    next: usize,
}

impl Drop for Recent {
    fn drop(&mut self) {
        for url in &mut self.urls {
            // SAFETY: every entry is a valid string this cache holds a reference to.
            unsafe { url.release() };
        }
    }
}

thread_local! {
    static URLS: RefCell<Recent> = const {
        RefCell::new(Recent { urls: [const { VeltStr::empty() }; RECENT], next: 0 })
    };
}

/// A request's absolute URL: `http(s)://`, the `host` header (HTTP/2: `:authority`; without
/// either, the server's address), then the path and query. A host that is not UTF-8 is decoded
/// lossily.
pub(super) fn url_of(parts: &hyper::http::request::Parts, conn: &Conn) -> VeltStr {
    let uri = &parts.uri;
    let path = uri.path_and_query().map_or("/", |p| p.as_str()).as_bytes();
    if let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) {
        // Absolute form (HTTP/2, or a proxy request over HTTP/1.1).
        let parts = [
            scheme.as_bytes(),
            b"://",
            authority.as_str().as_bytes(),
            path,
        ];
        return joined(&parts);
    }
    let scheme: &[u8] = if conn.tls { b"https://" } else { b"http://" };
    // A scan, not a hashed lookup: `host` is a request's first header, or close to it.
    let host = parts.headers.iter().find(|(name, _)| *name == HOST);
    match host.map(|(_, h)| h.as_bytes()) {
        Some(host) if !host.is_empty() => joined(&[scheme, host, path]),
        _ => joined(&[scheme, conn.local.to_string().as_bytes(), path]),
    }
}

/// `parts` joined into one string: a recent URL that has the same bytes, else a new string
/// (remembered when it is ASCII and short enough).
fn joined(parts: &[&[u8]]) -> VeltStr {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    if len > MAX_LEN {
        return text_of(&parts.concat());
    }
    URLS.with(|urls| {
        let mut urls = urls.borrow_mut();
        if let Some(url) = urls.urls.iter().find(|u| same(u, parts, len)) {
            // SAFETY: a valid string the cache holds.
            return unsafe { url.share() };
        }
        let url = build(parts, len);
        if url.is_heap() && url.is_ascii() {
            let at = urls.next;
            urls.next = (at + 1) % RECENT;
            // SAFETY: the old entry is a valid string the cache holds; the new one is shared.
            unsafe {
                urls.urls[at].release();
                urls.urls[at] = url.share();
            }
        }
        url
    })
}

/// Whether `url` has the bytes of `parts` (`len` bytes in all).
fn same(url: &VeltStr, parts: &[&[u8]], len: usize) -> bool {
    if url.len() != len {
        return false;
    }
    // SAFETY: a valid string the cache holds.
    let mut rest = unsafe { url.as_bytes() };
    for p in parts {
        let (head, tail) = rest.split_at(p.len());
        if !equal(head, p) {
            return false;
        }
        rest = tail;
    }
    true
}

/// `a == b` for slices of the same length, a word at a time: the pieces of a URL are short, and
/// `memcmp`'s dispatch would cost more than comparing them.
#[inline]
fn equal(a: &[u8], b: &[u8]) -> bool {
    let n = a.len();
    let word = |s: &[u8], at: usize| u64::from_ne_bytes(s[at..at + 8].try_into().unwrap_or([0; 8]));
    if n < 8 {
        return a.iter().zip(b).all(|(x, y)| x == y);
    }
    let mut at = 0;
    while at + 8 < n {
        if word(a, at) != word(b, at) {
            return false;
        }
        at += 8;
    }
    // The last word, overlapping the previous one when `n` is not a multiple of 8.
    word(a, n - 8) == word(b, n - 8)
}

/// A new string of `parts` (`len` ≤ [`MAX_LEN`] bytes), built on the stack, so the string's own
/// buffer is the only allocation. ASCII (the scheme, path and query always are, a host nearly
/// always) is taken as it is; anything else is decoded lossily.
fn build(parts: &[&[u8]], len: usize) -> VeltStr {
    let mut buf = [0u8; MAX_LEN];
    let mut at = 0;
    for p in parts {
        buf[at..at + p.len()].copy_from_slice(p);
        at += p.len();
    }
    let buf = &buf[..len];
    if buf.is_ascii() {
        // SAFETY: ASCII is UTF-8 with one UTF-16 unit per byte.
        return unsafe { VeltStr::from_text_counted(std::str::from_utf8_unchecked(buf), len) };
    }
    text_of(buf)
}

#[cfg(test)]
mod tests {
    use super::{equal, url_of, Conn};

    fn url(builder: hyper::http::request::Builder, tls: bool) -> String {
        let (parts, ()) = builder.body(()).unwrap().into_parts();
        let local = "10.0.0.1:8080".parse().unwrap();
        let conn = Conn {
            remote: "10.0.0.2:5000".parse().unwrap(),
            local,
            tls,
        };
        let mut url = url_of(&parts, &conn);
        let text = unsafe { url.as_bytes() }.to_vec();
        unsafe { url.release() };
        String::from_utf8(text).unwrap()
    }

    #[test]
    fn urls_are_absolute() {
        let get = |uri: &str| hyper::Request::get(uri);
        assert_eq!(
            url(get("/a/b?x=1").header("host", "example.com:81"), false),
            "http://example.com:81/a/b?x=1"
        );
        assert_eq!(
            url(get("/").header("host", "example.com"), true),
            "https://example.com/"
        );
        // HTTP/2 and proxy requests carry the scheme and authority in the request target.
        assert_eq!(
            url(get("https://h2.example/p?q").header("host", "other"), false),
            "https://h2.example/p?q"
        );
        // HTTP/1.0 may name no host: the server's address stands in.
        assert_eq!(url(get("/x"), false), "http://10.0.0.1:8080/x");
        let long = format!("/{}", "x".repeat(300));
        assert_eq!(
            url(get(&long).header("host", "h"), false),
            format!("http://h{long}")
        );
        assert_eq!(
            url(get("/caf%C3%A9").header("host", "h\u{e9}.example"), false),
            "http://h\u{e9}.example/caf%C3%A9"
        );
    }

    #[test]
    fn a_repeated_url_is_shared_and_a_different_one_is_not() {
        let parts = |uri: &str, host: &str| {
            let req = hyper::Request::get(uri).header("host", host);
            req.body(()).unwrap().into_parts().0
        };
        let conn = Conn {
            remote: "10.0.0.2:5000".parse().unwrap(),
            local: "10.0.0.1:8080".parse().unwrap(),
            tls: false,
        };
        let a = parts("/a/long/enough/path?to=be&on=the&heap", "example.com");
        let mut first = url_of(&a, &conn);
        let mut again = url_of(&a, &conn);
        let mut tls = url_of(&a, &Conn { tls: true, ..conn });
        let b = parts("/a/long/enough/path?to=be&on=the&heaq", "example.com");
        let mut other = url_of(&b, &conn);
        let c = parts("/a/long/enough/path?to=be&on=the&heap", "example.org");
        let mut host = url_of(&c, &conn);
        unsafe {
            assert!(first.is_heap());
            assert_eq!(first.as_bytes().as_ptr(), again.as_bytes().as_ptr());
            assert_eq!(
                tls.as_bytes(),
                b"https://example.com/a/long/enough/path?to=be&on=the&heap"
            );
            assert_eq!(
                other.as_bytes(),
                b"http://example.com/a/long/enough/path?to=be&on=the&heaq"
            );
            assert_eq!(
                host.as_bytes(),
                b"http://example.org/a/long/enough/path?to=be&on=the&heap"
            );
            assert_ne!(first.as_bytes().as_ptr(), other.as_bytes().as_ptr());
            // Every request's string is its own reference: releasing one leaves the others.
            first.release();
            assert_eq!(
                again.as_bytes(),
                b"http://example.com/a/long/enough/path?to=be&on=the&heap"
            );
            for s in [&mut again, &mut tls, &mut other, &mut host] {
                s.release();
            }
        }
    }

    #[test]
    fn equal_compares_every_byte() {
        for n in 0..40u8 {
            let a: Vec<u8> = (0..n).map(|i| i.wrapping_mul(7)).collect();
            assert!(equal(&a, &a.clone()));
            for i in 0..n as usize {
                let mut b = a.clone();
                b[i] ^= 0x20;
                assert!(!equal(&a, &b), "length {n}, byte {i}");
            }
        }
    }
}
