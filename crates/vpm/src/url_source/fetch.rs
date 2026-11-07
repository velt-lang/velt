//! Downloading an archive: redirects followed with each hop checked as the first URL is, plain
//! `http` (to this machine) only from a hop that was plain `http` already, so a public `https`
//! URL can't send the developer's machine to a local service; at most [`super::MAX_ARCHIVE`]
//! read; a relative `Location` resolved against the hop it came from. A fragment is never sent.

use super::{check_url, MAX_ARCHIVE};

/// How many redirects a download follows.
const MAX_REDIRECTS: usize = 5;

/// The bytes at `url`.
pub(super) fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    fetch_within(url, MAX_ARCHIVE)
}

/// [`fetch_bytes`], reading at most `max` bytes.
pub(super) fn fetch_within(url: &str, max: usize) -> Result<Vec<u8>, String> {
    check_url(url)?;
    let limits = velt_http::Limits {
        max_body: max,
        ..velt_http::Limits::DEFAULT
    };
    let mut at = without_fragment(url).to_string();
    for _ in 0..=MAX_REDIRECTS {
        let response = velt_http::fetch_within("GET", &at, &[("User-Agent", "velt")], &[], limits)
            .map_err(|e| format!("cannot download {at}: {e}"))?;
        match response.status {
            200 => return Ok(response.body),
            301 | 302 | 303 | 307 | 308 => {
                let location = response
                    .header("Location")
                    .ok_or_else(|| format!("{at}: a redirect without a Location"))?;
                let next = resolve(&at, location).ok_or_else(|| {
                    format!("{at}: a redirect to `{location}`, which isn't a URL")
                })?;
                let next = without_fragment(&next).to_string();
                check_hop(&at, &next)?;
                at = next;
            }
            404 => return Err(format!("{at} doesn't exist (HTTP 404)")),
            status => return Err(format!("cannot download {at}: HTTP {status}")),
        }
    }
    Err(format!("{url}: more than {MAX_REDIRECTS} redirects"))
}

/// `Err` unless a redirect from `at` to `next` may be followed: `next`'s scheme, host,
/// credentials and path pass [`check_url`], and it is plain `http` only if `at` was (a download
/// that started on https stays on https). Its query isn't checked: a hop's URL is the server's,
/// not one a manifest wrote, and a storage service's signed query holds escapes (`%3B`, `%2F`)
/// that a manifest's URL may not.
pub(super) fn check_hop(at: &str, next: &str) -> Result<(), String> {
    if is_plain_http(next) && !is_plain_http(at) {
        return Err(format!(
            "{at} redirects to plain http ({next}): a download that started on https stays on https"
        ));
    }
    let checked = next.split(['?', '#']).next().unwrap_or(next);
    check_url(checked).map_err(|e| format!("{at} redirects to {next}: {e}"))
}

/// `url` without its fragment, which names a part of the response and isn't sent.
fn without_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// Whether `location` begins with a scheme and `://` (`scheme = ALPHA *( ALPHA / DIGIT / "+" /
/// "-" / "." )`): only then is it absolute, so `/y?u=https://x` is a path.
fn is_absolute(location: &str) -> bool {
    location.split_once("://").is_some_and(|(scheme, _)| {
        let mut chars = scheme.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// Whether `url` is plain `http` (any case of the scheme).
pub(super) fn is_plain_http(url: &str) -> bool {
    url.get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
}

/// `location` (a redirect's target) resolved against `base`: an absolute URL, a scheme-relative
/// `//host/…`, a host-relative `/path`, or a path relative to `base`'s directory. `None` for
/// anything else.
pub(super) fn resolve(base: &str, location: &str) -> Option<String> {
    if is_absolute(location) {
        return Some(location.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(net) = location.strip_prefix("//") {
        return Some(format!("{scheme}://{net}"));
    }
    let host_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..host_end]);
    if location.starts_with('/') {
        return Some(format!("{origin}{location}"));
    }
    if location.is_empty() || location.starts_with(['?', '#']) {
        return None;
    }
    // Relative to the base's directory (its query and fragment dropped).
    let path = rest[host_end..].split(['?', '#']).next().unwrap_or("");
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    Some(format!("{origin}{dir}{location}"))
}
