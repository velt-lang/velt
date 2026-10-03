//! Registry tokens: `velt login <url>` stores the user's token for one registry in
//! `$VELT_HOME/credentials.json` (mode 0600 on Unix), keyed by the registry's URL, and vpm sends
//! it only to that registry. `$VELT_REGISTRY_TOKEN` overrides the file (CI). A token never travels
//! over plain `http://` except to this machine.
//!
//! ```json
//! {
//!   "registries": {
//!     "https://registry.example.com": { "token": "…" }
//!   }
//! }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::json_file;
use crate::locations::{is_url, velt_home};

/// The file in `$VELT_HOME`.
pub const CREDENTIALS_FILE: &str = "credentials.json";

/// Environment variable whose token overrides the stored ones (for CI).
pub const TOKEN_VAR: &str = "VELT_REGISTRY_TOKEN";

/// The contents of `credentials.json`. `Debug` never prints a token.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// Registry URL ([`registry_key`]) → its credential.
    #[serde(default)]
    pub registries: BTreeMap<String, Credential>,
}

/// The user's credential for one registry. `Debug` never prints the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    /// The token `velt registry user add` printed.
    pub token: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential").field("token", &"…").finish()
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("registries", &self.registries)
            .finish()
    }
}

/// `$VELT_HOME/credentials.json`.
pub fn default_path() -> Result<PathBuf, String> {
    Ok(velt_home()?.join(CREDENTIALS_FILE))
}

/// The key a registry URL is stored under: scheme and host lowercased, no trailing `/`, so
/// `HTTPS://Reg.example.com/` and `https://reg.example.com` share a token.
pub fn registry_key(url: &str) -> Result<String, String> {
    if !is_url(&url.to_ascii_lowercase()) {
        return Err(format!(
            "`{url}` is not a registry URL (http:// or https://)"
        ));
    }
    let url = url.trim_end_matches('/');
    let (scheme, rest) = url
        .split_once("://")
        .expect("ICE: is_url checked the scheme");
    let (host, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    if host.is_empty() {
        return Err(format!("`{url}` has no host"));
    }
    Ok(format!(
        "{}://{}{path}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

impl Credentials {
    /// The file at `path`; empty when there is none.
    pub fn read(path: &Path) -> Result<Credentials, String> {
        match std::fs::read_to_string(path) {
            // serde's message could quote the file's text, tokens included: only the position.
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                format!(
                    "invalid `{}` at line {}, column {}: fix or delete it, then run `velt login` again",
                    path.display(),
                    e.line(),
                    e.column()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
            Err(e) => Err(format!("cannot read `{}`: {e}", path.display())),
        }
    }

    /// Write the file (atomically, readable only by its owner).
    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
        }
        json_file::write_private(path, self)
    }

    /// The stored token of the registry at `url`.
    pub fn token(&self, url: &str) -> Option<&str> {
        let key = registry_key(url).ok()?;
        self.registries.get(&key).map(|c| c.token.as_str())
    }
}

/// `velt login`: store `token` for the registry at `url` in the file at `path`.
pub fn login(path: &Path, url: &str, token: &str) -> Result<(), String> {
    let key = registry_key(url)?;
    let token = token.trim();
    if token.is_empty() {
        return Err("the token is empty".into());
    }
    // A token goes into an HTTP header: never quoted back, since it may be a real one.
    if token.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(
            "the token contains a space, a line break or another control character; paste only the token that `velt registry user add` printed"
                .into(),
        );
    }
    check_transport(&key, "a registry token")?;
    let mut creds = Credentials::read(path)?;
    creds.registries.insert(
        key,
        Credential {
            token: token.to_string(),
        },
    );
    creds.write(path)
}

/// `velt logout`: forget the token of the registry at `url`; whether there was one.
pub fn logout(path: &Path, url: &str) -> Result<bool, String> {
    let key = registry_key(url)?;
    let mut creds = Credentials::read(path)?;
    if creds.registries.remove(&key).is_none() {
        return Ok(false);
    }
    creds.write(path)?;
    Ok(true)
}

/// The token to send to the registry at `url`: `$VELT_REGISTRY_TOKEN` when set, else the one
/// stored for exactly that registry in the credentials file at `path` (`None`: no file).
/// Refused over plain `http://` to another machine.
pub fn token_for(path: Option<&Path>, url: &str) -> Result<Option<String>, String> {
    let (token, what) = match std::env::var(TOKEN_VAR) {
        Ok(t) if !t.trim().is_empty() => (Some(t.trim().to_string()), format!("${TOKEN_VAR}")),
        _ => match path {
            Some(path) => (
                Credentials::read(path)?.token(url).map(str::to_string),
                "the token `velt login` stored".to_string(),
            ),
            None => (None, String::new()),
        },
    };
    if token.is_some() {
        check_transport(url, &what)?;
    }
    Ok(token)
}

/// A token may travel only over `https://`, or plain `http://` to this machine (`localhost`,
/// `127.0.0.0/8`, `[::1]`): anyone on the network path could read it otherwise. `what` says
/// whose token it is, for the message.
pub fn check_transport(url: &str, what: &str) -> Result<(), String> {
    if is_tls_or_loopback(url) {
        return Ok(());
    }
    Err(format!(
        "refusing to send {what} to {url} over plain http://: anyone on the way could read it; only an https:// registry, or http:// on this machine, gets a token (serve the registry over https://, with a TLS reverse proxy in front of `velt registry serve`)"
    ))
}

/// Whether `url` is `https://`, or plain `http://` to this machine (`localhost` or a loopback
/// address). The host is read by [`velt_http::url_host`], the HTTP client's own parser, so the
/// rule and the connection agree on it; a URL it refuses is neither.
pub fn is_tls_or_loopback(url: &str) -> bool {
    velt_http::url_host(url).is_ok_and(|h| h.tls || h.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_ignore_case_of_scheme_and_host_and_trailing_slashes() {
        let key = registry_key("HTTPS://Reg.Example.com/").unwrap();
        assert_eq!(key, "https://reg.example.com");
        assert_eq!(
            registry_key("https://reg.example.com/Team/").unwrap(),
            "https://reg.example.com/Team"
        );
        assert!(registry_key("reg.example.com").is_err());
        assert!(registry_key("https:///x").is_err());
    }

    #[test]
    fn tokens_are_never_printed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(CREDENTIALS_FILE);
        login(&path, "https://a.example.com", "tok-s3cret").unwrap();
        let creds = Credentials::read(&path).unwrap();
        let shown = format!("{creds:?} {:?}", creds.registries["https://a.example.com"]);
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(
            shown.contains("https://a.example.com") && shown.contains('…'),
            "{shown}"
        );
        for bad in [
            "tok s3cret",
            "tok\ts3cret",
            "tok\u{7f}s3cret",
            "tok\r\ns3cret",
        ] {
            let e = login(&path, "https://b.example.com", bad).unwrap_err();
            assert!(
                e.contains("a space, a line break or another control character"),
                "{e}"
            );
            assert!(!e.contains("s3cret"), "the token is not quoted: {e}");
        }
        assert_eq!(Credentials::read(&path).unwrap().registries.len(), 1);
    }

    #[test]
    fn a_damaged_file_is_reported_without_its_text() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(CREDENTIALS_FILE);
        std::fs::write(
            &path,
            "{\"registries\": {\"https://a\": {\"token\": 42, \"x\": \"s3cret\"}}}",
        )
        .unwrap();
        let e = Credentials::read(&path).unwrap_err();
        assert!(e.contains("at line 1, column"), "{e}");
        assert!(!e.contains("s3cret") && !e.contains("integer"), "{e}");
    }

    #[test]
    fn login_logout_and_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("home").join(CREDENTIALS_FILE);
        assert_eq!(Credentials::read(&path).unwrap(), Credentials::default());
        login(&path, "https://a.example.com/", " tok-a\n").unwrap();
        login(&path, "http://127.0.0.1:4873", "tok-local").unwrap();
        let creds = Credentials::read(&path).unwrap();
        assert_eq!(creds.token("https://A.example.com"), Some("tok-a"));
        assert_eq!(creds.token("http://127.0.0.1:4873/"), Some("tok-local"));
        // Only the registry it was stored for gets it.
        assert_eq!(creds.token("https://b.example.com"), None);
        assert_eq!(creds.token("https://a.example.com.evil.net"), None);
        assert_eq!(creds.token("http://a.example.com"), None);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"https://a.example.com\": {\n      \"token\": \"tok-a\""),
            "{text}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(logout(&path, "https://a.example.com").unwrap());
        assert!(!logout(&path, "https://a.example.com").unwrap());
        assert_eq!(Credentials::read(&path).unwrap().registries.len(), 1);
        assert!(login(&path, "https://a.example.com", "  ").is_err());
        // A plain-http registry on another machine never gets a token.
        let e = login(&path, "http://reg.example.com", "t").unwrap_err();
        assert!(e.contains("refusing to send a registry token"), "{e}");
    }

    #[test]
    fn tokens_travel_only_over_tls_or_to_this_machine() {
        for url in [
            "https://registry.example.com",
            "HTTPS://registry.example.com",
            "http://127.0.0.1:8091",
            "HTTP://localhost",
            "http://127.1.2.3",
            "http://localhost:8091/",
            "http://LOCALHOST",
            "http://[::1]:8091",
        ] {
            check_transport(url, "t").unwrap_or_else(|e| panic!("{url}: {e}"));
        }
        for url in [
            "http://registry.example.com",
            "http://192.168.1.10:8091",
            "http://10.0.0.5:4873",
            "http://[2001:db8::1]:8091",
            "http://localhost.example.com",
            "http://localhost.evil.net",
            "http://127.0.0.1@evil.example.com",
            "http://localhost?.attacker.example/",
            "http://localhost#.attacker.example/",
            "http://localhost@attacker.example/",
            "http://user@localhost/",
            "http://localhost\\.attacker.example/",
            "HTTP://registry.example.com",
            "ftp://registry.example.com",
            "registry.example.com",
        ] {
            let err = check_transport(url, "$VELT_REGISTRY_TOKEN").unwrap_err();
            assert!(
                err.starts_with("refusing to send $VELT_REGISTRY_TOKEN to"),
                "{url}: {err}"
            );
            assert!(err.contains("https://"), "{url}: {err}");
        }
    }
}
