//! Package and native-library downloads and uploads: each upload is checked (caller, owner,
//! checksum, manifest) in a staging directory before it reaches the registry.

use std::path::{Path, PathBuf};

use velt_http::{Request, Response};
use vpm::native::bundle;
use vpm::{archive, Manifest};

use crate::auth::{self, Caller};
use crate::{owners, staging_dir, Registry};

impl Registry {
    pub(crate) fn download(&self, name: &str, version: &str) -> Response {
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

    fn native_dir(&self, name: &str, version: &str, target: &str) -> Option<PathBuf> {
        let v = semver::Version::parse(version).ok()?;
        let ok = vpm::manifest::is_valid_package_name(name)
            && vpm::manifest::NATIVE_TARGETS.contains(&target);
        ok.then(|| self.loc(PathBuf::new()).registry_native(name, &v, target))
    }

    pub(crate) fn download_native(&self, name: &str, version: &str, target: &str) -> Response {
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

    /// The caller of a write to package `name`, once it is known to be allowed.
    fn writer(&self, req: &Request, name: &str) -> Result<Caller, Response> {
        let caller = auth::caller(&self.root, req)?;
        owners::authorize(&self.root, name, &caller)?;
        Ok(caller)
    }

    pub(crate) fn upload_native(
        &self,
        req: &Request,
        name: &str,
        version: &str,
        target: &str,
    ) -> Response {
        if self.native_dir(name, version, target).is_none() {
            return Response::text(400, "invalid package name, version or target");
        }
        if let Err(resp) = self.writer(req, name) {
            return resp;
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

    pub(crate) fn upload(&self, req: &Request, name: &str, version: &str) -> Response {
        if self.package_dir(name, version).is_none() {
            return Response::text(400, "invalid package name or version");
        }
        let caller = match self.writer(req, name) {
            Ok(c) => c,
            Err(resp) => return resp,
        };
        let stored = self.store(req, name, version).and_then(|entry| {
            match owners::claim(&self.root, name, &caller) {
                Ok(()) => Ok(entry),
                Err(e) => Err((500, e)),
            }
        });
        match stored {
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
        // Read with the limits of every manifest; messages name `package.vlt`, never the
        // server's staging directory.
        let file = vpm::manifest::MANIFEST_FILE;
        let path = staging.join(file);
        if !path.is_file() {
            return Err((400, format!("the archive has no {file}")));
        }
        let manifest =
            Manifest::from_path_shown_as(&path, Path::new(file)).map_err(|e| (400, e))?;
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
