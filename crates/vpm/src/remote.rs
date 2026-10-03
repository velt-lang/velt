//! A remote registry over HTTP (served by `velt registry serve`, crate `velt_registry`):
//!
//! | Request | Answer |
//! |---|---|
//! | `GET  <url>/api/v1/<name>/index` | the package's `index.json` (see [`crate::registry`]), 404 if unknown |
//! | `GET  <url>/api/v1/<name>/<version>` | the version's [`crate::archive`] |
//! | `PUT  <url>/api/v1/<name>/<version>` | publish: body = archive, `X-Velt-Checksum: sha256:…`; `Authorization: Bearer <token>` when the server requires one |
//! | `GET  <url>/api/v1/<name>/<version>/native/<triple>` | a native bundle ([`crate::native::bundle`]) |
//! | `PUT  <url>/api/v1/<name>/<version>/native/<triple>` | add a native bundle to a published version (same headers; never replaces one) |
//! | `PUT` / `DELETE <url>/api/v1/<name>/<version>/yank` | yank / unyank a version ([`crate::yank`]) |
//! | `GET  <url>/api/v1/<name>/owners` | the package's owners, one per line |
//! | `PUT` / `DELETE <url>/api/v1/<name>/owners/<user>` | add / remove an owner |
//! | `GET  <url>/api/v1/search?q=<text>` | packages whose name contains the text ([`crate::search`]), as JSON |
//!
//! Every write sends `Authorization: Bearer $VELT_REGISTRY_TOKEN`, the user's own token; a server
//! with users answers 401 without a valid one and 403 when the user does not own the package.
//! Downloads are verified against the checksum the index (and `velt.lock.json`) records before they
//! are unpacked into the cache.

use std::path::{Path, PathBuf};

use crate::archive;
use crate::native::bundle;
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
        // A server from before the index was JSON answers with `index.toml`, typed as TOML.
        200 if is_toml(resp.header("content-type")) => Err(format!(
            "registry {url} answered with an `index.toml` for `{name}` (an older `velt registry serve`): upgrade the server, which now serves `index.json`"
        )),
        200 => parse_index(&resp.body_text(), &format!("{url} ({name})")).map(Some),
        404 => Ok(None),
        s => Err(format!(
            "registry {url}: {s} for `{name}`: {}",
            resp.body_text().trim()
        )),
    }
}

/// Whether a `Content-Type` is TOML's (`application/toml`, parameters ignored).
fn is_toml(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|t| {
        t.split(';')
            .next()
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/toml"))
    })
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

/// Download the native bundle of `name` `version` for `target`, verify it against `checksum`
/// and its metadata, and unpack it into `dest` (replacing what was there).
pub fn download_native(
    url: &str,
    name: &str,
    version: &semver::Version,
    target: &str,
    checksum: &str,
    dest: &Path,
) -> Result<(), String> {
    let path = format!("{version}/native/{target}");
    let resp = velt_http::fetch("GET", &api(url, name, &path), &[], b"")?;
    if resp.status != 200 {
        return Err(format!(
            "registry {url}: {} downloading the {target} native library of `{name}` {version}: {}",
            resp.status,
            resp.body_text().trim()
        ));
    }
    let id = (name, &*version.to_string(), target);
    bundle::unpack_verified(&resp.body, checksum, id, dest, url)
}

/// Upload the bundle at `dir` as the `target` library of the published `name` `version`;
/// returns its checksum.
pub fn publish_native(
    url: &str,
    name: &str,
    version: &str,
    target: &str,
    dir: &Path,
) -> Result<String, String> {
    let body = bundle::pack(dir)?;
    let sum = bundle::checksum(dir)?;
    put(
        url,
        &api(url, name, &format!("{version}/native/{target}")),
        &sum,
        &body,
    )?;
    Ok(sum)
}

/// Publish the package at `root` (already validated) as `entry`.
pub fn publish(url: &str, root: &Path, name: &str, entry: &IndexEntry) -> Result<(), String> {
    let body = archive::pack(root)?;
    put(url, &api(url, name, &entry.version), &entry.checksum, &body)
}

fn put(url: &str, target: &str, checksum: &str, body: &[u8]) -> Result<(), String> {
    write(url, "PUT", target, &[("X-Velt-Checksum", checksum)], body).map(drop)
}

/// Send a write request with the user's token; `Ok` with the answer's text on 2xx.
fn write(
    url: &str,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<String, String> {
    let token = std::env::var(TOKEN_VAR).unwrap_or_default();
    let auth = format!("Bearer {token}");
    let mut headers = headers.to_vec();
    if !token.is_empty() {
        headers.push(("Authorization", auth.as_str()));
    }
    let resp = velt_http::fetch(method, target, &headers, body)?;
    let text = resp.body_text().trim().to_string();
    match resp.status {
        200..=299 => Ok(text),
        401 => Err(format!(
            "registry {url} refused the request ({text}): set ${TOKEN_VAR} to your token"
        )),
        s => Err(format!("registry {url}: {s}: {text}")),
    }
}

/// Yank (`yanked`) or unyank `name` `version`.
pub fn yank(url: &str, name: &str, version: &str, yanked: bool) -> Result<(), String> {
    let method = if yanked { "PUT" } else { "DELETE" };
    write(
        url,
        method,
        &api(url, name, &format!("{version}/yank")),
        &[],
        b"",
    )
    .map(drop)
}

/// The owners of `name`.
pub fn owners(url: &str, name: &str) -> Result<Vec<String>, String> {
    check_names(&[name])?;
    let resp = velt_http::fetch("GET", &api(url, name, "owners"), &[], b"")?;
    match resp.status {
        200 => Ok(resp.body_text().lines().map(str::to_string).collect()),
        404 => Err(format!("`{name}` is not in the registry {url}")),
        s => Err(format!("registry {url}: {s}: {}", resp.body_text().trim())),
    }
}

/// Add (`add`) or remove `user` as an owner of `name`.
pub fn set_owner(url: &str, name: &str, user: &str, add: bool) -> Result<(), String> {
    check_names(&[name, user])?;
    let method = if add { "PUT" } else { "DELETE" };
    write(
        url,
        method,
        &api(url, name, &format!("owners/{user}")),
        &[],
        b"",
    )
    .map(drop)
}

/// Package and user names go into URL paths: only `[a-z][a-z0-9_-]*` ones are sent.
fn check_names(names: &[&str]) -> Result<(), String> {
    match names
        .iter()
        .find(|n| !crate::manifest::is_valid_package_name(n))
    {
        Some(bad) => Err(format!(
            "invalid name `{bad}` (use lowercase letters, digits, `-` and `_`, starting with a letter)"
        )),
        None => Ok(()),
    }
}

/// The registry's answer to a search for `query`.
pub fn search(url: &str, query: &str) -> Result<Vec<crate::search::Hit>, String> {
    let target = format!(
        "{}/api/v1/search?q={}",
        url.trim_end_matches('/'),
        crate::search::encode_query(query)
    );
    let resp = velt_http::fetch("GET", &target, &[], b"")?;
    if resp.status != 200 {
        return Err(format!(
            "registry {url}: {} searching: {}",
            resp.status,
            resp.body_text().trim()
        ));
    }
    crate::search::from_json(&resp.body_text()).map_err(|e| format!("registry {url}: {e}"))
}
