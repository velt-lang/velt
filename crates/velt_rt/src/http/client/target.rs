//! A request's URL ([`Target`]): its WHATWG serialization (what `res.url` reports) and the
//! `Uri` hyper sends to.
//!
//! WHATWG URL parsing (the `url` crate, with IDNA for the host) costs a few thousand
//! instructions, more than the rest of building a request. Most URLs a program fetches are
//! already in the form the parser would serialize them to (`http://host:port/path?query`, a
//! lowercase ASCII host), so [`Target::parse`] first checks for that form and, when it holds,
//! takes the text as it is; anything else goes through the full parser. The check only accepts
//! what it can prove the parser leaves unchanged, and a unit test holds it to that.

use super::send::invalid;
use crate::result::{code, VeltErr};
use bytes::Bytes;
use hyper::header::HeaderValue;
use hyper::http::uri::Scheme;
use hyper::Uri;
use url::Url;

/// A request URL: `http:` or `https:`, without a fragment.
pub(super) struct Target {
    /// The WHATWG serialization (UTF-8; ASCII in fact). The URI's parts share its buffer.
    pub href: Bytes,
    /// The same URL for hyper.
    pub uri: Uri,
}

impl Target {
    /// Parse `url` as WHATWG URL (as Node's `fetch` does) and check it is `http:` or `https:`.
    pub fn parse(url: &str) -> Result<Target, VeltErr> {
        if is_canonical(url) {
            return Target::with_href(url.to_owned());
        }
        let mut u = Url::parse(url).map_err(|e| invalid(&format!("Invalid URL {url:?}: {e}")))?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(VeltErr::new(
                code::UNSUPPORTED,
                &format!("fetch supports http: and https: URLs, not {}", u.scheme()),
            ));
        }
        u.set_fragment(None);
        Target::from_url(u)
    }

    /// The target of a parsed `url` (`http:` or `https:`, no fragment).
    pub fn from_url(url: Url) -> Result<Target, VeltErr> {
        Target::with_href(url.into())
    }

    fn with_href(href: String) -> Result<Target, VeltErr> {
        let href = Bytes::from(href);
        let uri = Uri::from_maybe_shared(href.clone())
            .map_err(|_| invalid(&format!("Invalid URL {:?}", String::from_utf8_lossy(&href))))?;
        Ok(Target { href, uri })
    }

    /// The WHATWG serialization.
    pub fn href(&self) -> &str {
        as_text(&self.href)
    }

    /// The URL parsed, for resolving a redirect's `location` against it and comparing origins
    /// (only a redirect needs it).
    pub fn url(&self) -> Result<Url, VeltErr> {
        Url::parse(self.href()).map_err(|_| invalid(&format!("Invalid URL {:?}", self.href())))
    }

    /// The `host` header of a request to an `http:` URL: its host, and its port unless that is
    /// the default one (a WHATWG serialization has none then), as hyper would make it with
    /// `format!` for each request. `None` for `https:`, where HTTP/2 has no `host` and hyper adds
    /// one for HTTP/1.1.
    pub fn host(&self) -> Option<HeaderValue> {
        if self.uri.scheme() != Some(&Scheme::HTTP) {
            return None;
        }
        let authority = self.uri.authority()?.as_str();
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        HeaderValue::from_str(host).ok()
    }

    pub fn is_https(&self) -> bool {
        self.uri.scheme_str() == Some("https")
    }
}

/// A URL's text: [`Target::href`] and the `url` of a response, both made from a `String`.
pub(super) fn as_text(href: &Bytes) -> &str {
    std::str::from_utf8(href).expect("ICE: a URL is made from a String")
}

/// Whether `url` is an `http:` or `https:` URL that WHATWG URL parsing serializes to exactly
/// `url`: a lowercase scheme, a host of lowercase ASCII letters, digits, `-` and `.` (a domain no
/// IDNA step changes, or a dotted-decimal IPv4 address), a port only when it is not the default
/// one and has no leading zero, a path that starts with `/` and has no dot segments, and path and
/// query bytes that are never percent-encoded or rewritten. No userinfo, no fragment. `false`
/// says nothing: the URL may still be valid. One pass over the bytes, with no allocation.
pub(super) fn is_canonical(url: &str) -> bool {
    let b = url.as_bytes();
    let (start, default_port) = if b.starts_with(b"http://") {
        (7, 80)
    } else if b.starts_with(b"https://") {
        (8, 443)
    } else {
        return false;
    };
    let Some(host_len) = canonical_host(&b[start..]) else {
        return false;
    };
    let mut i = start + host_len;
    if b.get(i) == Some(&b':') {
        let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        let port = number(&b[i + 1..i + 1 + digits]);
        if !port.is_some_and(|p| p <= 65535 && p != default_port) {
            return false;
        }
        i += 1 + digits;
    }
    b.get(i) == Some(&b'/') && is_canonical_tail(&b[i..])
}

/// The length of the host `b` starts with (it ends at the first byte that is not `a-z`, `0-9`,
/// `-` or `.`), when the parser keeps it as it is: dot-separated non-empty labels, none an IDNA
/// `xn--` label. A host whose last label starts with a digit may be an IPv4 address in another
/// notation (`127.1`, `0x7f.0.0.1`), so it must be four canonical decimal numbers up to 255.
fn canonical_host(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    let mut labels = 0;
    // Every label so far is a canonical decimal number up to 255.
    let mut ipv4 = true;
    loop {
        let start = i;
        // The label's value, capped (only whether it is above 255 matters).
        let mut n: u32 = 0;
        while let Some(&c) = b.get(i) {
            match c {
                b'0'..=b'9' => n = (n * 10 + u32::from(c - b'0')).min(256),
                b'a'..=b'z' | b'-' => ipv4 = false,
                _ => break,
            }
            i += 1;
        }
        let label = &b[start..i];
        if label.is_empty() || label.starts_with(b"xn--") {
            return None;
        }
        labels += 1;
        ipv4 &= n <= 255 && (label.len() == 1 || label[0] != b'0');
        if b.get(i) != Some(&b'.') {
            let numeric = label[0].is_ascii_digit();
            return (!numeric || (ipv4 && labels == 4)).then_some(i);
        }
        i += 1;
    }
}

/// Whether the parser keeps the path and query `t` (which starts with `/`) as they are: bytes
/// it never percent-encodes or rewrites ([`PLAIN`]), and no dot segment in the path (`.`, `..`,
/// or one spelled with `%2e`, which it removes).
fn is_canonical_tail(t: &[u8]) -> bool {
    let mut in_path = true;
    let mut segment = 1;
    for (i, &c) in t.iter().enumerate() {
        if !PLAIN[c as usize] {
            return false;
        }
        if !in_path {
            continue;
        }
        match c {
            b'/' | b'?' if i > 0 => {
                if is_dot_segment(&t[segment..i]) {
                    return false;
                }
                segment = i + 1;
                in_path = c == b'/';
            }
            b'%' if t
                .get(i + 1..i + 3)
                .is_some_and(|h| h.eq_ignore_ascii_case(b"2e")) =>
            {
                return false;
            }
            _ => {}
        }
    }
    !(in_path && is_dot_segment(&t[segment..]))
}

fn is_dot_segment(seg: &[u8]) -> bool {
    matches!(seg, b"." | b"..")
}

/// The bytes the parser keeps as they are in a path and in a query: printable ASCII except the
/// ones it percent-encodes (`"`, `#`, `<`, `>`, `` ` ``, `{`, `}`, `^`, `'`), `\` (a path
/// separator in `http:`) and `|`.
const PLAIN: [bool; 256] = {
    let mut table = [false; 256];
    let mut c = b'!';
    while c <= b'~' {
        table[c as usize] = !matches!(
            c,
            b'"' | b'#' | b'<' | b'>' | b'`' | b'{' | b'}' | b'^' | b'\'' | b'\\' | b'|'
        );
        c += 1;
    }
    table
};

/// The value of up to five decimal digits without a leading zero (`0` itself is fine).
fn number(p: &[u8]) -> Option<u32> {
    let canonical = !p.is_empty() && p.len() <= 5 && (p.len() == 1 || p[0] != b'0');
    canonical.then(|| p.iter().fold(0, |n, &d| n * 10 + u32::from(d - b'0')))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every URL the check accepts is one the parser leaves unchanged.
    fn agrees(url: &str) {
        if is_canonical(url) {
            let parsed = Url::parse(url).map(String::from);
            assert_eq!(parsed.as_deref(), Ok(url), "accepted {url:?}");
        }
    }

    #[test]
    fn accepts_common_urls() {
        for url in [
            "http://127.0.0.1:18090/small",
            "http://localhost:8080/",
            "https://example.com/a/b?x=1&y=%20z",
            "https://api.example-1.com/v1/users/42?q=a,b;c:d@e/f?g",
            "http://0.0.0.0:1/",
            "http://a/.well-known/x",
        ] {
            assert!(is_canonical(url), "{url}");
            agrees(url);
        }
    }

    #[test]
    fn leaves_the_rest_to_the_parser() {
        for url in [
            "HTTP://example.com/",
            "http://Example.com/",
            "http://example.com",
            "http://example.com?x",
            "http://example.com:80/",
            "https://example.com:443/",
            "http://example.com:080/",
            "http://example.com:/",
            "http://example.com:65536/",
            "http://127.1/",
            "http://0x7f.0.0.1/",
            "http://127.0.0.01/",
            "http://1.2.3.4.5/",
            "http://example.1/",
            "http://example.com./",
            "http://a..b/",
            "http://xn--nxasmq6b.com/",
            "http://user@example.com/",
            "http://example.com/a/../b",
            "http://example.com/a/./b",
            "http://example.com/a/%2E%2e/b",
            "http://example.com/a/.",
            "http://example.com/a b",
            "http://example.com/a#frag",
            "http://example.com/a\\b",
            "http://example.com/{x}",
            "http://example.com/?q='x'",
            "http://exa_mple.com/",
            "ftp://example.com/",
            "http://[::1]/",
            "http://example.com/\u{e9}",
        ] {
            assert!(!is_canonical(url), "{url}");
        }
    }

    /// Generated URLs from pieces that are each sometimes canonical and sometimes not.
    #[test]
    fn agrees_with_the_parser_on_generated_urls() {
        let schemes = ["http://", "https://", "HTTP://"];
        let hosts = [
            "a",
            "a.b",
            "1.2.3.4",
            "1.2.3",
            "255.255.255.255",
            "256.0.0.1",
            "a-b",
            "-a",
            "a.1",
            "1a.b",
            "0.0.0.0",
            "00.0.0.0",
            "xn--a",
            "localhost",
            "1.2.3.04",
            "a.b-",
            "a_b",
            "u@a",
            "a.b.",
            "127.0.0.1",
        ];
        let ports = [
            "", ":0", ":1", ":80", ":443", ":8080", ":08", ":65535", ":99999",
        ];
        let tails = [
            "/",
            "/a",
            "/a/",
            "//a",
            "/./",
            "/../a",
            "/a/..",
            "/.a",
            "/a.",
            "/%2e/",
            "/%41",
            "/%zz",
            "/?",
            "/?a=b&c",
            "/a?b/c?d",
            "/a;b=c",
            "/~x!$&()*+,=:@",
            "/a%",
            "/%2E.",
            "/a?%2e",
            "/A/B",
            "/a/%2E",
            "/a?x#y",
            "/a b",
            "/a^b",
            r"/a\b",
        ];
        for s in schemes {
            for h in hosts {
                for p in ports {
                    for t in tails {
                        agrees(&format!("{s}{h}{p}{t}"));
                    }
                }
            }
        }
    }

    #[test]
    fn parses_into_href_and_uri() {
        let t = Target::parse("http://127.0.0.1:9/x?y").unwrap();
        assert_eq!(t.href(), "http://127.0.0.1:9/x?y");
        assert_eq!(t.uri.path_and_query().map(|p| p.as_str()), Some("/x?y"));
        assert_eq!(t.host().unwrap(), "127.0.0.1:9");
        assert_eq!(
            Target::parse("http://u:p@a.b:80/").unwrap().host().unwrap(),
            "a.b"
        );
        assert!(!t.is_https());
        let t = Target::parse("HTTPS://Example.COM:443/a/../b#f").unwrap();
        assert_eq!(t.href(), "https://example.com/b");
        assert!(t.is_https());
        assert!(t.host().is_none());
        assert!(Target::parse("ftp://x/").is_err());
        assert!(Target::parse("http://").is_err());
    }
}
