//! A remote registry over HTTP (served by `velt registry serve`, crate `velt_registry`):
//!
//! | Request | Answer |
//! |---|---|
//! | `GET  <url>/api/v1/<name>/index` | the package's `index.toml` (see [`crate::registry`]), 404 if unknown |
//! | `GET  <url>/api/v1/<name>/<version>` | the version's [`crate::archive`] |
//! | `PUT  <url>/api/v1/<name>/<version>` | publish: body = archive, `X-Velt-Checksum: sha256:…`; `Authorization: Bearer <token>` when the server requires one |
//!
//! Downloads are verified against the checksum the index (and `velt.lock`) records before they
//! are unpacked into the cache.

use std::path::{Path, PathBuf};

use crate::archive;
use crate::registry::{parse_index, Index, IndexEntry};

/// Environment variable holding the token for `velt publish` to a remote registry.
pub const TOKEN_VAR: &str = "VELT_REGISTRY_TOKEN";

fn api(url: &str, name: &str, rest: &str) -> String {
    format!("{}/api/v1/{name}/{rest}", url.trim_end_matches('/'))
}

/// The package's index; `Ok(None)` if the registry does not know it.
pub fn read_index(url: &str, name: &str) -> Result<Option<Index>, String> {
    let resp = velt_http::fetch("GET", &api(url, name, "index"), &[], b"")?;
    match resp.status {
        200 => parse_index(&resp.body_text(), &format!("{url} ({name})")).map(Some),
        404 => Ok(None),
        s => Err(format!(
            "registry {url}: {s} for `{name}`: {}",
            resp.body_text().trim()
        )),
    }
}

/// Download `name` `version`, verify it against `checksum`, and unpack it into `dest`
/// (replacing what was there).
pub fn download(
    url: &str,
    name: &str,
    version: &semver::Version,
    checksum: &str,
    dest: &Path,
) -> Result<PathBuf, String> {
    let resp = velt_http::fetch("GET", &api(url, name, &version.to_string()), &[], b"")?;
    if resp.status != 200 {
        return Err(format!(
            "registry {url}: {} downloading `{name}` {version}: {}",
            resp.status,
            resp.body_text().trim()
        ));
    }
    let actual = archive::checksum(&resp.body)?;
    if actual != checksum {
        return Err(format!(
            "checksum mismatch for `{name}` {version} from {url}: expected {checksum}, got {actual}"
        ));
    }
    if dest.exists() {
        std::fs::remove_dir_all(dest)
            .map_err(|e| format!("cannot clean `{}`: {e}", dest.display()))?;
    }
    archive::unpack(&resp.body, dest)?;
    Ok(dest.to_path_buf())
}

/// Publish the package at `root` (already validated) as `entry`.
pub fn publish(url: &str, root: &Path, name: &str, entry: &IndexEntry) -> Result<(), String> {
    let body = archive::pack(root)?;
    let token = std::env::var(TOKEN_VAR).unwrap_or_default();
    let auth = format!("Bearer {token}");
    let mut headers = vec![("X-Velt-Checksum", entry.checksum.as_str())];
    if !token.is_empty() {
        headers.push(("Authorization", auth.as_str()));
    }
    let resp = velt_http::fetch("PUT", &api(url, name, &entry.version), &headers, &body)?;
    match resp.status {
        200 | 201 => Ok(()),
        401 | 403 => Err(format!(
            "registry {url} refused the upload ({}): set ${TOKEN_VAR}",
            resp.status
        )),
        s => Err(format!("registry {url}: {s}: {}", resp.body_text().trim())),
    }
}
