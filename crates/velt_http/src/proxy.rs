//! The proxy a request goes through, from the environment as curl reads it: `https_proxy` /
//! `HTTPS_PROXY` for `https://`, `http_proxy` for `http://` (never the uppercase `HTTP_PROXY`,
//! which a CGI program's request can set through a `Proxy:` header, "httpoxy"), else
//! `all_proxy` / `ALL_PROXY`; lowercase first, and an empty value counts as unset. `no_proxy` /
//! `NO_PROXY` lists the hosts reached directly, and this machine (`localhost`, a loopback
//! address) is always reached directly.
//!
//! A proxy is `http://[user:password@]host[:port]` (the scheme may be left out; the port is 80
//! by default). An `https://` request is tunneled through it with `CONNECT`, then TLS runs to the
//! origin as without a proxy; an `http://` request is sent to it whole (`GET http://host/path`).
//! A proxy that terminates TLS is trusted with `$VELT_CA_FILE` ([`crate::tls`]).

use std::net::IpAddr;

/// A proxy to send requests through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    /// `host:port` to connect to (`[v6]:port` for an IPv6 address).
    pub authority: String,
    /// The `Proxy-Authorization` header's value (`Basic …`), from the URL's user and password.
    pub authorization: Option<String>,
}

impl Proxy {
    /// The proxy a URL like `http://user:pw@proxy.example:3128` names. `what` names where it came
    /// from in the error (`$HTTPS_PROXY`), which never repeats the value: it may hold a password.
    pub fn parse(value: &str, what: &str) -> Result<Proxy, String> {
        let invalid = |why: &str| format!("invalid proxy in {what}: {why}");
        let value = value.trim();
        let rest = match value.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
            Some((scheme, _)) => {
                return Err(invalid(&format!(
                    "`{scheme}://` proxies are not supported (only `http://`)"
                )))
            }
            None => value,
        };
        // Up to the path, which a proxy URL may end with (`http://proxy:3128/`).
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (userinfo, hostport) = match rest[..end].rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, &rest[..end]),
        };
        if hostport.is_empty() || hostport.contains(|c: char| c.is_whitespace() || c.is_control()) {
            return Err(invalid("no host"));
        }
        let authority = match split_port(hostport) {
            Some((_, port)) if port.parse::<u16>().is_err() => return Err(invalid("bad port")),
            Some(_) => hostport.to_string(),
            None => format!("{hostport}:80"),
        };
        let authorization = match userinfo {
            Some(u) => {
                let (user, password) = u.split_once(':').unwrap_or((u, ""));
                let plain = format!("{}:{}", percent_decode(user), percent_decode(password));
                if plain.contains(|c: char| c.is_control()) {
                    return Err(invalid("a control character in the user or password"));
                }
                Some(format!("Basic {}", base64(plain.as_bytes())))
            }
            None => None,
        };
        Ok(Proxy {
            authority,
            authorization,
        })
    }
}

/// The proxy for a request to `url` as the environment sets it, or `None` to connect directly
/// (also for a URL this client does not handle: it is refused when the request is sent).
pub fn from_env(url: &str) -> Result<Option<Proxy>, String> {
    choose(url, |name| {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    })
}

/// [`from_env`] with the variables `var` gives (set and not empty).
pub fn choose(url: &str, var: impl Fn(&str) -> Option<String>) -> Result<Option<Proxy>, String> {
    let Ok(target) = crate::client::url_host(url) else {
        return Ok(None);
    };
    if target.is_loopback() {
        return Ok(None);
    }
    let names: &[&str] = if target.tls {
        &["https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"]
    } else {
        &["http_proxy", "all_proxy", "ALL_PROXY"]
    };
    let Some((name, value)) = names.iter().find_map(|n| var(n).map(|v| (*n, v))) else {
        return Ok(None);
    };
    let no_proxy = var("no_proxy")
        .or_else(|| var("NO_PROXY"))
        .unwrap_or_default();
    let port = match split_port(target.authority) {
        Some((_, p)) => p.parse().ok(),
        None => Some(if target.tls { 443 } else { 80 }),
    };
    if bypasses(&no_proxy, target.host(), port) {
        return Ok(None);
    }
    Proxy::parse(&value, &format!("${name}")).map(Some)
}

/// Whether `no_proxy` (entries separated by commas or white space, as curl and Node read it: `*`;
/// a domain, also with a leading `.` or `*.`, which takes its subdomains too; an IP address or a
/// CIDR range; each optionally with `:port`) lists `host` (a name or an address without
/// brackets) on `port`.
pub fn bypasses(no_proxy: &str, host: &str, port: Option<u16>) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let ip = host.parse::<IpAddr>().ok();
    no_proxy
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|entry| {
            let entry = entry.trim();
            if entry == "*" {
                return true;
            }
            // An entry with a port (`host:8080`, `[::1]:8080`) only covers that port; a bare IPv6
            // address (`::1`) has colons but no port.
            let (entry, want) = match split_port(entry) {
                Some((h, p)) if entry.parse::<IpAddr>().is_err() && !entry.contains('/') => {
                    match p.parse::<u16>() {
                        Ok(p) => (h, Some(p)),
                        Err(_) => return false,
                    }
                }
                _ => (entry, None),
            };
            if want.is_some() && want != port {
                return false;
            }
            let entry = entry.trim_start_matches('[').trim_end_matches(']');
            if let Some((net, bits)) = entry.split_once('/') {
                return match (ip, net.parse::<IpAddr>(), bits.parse::<u32>()) {
                    (Some(ip), Ok(net), Ok(bits)) => in_range(ip, net, bits),
                    _ => false,
                };
            }
            if let Ok(addr) = entry.parse::<IpAddr>() {
                return ip == Some(addr);
            }
            let domain = entry
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .trim_end_matches('.')
                .to_ascii_lowercase();
            !domain.is_empty()
                && (host == domain
                    || host
                        .strip_suffix(&domain)
                        .is_some_and(|sub| sub.ends_with('.')))
        })
}

/// Whether `ip` is in `net/bits`.
fn in_range(ip: IpAddr, net: IpAddr, bits: u32) -> bool {
    let mask = |width: u32| {
        if bits == 0 {
            0u128
        } else {
            u128::MAX << (width - bits.min(width))
        }
    };
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) if bits <= 32 => {
            let m = mask(32) as u32;
            u32::from(a) & m == u32::from(n) & m
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if bits <= 128 => {
            let m = mask(128);
            u128::from(a) & m == u128::from(n) & m
        }
        _ => false,
    }
}

/// `host` and `port` of `host:port` or `[v6]:port`; `None` without a port.
pub(crate) fn split_port(authority: &str) -> Option<(&str, &str)> {
    let (host, port) = authority.rsplit_once(':')?;
    if port.contains(']') || (host.contains(':') && !host.ends_with(']')) {
        return None;
    }
    Some((host, port))
}

/// `%XX` escapes decoded (a password may contain `@` or `:` written so); invalid ones are kept.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64 with padding.
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
