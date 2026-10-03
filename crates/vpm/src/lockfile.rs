//! `velt.lock.json`: the exact versions, sources and content hashes a package was resolved to.
//! Generated JSON ([`crate::json_file`]); nobody edits it.
//!
//! ```json
//! {
//!   "version": 1,
//!   "packages": [
//!     {
//!       "name": "json",
//!       "version": "1.2.0",
//!       "source": "registry",
//!       "checksum": "sha256:…",
//!       "dependencies": ["util"]
//!     },
//!     { "name": "util", "version": "0.1.0", "source": "path+../util" },
//!     {
//!       "name": "sqlite",
//!       "version": "0.1.0",
//!       "source": "registry",
//!       "checksum": "sha256:…",
//!       "native": { "aarch64-apple-darwin": "sha256:…", "x86_64-unknown-linux-gnu": "sha256:…" }
//!     }
//!   ]
//! }
//! ```
//! `native` pins the prebuilt library of **every** published target, so the lockfile is the same
//! on every platform and each machine verifies the one it uses.
//! Path sources are relative to the root package, `/`-separated. The root package itself is not
//! listed (its dependencies come from package.vlt). The former `velt.lock` (TOML) is not read: a
//! package that still has one gets an error saying to run `velt install`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name of the lockfile at the root package.
pub const LOCK_FILE: &str = "velt.lock.json";
/// The lockfile's former (TOML) name, no longer read.
pub const LEGACY_LOCK_FILE: &str = "velt.lock";
const FORMAT_VERSION: u32 = 1;

/// A whole lockfile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    /// Lockfile format version (currently 1).
    pub version: u32,
    /// Resolved packages, sorted by name.
    #[serde(default)]
    pub packages: Vec<LockedPackage>,
}

/// One resolved package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackage {
    /// Package name.
    pub name: String,
    /// Exact version.
    pub version: String,
    /// `"registry"` or `"path+<dir relative to the root package>"`.
    pub source: String,
    /// Content checksum (registry packages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    /// Names of this package's direct dependencies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    /// Registry packages with native code: target triple → checksum of its prebuilt library.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub native: BTreeMap<String, String>,
}

/// The `source` value for registry packages.
pub const REGISTRY_SOURCE: &str = "registry";

impl Lockfile {
    /// A lockfile over `packages` (sorted by name for stable diffs).
    pub fn new(mut packages: Vec<LockedPackage>) -> Lockfile {
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Lockfile {
            version: FORMAT_VERSION,
            packages,
        }
    }

    /// Parse lockfile text.
    pub fn parse(text: &str) -> Result<Lockfile, String> {
        let lock: Lockfile = crate::json_file::parse(text, LOCK_FILE)?;
        if lock.version != FORMAT_VERSION {
            return Err(format!(
                "unsupported {LOCK_FILE} format version {} (expected {FORMAT_VERSION})",
                lock.version
            ));
        }
        Ok(lock)
    }

    /// Read `<root>/velt.lock.json`; `Ok(None)` if there is none (an error if only the former
    /// `velt.lock` is there).
    pub fn read(root: &Path) -> Result<Option<Lockfile>, String> {
        let path = root.join(LOCK_FILE);
        if !path.is_file() {
            let old = root.join(LEGACY_LOCK_FILE);
            if old.is_file() {
                return Err(crate::json_file::legacy_error(
                    &old,
                    LOCK_FILE,
                    "delete it and run `velt install` to write the new one",
                ));
            }
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        Lockfile::parse(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Write `<root>/velt.lock.json`.
    pub fn write(&self, root: &Path) -> Result<(), String> {
        crate::json_file::write_atomic(&root.join(LOCK_FILE), &self.to_json())
    }

    /// The file's text.
    pub fn to_json(&self) -> String {
        crate::json_file::to_text(self)
    }

    /// The locked entry for `name`, if any.
    pub fn get(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let lock = Lockfile::new(vec![
            LockedPackage {
                name: "util".into(),
                version: "0.1.0".into(),
                source: "path+../util".into(),
                checksum: None,
                dependencies: vec![],
                native: BTreeMap::new(),
            },
            LockedPackage {
                name: "json".into(),
                version: "1.2.0".into(),
                source: REGISTRY_SOURCE.into(),
                checksum: Some("sha256:00".into()),
                dependencies: vec!["util".into()],
                native: BTreeMap::from([("x86_64-unknown-linux-gnu".into(), "sha256:01".into())]),
            },
        ]);
        assert_eq!(lock.packages[0].name, "json");
        let text = lock.to_json();
        assert!(
            text.starts_with("{\n  \"version\": 1,\n  \"packages\": [\n"),
            "{text}"
        );
        assert!(text.ends_with("}\n"));
        assert_eq!(Lockfile::parse(&text).unwrap(), lock);
        assert_eq!(lock.get("util").unwrap().version, "0.1.0");
    }

    #[test]
    fn rejects_unknown_format() {
        assert!(Lockfile::parse("{\"version\": 9}")
            .unwrap_err()
            .contains("format version"));
        assert!(Lockfile::parse("version = 1\n")
            .unwrap_err()
            .contains("invalid velt.lock.json"));
    }

    #[test]
    fn a_former_velt_lock_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(Lockfile::read(tmp.path()).unwrap(), None);
        std::fs::write(tmp.path().join(LEGACY_LOCK_FILE), "version = 1\n").unwrap();
        let err = Lockfile::read(tmp.path()).unwrap_err();
        assert!(
            err.contains("is no longer read (the file is now `velt.lock.json`)"),
            "{err}"
        );
        assert!(err.contains("velt install"), "{err}");
        Lockfile::new(vec![]).write(tmp.path()).unwrap();
        assert!(Lockfile::read(tmp.path()).unwrap().is_some());
    }
}
