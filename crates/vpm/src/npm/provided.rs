//! The npm packages a directory provides to a git dependency ([`crate::git`]) whose target has a
//! `package.json` and no `package.vlt`: the package in its own `package.json`, as
//! `npm install git+…` installs it, and when it is a workspace's root (`pnpm-workspace.yaml`, or
//! `workspaces` in its `package.json`) every package of the workspace too.

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

/// What a directory provides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packages {
    /// The packages by name.
    pub by_name: BTreeMap<String, Provided>,
    /// The workspace pattern whose walk stopped at [`workspace::MAX_DIRS`] directories, if one
    /// did: packages past it are missing.
    pub cut: Option<String>,
}

/// The packages `dir` provides. An error when it provides none, when a workspace member's
/// `package.json` doesn't read or has no `name` (as npm refuses it), and when two packages share
/// a name (as npm and pnpm refuse it). Paths in messages are relative to `dir`.
pub fn packages_at(dir: &Path) -> Result<Packages, String> {
    let mut by_name: BTreeMap<String, Provided> = BTreeMap::new();
    let mut add = |pkg: &Path, name: String, version| {
        if let Some(first) = by_name.get(&name) {
            return Err(format!(
                "has two packages named `{name}`: `{}` and `{}`",
                shown(dir, &first.dir),
                shown(dir, pkg)
            ));
        }
        let dir = pkg.to_path_buf();
        by_name.insert(name, Provided { dir, version });
        Ok(())
    };
    let root = read(dir, dir)?;
    if let Some((Some(name), version)) = root {
        add(dir, name, version)?;
    }
    let mut cut = None;
    if let Some(patterns) = workspace::patterns(dir) {
        let listing = workspace::package_dirs(dir, &patterns);
        cut = listing.cut;
        for pkg in listing.dirs.iter().filter(|p| p.as_path() != dir) {
            match read(dir, pkg)? {
                Some((Some(name), version)) => add(pkg, name, version)?,
                _ => {
                    return Err(format!(
                        "has a workspace package without a `name`: `{}/package.json`",
                        shown(dir, pkg)
                    ))
                }
            }
        }
    }
    if by_name.is_empty() {
        return Err(
            "has neither a package.vlt nor an npm package (a package.json with a `name`, or a workspace of them)".to_string(),
        );
    }
    Ok(Packages { by_name, cut })
}

/// A `package.json`'s `name` and `version`, when it has them.
type NameAndVersion = (Option<String>, Option<semver::Version>);

/// The `name` and `version` of the `package.json` in `pkg`: `None` without one, an error when it
/// doesn't parse.
fn read(root: &Path, pkg: &Path) -> Result<Option<NameAndVersion>, String> {
    let Ok(text) = crate::probe::read_to_string(&pkg.join("package.json")) else {
        return Ok(None);
    };
    let json = Json::parse(&text).map_err(|e| {
        format!(
            "has a `package.json` that doesn't parse: `{}/package.json`: {e}",
            shown(root, pkg)
        )
    })?;
    let name = json.get("name").and_then(Json::as_str).map(str::to_string);
    let version = json
        .get("version")
        .and_then(Json::as_str)
        .and_then(|v| semver::Version::parse(v).ok());
    Ok(Some((name, version)))
}

/// `pkg` relative to `root`, for messages.
fn shown(root: &Path, pkg: &Path) -> String {
    crate::relpath::relative(pkg, root)
}
