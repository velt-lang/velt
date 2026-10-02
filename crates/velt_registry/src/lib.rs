//! A package registry server: vpm's registry directory (`<root>/<name>/index.toml`,
//! `<root>/<name>/<version>/`) served over the HTTP protocol of `vpm::remote`, so packages can be
//! shared across machines (`registry: "http://host:port"` in `package.vlt`).
//!
//! Uploads are verified before they are stored: the archive must be well-formed, its content
//! hash must equal the `X-Velt-Checksum` header, its `package.vlt` must name the package and
//! version of the URL, and versions are immutable. Native bundles (`vpm::native`) are uploaded per
//! target to a published version: verified the same way (checksum, metadata naming the package,
//! version and target), and a published target is never replaced. When a token is configured,
//! uploads need `Authorization: Bearer <token>`; downloads are always public.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use velt_http::{Handler, Request, Response};
use vpm::native::bundle;
use vpm::{archive, Locations, Manifest};

/// Largest accepted upload.
pub const MAX_ARCHIVE: usize = 64 << 20;

/// Server configuration.
#[derive(Clone, Debug)]
pub struct Registry {
    /// The registry directory.
    pub root: PathBuf,
    /// Token required for uploads (`None`: anyone who can reach the server may publish).
    pub token: Option<String>,
}

impl Registry {
    /// The HTTP handler (publishes are serialized so index updates never interleave).
    pub fn handler(self) -> Handler {
        let lock = Arc::new(Mutex::new(()));
        Arc::new(move |req: Request| {
            let _one_publish_at_a_time =
                (req.method == "PUT").then(|| lock.lock().unwrap_or_else(|e| e.into_inner()));
            self.route(&req)
        })
    }

    /// Answer one request.
    pub fn route(&self, req: &Request) -> Response {
        let parts: Vec<&str> = req.path.trim_matches('/').split('/').collect();
        match (req.method.as_str(), parts.as_slice()) {
            ("GET", [""]) => self.listing(),
            ("GET", ["api", "v1", name, "index"]) => self.index(name),
            ("GET", ["api", "v1", name, version]) => self.download(name, version),
            ("PUT", ["api", "v1", name, version]) => self.upload(req, name, version),
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

    fn index(&self, name: &str) -> Response {
        if !vpm::manifest::is_valid_package_name(name) {
            return Response::text(400, "invalid package name");
        }
        match std::fs::read(self.root.join(name).join(vpm::registry::INDEX_FILE)) {
            Ok(bytes) => Response::bytes(200, "application/toml", bytes),
            Err(_) => Response::text(404, format!("no package `{name}`")),
        }
    }

    fn download(&self, name: &str, version: &str) -> Response {
        let Some(dir) = self.package_dir(name, version) else {
            return Response::text(400, "invalid package name or version");
        };
        if !dir.is_dir() {
            return Response::text(404, format!("no `{name}` {version}"));
        }
        match archive::pack(&dir) {
            Ok(bytes) => Response::bytes(200, "application/octet-stream", bytes),
            Err(e) => Response::text(500, e),
        }
    }

    fn loc(&self, cache: PathBuf) -> Locations {
        Locations {
            registry: self.root.clone(),
            cache,
            remote: None,
        }
    }

    fn native_dir(&self, name: &str, version: &str, target: &str) -> Option<PathBuf> {
        let v = semver::Version::parse(version).ok()?;
        let ok = vpm::manifest::is_valid_package_name(name)
            && vpm::manifest::NATIVE_TARGETS.contains(&target);
        ok.then(|| self.loc(PathBuf::new()).registry_native(name, &v, target))
    }

    fn download_native(&self, name: &str, version: &str, target: &str) -> Response {
        let Some(dir) = self.native_dir(name, version, target) else {
            return Response::text(400, "invalid package name, version or target");
        };
        if !dir.is_dir() {
            return Response::text(
                404,
                format!("no {target} native library for `{name}` {version}"),
            );
        }
        match bundle::pack(&dir) {
            Ok(bytes) => Response::bytes(200, "application/octet-stream", bytes),
            Err(e) => Response::text(500, e),
        }
    }

    fn upload_native(&self, req: &Request, name: &str, version: &str, target: &str) -> Response {
        if let Some(token) = &self.token {
            if req.header("authorization") != Some(&format!("Bearer {token}")) {
                return Response::text(401, "a valid `Authorization: Bearer <token>` is required");
            }
        }
        if self.native_dir(name, version, target).is_none() {
            return Response::text(400, "invalid package name, version or target");
        }
        let Some(sum) = req.header("x-velt-checksum").map(str::to_string) else {
            return Response::text(400, "missing X-Velt-Checksum");
        };
        let staging = match staging_dir(&self.root) {
            Ok(d) => d,
            Err(e) => return Response::text(500, e),
        };
        let bundle_dir = staging.join("bundle");
        let result = bundle::unpack_verified(
            &req.body,
            &sum,
            (name, version, target),
            &bundle_dir,
            "the upload",
        )
        .map_err(|e| (400, e))
        .and_then(|()| {
            let loc = self.loc(staging.join(".cache"));
            vpm::registry::add_native_local(&loc, name, version, target, &bundle_dir).map_err(|e| {
                let status = if e.contains("never replaced") {
                    409
                } else if e.contains("is not published") {
                    404
                } else {
                    400
                };
                (status, e)
            })
        });
        let _ = std::fs::remove_dir_all(&staging);
        match result {
            Ok(_) => Response::text(
                201,
                format!("published the {target} native library of `{name}` {version}\n"),
            ),
            Err((status, msg)) => Response::text(status, msg),
        }
    }

    fn package_dir(&self, name: &str, version: &str) -> Option<PathBuf> {
        let v = semver::Version::parse(version).ok()?;
        vpm::manifest::is_valid_package_name(name).then(|| self.root.join(name).join(v.to_string()))
    }

    fn upload(&self, req: &Request, name: &str, version: &str) -> Response {
        if let Some(token) = &self.token {
            if req.header("authorization") != Some(&format!("Bearer {token}")) {
                return Response::text(401, "a valid `Authorization: Bearer <token>` is required");
            }
        }
        if self.package_dir(name, version).is_none() {
            return Response::text(400, "invalid package name or version");
        }
        match self.store(req, name, version) {
            Ok(entry) => Response::text(201, format!("published `{name}` {}\n", entry.version)),
            Err((status, msg)) => Response::text(status, msg),
        }
    }

    fn store(
        &self,
        req: &Request,
        name: &str,
        version: &str,
    ) -> Result<vpm::registry::IndexEntry, (u16, String)> {
        let bad = |m: String| (400, m);
        let sum = archive::checksum(&req.body).map_err(bad)?;
        if req.header("x-velt-checksum") != Some(sum.as_str()) {
            return Err(bad(format!(
                "checksum mismatch: the archive hashes to {sum}"
            )));
        }
        let staging = staging_dir(&self.root).map_err(|e| (500, e))?;
        let result = self.publish_staged(&req.body, &staging, name, version);
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    fn publish_staged(
        &self,
        body: &[u8],
        staging: &Path,
        name: &str,
        version: &str,
    ) -> Result<vpm::registry::IndexEntry, (u16, String)> {
        archive::unpack(body, staging).map_err(|e| (400, e))?;
        let manifest = Manifest::from_dir(staging).map_err(|e| (400, e))?;
        if manifest.package.name != name || manifest.version().to_string() != version {
            return Err((
                400,
                format!(
                    "the archive is `{}` {}, not `{name}` {version}",
                    manifest.package.name, manifest.package.version
                ),
            ));
        }
        let loc = self.loc(staging.join(".cache"));
        vpm::registry::publish_local(staging, &loc).map_err(|e| {
            let status = if e.contains("already published") {
                409
            } else {
                400
            };
            (status, e)
        })
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
