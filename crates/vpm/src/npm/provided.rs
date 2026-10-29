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

/// The packages `dir` provides. A workspace member without a `name` is named after its directory,
/// as npm names it (`@scope/name` when its parent directory is a scope). An error when it
/// provides none, when a `package.json` can't be read or doesn't parse, and when two packages
/// share a name (as npm and pnpm refuse it). Paths in messages are relative to `dir`.
pub fn packages_at(dir: &Path) -> Result<Packages, String> {
    packages_at_limited(dir, workspace::MAX_DIRS)
}

/// [`packages_at`], listing at most `max_dirs` directories of a workspace.
pub fn packages_at_limited(dir: &Path, max_dirs: usize) -> Result<Packages, String> {
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
        let listing = workspace::package_dirs_limited(dir, &patterns, max_dirs);
        cut = listing.cut;
        for pkg in listing.dirs.iter().filter(|p| p.as_path() != dir) {
            if let Some((name, version)) = read(dir, pkg)? {
                let name = name.unwrap_or_else(|| name_of_dir(pkg));
                add(pkg, name, version)?;
            }
        }
    }
    if by_name.is_empty() {
        let mut message = "has neither a package.vlt nor an npm package (a package.json with a `name`, or a workspace of them)".to_string();
        if let Some(pattern) = &cut {
            message.push_str(&cut_note(pattern, max_dirs));
        }
        return Err(message);
    }
    Ok(Packages { by_name, cut })
}

/// A `package.json`'s `name` and `version`, when it has them.
type NameAndVersion = (Option<String>, Option<semver::Version>);

/// What a cut workspace walk leaves out, for messages.
pub fn cut_note(pattern: &str, max_dirs: usize) -> String {
    format!(
        "; the walk of its workspace pattern `{pattern}` stopped at {max_dirs} directories, so packages past it are missing"
    )
}

/// The name npm gives a workspace package without one: its directory's, under its scope when the
/// parent directory is one (`@scope/name`).
fn name_of_dir(pkg: &Path) -> String {
    let name = pkg.file_name().unwrap_or_default().to_string_lossy();
    match pkg
        .parent()
        .and_then(Path::file_name)
        .map(|p| p.to_string_lossy())
    {
        Some(scope) if scope.starts_with('@') => format!("{scope}/{name}"),
        _ => name.into_owned(),
    }
}

/// The `name` and `version` of the `package.json` in `pkg`: `None` without one, an error when it
/// can't be read or doesn't parse.
fn read(root: &Path, pkg: &Path) -> Result<Option<NameAndVersion>, String> {
    let file = pkg.join("package.json");
    if !crate::probe::is_file(&file) {
        return Ok(None);
    }
    let text = crate::probe::read_to_string(&file).map_err(|e| {
        format!(
            "has a `package.json` that can't be read: `{}/package.json`: {e}",
            shown(root, pkg)
        )
    })?;
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
