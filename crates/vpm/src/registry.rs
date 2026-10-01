//! The registry: a local directory, or a remote one over HTTP ([`crate::remote`]) when
//! [`Locations::remote`] is set. The local layout is also what the registry server stores.
//!
//! Layout: `<registry>/<name>/<version>/` holds a copy of the published package (velt.toml + src/**)
//! and `<registry>/<name>/index.toml` lists every published version with its content checksum and
//! dependency requirements, so resolution only reads index files:
//!
//! ```toml
//! [[version]]
//! version = "1.0.0"
//! checksum = "sha256:…"
//! dependencies = { util = "^0.2" }
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::contents;
use crate::locations::Locations;
use crate::manifest::Manifest;

/// File name of a package's index inside the registry.
pub const INDEX_FILE: &str = "index.toml";

/// All published versions of one package.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Published versions, in publication order.
    #[serde(default, rename = "version")]
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
}

impl IndexEntry {
    /// The version as parsed semver (entries with invalid versions are skipped by [`read_index`]).
    pub fn semver(&self) -> semver::Version {
        semver::Version::parse(&self.version).expect("ICE: index versions are validated on read")
    }
}

/// Read `<name>/index.toml`; `Ok(None)` if the package was never published.
pub fn read_index(loc: &Locations, name: &str) -> Result<Option<Index>, String> {
    if let Some(url) = &loc.remote {
        return crate::remote::read_index(url, name);
    }
    let path = loc.registry.join(name).join(INDEX_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    parse_index(&text, &path.display().to_string()).map(Some)
}

/// Parse index text (`what` names its origin in errors); entries with invalid versions are
/// dropped.
pub fn parse_index(text: &str, what: &str) -> Result<Index, String> {
    let mut index: Index =
        toml::from_str(text).map_err(|e| format!("corrupt registry index `{what}`: {e}"))?;
    index
        .versions
        .retain(|v| semver::Version::parse(&v.version).is_ok());
    Ok(index)
}

fn write_index(loc: &Locations, name: &str, index: &Index) -> Result<(), String> {
    let path = loc.registry.join(name).join(INDEX_FILE);
    let text = toml::to_string(index).expect("ICE: index serialization cannot fail");
    std::fs::write(&path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// Publish the package rooted at `root` into the registry (uploading it to a remote one). Versions are immutable: publishing an
/// existing version is an error. Path-only dependencies cannot be published (importers could not
/// resolve them); a dependency with both `path` and `version` is published as its version.
pub fn publish(root: &Path, loc: &Locations) -> Result<IndexEntry, String> {
    let manifest = Manifest::from_dir(root)?;
    publish_to(root, &loc.clone().with_manifest(&manifest), &manifest)
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
            "`{name}` {version} is already published (bump `version` in velt.toml)"
        ));
    }
    if let Some(url) = &loc.remote {
        let entry = IndexEntry {
            version: version.to_string(),
            checksum: contents::checksum(root)?,
            dependencies,
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
        std::fs::write(dir.join("velt.toml"), manifest).unwrap();
        std::fs::write(
            dir.join("src/lib.vlt"),
            "export function f(): i64 { return 1; }\n",
        )
        .unwrap();
    }

    #[test]
    fn publish_records_index_and_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = Locations::under(&tmp.path().join("home"));
        let pkg = tmp.path().join("lib");
        package(&pkg, "[package]\nname = \"lib\"\nversion = \"1.0.0\"\n[dependencies]\nu = { version = \"0.2\", path = \"../u\" }\n");
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
        package(&pkg, "[package]\nname = \"lib\"\nversion = \"1.0.0\"\n[dependencies]\nu = { path = \"../u\" }\n");
        assert!(publish(&pkg, &loc).unwrap_err().contains("only a `path`"));
    }
}
