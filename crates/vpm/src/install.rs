//! `velt install` (and the implicit install before a package build): resolve, verify against and
//! update `velt.lock`, fetch registry packages into the cache, and produce the [`PackageGraph`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache;
use crate::graph::PackageGraph;
use crate::locations::Locations;
use crate::lockfile::{LockedPackage, Lockfile, LOCK_FILE, REGISTRY_SOURCE};
use crate::manifest::Manifest;
use crate::relpath;
use crate::resolve::{resolve, Resolution, Source};

/// How to treat the existing lockfile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Fail instead of changing `velt.lock` (CI mode).
    pub locked: bool,
    /// Ignore locked versions and pick the newest compatible ones (`velt update`).
    pub update: bool,
}

/// The outcome of an install.
#[derive(Clone, Debug)]
pub struct Installed {
    /// Package graph for the compiler (root package first).
    pub graph: PackageGraph,
    /// The lockfile now on disk.
    pub lockfile: Lockfile,
    /// Whether `velt.lock` was created or changed.
    pub lock_changed: bool,
}

/// Install the dependencies of the package rooted at `root`.
pub fn install(root: &Path, loc: &Locations, opts: InstallOptions) -> Result<Installed, String> {
    let root = relpath::absolute(root);
    let manifest = Manifest::from_dir(&root)?;
    let loc = &loc.clone().with_manifest(&manifest);
    let existing = Lockfile::read(&root)?;
    let prefer = if opts.update { None } else { existing.as_ref() };
    let resolution = resolve(&root, &manifest, loc, prefer)?;
    let lockfile = to_lockfile(&resolution, &root);
    let lock_changed = existing.as_ref() != Some(&lockfile);
    if lock_changed && opts.locked {
        let what = if existing.is_some() {
            "needs to be updated"
        } else {
            "does not exist"
        };
        return Err(format!(
            "{LOCK_FILE} {what}, but --locked was passed (run `velt install` without --locked)"
        ));
    }
    if let Some(old) = &existing {
        check_locked_checksums(old, &lockfile)?;
    }

    let mut dirs = BTreeMap::new();
    for pkg in resolution.packages.values() {
        let dir = match &pkg.source {
            Source::Registry { checksum } => cache::fetch(loc, &pkg.name, &pkg.version, checksum)?,
            Source::Path { dir } => dir.clone(),
        };
        dirs.insert(pkg.name.clone(), dir);
    }
    if lock_changed {
        lockfile.write(&root)?;
    }
    let graph = build_graph(&manifest, &root, &resolution, &dirs);
    Ok(Installed {
        graph,
        lockfile,
        lock_changed,
    })
}

/// A registry package whose version is unchanged must still have the locked content hash.
fn check_locked_checksums(old: &Lockfile, new: &Lockfile) -> Result<(), String> {
    for pkg in &new.packages {
        let Some(locked) = old.get(&pkg.name) else {
            continue;
        };
        if locked.version == pkg.version
            && locked.source == pkg.source
            && locked.checksum != pkg.checksum
        {
            return Err(format!(
                "checksum mismatch for `{}` {}: {LOCK_FILE} has {}, the registry has {} (the published package changed)",
                pkg.name,
                pkg.version,
                locked.checksum.as_deref().unwrap_or("none"),
                pkg.checksum.as_deref().unwrap_or("none"),
            ));
        }
    }
    Ok(())
}

fn to_lockfile(resolution: &Resolution, root: &Path) -> Lockfile {
    let packages = resolution
        .packages
        .values()
        .map(|p| {
            let (source, checksum) = match &p.source {
                Source::Registry { checksum } => {
                    (REGISTRY_SOURCE.to_string(), Some(checksum.clone()))
                }
                Source::Path { dir } => (format!("path+{}", relpath::relative(dir, root)), None),
            };
            LockedPackage {
                name: p.name.clone(),
                version: p.version.to_string(),
                source,
                checksum,
                dependencies: p.dependencies.clone(),
            }
        })
        .collect();
    Lockfile::new(packages)
}

fn build_graph(
    manifest: &Manifest,
    root: &Path,
    resolution: &Resolution,
    dirs: &BTreeMap<String, PathBuf>,
) -> PackageGraph {
    let deps_of = |names: &[String]| names.iter().map(|n| (n.clone(), dirs[n].clone())).collect();
    let mut graph = PackageGraph::default();
    let added = graph.add(
        &manifest.package.name,
        root,
        deps_of(&resolution.root_dependencies),
    );
    configure(added, manifest);
    for pkg in resolution.packages.values() {
        let dir = &dirs[&pkg.name];
        let added = graph.add(&pkg.name, dir, deps_of(&pkg.dependencies));
        // A dependency's aliases and JSX source apply inside that dependency; an unreadable
        // manifest (already reported by resolution) simply contributes none.
        if let Ok(m) = Manifest::from_dir(dir) {
            configure(added, &m);
        }
    }
    graph
}

/// The per-package compile settings of `manifest`: `[paths]` and `[jsx]`.
fn configure(package: &mut crate::graph::GraphPackage, manifest: &Manifest) {
    package.paths = manifest.paths.clone();
    package.jsx_import_source = manifest.jsx.as_ref().and_then(|j| j.import_source.clone());
}
