//! A package registry server: vpm's registry directory (`<root>/<name>/index.json`,
//! `<root>/<name>/<version>/`) served over the HTTP protocol of `vpm::remote`, so packages can be
//! shared across machines (`registry: "http://host:port"` in `package.vlt`).
//!
//! Uploads are verified before they are stored ([`packages`]): the archive must be well-formed,
//! its content hash must equal the `X-Velt-Checksum` header, its `package.vlt` must be a valid
//! manifest (read without running anything, within the reader's size limits) naming the package
//! and version of the URL, and versions are immutable. An archive with a `velt.toml` is refused.
//! Native bundles (`vpm::native`) are uploaded per target to a published version: verified the
//! same way (checksum, metadata naming the package, version and target), and a published target
//! is never replaced.
//!
//! Writes (publishing, native libraries, yanking, owners) need a user's token unless the registry
//! is open ([`auth`]) and, for an existing package, one of its owners ([`owners`]). Downloads,
//! indexes, owner lists and search are public. The server speaks plain HTTP: beyond localhost,
//! put it behind a TLS reverse proxy so tokens never travel in cleartext.

pub mod auth;
pub mod owners;
mod packages;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use velt_http::{Handler, Request, Response};
use vpm::Locations;

/// Refuse a registry directory written by an older velt (packages with an `index.toml` and no
/// `index.json`): served as is, every one of its packages would answer "no package".
pub fn check_dir(root: &Path) -> Result<(), String> {
    let old = vpm::registry::legacy_packages(root);
    if old.is_empty() {
        return Ok(());
    }
    let shown: Vec<&str> = old.iter().take(5).map(String::as_str).collect();
    let more = if old.len() > shown.len() { ", …" } else { "" };
    Err(format!(
        "`{}` was written by an older velt: {} package(s) ({}{more}) have an `{}` and no `{}`, which this version reads; publish them again into a new registry directory",
        root.display(),
        old.len(),
        shown.join(", "),
        vpm::registry::LEGACY_INDEX_FILE,
        vpm::registry::INDEX_FILE,
    ))
}

/// Largest accepted upload.
pub const MAX_ARCHIVE: usize = 64 << 20;

/// Server configuration.
#[derive(Clone, Debug)]
pub struct Registry {
    /// The registry directory (users in [`auth::USERS_FILE`] inside it).
    pub root: PathBuf,
}

impl Registry {
    /// The HTTP handler (writes are serialized so index and owner updates never interleave).
    pub fn handler(self) -> Handler {
        let lock = Arc::new(Mutex::new(()));
        Arc::new(move |req: Request| {
            let _one_write_at_a_time =
                (req.method != "GET").then(|| lock.lock().unwrap_or_else(|e| e.into_inner()));
            self.route(&req)
        })
    }

    /// Answer one request.
    pub fn route(&self, req: &Request) -> Response {
        let parts: Vec<&str> = req.path.trim_matches('/').split('/').collect();
        let root = self.root.as_path();
        match (req.method.as_str(), parts.as_slice()) {
            ("GET", [""]) => self.listing(),
            ("GET", ["api", "v1", "search"]) => self.search(&req.query),
            ("GET", ["api", "v1", name, "index"]) => self.index(name),
            ("GET", ["api", "v1", name, "owners"]) => owners::list(root, name),
            ("PUT", ["api", "v1", name, "owners", user]) => {
                owners::change(root, req, name, user, true)
            }
            ("DELETE", ["api", "v1", name, "owners", user]) => {
                owners::change(root, req, name, user, false)
            }
            ("GET", ["api", "v1", name, version]) => self.download(name, version),
            ("PUT", ["api", "v1", name, version]) => self.upload(req, name, version),
            ("PUT", ["api", "v1", name, version, "yank"]) => self.yank(req, name, version, true),
            ("DELETE", ["api", "v1", name, version, "yank"]) => {
                self.yank(req, name, version, false)
            }
            ("GET", ["api", "v1", name, version, "native", target]) => {
                self.download_native(name, version, target)
            }
            ("PUT", ["api", "v1", name, version, "native", target]) => {
                self.upload_native(req, name, version, target)
            }
            _ => Response::text(404, "not found"),
        }
    }

    fn listing(&self) -> Response {
        let mut names: Vec<String> = std::fs::read_dir(&self.root)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().join(vpm::registry::INDEX_FILE).is_file())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let mut text = format!("Velt package registry: {} packages\n", names.len());
        for n in names {
            text.push_str(&format!("{n}\n"));
        }
        Response::text(200, text)
    }

    fn search(&self, query: &str) -> Response {
        let q = vpm::search::query_param(query);
        match vpm::search::search_local(&self.root, &q) {
            Ok(hits) => Response::bytes(
                200,
                "application/json",
                vpm::search::to_json(&hits).into_bytes(),
            ),
            Err(e) => Response::text(500, e),
        }
    }

    fn index(&self, name: &str) -> Response {
        if !vpm::manifest::is_valid_package_name(name) {
            return Response::text(400, "invalid package name");
        }
        match std::fs::read(self.root.join(name).join(vpm::registry::INDEX_FILE)) {
            Ok(bytes) => Response::bytes(200, "application/json", bytes),
            Err(_) => match vpm::registry::legacy_index_error(&self.root, name) {
                Some(e) => Response::text(500, e),
                None => Response::text(404, format!("no package `{name}`")),
            },
        }
    }

    fn yank(&self, req: &Request, name: &str, version: &str, yanked: bool) -> Response {
        if self.package_dir(name, version).is_none() {
            return Response::text(400, "invalid package name or version");
        }
        let result = auth::caller(&self.root, req)
            .and_then(|caller| owners::authorize(&self.root, name, &caller))
            .and_then(|()| {
                let loc = self.loc(PathBuf::new());
                vpm::yank::yank_local(&loc, name, version, yanked).map_err(|e| {
                    let missing = e.contains("not in the registry") || e.contains("not published");
                    Response::text(if missing { 404 } else { 500 }, e)
                })
            });
        match result {
            Ok(()) if yanked => Response::text(200, format!("yanked `{name}` {version}\n")),
            Ok(()) => Response::text(200, format!("unyanked `{name}` {version}\n")),
            Err(resp) => resp,
        }
    }

    fn loc(&self, cache: PathBuf) -> Locations {
        Locations {
            registry: self.root.clone(),
            cache,
            remote: None,
        }
    }

    fn package_dir(&self, name: &str, version: &str) -> Option<PathBuf> {
        let v = semver::Version::parse(version).ok()?;
        vpm::manifest::is_valid_package_name(name).then(|| self.root.join(name).join(v.to_string()))
    }
}

/// A fresh directory for an upload being checked (inside the registry, so it is on the same
/// file system, and hidden from the listing).
fn staging_dir(root: &Path) -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = root.join(".staging").join(format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests;
