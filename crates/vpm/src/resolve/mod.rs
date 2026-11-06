//! Dependency resolution: from a root manifest to one exact version per package name.
//!
//! A small backtracking solver (enough for the POC's graph sizes): requirements are processed in
//! breadth-first order; an unselected package tries its candidates highest-first (a still-valid
//! locked version goes first), an already-selected one must satisfy the new requirement or the
//! branch fails. When every branch fails, the error of the first (most preferred) branch is
//! reported, listing each conflicting requirement with the chain that introduced it.
//!
//! **Sources** (#2367): one name has one source. The requirements written by manifests the root
//! controls (the root's, a workspace member's, a path package's: only those can name a `path`,
//! since a git package's paths become its repository) are solved first, and they win: a
//! transitive requirement of the same name from another source is replaced by the root's entry,
//! as long as the version still matches, and [`Resolution::overrides`] records it. Two such
//! root-controlled entries that disagree, or two transitive ones the root doesn't settle, are an
//! error naming both; never a silent pick.

mod candidates;
mod report;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};

use crate::locations::Locations;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
pub use crate::tree_stamp::TreeCheck;
use candidates::{Candidate, Provider};

/// Where a resolved package comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The registry; `checksum` and the native libraries from its index.
    Registry {
        /// `sha256:<hex>` of the published contents.
        checksum: String,
        /// Target triple → checksum of the prebuilt native library (packages with native code).
        native: BTreeMap<String, String>,
        /// The runtime table version those libraries need.
        native_abi: Option<u32>,
    },
    /// A local directory (absolute, normalized).
    Path {
        /// Package root.
        dir: PathBuf,
    },
    /// A commit of a git repository ([`crate::git`]).
    Git {
        spec: crate::git::GitSpec,
        /// The commit.
        commit: String,
        /// [`crate::git::tree_checksum`] of the checkout.
        checksum: String,
        /// The package root: the checkout's `spec.subdir`.
        dir: PathBuf,
        /// For a target with no `package.velt`: the npm packages it provides, by name (one, or
        /// every package of its workspace), which its dependents import as npm packages
        /// (`crate::npm::provided`). Empty for a Velt package.
        npm: BTreeMap<String, PathBuf>,
    },
    /// A directory of a release archive pinned by its hash ([`crate::url_source`]).
    Url {
        spec: crate::url_source::UrlSpec,
        /// [`crate::git::tree_checksum`] of the package directory.
        checksum: String,
        /// The package root, in the extracted archive.
        dir: PathBuf,
    },
}

/// One package in a resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPackage {
    /// Package name.
    pub name: String,
    /// Selected version.
    pub version: Version,
    /// Where it comes from.
    pub source: Source,
    /// Names of its direct dependencies (each is also in the resolution).
    pub dependencies: Vec<String>,
}

/// The result of resolving a root package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    /// Names of the root package's direct dependencies.
    pub root_dependencies: Vec<String>,
    /// Every transitive dependency by name (the root package is not included).
    pub packages: BTreeMap<String, ResolvedPackage>,
    /// Selected registry versions that are yanked (kept only because the lockfile pins them).
    pub yanked: Vec<(String, Version)>,
    /// Things the user should know about the packages (a git package's links on Windows).
    pub warnings: Vec<String>,
    /// Package name → the requirements of other packages for it that the root's own entry
    /// replaced with its source (the root's direct dependency wins).
    pub overrides: BTreeMap<String, Vec<Override>>,
}

/// A transitive requirement that the root's entry for the same package overrode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    /// The package that asked.
    pub by: String,
    /// What it asked for, as messages show it (`` `foo ^1.2` ``, `` `foo` from https://… ``).
    pub wanted: String,
}

/// What a requirement asks for.
#[derive(Clone, Debug)]
enum Want {
    Registry(VersionReq),
    Path {
        dir: PathBuf,
        req: Option<VersionReq>,
    },
    Git {
        spec: crate::git::GitSpec,
        req: Option<VersionReq>,
    },
    Url {
        spec: crate::url_source::UrlSpec,
        req: Option<VersionReq>,
    },
}

/// A dependency edge being solved, with the chain of packages that introduced it
/// (`["app 0.1.0", "json 1.2.0"]`, last element is the direct requester).
#[derive(Clone, Debug)]
struct Requirement {
    name: String,
    want: Want,
    chain: Vec<String>,
    /// The package whose manifest wrote it.
    by: String,
    /// Whether that manifest is one the root controls (module docs).
    root: bool,
}

#[derive(Clone, Debug)]
struct Selected {
    pkg: ResolvedPackage,
    by: Vec<Requirement>,
}

#[derive(Clone, Debug, Default)]
struct State {
    selected: BTreeMap<String, Selected>,
    pending: VecDeque<Requirement>,
}

/// Resolve the package rooted at `root_dir` (absolute) with manifest `manifest`. `prefer` is the
/// existing lockfile: its versions are tried first while they still satisfy the requirements.
pub fn resolve(
    root_dir: &Path,
    manifest: &Manifest,
    loc: &Locations,
    prefer: Option<&Lockfile>,
) -> Result<Resolution, String> {
    resolve_with(root_dir, manifest, loc, prefer, TreeCheck::Files)
}

/// [`resolve`], looking at fetched trees as `check` says (`crate::tree_stamp`).
pub fn resolve_with(
    root_dir: &Path,
    manifest: &Manifest,
    loc: &Locations,
    prefer: Option<&Lockfile>,
    check: TreeCheck,
) -> Result<Resolution, String> {
    let root_label = format!("{} {}", manifest.package.name, manifest.package.version);
    let root_reqs = dependency_requirements(manifest, root_dir, &[root_label], true)?;
    let resolution = solve_root(root_dir, root_reqs, (loc, check), prefer, &BTreeMap::new())?;
    refuse_root_name(root_dir, &manifest.package.name, &resolution)?;
    Ok(resolution)
}

/// A package with the root's name from anywhere but the root's directory is an error: one name
/// would stand for two packages, in the lock and in what the program allows. A dependency may
/// depend back on the root itself.
fn refuse_root_name(root_dir: &Path, name: &str, resolution: &Resolution) -> Result<(), String> {
    let Some(pkg) = resolution.packages.get(name) else {
        return Ok(());
    };
    let from = match &pkg.source {
        Source::Path { dir } => {
            if let (Ok(a), Ok(b)) = (dir.canonicalize(), root_dir.canonicalize()) {
                if a == b {
                    return Ok(());
                }
            }
            format!("from {}", dir.display())
        }
        Source::Git { spec, .. } => format!("from {}", spec.url),
        Source::Url { spec, .. } => format!("from {}", spec.describe()),
        Source::Registry { .. } => "from the registry".to_string(),
    };
    let by: Vec<String> = resolution
        .packages
        .values()
        .filter(|p| p.dependencies.iter().any(|d| d == name))
        .map(|p| format!("`{}`", p.name))
        .collect();
    let by = match by.is_empty() {
        true => "this package".to_string(),
        false => by.join(", "),
    };
    Err(format!(
        "{by} depends on a package named `{name}` {from}, but `{name}` is the name of this package: a dependency can't take the name of the package that installs it"
    ))
}

/// Resolve the members of a workspace rooted at `root_dir` together: every member (its directory
/// → its manifest, as [`crate::workspace::Workspace::effective_manifests`] gives them) is a path
/// package of the resolution, and [`Resolution::root_dependencies`] are their names. One version
/// per package name holds across all of them.
pub fn resolve_members(
    root_dir: &Path,
    members: &BTreeMap<PathBuf, Manifest>,
    loc: &Locations,
    prefer: Option<&Lockfile>,
) -> Result<Resolution, String> {
    resolve_members_with(root_dir, members, loc, prefer, TreeCheck::Files)
}

/// [`resolve_members`], looking at fetched trees as `check` says (`crate::tree_stamp`).
pub fn resolve_members_with(
    root_dir: &Path,
    members: &BTreeMap<PathBuf, Manifest>,
    loc: &Locations,
    prefer: Option<&Lockfile>,
    check: TreeCheck,
) -> Result<Resolution, String> {
    let root_reqs = members
        .iter()
        .map(|(dir, m)| Requirement {
            name: m.package.name.clone(),
            want: Want::Path {
                dir: dir.clone(),
                req: None,
            },
            chain: vec!["the workspace".to_string()],
            by: "the workspace".to_string(),
            root: true,
        })
        .collect();
    solve_root(root_dir, root_reqs, (loc, check), prefer, members)
}

fn solve_root(
    root_dir: &Path,
    root_reqs: Vec<Requirement>,
    (loc, check): (&Locations, TreeCheck),
    prefer: Option<&Lockfile>,
    manifests: &BTreeMap<PathBuf, Manifest>,
) -> Result<Resolution, String> {
    let root_dependencies = root_reqs.iter().map(|r| r.name.clone()).collect();
    let mut provider = Provider::new(loc, root_dir, prefer, manifests);
    provider.check = check;
    let state = State {
        selected: BTreeMap::new(),
        pending: root_reqs.into(),
    };
    let solved = solve(state, &mut provider)?;
    let mut overrides = BTreeMap::new();
    for (name, sel) in &solved.selected {
        let replaced: Vec<Override> = sel
            .by
            .iter()
            .filter(|r| !r.root && !same_source(&sel.pkg.source, &r.want))
            .map(|r| Override {
                by: r.by.clone(),
                wanted: report::describe_want(&r.name, &r.want),
            })
            .collect();
        if !replaced.is_empty() {
            overrides.insert(name.clone(), replaced);
        }
    }
    let packages: BTreeMap<String, ResolvedPackage> = solved
        .selected
        .into_iter()
        .map(|(name, sel)| (name, sel.pkg))
        .collect();
    let mut yanked = vec![];
    for pkg in packages.values() {
        if matches!(pkg.source, Source::Registry { .. })
            && provider.is_yanked(&pkg.name, &pkg.version)?
        {
            yanked.push((pkg.name.clone(), pkg.version.clone()));
        }
    }
    Ok(Resolution {
        root_dependencies,
        packages,
        yanked,
        warnings: provider.warnings.into_iter().collect(),
        overrides,
    })
}

fn solve(mut state: State, provider: &mut Provider) -> Result<State, String> {
    // The root's own requirements first, so each of its entries is selected before any
    // transitive requirement of the same name arrives.
    let next = state.pending.iter().position(|r| r.root).unwrap_or(0);
    let Some(req) = state.pending.remove(next) else {
        return Ok(state);
    };
    if let Some(sel) = state.selected.get_mut(&req.name) {
        if !same_source(&sel.pkg.source, &req.want) {
            let root_chose = sel.by.iter().any(|r| r.root);
            if !root_chose || req.root {
                return Err(report::two_sources(&req, sel));
            }
            // The root's entry wins over a transitive one, if the version still fits.
            if !version_fits(&sel.pkg, &req.want) {
                return Err(report::overridden_version(&req, sel));
            }
            sel.by.push(req);
            return solve(state, provider);
        }
        if satisfies(&sel.pkg, &req.want) {
            sel.by.push(req);
            return solve(state, provider);
        }
        return Err(report::conflict(&req, sel));
    }
    let candidates = provider.candidates(&req)?;
    if candidates.is_empty() {
        return Err(provider.no_match(&req));
    }
    let mut first_err = None;
    for cand in candidates {
        match solve(select(&state, &req, cand), provider) {
            Ok(done) => return Ok(done),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.expect("ICE: non-empty candidate list produced no outcome"))
}

/// A copy of `state` with `cand` selected for `req` and its dependencies queued.
fn select(state: &State, req: &Requirement, cand: Candidate) -> State {
    let mut next = state.clone();
    let mut chain = req.chain.clone();
    chain.push(format!("{} {}", req.name, cand.version));
    let dependencies = cand
        .dependencies
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    for (name, want) in cand.dependencies {
        next.pending.push_back(Requirement {
            name,
            want,
            chain: chain.clone(),
            by: req.name.clone(),
            root: cand.root_controlled,
        });
    }
    let pkg = ResolvedPackage {
        name: req.name.clone(),
        version: cand.version,
        source: cand.source,
        dependencies,
    };
    next.selected.insert(
        req.name.clone(),
        Selected {
            pkg,
            by: vec![req.clone()],
        },
    );
    next
}

/// Whether `want` asks for the source `source` is: the registry for the registry, the same
/// directory, the same repository, reference and directory, the same archive (by its hash: two
/// URLs serving one archive give one tree) and directory in it.
fn same_source(source: &Source, want: &Want) -> bool {
    match (want, source) {
        (Want::Registry(_), Source::Registry { .. }) => true,
        (Want::Path { dir, .. }, Source::Path { dir: selected }) => dir == selected,
        (Want::Git { spec, .. }, Source::Git { spec: selected, .. }) => spec == selected,
        (Want::Url { spec, .. }, Source::Url { spec: selected, .. }) => {
            same_archive(spec, selected)
        }
        _ => false,
    }
}

/// One archive (by hash) and one directory in it, from whichever URL.
fn same_archive(a: &crate::url_source::UrlSpec, b: &crate::url_source::UrlSpec) -> bool {
    (&a.sha256, a.strip, &a.subdir) == (&b.sha256, b.strip, &b.subdir)
}

/// Whether `pkg`'s version is one `want` accepts, whatever its source.
fn version_fits(pkg: &ResolvedPackage, want: &Want) -> bool {
    match want {
        Want::Registry(req) => req.matches(&pkg.version),
        Want::Path { req, .. } | Want::Git { req, .. } | Want::Url { req, .. } => {
            req.as_ref().is_none_or(|r| r.matches(&pkg.version))
        }
    }
}

fn satisfies(pkg: &ResolvedPackage, want: &Want) -> bool {
    match (want, &pkg.source) {
        (Want::Registry(req), _) => req.matches(&pkg.version),
        (Want::Path { dir, req }, Source::Path { dir: selected }) => {
            dir == selected && req.as_ref().is_none_or(|r| r.matches(&pkg.version))
        }
        (Want::Git { spec, req }, Source::Git { spec: selected, .. }) => {
            spec == selected && req.as_ref().is_none_or(|r| r.matches(&pkg.version))
        }
        (Want::Url { spec, req }, Source::Url { spec: selected, .. }) => {
            same_archive(spec, selected) && req.as_ref().is_none_or(|r| r.matches(&pkg.version))
        }
        (Want::Path { .. } | Want::Git { .. } | Want::Url { .. }, _) => false,
    }
}

/// The requirements declared by `manifest` (whose package root is `dir`); `root`: whether the
/// root controls that manifest (module docs).
fn dependency_requirements(
    manifest: &Manifest,
    dir: &Path,
    chain: &[String],
    root: bool,
) -> Result<Vec<Requirement>, String> {
    let by = &manifest.package.name;
    let mut out = vec![];
    for (name, dep) in &manifest.dependencies {
        if dep.is_workspace() {
            return Err(format!(
                "dependency `{name}` of `{0}` is `\"workspace\"`, but `{0}` is not a member of a workspace (no velt.workspace.velt above it lists it)",
                manifest.package.name
            ));
        }
        let req = dep.version().map(parse_req).transpose()?;
        if let Some(spec) = dep.git() {
            out.push(Requirement {
                name: name.clone(),
                want: Want::Git { spec, req },
                chain: chain.to_vec(),
                by: by.clone(),
                root,
            });
            continue;
        }
        if let Some(spec) = dep.url_spec() {
            out.push(Requirement {
                name: name.clone(),
                want: Want::Url { spec, req },
                chain: chain.to_vec(),
                by: by.clone(),
                root,
            });
            continue;
        }
        let want = match (dep.path(), req) {
            (Some(path), req) => Want::Path {
                dir: crate::relpath::normalize(&dir.join(path)),
                req,
            },
            (None, Some(req)) => Want::Registry(req),
            (None, None) => {
                return Err(format!(
                    "dependency `{name}` needs a `version`, a `path`, a `git` repository or a `url`"
                ))
            }
        };
        out.push(Requirement {
            name: name.clone(),
            want,
            chain: chain.to_vec(),
            by: by.clone(),
            root,
        });
    }
    Ok(out)
}

fn parse_req(req: &str) -> Result<VersionReq, String> {
    VersionReq::parse(req).map_err(|e| format!("invalid version requirement `{req}`: {e}"))
}

#[cfg(test)]
mod tests;
