//! Dependency resolution: from a root manifest to one exact version per package name.
//!
//! A small backtracking solver (enough for the POC's graph sizes): requirements are processed in
//! breadth-first order; an unselected package tries its candidates highest-first (a still-valid
//! locked version goes first), an already-selected one must satisfy the new requirement or the
//! branch fails. When every branch fails, the error of the first (most preferred) branch is
//! reported, listing each conflicting requirement with the chain that introduced it.

mod candidates;
mod report;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};

use crate::locations::Locations;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use candidates::{Candidate, Provider};

/// Where a resolved package comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The local registry; `checksum` from its index.
    Registry {
        /// `sha256:<hex>` of the published contents.
        checksum: String,
    },
    /// A local directory (absolute, normalized).
    Path {
        /// Package root.
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
}

/// What a requirement asks for.
#[derive(Clone, Debug)]
enum Want {
    Registry(VersionReq),
    Path {
        dir: PathBuf,
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
    let root_label = format!("{} {}", manifest.package.name, manifest.package.version);
    let root_reqs = dependency_requirements(manifest, root_dir, &[root_label])?;
    let root_dependencies = root_reqs.iter().map(|r| r.name.clone()).collect();
    let mut provider = Provider::new(loc, root_dir, prefer);
    let state = State {
        selected: BTreeMap::new(),
        pending: root_reqs.into(),
    };
    let solved = solve(state, &mut provider)?;
    let packages = solved
        .selected
        .into_iter()
        .map(|(name, sel)| (name, sel.pkg))
        .collect();
    Ok(Resolution {
        root_dependencies,
        packages,
    })
}

fn solve(mut state: State, provider: &mut Provider) -> Result<State, String> {
    let Some(req) = state.pending.pop_front() else {
        return Ok(state);
    };
    if let Some(sel) = state.selected.get_mut(&req.name) {
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

fn satisfies(pkg: &ResolvedPackage, want: &Want) -> bool {
    match (want, &pkg.source) {
        (Want::Registry(req), _) => req.matches(&pkg.version),
        (Want::Path { dir, req }, Source::Path { dir: selected }) => {
            dir == selected && req.as_ref().is_none_or(|r| r.matches(&pkg.version))
        }
        (Want::Path { .. }, Source::Registry { .. }) => false,
    }
}

/// The requirements declared by `manifest` (whose package root is `dir`).
fn dependency_requirements(
    manifest: &Manifest,
    dir: &Path,
    chain: &[String],
) -> Result<Vec<Requirement>, String> {
    let mut out = vec![];
    for (name, dep) in &manifest.dependencies {
        let req = dep.version().map(parse_req).transpose()?;
        let want = match (dep.path(), req) {
            (Some(path), req) => Want::Path {
                dir: crate::relpath::normalize(&dir.join(path)),
                req,
            },
            (None, Some(req)) => Want::Registry(req),
            (None, None) => {
                return Err(format!("dependency `{name}` needs a `version` or a `path`"))
            }
        };
        out.push(Requirement {
            name: name.clone(),
            want,
            chain: chain.to_vec(),
        });
    }
    Ok(out)
}

fn parse_req(req: &str) -> Result<VersionReq, String> {
    VersionReq::parse(req).map_err(|e| format!("invalid version requirement `{req}`: {e}"))
}

#[cfg(test)]
mod tests;
