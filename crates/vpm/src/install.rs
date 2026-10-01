//! `velt install` (and the implicit install before a package build): resolve, verify against and
//! update `velt.lock`, fetch registry packages into the cache, provide the native library of every
//! package with native code for the build target, and produce the [`PackageGraph`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache;
use crate::graph::PackageGraph;
use crate::locations::Locations;
use crate::lockfile::{LockedPackage, Lockfile, LOCK_FILE, REGISTRY_SOURCE};
use crate::manifest::Manifest;
use crate::native::build::{build, cargo_available, BuildRequest};
use crate::native::{missing_target_message, NativeLib, NativeOrigin, NATIVE_ABI};
use crate::relpath;
use crate::resolve::{resolve, Resolution, Source};

/// How to treat the existing lockfile, and what to install for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Fail instead of changing `velt.lock` (CI mode).
    pub locked: bool,
    /// Ignore locked versions and pick the newest compatible ones (`velt update`).
    pub update: bool,
    /// The target triple to provide native libraries for (`None`: none are fetched or built, and
    /// the graph carries no native libraries).
    pub target: Option<String>,
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
    let mut lockfile = to_lockfile(&resolution, &root);
    if opts.locked {
        if let Some(old) = &existing {
            keep_locked_native(old, &mut lockfile);
        }
    }
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
            Source::Registry { checksum, .. } => {
                cache::fetch(loc, &pkg.name, &pkg.version, checksum)?
            }
            Source::Path { dir } => dir.clone(),
        };
        dirs.insert(pkg.name.clone(), dir);
    }
    let mut natives = BTreeMap::new();
    if let Some(target) = &opts.target {
        if let Some(lib) = native_from_source(&root, &manifest, target)? {
            natives.insert(manifest.package.name.clone(), lib);
        }
        for pkg in resolution.packages.values() {
            let dir = &dirs[&pkg.name];
            let lib = match &pkg.source {
                Source::Registry { native_abi, .. } => {
                    let locked = lockfile.get(&pkg.name).map(|p| &p.native);
                    let published = locked.cloned().unwrap_or_default();
                    registry_native(
                        loc,
                        (&pkg.name, &pkg.version),
                        dir,
                        &published,
                        *native_abi,
                        target,
                    )?
                }
                Source::Path { dir } => {
                    let m = Manifest::from_dir(dir)?;
                    native_from_source(dir, &m, target)?
                }
            };
            if let Some(lib) = lib {
                natives.insert(pkg.name.clone(), lib);
            }
        }
    }
    if lock_changed {
        lockfile.write(&root)?;
    }
    let graph = build_graph(&manifest, &root, &resolution, &dirs, natives);
    Ok(Installed {
        graph,
        lockfile,
        lock_changed,
    })
}

/// The environment variable that allows building registry packages' native code from source.
pub const FROM_SOURCE_VAR: &str = "VELT_NATIVE_FROM_SOURCE";

fn from_source_allowed() -> bool {
    std::env::var(FROM_SOURCE_VAR).is_ok_and(|v| v == "1")
}

/// Native libraries cannot be used for WebAssembly yet (no `[native] wasm` support).
fn check_not_wasm(name: &str, target: &str) -> Result<(), String> {
    if target.starts_with("wasm32") {
        return Err(format!(
            "package `{name}` has native code, which cannot be used for WebAssembly ({target})"
        ));
    }
    Ok(())
}

/// A path package (or the root package) with native code: built from its sources with cargo, into
/// `<package>/target/velt-native/<triple>/`.
fn native_from_source(
    dir: &Path,
    manifest: &Manifest,
    target: &str,
) -> Result<Option<NativeLib>, String> {
    if manifest.native.is_none() {
        return Ok(None);
    }
    let name = &manifest.package.name;
    check_not_wasm(name, target)?;
    if !cargo_available() {
        return Err(format!(
            "package `{name}` at `{}` has native code, which needs cargo to build from source \
             (install Rust from https://rustup.rs, or depend on a published version)",
            dir.display()
        ));
    }
    let work = dir.join("target").join("velt-native");
    let out = work.join(target);
    build(BuildRequest {
        root: dir,
        manifest,
        target,
        out: &out,
        cargo_target_dir: &work.join("cargo"),
        locked: false,
    })?;
    NativeLib::open(&out, NativeOrigin::BuiltFromSource).map(Some)
}

/// A registry package's library for `target`: the verified prebuilt bundle pinned by the lockfile,
/// else built from its cached sources when cargo exists.
fn registry_native(
    loc: &Locations,
    (name, version): (&str, &semver::Version),
    dir: &Path,
    published: &BTreeMap<String, String>,
    native_abi: Option<u32>,
    target: &str,
) -> Result<Option<NativeLib>, String> {
    let manifest = Manifest::from_dir(dir)?;
    if manifest.native.is_none() && published.is_empty() {
        return Ok(None);
    }
    check_not_wasm(name, target)?;
    if let Some(abi) = native_abi.filter(|&a| a > NATIVE_ABI) {
        return Err(format!(
            "`{name} {version}` needs Velt native ABI {abi}; this velt provides {NATIVE_ABI}. Update velt."
        ));
    }
    if let Some(checksum) = published.get(target) {
        let bundle = cache::fetch_native(loc, name, version, target, checksum)?;
        return NativeLib::open(&bundle, NativeOrigin::Prebuilt).map(Some);
    }
    let missing = missing_target_message(name, &version.to_string(), target, published);
    if manifest.native.is_none() || !cargo_available() {
        return Err(missing);
    }
    // Building runs the package's build scripts: only when the user asks for it.
    if !from_source_allowed() {
        return Err(format!(
            "{missing}\nRust is installed: set {FROM_SOURCE_VAR}=1 to build it from source with cargo (this runs the package's build scripts)."
        ));
    }
    eprintln!(
        "    Building `{name}` {version} from source with cargo for {target} (it runs the package's build scripts)"
    );
    let work = loc.cached_native_root(name, version);
    let out = work.join(format!("{target}-source"));
    build(BuildRequest {
        root: dir,
        manifest: &manifest,
        target,
        out: &out,
        cargo_target_dir: &work.join("cargo"),
        locked: true,
    })
    .map_err(|e| {
        format!(
            "{}\nBuilding it from source failed: {e}",
            missing_target_message(name, &version.to_string(), target, published)
        )
    })?;
    NativeLib::open(&out, NativeOrigin::BuiltFromSource).map(Some)
}

/// Under `--locked`, a version that is still locked keeps exactly its locked native libraries
/// (targets the author added since are ignored).
fn keep_locked_native(old: &Lockfile, new: &mut Lockfile) {
    for pkg in &mut new.packages {
        if let Some(locked) = old.get(&pkg.name) {
            if locked.version == pkg.version
                && locked.source == pkg.source
                && locked.checksum == pkg.checksum
            {
                pkg.native = locked.native.clone();
            }
        }
    }
}

/// A registry package whose version is unchanged must still have the locked content hash, and
/// each of its locked native libraries the locked hash.
fn check_locked_checksums(old: &Lockfile, new: &Lockfile) -> Result<(), String> {
    for pkg in &new.packages {
        let Some(locked) = old.get(&pkg.name) else {
            continue;
        };
        if locked.version != pkg.version || locked.source != pkg.source {
            continue;
        }
        if locked.checksum != pkg.checksum {
            return Err(format!(
                "checksum mismatch for `{}` {}: {LOCK_FILE} has {}, the registry has {} (the published package changed)",
                pkg.name,
                pkg.version,
                locked.checksum.as_deref().unwrap_or("none"),
                pkg.checksum.as_deref().unwrap_or("none"),
            ));
        }
        for (target, sum) in &locked.native {
            match pkg.native.get(target) {
                Some(now) if now == sum => {}
                now => {
                    return Err(format!(
                        "checksum mismatch for the {target} native library of `{}` {}: {LOCK_FILE} has {sum}, the registry has {} (the published library changed)",
                        pkg.name,
                        pkg.version,
                        now.map_or("none", |s| s.as_str()),
                    ))
                }
            }
        }
    }
    Ok(())
}

fn to_lockfile(resolution: &Resolution, root: &Path) -> Lockfile {
    let packages = resolution
        .packages
        .values()
        .map(|p| {
            let (source, checksum, native) = match &p.source {
                Source::Registry {
                    checksum, native, ..
                } => (
                    REGISTRY_SOURCE.to_string(),
                    Some(checksum.clone()),
                    native.clone(),
                ),
                Source::Path { dir } => (
                    format!("path+{}", relpath::relative(dir, root)),
                    None,
                    BTreeMap::new(),
                ),
            };
            LockedPackage {
                name: p.name.clone(),
                version: p.version.to_string(),
                source,
                checksum,
                dependencies: p.dependencies.clone(),
                native,
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
    mut natives: BTreeMap<String, NativeLib>,
) -> PackageGraph {
    let deps_of = |names: &[String]| names.iter().map(|n| (n.clone(), dirs[n].clone())).collect();
    let mut graph = PackageGraph::default();
    let added = graph.add(
        &manifest.package.name,
        root,
        deps_of(&resolution.root_dependencies),
    );
    configure(added, manifest);
    added.native = natives.remove(&manifest.package.name);
    for pkg in resolution.packages.values() {
        let dir = &dirs[&pkg.name];
        let added = graph.add(&pkg.name, dir, deps_of(&pkg.dependencies));
        // A dependency's aliases and JSX source apply inside that dependency; an unreadable
        // manifest (already reported by resolution) simply contributes none.
        if let Ok(m) = Manifest::from_dir(dir) {
            configure(added, &m);
        }
        added.native = natives.remove(&pkg.name);
    }
    graph
}

/// The per-package compile settings of `manifest`: `[paths]` and `[jsx]`.
fn configure(package: &mut crate::graph::GraphPackage, manifest: &Manifest) {
    package.paths = manifest.paths.clone();
    package.jsx_import_source = manifest.jsx.as_ref().and_then(|j| j.import_source.clone());
}
