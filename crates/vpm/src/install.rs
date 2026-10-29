//! `velt install` (and the implicit install before a package build): resolve, verify against and
//! update `velt.lock.json`, fetch registry packages into the cache, provide the native library of
//! every package with native code for the build target, and produce the [`PackageGraph`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache;
use crate::graph::PackageGraph;
use crate::locations::Locations;
use crate::lockfile::{LockedPackage, Lockfile, LOCK_FILE, REGISTRY_SOURCE, WORKSPACE_SOURCE};
use crate::manifest::Manifest;
use crate::native::build::{build, cargo_available, BuildRequest};
use crate::native::{missing_target_message, NativeLib, NativeOrigin, NATIVE_ABI};
use crate::relpath;
use crate::resolve::{resolve, Resolution, ResolvedPackage, Source};
use crate::workspace::{Workspace, WorkspaceFile};

pub mod workspace;

/// How to treat the existing lockfile, and what to install for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Fail instead of changing `velt.lock.json` (CI mode).
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
    /// Whether `velt.lock.json` was created or changed.
    pub lock_changed: bool,
    /// Installed registry versions that are yanked (pinned by `velt.lock.json`); callers warn.
    pub yanked: Vec<(String, semver::Version)>,
    /// Warnings of the resolution, printed with the yanked versions ([`Installed::yank_warnings`]).
    pub warnings: Vec<String>,
    /// Things `velt install` tells the user (a package below a workspace that doesn't list it).
    pub notes: Vec<String>,
}

impl Installed {
    /// One `<name> <version> is yanked (pinned by velt.lock.json)` line per yanked version, then
    /// the resolution's other warnings (a git package's links on Windows).
    pub fn yank_warnings(&self) -> Vec<String> {
        self.yanked
            .iter()
            .map(|(name, version)| {
                format!("`{name}` {version} is yanked (pinned by velt.lock.json)")
            })
            .chain(self.warnings.iter().cloned())
            .collect()
    }
}

/// This toolchain's version, which a new lockfile records.
const TOOLCHAIN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The toolchain version the lockfile records (`velt`): none for a package without a `velt` pin
/// (`pin`; the launcher only follows a lock inside a pinned package); else the one it has, unless
/// `velt update` runs (`opts.update`) or the pin no longer accepts it; then `running`.
/// `--locked` never adds or moves it (a lock from before the field stays without one).
fn locked_toolchain(
    locked: Option<&str>,
    pin: Option<&str>,
    running: &str,
    opts: &InstallOptions,
) -> Option<String> {
    if opts.locked {
        return locked.map(str::to_string);
    }
    let pin = pin?;
    match locked {
        Some(v) if !opts.update && pin_accepts(pin, v) => Some(v.to_string()),
        _ => Some(running.to_string()),
    }
}

/// Whether the `velt` requirement `pin` accepts version `v` (false when either doesn't parse).
fn pin_accepts(pin: &str, v: &str) -> bool {
    match (
        velt_toolchain::Requirement::parse(pin),
        velt_toolchain::Version::parse(v),
    ) {
        (Ok(req), Ok(v)) => req.matches(&v),
        _ => false,
    }
}

/// Under `--locked`: the lock's `velt` must be the running toolchain, in a pinned package.
fn check_locked_toolchain(
    locked: Option<&str>,
    pin: Option<&str>,
    running: &str,
) -> Result<(), String> {
    let (Some(locked), Some(pin)) = (locked, pin) else {
        return Ok(());
    };
    if locked == running {
        return Ok(());
    }
    if !pin_accepts(pin, locked) {
        return Err(format!(
            "{LOCK_FILE} pins velt {locked}, which the package's velt pin \"{pin}\" no longer \
             accepts, and --locked was passed: run `velt install` (or `velt update`) without \
             --locked to record velt {running}"
        ));
    }
    Err(format!(
        "{LOCK_FILE} pins velt {locked}, but this is velt {running}, and --locked was passed: \
         run the velt launcher, which runs {locked} here (`velt +{locked} …`), or `velt update` \
         to build with this one"
    ))
}

/// Install the dependencies of the package rooted at `root`. A member of a workspace installs the
/// workspace's, into its lock ([`workspace`]), and gets the graph of what it uses.
pub fn install(root: &Path, loc: &Locations, opts: InstallOptions) -> Result<Installed, String> {
    let root = relpath::absolute(root);
    let mut notes = vec![];
    if let Some(ws_root) = crate::workspace::find_root(&root) {
        // Membership by the globs alone: a package the workspace doesn't list never depends on
        // the members being in order.
        let file = ws_root.join(crate::workspace::WORKSPACE_FILE);
        let globs = WorkspaceFile::from_path(&file)?.members;
        if velt_toolchain::members::lists(&globs, &relpath::relative(&root, &ws_root)) {
            let ws = Workspace::load(&ws_root)?;
            if ws.member_at(&root).is_some() {
                return workspace::install(&ws, Some(&root), loc, opts);
            }
        }
        notes.push(format!(
            "{} does not list this package, so it is installed on its own, with its own {LOCK_FILE}",
            file.display()
        ));
    }
    let manifest = Manifest::from_dir(&root)?;
    let loc = &loc.clone().with_manifest(&manifest);
    let existing = Lockfile::read(&root)?;
    let prefer = if opts.update { None } else { existing.as_ref() };
    let resolution = resolve(&root, &manifest, loc, prefer)?;
    let lockfile = to_lockfile(&resolution, &root, &[]);
    let (lockfile, lock_changed) = settle_lock(
        existing.as_ref(),
        lockfile,
        manifest.toolchain.as_deref(),
        &opts,
    )?;
    let dirs = fetch(loc, resolution.packages.values())?;
    let natives = natives(
        loc,
        Some((&root, &manifest)),
        resolution.packages.values(),
        (&lockfile, &dirs),
        &opts,
    )?;
    if lock_changed {
        lockfile.write(&root)?;
    }
    check_npm_overlap(resolution.packages.values())?;
    let graph = build_graph(
        Some((&manifest, &root, &resolution.root_dependencies)),
        resolution.packages.values(),
        &dirs,
        natives,
    );
    Ok(Installed {
        graph,
        lockfile,
        lock_changed,
        yanked: resolution.yanked.clone(),
        warnings: resolution.warnings.clone(),
        notes,
    })
}

/// The lockfile to keep, from the `existing` one and the resolution's `lockfile`: the toolchain
/// recorded, `--locked` enforced, and checksums of unchanged versions checked. Returns it and
/// whether it differs from `existing` (and must be written).
fn settle_lock(
    existing: Option<&Lockfile>,
    mut lockfile: Lockfile,
    pin: Option<&str>,
    opts: &InstallOptions,
) -> Result<(Lockfile, bool), String> {
    if opts.locked {
        if let Some(old) = existing {
            keep_locked_native(old, &mut lockfile);
        }
    }
    lockfile.velt = locked_toolchain(
        existing.and_then(|l| l.velt.as_deref()),
        pin,
        TOOLCHAIN_VERSION,
        opts,
    );
    if opts.locked {
        check_locked_toolchain(lockfile.velt.as_deref(), pin, TOOLCHAIN_VERSION)?;
    }
    let lock_changed = existing != Some(&lockfile);
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
    if let Some(old) = existing {
        check_locked_checksums(old, &lockfile)?;
    }
    Ok((lockfile, lock_changed))
}

/// Each package's directory: the verified cache entry of a registry package, a path package's own.
fn fetch<'r>(
    loc: &Locations,
    packages: impl Iterator<Item = &'r ResolvedPackage>,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut dirs = BTreeMap::new();
    for pkg in packages {
        let dir = match &pkg.source {
            Source::Registry { checksum, .. } => {
                cache::fetch(loc, &pkg.name, &pkg.version, checksum)?
            }
            // Checked out, and verified against the lock, by the resolution.
            Source::Path { dir } | Source::Git { dir, .. } => dir.clone(),
        };
        dirs.insert(pkg.name.clone(), dir);
    }
    Ok(dirs)
}

/// The native libraries for `opts.target` (none without one) of the `root` package and of
/// `packages`, whose directories are in `dirs`, as `lockfile` pins them.
fn natives<'r>(
    loc: &Locations,
    root: Option<(&Path, &Manifest)>,
    packages: impl Iterator<Item = &'r ResolvedPackage>,
    (lockfile, dirs): (&Lockfile, &BTreeMap<String, PathBuf>),
    opts: &InstallOptions,
) -> Result<BTreeMap<String, NativeLib>, String> {
    let mut natives = BTreeMap::new();
    let Some(target) = &opts.target else {
        return Ok(natives);
    };
    if let Some((dir, manifest)) = root {
        if let Some(lib) = native_from_source(dir, manifest, target)? {
            natives.insert(manifest.package.name.clone(), lib);
        }
    }
    for pkg in packages {
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
            // npm packages from git have no native code.
            Source::Git { npm, .. } if !npm.is_empty() => None,
            Source::Git {
                dir, spec, commit, ..
            } => {
                let m = Manifest::from_dir(dir)?;
                // As a registry package without a prebuilt library: building runs its build
                // scripts, so only when the user asks for it.
                if m.native.is_some() && !from_source_allowed() {
                    return Err(format!(
                        "`{}` from {} has native code, which a git dependency builds from source with cargo (it runs the package's build scripts): set {FROM_SOURCE_VAR}=1 to allow it",
                        pkg.name,
                        spec.describe()
                    ));
                }
                // Built from a copy: cargo writes `Cargo.lock` beside the crate, and the checkout
                // must stay as fetched (its checksum is verified on every use).
                let work = loc.cache.join("git-native").join(&pkg.name).join(commit);
                let copy = work.join("package");
                if m.native.is_some() && !copy.is_dir() {
                    crate::contents::copy_package(dir, &copy)?;
                }
                native_from_source_in(&copy, &m, target, &work)?
            }
        };
        if let Some(lib) = lib {
            natives.insert(pkg.name.clone(), lib);
        }
    }
    check_native_names(&natives)?;
    Ok(natives)
}

/// The environment variable that allows building registry packages' native code from source.
pub const FROM_SOURCE_VAR: &str = "VELT_NATIVE_FROM_SOURCE";

fn from_source_allowed() -> bool {
    std::env::var(FROM_SOURCE_VAR).is_ok_and(|v| v == "1")
}

/// Native libraries cannot be used for WebAssembly yet (no `native.wasm` support).
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
    native_from_source_in(
        dir,
        manifest,
        target,
        &dir.join("target").join("velt-native"),
    )
}

/// [`native_from_source`], building in `work` (a git checkout in the cache stays as fetched).
fn native_from_source_in(
    dir: &Path,
    manifest: &Manifest,
    target: &str,
    work: &Path,
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

/// The lockfile of `resolution`, with path sources relative to `root`; the path packages in
/// `members` (a workspace's) are `workspace+<dir>`.
fn to_lockfile(resolution: &Resolution, root: &Path, members: &[PathBuf]) -> Lockfile {
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
                Source::Git {
                    spec,
                    commit,
                    checksum,
                    ..
                } => (spec.source(commit), Some(checksum.clone()), BTreeMap::new()),
                Source::Path { dir } => (
                    format!(
                        "{}+{}",
                        match members.contains(dir) {
                            true => WORKSPACE_SOURCE,
                            false => "path",
                        },
                        relpath::relative(dir, root)
                    ),
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

/// Two packages with native code whose names differ only in `-` versus `_` would share an export
/// prefix, an init function and library file names: refused.
fn check_native_names(natives: &BTreeMap<String, NativeLib>) -> Result<(), String> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for name in natives.keys() {
        let norm = crate::manifest::normalized_name(name);
        if let Some(other) = seen.insert(norm, name) {
            return Err(format!(
                "packages `{other}` and `{name}` both have native code, and their names differ only in `-` and `_`, so their native functions would share names (`velt_{}__…`); depend on only one of them",
                crate::manifest::normalized_name(name)
            ));
        }
    }
    Ok(())
}

/// Two git dependencies that provide the same npm package would leave it to chance which one an
/// import gets: an error naming both.
fn check_npm_overlap<'r>(
    packages: impl Iterator<Item = &'r ResolvedPackage>,
) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for pkg in packages {
        let Source::Git { npm, .. } = &pkg.source else {
            continue;
        };
        for name in npm.keys() {
            if let Some(other) = seen.insert(name, &pkg.name) {
                return Err(format!(
                    "git dependencies `{other}` and `{}` both provide the npm package `{name}`; depend on one of them",
                    pkg.name
                ));
            }
        }
    }
    Ok(())
}

/// The graph of the `root` package (its manifest, directory and direct dependencies), added first
/// so its `define` applies, and of `packages`. A git dependency that provides npm packages
/// (`Source::Git::npm`) is no package of the graph: its dependents get its npm packages
/// ([`crate::graph::GraphPackage::npm`]).
fn build_graph<'r>(
    root: Option<(&Manifest, &Path, &[String])>,
    packages: impl Iterator<Item = &'r ResolvedPackage>,
    dirs: &BTreeMap<String, PathBuf>,
    mut natives: BTreeMap<String, NativeLib>,
) -> PackageGraph {
    let packages: Vec<&ResolvedPackage> = packages.collect();
    let npm: BTreeMap<&str, &BTreeMap<String, PathBuf>> = packages
        .iter()
        .filter_map(|p| match &p.source {
            Source::Git { npm, .. } if !npm.is_empty() => Some((p.name.as_str(), npm)),
            _ => None,
        })
        .collect();
    let add = |graph: &mut PackageGraph, name: &str, dir: &Path, deps: &[String]| {
        let velt = deps
            .iter()
            .filter(|n| !npm.contains_key(n.as_str()))
            .map(|n| (n.clone(), dirs[n].clone()))
            .collect();
        let added = graph.add(name, dir, velt);
        for n in deps {
            if let Some(provided) = npm.get(n.as_str()) {
                added
                    .npm
                    .extend(provided.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
    };
    let mut graph = PackageGraph::default();
    if let Some((manifest, root, dependencies)) = root {
        add(&mut graph, &manifest.package.name, root, dependencies);
        let added = graph.last_mut();
        configure(added, manifest);
        added.native = natives.remove(&manifest.package.name);
    }
    for pkg in packages
        .iter()
        .filter(|p| !npm.contains_key(p.name.as_str()))
    {
        let dir = &dirs[&pkg.name];
        add(&mut graph, &pkg.name, dir, &pkg.dependencies);
        let added = graph.last_mut();
        // A dependency's aliases and JSX source apply inside that dependency; an unreadable
        // manifest (already reported by resolution) simply contributes none.
        if let Ok(m) = Manifest::from_dir(dir) {
            configure(added, &m);
        }
        added.native = natives.remove(&pkg.name);
    }
    graph
}

/// The per-package compile settings of `manifest`: `paths` and `jsx`.
fn configure(package: &mut crate::graph::GraphPackage, manifest: &Manifest) {
    package.paths = manifest.paths.clone();
    package.jsx_import_source = manifest.jsx.as_ref().and_then(|j| j.import_source.clone());
    package.define = manifest.define.clone();
}

#[cfg(test)]
mod toolchain_tests {
    use super::*;

    fn opts(locked: bool, update: bool) -> InstallOptions {
        InstallOptions {
            locked,
            update,
            target: None,
        }
    }

    #[test]
    fn the_lock_keeps_its_toolchain_until_update_or_the_pin_moves() {
        let normal = opts(false, false);
        // A new lock records the running toolchain.
        assert_eq!(
            locked_toolchain(None, Some("0.2"), "0.2.1", &normal).as_deref(),
            Some("0.2.1")
        );
        // Without a pin, nothing is recorded (and a recorded one is dropped).
        assert_eq!(locked_toolchain(None, None, "0.2.1", &normal), None);
        assert_eq!(
            locked_toolchain(Some("0.1.0"), None, "0.2.1", &normal),
            None
        );
        // Another toolchain running the package (an editor, $VELT_TOOLCHAIN) leaves it alone.
        assert_eq!(
            locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.1", &normal).as_deref(),
            Some("0.2.0")
        );
        // `velt update` moves it to the running toolchain (the launcher ran the newest).
        assert_eq!(
            locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.1", &opts(false, true)).as_deref(),
            Some("0.2.1")
        );
        // A pin that no longer accepts it moves it too.
        assert_eq!(
            locked_toolchain(Some("0.2.0"), Some("0.3"), "0.3.0", &normal).as_deref(),
            Some("0.3.0")
        );
        // --locked never adds or moves it.
        let locked = opts(true, false);
        assert_eq!(locked_toolchain(None, Some("0.2"), "0.2.1", &locked), None);
        assert_eq!(
            locked_toolchain(Some("0.2.0"), Some("0.3"), "0.3.0", &locked).as_deref(),
            Some("0.2.0")
        );
    }

    #[test]
    fn locked_checks_the_toolchain_in_pinned_packages_only() {
        check_locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.0").unwrap();
        check_locked_toolchain(None, Some("0.2"), "0.2.0").unwrap();
        // Unpinned: the launcher doesn't follow the lock there, so it isn't checked.
        check_locked_toolchain(Some("0.1.0"), None, "0.2.0").unwrap();
        let other = check_locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.1").unwrap_err();
        assert!(
            other.contains("runs 0.2.0 here (`velt +0.2.0 …`)"),
            "{other}"
        );
        let moved = check_locked_toolchain(Some("0.1.9"), Some("0.2"), "0.2.0").unwrap_err();
        assert!(
            moved.contains(
                "pins velt 0.1.9, which the package's velt pin \"0.2\" no longer accepts"
            ) && moved.contains("to record velt 0.2.0"),
            "{moved}"
        );
    }
}
