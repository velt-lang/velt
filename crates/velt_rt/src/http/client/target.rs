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
use hyper::Uri;
use url::Url;

/// A request URL: `http:` or `https:`, without a fragment.
pub(super) struct Target {
    /// The WHATWG serialization.
    pub href: String,
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
        let uri = href
            .parse()
            .map_err(|_| invalid(&format!("Invalid URL {href:?}")))?;
        Ok(Target { href, uri })
    }

    /// The URL parsed, for resolving a redirect's `location` against it and comparing origins
    /// (only a redirect needs it).
    pub fn url(&self) -> Result<Url, VeltErr> {
        Url::parse(&self.href).map_err(|_| invalid(&format!("Invalid URL {:?}", self.href)))
    }

    pub fn is_https(&self) -> bool {
        self.uri.scheme_str() == Some("https")
    }
}

/// Whether `url` is an `http:` or `https:` URL that WHATWG URL parsing serializes to exactly
/// `url`: a lowercase scheme, a host of lowercase ASCII letters, digits, `-` and `.` (a domain no
/// IDNA step changes, or a dotted-decimal IPv4 address), a port only when it is not the default
/// one and has no leading zero, a path that starts with `/` and has no dot segments, and path and
/// query bytes that are never percent-encoded or rewritten. No userinfo, no fragment. `false`
/// says nothing: the URL may still be valid.
pub(super) fn is_canonical(url: &str) -> bool {
    let b = url.as_bytes();
    let (rest, default_port) = if let Some(r) = b.strip_prefix(b"http://") {
        (r, 80)
    } else if let Some(r) = b.strip_prefix(b"https://") {
        (r, 443)
    } else {
        return false;
    };
    let Some(slash) = rest.iter().position(|&c| matches!(c, b'/' | b'?')) else {
        return false;
    };
    let (authority, tail) = rest.split_at(slash);
    if tail[0] != b'/' {
        return false;
    }
    let (host, port) = match authority.iter().position(|&c| c == b':') {
        Some(i) => (&authority[..i], Some(&authority[i + 1..])),
        None => (authority, None),
    };
    let path_end = tail.iter().position(|&c| c == b'?').unwrap_or(tail.len());
    is_canonical_host(host)
        && port.is_none_or(|p| is_canonical_port(p, default_port))
        && tail.iter().all(|&c| is_plain_byte(c))
        && !tail[..path_end].split(|&c| c == b'/').any(is_dot_segment)
}

/// A byte that the parser keeps as it is in a path and in a query: printable ASCII except the
/// ones it percent-encodes (`"`, `#`, `<`, `>`, `` ` ``, `{`, `}`, `^`, `'`), `\` (a path
/// separator in `http:`) and `|`.
fn is_plain_byte(c: u8) -> bool {
    matches!(c, b'!'..=b'~')
        && !matches!(
            c,
            b'"' | b'#' | b'<' | b'>' | b'`' | b'{' | b'}' | b'^' | b'\'' | b'\\' | b'|'
        )
}

/// `.`, `..` and their percent-encoded spellings, which the parser removes from a path.
fn is_dot_segment(seg: &[u8]) -> bool {
    matches!(seg, b"." | b"..") || seg.windows(3).any(|w| w.eq_ignore_ascii_case(b"%2e"))
}

/// A host the parser keeps as it is: dot-separated non-empty labels of `a-z`, `0-9` and `-`,
/// none an IDNA `xn--` label. A host whose last label starts with a digit may be an IPv4
/// address in another notation (`127.1`, `0x7f.0.0.1`), so it must be four canonical decimal
/// numbers up to 255.
fn is_canonical_host(host: &[u8]) -> bool {
    let labels_ok = host.split(|&c| c == b'.').all(|l| {
        !l.is_empty()
            && !l.starts_with(b"xn--")
            && l.iter()
                .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    });
    if !labels_ok {
        return false;
    }
    let last = host.rsplit(|&c| c == b'.').next().unwrap_or_default();
    if !last.first().is_some_and(u8::is_ascii_digit) {
        return true;
    }
    let mut parts = 0;
    host.split(|&c| c == b'.').all(|p| {
        parts += 1;
        number(p).is_some_and(|n| n <= 255)
    }) && parts == 4
}

/// A port the parser keeps: a canonical number up to 65535, not the scheme's default.
fn is_canonical_port(p: &[u8], default: u32) -> bool {
    number(p).is_some_and(|n| n <= 65535 && n != default)
}

/// The value of up to five decimal digits without a leading zero (`0` itself is fine).
fn number(p: &[u8]) -> Option<u32> {
    let canonical = !p.is_empty()
        && p.len() <= 5
        && p.iter().all(u8::is_ascii_digit)
        && (p.len() == 1 || p[0] != b'0');
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
        assert_eq!(t.href, "http://127.0.0.1:9/x?y");
        assert_eq!(t.uri.path_and_query().map(|p| p.as_str()), Some("/x?y"));
        assert!(!t.is_https());
        let t = Target::parse("HTTPS://Example.COM:443/a/../b#f").unwrap();
        assert_eq!(t.href, "https://example.com/b");
        assert!(t.is_https());
        assert!(Target::parse("ftp://x/").is_err());
        assert!(Target::parse("http://").is_err());
    }
}
