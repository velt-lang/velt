//! Connection URLs: `redis://[[user]:password@]host[:port][/db]` and `rediss://…` (TLS), as
//! ioredis and redis-cli read them. The user name and password are percent-decoded; IPv6 hosts
//! are written in brackets. `?db=N` is accepted as an alternative to the path.

/// Where and how to connect.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// `rediss://`: TLS.
    pub tls: bool,
    /// Host name or IP address (without brackets).
    pub host: String,
    /// TCP port (6379 by default).
    pub port: u16,
    /// ACL user name for `AUTH` (None = the default user).
    pub user: Option<String>,
    /// Password for `AUTH` (None = no `AUTH`).
    pub password: Option<String>,
    /// Database index for `SELECT` (0 = none sent).
    pub db: i64,
}

/// Parse a connection URL; errors are messages for `EINVAL`.
pub fn parse(url: &str) -> Result<Target, String> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("redis://") {
        (false, r)
    } else if let Some(r) = url.strip_prefix("rediss://") {
        (true, r)
    } else {
        return Err(format!(
            "invalid Redis URL {url:?}: expected redis:// or rediss://"
        ));
    };
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (user, password) = credentials(userinfo)?;
    let (host, port) = host_port(hostport)?;
    let mut db = database(path)?;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        if let Some(v) = pair.strip_prefix("db=") {
            db = database(v)?;
        }
    }
    Ok(Target {
        tls,
        host,
        port,
        user,
        password,
        db,
    })
}

type Credentials = (Option<String>, Option<String>);

fn credentials(userinfo: Option<&str>) -> Result<Credentials, String> {
    let Some(info) = userinfo else {
        return Ok((None, None));
    };
    let (user, password) = match info.split_once(':') {
        Some((u, p)) => (decode(u)?, Some(decode(p)?)),
        // `redis://secret@host` means a password, as in ioredis.
        None => (String::new(), Some(decode(info)?)),
    };
    Ok(((!user.is_empty()).then_some(user), password))
}

fn host_port(hostport: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (h, after) = v6
            .split_once(']')
            .ok_or("invalid Redis URL: unclosed '[' in the host")?;
        (h, after.strip_prefix(':'))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    let host = if host.is_empty() { "127.0.0.1" } else { host };
    let port = match port {
        None | Some("") => 6379,
        Some(p) => p
            .parse()
            .map_err(|_| format!("invalid Redis URL: bad port {p:?}"))?,
    };
    Ok((host.to_string(), port))
}

fn database(path: &str) -> Result<i64, String> {
    if path.is_empty() {
        return Ok(0);
    }
    path.parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| format!("invalid Redis URL: bad database index {path:?}"))
}

/// Percent-decode a URL component.
fn decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or("invalid Redis URL: bad percent escape")?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "invalid Redis URL: credentials are not UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let t = parse("redis://127.0.0.1:6379").unwrap();
        assert_eq!(
            (t.tls, t.host.as_str(), t.port, t.db),
            (false, "127.0.0.1", 6379, 0)
        );
        assert_eq!((t.user, t.password), (None, None));
        let t = parse("rediss://al%40x:p%3Ass@cache.example.com/3").unwrap();
        assert_eq!(
            (t.tls, t.host.as_str(), t.port, t.db),
            (true, "cache.example.com", 6379, 3)
        );
        assert_eq!(t.user.as_deref(), Some("al@x"));
        assert_eq!(t.password.as_deref(), Some("p:ss"));
        let t = parse("redis://:pw@[::1]:7000?db=2").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.db), ("::1", 7000, 2));
        assert_eq!((t.user, t.password.as_deref()), (None, Some("pw")));
        assert_eq!(
            parse("redis://secret@h").unwrap().password.as_deref(),
            Some("secret")
        );
        assert_eq!(parse("redis://").unwrap().host, "127.0.0.1");
    }

    #[test]
    fn rejects_bad_urls() {
        for bad in [
            "http://x",
            "redis://h:99999",
            "redis://h/x",
            "redis://[::1",
            "redis://%zz@h",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
