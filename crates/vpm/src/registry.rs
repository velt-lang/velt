//! The registry: a local directory, or a remote one over HTTP ([`crate::remote`]) when
//! [`Locations::remote`] is set. The local layout is also what the registry server stores.
//!
//! Layout: `<registry>/<name>/<version>/` holds a copy of the published package (package.vlt +
//! src/**) and `<registry>/<name>/index.json` lists every published version with its content
//! checksum and dependency requirements, so resolution only reads index files (generated JSON,
//! [`crate::json_file`]; `native_abi` and `native` only for packages with native code):
//!
//! ```json
//! {
//!   "versions": [
//!     {
//!       "version": "1.0.0",
//!       "checksum": "sha256:…",
//!       "dependencies": { "util": "^0.2" },
//!       "native_abi": 1,
//!       "native": { "x86_64-unknown-linux-gnu": "sha256:…" }
//!     }
//!   ]
//! }
//! ```
//!
//! A registry directory written by an older velt (`index.toml`) is refused with an error.
//!
//! A native bundle ([`crate::native`]) of a version is stored at
//! `<registry>/<name>/<version>.native/<triple>/`. Versions are immutable, except that a target
//! may be **added** to a published version (never replaced).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::contents;
use crate::locations::Locations;
use crate::manifest::Manifest;
use crate::native::{bundle, NativeMeta, NATIVE_ABI};

/// File name of a package's index inside the registry.
pub const INDEX_FILE: &str = "index.json";
/// The index's former (TOML) name, no longer read.
pub const LEGACY_INDEX_FILE: &str = "index.toml";

/// All published versions of one package.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Published versions, in publication order.
    #[serde(default)]
    pub versions: Vec<IndexEntry>,
}

/// One published version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Exact semver version.
    pub version: String,
    /// `sha256:<hex>` of the package contents (see [`contents::checksum`]).
    pub checksum: String,
    /// Dependency name → semver requirement.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    /// The runtime table version its native libraries need (packages with native code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_abi: Option<u32>,
    /// Target triple → checksum of the prebuilt native bundle ([`bundle::checksum`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub native: BTreeMap<String, String>,
    /// Withdrawn by an owner ([`crate::yank`]): still downloadable for lockfiles that pin it,
    /// never chosen for a new requirement.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub yanked: bool,
}

impl IndexEntry {
    /// The version as parsed semver (entries with invalid versions are skipped by [`read_index`]).
    pub fn semver(&self) -> semver::Version {
        semver::Version::parse(&self.version).expect("ICE: index versions are validated on read")
    }
}

/// Read `<name>/index.json`; `Ok(None)` if the package was never published.
pub fn read_index(loc: &Locations, name: &str) -> Result<Option<Index>, String> {
    if let Some(url) = &loc.remote {
        return crate::remote::read_index(url, name);
    }
    let path = loc.registry.join(name).join(INDEX_FILE);
    if !path.is_file() {
        let old = loc.registry.join(name).join(LEGACY_INDEX_FILE);
        if old.is_file() {
            return Err(crate::json_file::legacy_error(
                &old,
                INDEX_FILE,
                "this registry was written by an older velt; publish the package again into a new registry",
            ));
        }
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    parse_index(&text, &path.display().to_string()).map(Some)
}

/// Parse index text (`what` names its origin in errors); entries with invalid versions are
/// dropped.
pub fn parse_index(text: &str, what: &str) -> Result<Index, String> {
    let mut index: Index = crate::json_file::parse(text, &format!("registry index `{what}`"))?;
    index
        .versions
        .retain(|v| semver::Version::parse(&v.version).is_ok());
    Ok(index)
}

pub(crate) fn write_index(loc: &Locations, name: &str, index: &Index) -> Result<(), String> {
    let path = loc.registry.join(name).join(INDEX_FILE);
    std::fs::write(&path, crate::json_file::to_text(index))
        .map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// Publish the package rooted at `root` into the registry (uploading it to a remote one). Versions are immutable: publishing an
/// existing version is an error. Path-only dependencies cannot be published (importers could not
/// resolve them); a dependency with both `path` and `version` is published as its version.
pub fn publish(root: &Path, loc: &Locations) -> Result<IndexEntry, String> {
    publish_with_native(root, loc, &BTreeMap::new())
}

/// [`publish`] plus the native bundles (target triple → bundle directory) of a package with a
/// `native` object: one for every target it lists.
pub fn publish_with_native(
    root: &Path,
    loc: &Locations,
    bundles: &BTreeMap<String, PathBuf>,
) -> Result<IndexEntry, String> {
    let manifest = Manifest::from_dir(root)?;
    let loc = &loc.clone().with_manifest(&manifest);
    check_bundles(&manifest, bundles)?;
    if let Some(native) = &manifest.native {
        let lock = root.join(&native.path).join("Cargo.lock");
        if !lock.is_file() {
            return Err(format!(
                "cannot publish `{}`: `{}` is missing (commit the native crate's lockfile: builds from source use exactly it)",
                manifest.package.name,
                lock.display()
            ));
        }
        let missing: Vec<&String> = native
            .targets
            .iter()
            .filter(|t| !bundles.contains_key(*t))
            .collect();
        if !missing.is_empty() || native.targets.is_empty() {
            let list: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
            return Err(if native.targets.is_empty() {
                format!("cannot publish `{}`: `native.targets` is empty (list the targets to publish prebuilt libraries for)", manifest.package.name)
            } else {
                format!(
                    "cannot publish `{}`: no native library for {} (build it with `velt native build --target <triple>` on a machine for that target, then pass the bundles with `--native-artifacts <dir>`)",
                    manifest.package.name,
                    list.join(", ")
                )
            });
        }
    }
    let mut entry = publish_to(root, loc, &manifest)?;
    for (target, dir) in bundles {
        let sum = add_native_to(loc, &manifest.package.name, &entry.version, target, dir)?;
        entry.native.insert(target.clone(), sum);
    }
    Ok(entry)
}

/// Add native bundles for targets not yet published to an already published version (they are
/// never replaced; re-adding an identical bundle is a no-op). Returns the updated index entry.
pub fn add_native(
    root: &Path,
    loc: &Locations,
    bundles: &BTreeMap<String, PathBuf>,
) -> Result<IndexEntry, String> {
    let manifest = Manifest::from_dir(root)?;
    let loc = &loc.clone().with_manifest(&manifest);
    if manifest.native.is_none() {
        return Err(format!(
            "package `{}` has no `native` in package.vlt",
            manifest.package.name
        ));
    }
    check_bundles(&manifest, bundles)?;
    let name = &manifest.package.name;
    let version = manifest.version().to_string();
    for (target, dir) in bundles {
        add_native_to(loc, name, &version, target, dir)?;
    }
    read_index(loc, name)?
        .and_then(|index| index.versions.into_iter().find(|v| v.version == version))
        .ok_or_else(|| format!("`{name}` {version} is not published"))
}

/// Every bundle must belong to this package version and target, and need no newer table than
/// this `velt` provides.
fn check_bundles(manifest: &Manifest, bundles: &BTreeMap<String, PathBuf>) -> Result<(), String> {
    let name = &manifest.package.name;
    if manifest.native.is_none() && !bundles.is_empty() {
        return Err(format!(
            "package `{name}` has no `native` in package.vlt but native libraries were given"
        ));
    }
    for (target, dir) in bundles {
        let meta = NativeMeta::read(dir)?;
        let what = format!("`{}`", dir.display());
        let id = (
            name.as_str(),
            manifest.package.version.as_str(),
            target.as_str(),
        );
        bundle::check_meta(&meta, id, &bundle::list_files(dir)?, &what)?;
        if meta.abi > NATIVE_ABI {
            return Err(format!(
                "{what} needs native ABI {}; this velt provides {NATIVE_ABI}",
                meta.abi
            ));
        }
    }
    Ok(())
}

/// Store one bundle for a published version (`loc`'s registry, local or remote); returns its
/// checksum.
fn add_native_to(
    loc: &Locations,
    name: &str,
    version: &str,
    target: &str,
    dir: &Path,
) -> Result<String, String> {
    if let Some(url) = &loc.remote {
        return crate::remote::publish_native(url, name, version, target, dir);
    }
    add_native_local(loc, name, version, target, dir)
}

/// [`add_native`] for one bundle in a local registry: what a registry server does with an upload.
pub fn add_native_local(
    loc: &Locations,
    name: &str,
    version: &str,
    target: &str,
    dir: &Path,
) -> Result<String, String> {
    let meta = NativeMeta::read(dir)?;
    let what = format!("`{}`", dir.display());
    bundle::check_meta(
        &meta,
        (name, version, target),
        &bundle::list_files(dir)?,
        &what,
    )?;
    let sum = bundle::checksum(dir)?;
    let mut index = read_index(
        &Locations {
            remote: None,
            ..loc.clone()
        },
        name,
    )?
    .unwrap_or_default();
    let Some(entry) = index.versions.iter_mut().find(|v| v.version == version) else {
        return Err(format!("`{name}` {version} is not published"));
    };
    match entry.native.get(target) {
        Some(existing) if *existing == sum => return Ok(sum),
        Some(_) => {
            return Err(format!(
                "`{name}` {version} already has a native library for {target} (published libraries are never replaced; bump `version`)"
            ))
        }
        None => {}
    }
    let semver = entry.semver();
    let dest = loc.registry_native(name, &semver, target);
    bundle::copy(dir, &dest)?;
    let stored = bundle::checksum(&dest)?;
    if stored != sum {
        return Err(format!(
            "`{}` changed while it was published",
            dir.display()
        ));
    }
    entry.native.insert(target.to_string(), sum.clone());
    entry.native_abi = Some(entry.native_abi.unwrap_or(0).max(meta.abi));
    write_index(loc, name, &index)?;
    Ok(sum)
}

/// Publish into exactly `loc`, ignoring the package's own `registry` setting: what a registry
/// server does with an upload (whose manifest names that very server).
pub fn publish_local(root: &Path, loc: &Locations) -> Result<IndexEntry, String> {
    let manifest = Manifest::from_dir(root)?;
    let loc = Locations {
        remote: None,
        ..loc.clone()
    };
    publish_to(root, &loc, &manifest)
}

/// Publish the package itself (its native bundles are added afterwards).
fn publish_to(root: &Path, loc: &Locations, manifest: &Manifest) -> Result<IndexEntry, String> {
    let name = &manifest.package.name;
    let version = manifest.version();
    let mut dependencies = BTreeMap::new();
    for (dep, spec) in &manifest.dependencies {
        let req = spec.version().ok_or_else(|| {
            format!("cannot publish `{name}`: dependency `{dep}` has only a `path`; add a `version` requirement")
        })?;
        dependencies.insert(dep.clone(), req.to_string());
    }

    let mut index = read_index(loc, name)?.unwrap_or_default();
    if index.versions.iter().any(|v| v.semver() == version) {
        return Err(format!(
            "`{name}` {version} is already published (bump `version` in package.vlt)"
        ));
    }
    if let Some(url) = &loc.remote {
        let entry = IndexEntry {
            version: version.to_string(),
            checksum: contents::checksum(root)?,
            dependencies,
            native_abi: None,
            native: BTreeMap::new(),
            yanked: false,
        };
        crate::remote::publish(url, root, name, &entry)?;
        return Ok(entry);
    }
    let dest = loc.registry_package(name, &version);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .map_err(|e| format!("cannot clean `{}`: {e}", dest.display()))?;
    }
    contents::copy_package(root, &dest)?;
    let entry = IndexEntry {
        version: version.to_string(),
        checksum: contents::checksum(&dest)?,
        dependencies,
        native_abi: None,
        native: BTreeMap::new(),
        yanked: false,
    };
    index.versions.push(entry.clone());
    write_index(loc, name, &index)?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(dir: &Path, manifest: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join(crate::manifest::MANIFEST_FILE), manifest).unwrap();
        std::fs::write(
            dir.join("src/lib.vlt"),
            "export function f(): i64 { return 1; }\n",
        )
        .unwrap();
    }

    #[test]
    fn a_former_index_toml_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = Locations::under(&tmp.path().join("home"));
        assert_eq!(read_index(&loc, "lib").unwrap(), None);
        let dir = loc.registry.join("lib");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LEGACY_INDEX_FILE), "[[version]]\n").unwrap();
        let e = read_index(&loc, "lib").unwrap_err();
        assert!(e.contains("(the file is now `index.json`)"), "{e}");
    }

    #[test]
    fn publish_records_index_and_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = Locations::under(&tmp.path().join("home"));
        let pkg = tmp.path().join("lib");
        package(&pkg, "export const pkg: Package = { name: \"lib\", version: \"1.0.0\", dependencies: { u: { version: \"0.2\", path: \"../u\" } } };");
        let entry = publish(&pkg, &loc).unwrap();
        assert_eq!(entry.dependencies["u"], "0.2");
        assert!(loc.registry.join("lib/1.0.0/src/lib.vlt").is_file());
        let index = read_index(&loc, "lib").unwrap().unwrap();
        assert_eq!(index.versions, vec![entry]);
        assert!(publish(&pkg, &loc)
            .unwrap_err()
            .contains("already published"));
        assert_eq!(read_index(&loc, "nope").unwrap(), None);
    }

    #[test]
    fn path_only_dependency_cannot_be_published() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = Locations::under(tmp.path());
        let pkg = tmp.path().join("lib");
        package(&pkg, "export const pkg: Package = { name: \"lib\", version: \"1.0.0\", dependencies: { u: { path: \"../u\" } } };");
        assert!(publish(&pkg, &loc).unwrap_err().contains("only a `path`"));
    }
}
