//! The npm packages a directory provides to a git dependency ([`crate::git`]) whose target has a
//! `package.json` and no `package.vlt`: every package of its workspace when it is a workspace's
//! root (`pnpm-workspace.yaml`, or `workspaces` in its `package.json`), else the package itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::json::Json;
use super::workspace;

/// One npm package: its directory and `version` (when it has a valid one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provided {
    pub dir: PathBuf,
    pub version: Option<semver::Version>,
}

/// The packages `dir` provides, by name; an error when it provides none.
pub fn packages_at(dir: &Path) -> Result<BTreeMap<String, Provided>, String> {
    let mut out = BTreeMap::new();
    if let Some(patterns) = workspace::patterns(dir) {
        let listing = workspace::package_dirs(dir, &patterns);
        for pkg in listing.dirs {
            if let Some((name, version)) = name_and_version(&pkg) {
                out.entry(name).or_insert(Provided { dir: pkg, version });
            }
        }
    }
    if out.is_empty() {
        if let Some((name, version)) = name_and_version(dir) {
            out.insert(
                name,
                Provided {
                    dir: dir.to_path_buf(),
                    version,
                },
            );
        }
    }
    if out.is_empty() {
        return Err(
            "has neither a package.vlt nor an npm package (a package.json with a `name`, or a workspace of them)".to_string(),
        );
    }
    Ok(out)
}

/// The `name` and `version` of the `package.json` in `dir`.
fn name_and_version(dir: &Path) -> Option<(String, Option<semver::Version>)> {
    let text = crate::probe::read_to_string(&dir.join("package.json")).ok()?;
    let json = Json::parse(&text).ok()?;
    let name = json.get("name")?.as_str()?.to_string();
    let version = json
        .get("version")
        .and_then(Json::as_str)
        .and_then(|v| semver::Version::parse(v).ok());
    Some((name, version))
}
