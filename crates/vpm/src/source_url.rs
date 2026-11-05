//! Comparing the URLs that name a package's source, where the comparison is a security boundary:
//! an `allowNative` entry naming a git repository or a URL prefix (#2367) allows native code
//! only from there.
//!
//! Both sides are refused, not repaired, when they hold anything that could make two spellings
//! of one place compare unequal or two places compare equal: `..` or `.` segments, a backslash,
//! percent-encoded `.`, `/` or `\`, and credentials. Git URLs compare in one normal form
//! ([`git_identity`]); a URL prefix matches on a `/` boundary ([`prefix_matches`]).

/// Characters and escapes refused anywhere in a compared URL: a backslash (a separator on
/// Windows), and percent-encoded `.`, `/` and `\`, which servers may decode into a path that a
/// textual comparison never saw.
fn refuse_escapes(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if url.contains('\\') {
        return Err(format!("`{url}` contains a backslash"));
    }
    for escape in ["%2e", "%2f", "%5c"] {
        if lower.contains(escape) {
            return Err(format!(
                "`{url}` contains `{escape}` (an encoded `.`, `/` or `\\`)"
            ));
        }
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "`{url}` contains whitespace or a control character"
        ));
    }
    Ok(())
}

/// `path` (a URL's path, after the authority) has no `.` or `..` segment.
fn refuse_dot_segments(url: &str, path: &str) -> Result<(), String> {
    if path.split('/').any(|s| s == "." || s == "..") {
        return Err(format!("`{url}` has a `.` or `..` path segment"));
    }
    Ok(())
}

/// A URL split at its authority: scheme (lowercase), user (if any), host (lowercase), port (if
/// any, and not the scheme's default) and path (`/`-led, or empty).
struct Parts<'a> {
    scheme: String,
    user: Option<&'a str>,
    host: String,
    port: Option<&'a str>,
    path: &'a str,
}

fn split(url: &str) -> Result<Parts<'_>, String> {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => return split_scp(url),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((user, hostport)) => (Some(user), hostport),
        None => (None, authority),
    };
    if user.is_some_and(|u| u.contains(':')) {
        return Err(format!("`{url}` holds credentials"));
    }
    // A port is digits after the last `:` (`[::1]` has colons but no port).
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => (host, Some(port)),
        _ => (hostport, None),
    };
    let default_port = match scheme.as_str() {
        "https" => Some("443"),
        "http" => Some("80"),
        "ssh" => Some("22"),
        "git" => Some("9418"),
        _ => None,
    };
    let port = port.filter(|p| Some(*p) != default_port && !p.is_empty());
    Ok(Parts {
        scheme,
        user,
        host: host.to_ascii_lowercase(),
        port,
        path,
    })
}

/// `user@host:path`, as git reads it: SSH to `host`, `path` relative to the user's home unless
/// it starts with `/`.
fn split_scp(url: &str) -> Result<Parts<'_>, String> {
    let Some((userhost, path)) = url.split_once(':') else {
        return Err(format!("`{url}` is not a URL"));
    };
    let (user, host) = match userhost.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, userhost),
    };
    if host.is_empty() || host.contains('/') || path.is_empty() {
        return Err(format!("`{url}` is not a URL"));
    }
    Ok(Parts {
        scheme: "ssh".to_string(),
        user,
        host: host.to_ascii_lowercase(),
        port: None,
        path: path.strip_prefix('/').unwrap_or(path),
    })
}

/// One spelling for every way of writing one git repository's URL: scheme and host lowercase,
/// a default port dropped, `user@host:path` as `ssh://user@host/path`, and a trailing `/` and
/// `.git` removed. The path keeps its case (hosts differ on whether it matters). An error for a
/// URL that can't be compared safely (module docs) or that `crate::git::check_url` refuses.
pub fn git_identity(url: &str) -> Result<String, String> {
    crate::git::check_url(url)?;
    refuse_escapes(url)?;
    let parts = split(url)?;
    refuse_dot_segments(url, parts.path)?;
    let mut path = parts.path.trim_end_matches('/');
    path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/').trim_start_matches('/');
    let user = parts.user.map(|u| format!("{u}@")).unwrap_or_default();
    let port = parts.port.map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!(
        "{}://{user}{}{port}/{path}",
        parts.scheme, parts.host
    ))
}

/// Check an `allowNative` URL prefix: `https`, a host and at least one path segment under it
/// (`https://github.com/` alone would allow everything there), no credentials, query or
/// fragment, and nothing [`refuse_escapes`] or a dot segment refuses.
pub fn check_prefix(prefix: &str) -> Result<(), String> {
    let (_, path) = https_parts(prefix)?;
    if prefix.contains(['?', '#']) {
        return Err(format!("`{prefix}` has a query or a fragment"));
    }
    if path.split('/').all(str::is_empty) {
        return Err(format!(
            "`{prefix}` names a whole host: name a path under it, such as `https://example.com/owner/project/releases/download/`"
        ));
    }
    Ok(())
}

/// The lowercase `scheme://host[:port]` and the path of an `https` URL that is safe to compare.
fn https_parts(url: &str) -> Result<(String, &str), String> {
    refuse_escapes(url)?;
    if !url
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
    {
        return Err(format!("`{url}` is not an https URL"));
    }
    let parts = split(url)?;
    if parts.user.is_some() {
        return Err(format!("`{url}` holds credentials"));
    }
    if parts.host.is_empty() {
        return Err(format!("`{url}` has no host"));
    }
    let path = parts.path.split(['?', '#']).next().unwrap_or("");
    refuse_dot_segments(url, path)?;
    let port = parts.port.map(|p| format!(":{p}")).unwrap_or_default();
    Ok((format!("{}://{}{port}", parts.scheme, parts.host), path))
}

/// Whether `url` (a source as the manifest writes it) is under `prefix` (an `allowNative` entry,
/// checked by [`check_prefix`]): same scheme, host and port, and the prefix's path is the URL's
/// path or ends where one of its segments ends (`…/o/duck` never matches `…/o/duck-evil`). The
/// URL's query and fragment are not compared. An error when `url` can't be compared safely.
pub fn prefix_matches(prefix: &str, url: &str) -> Result<bool, String> {
    let (prefix_origin, prefix_path) = https_parts(prefix)?;
    let (origin, path) = https_parts(url)?;
    if prefix_origin != origin {
        return Ok(false);
    }
    let prefix_path = prefix_path.trim_end_matches('/');
    Ok(match path.strip_prefix(prefix_path) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    })
}

#[cfg(test)]
mod tests;
