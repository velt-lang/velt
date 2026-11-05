//! Comparing the URLs that name a package's source, where the comparison is a security boundary:
//! an `allowNative` entry naming a git repository or a URL prefix (#2367) allows native code
//! only from there.
//!
//! Both sides are refused, not repaired, when they hold anything that could make two spellings
//! of one place compare unequal or two places compare equal: anything but ASCII, `..` or `.`
//! segments and any segment starting with `..` (judged before a `;`), empty segments, a
//! backslash, escapes of bytes that can change where a path goes (`.`, `/`, `\`, `;`, `?`, `#`,
//! `%`, controls, anything past ASCII), a host with a `%` or a stray `:`, and credentials. Git URLs compare in one normal form
//! ([`git_identity`]); a URL prefix matches on a `/` boundary ([`prefix_matches`]).

/// Characters and escapes refused anywhere in a compared URL: anything but ASCII (an invisible
/// or look-alike character would make a printed entry look like another repository), a
/// backslash (a separator on Windows), whitespace and control characters, and any `%XX` escape
/// that isn't two hex digits or that decodes to a byte [`escape_is_safe`] refuses: servers decode
/// escapes into a path that a textual comparison never saw.
fn refuse_escapes(url: &str) -> Result<(), String> {
    if !url.is_ascii() {
        return Err(format!(
            "`{}` contains a character that isn't ASCII",
            url.escape_default()
        ));
    }
    if url.contains('\\') {
        return Err(format!("`{url}` contains a backslash"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "`{url}` contains whitespace or a control character"
        ));
    }
    let b = url.as_bytes();
    for (i, _) in url.match_indices('%') {
        let hex = b
            .get(i + 1..i + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit));
        let Some(hex) = hex else {
            return Err(format!("`{url}` has a malformed `%` escape"));
        };
        // Two hex digits, checked: `from_str_radix` alone would take `%+1`.
        let text = std::str::from_utf8(hex).unwrap_or_default();
        let Ok(byte) = u8::from_str_radix(text, 16) else {
            return Err(format!("`{url}` has a malformed `%` escape"));
        };
        if !escape_is_safe(byte) {
            return Err(format!(
                "`{url}` contains `%{text}`, an encoded byte that can change where the path goes \
                 (`.`, `/`, `\\`, `;`, `?`, `#`, `%`, a control character or a byte past ASCII)"
            ));
        }
    }
    Ok(())
}

/// Whether a `%XX` escape may decode to `byte`: not a control byte, `.`, `/`, `\`, `;`, `?`, `#`
/// or `%` (double encoding), nor anything past ASCII (overlong or look-alike forms of `.` and
/// `/`). `%20` stays: a space can't change where a path goes, and real repository names have one.
fn escape_is_safe(byte: u8) -> bool {
    byte.is_ascii() && !byte.is_ascii_control() && !b"./\\;?#%".contains(&byte)
}

/// `path` (a URL's path after the authority, `/`-led or empty) has no `.` or `..` segment, no
/// segment starting with `..`, and no empty segment but for one trailing `/`, each judged by its
/// part before a `;` (`..;` and `.;x` are `..` and `.` to some servers): `https://h//a/b` and `https://h/a//b` are other paths on a server, and a
/// normal form that dropped the empty segments would make them equal to `/a/b`.
fn refuse_dot_segments(url: &str, path: &str) -> Result<(), String> {
    let Some(rest) = path.strip_prefix('/') else {
        return match path.is_empty() {
            true => Ok(()),
            false => Err(format!("`{url}` has a path that doesn't start with `/`")),
        };
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        return Ok(());
    }
    for segment in rest.split('/') {
        // Some servers drop what follows `;` in a segment (`..;x` is `..`).
        let name = segment.split(';').next().unwrap_or_default();
        if name == "." || name.starts_with("..") {
            return Err(format!("`{url}` has a `.` or `..` path segment"));
        }
        if name.is_empty() {
            return Err(format!(
                "`{url}` has an empty path segment (`//`, or one starting with `;`)"
            ));
        }
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
    path: String,
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
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            (host, Some(port))
        }
        _ => (hostport, None),
    };
    check_host(url, host)?;
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
        path: path.to_string(),
    })
}

/// `host` (without its port) is a name or an address with no `%`, and a `:` only inside `[…]`
/// (an IPv6 address): `github.com:8443:443` would otherwise compare as `github.com:8443`.
fn check_host(url: &str, host: &str) -> Result<(), String> {
    let ok = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(inner) => !inner.contains(['[', ']', '%']),
        None => !host.contains([':', '[', ']', '%']),
    };
    match ok {
        true => Ok(()),
        false => Err(format!(
            "`{url}` has a host that isn't a plain name or `[address]` (a `%`, or a second `:`)"
        )),
    }
}

/// `user@host:path`, as git reads it: SSH to `host`, `path` relative to the user's home unless
/// it starts with `/`. A relative path is spelled `/~/path`, as an `ssh://` URL writes the home,
/// so `git@h:srv/x` and `git@h:/srv/x` stay two repositories.
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
    check_host(url, host)?;
    let path = match path.starts_with('/') {
        true => path.to_string(),
        false => format!("/~/{path}"),
    };
    Ok(Parts {
        scheme: "ssh".to_string(),
        user,
        host: host.to_ascii_lowercase(),
        port: None,
        path,
    })
}

/// One spelling for every way of writing one git repository's URL: scheme and host lowercase,
/// a default port dropped, `user@host:/path` as `ssh://user@host/path` (and a home-relative
/// `user@host:path` as `ssh://user@host/~/path`), and a trailing `/` and a `.git` ending the
/// last segment's name removed (a segment that is only `.git` is kept: it is another path). The
/// path keeps its case (hosts differ on whether it matters). An error for a URL that can't be
/// compared safely (module docs) or that `crate::git::check_url` refuses.
pub fn git_identity(url: &str) -> Result<String, String> {
    crate::git::check_url(url)?;
    refuse_escapes(url)?;
    let parts = split(url)?;
    refuse_dot_segments(url, &parts.path)?;
    let path = parts.path.strip_suffix('/').unwrap_or(&parts.path);
    let path = match path.rsplit_once('/') {
        Some((_, last)) if last.len() > ".git".len() => path.strip_suffix(".git").unwrap_or(path),
        _ => path,
    };
    let user = parts.user.map(|u| format!("{u}@")).unwrap_or_default();
    let port = parts.port.map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!(
        "{}://{user}{}{port}{path}",
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
fn https_parts(url: &str) -> Result<(String, String), String> {
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
    Ok((
        format!("{}://{}{port}", parts.scheme, parts.host),
        path.to_string(),
    ))
}

/// Whether `url` (a source as the manifest writes it) is under `prefix` (an `allowNative` entry,
/// which must pass [`check_prefix`], or this is an error): same scheme, host and port, and the prefix's path is the URL's
/// path or ends where one of its segments ends (`…/o/duck` never matches `…/o/duck-evil`). The
/// URL's query and fragment are not compared. An error when `url` can't be compared safely.
pub fn prefix_matches(prefix: &str, url: &str) -> Result<bool, String> {
    check_prefix(prefix)?;
    let (prefix_origin, prefix_path) = https_parts(prefix)?;
    let (origin, path) = https_parts(url)?;
    if prefix_origin != origin {
        return Ok(false);
    }
    let prefix_path = prefix_path.strip_suffix('/').unwrap_or(&prefix_path);
    Ok(match path.strip_prefix(prefix_path) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    })
}

#[cfg(test)]
mod tests;
