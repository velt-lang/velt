//! Candidate versions for a requirement: registry index entries (cached per name) or the single
//! package at a path, each with its own dependency requirements.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use semver::Version;

use super::{dependency_requirements, parse_req, report, Requirement, Source, Want};
use crate::git::{GitRef, GitSpec};
use crate::locations::Locations;
use crate::lockfile::{Lockfile, REGISTRY_SOURCE};
use crate::manifest::Manifest;
use crate::registry::{self, Index};

/// One version that could be selected.
pub(super) struct Candidate {
    pub version: Version,
    pub source: Source,
    pub dependencies: Vec<(String, Want)>,
}

pub(super) struct Provider<'a> {
    loc: &'a Locations,
    root_dir: &'a Path,
    prefer: Option<&'a Lockfile>,
    /// Manifests to use instead of reading `package.vlt`, by package directory: a workspace's
    /// members, with `"workspace"` replaced.
    manifests: &'a BTreeMap<PathBuf, Manifest>,
    indexes: HashMap<String, Option<Index>>,
}

impl<'a> Provider<'a> {
    pub fn new(
        loc: &'a Locations,
        root_dir: &'a Path,
        prefer: Option<&'a Lockfile>,
        manifests: &'a BTreeMap<PathBuf, Manifest>,
    ) -> Self {
        Provider {
            loc,
            root_dir,
            prefer,
            manifests,
            indexes: HashMap::new(),
        }
    }

    /// Candidates in preference order (may be empty: see [`Provider::no_match`]).
    pub fn candidates(&mut self, req: &Requirement) -> Result<Vec<Candidate>, String> {
        match &req.want {
            Want::Registry(version_req) => {
                let locked = self.locked_version(&req.name);
                let Some(index) = self.index(&req.name)? else {
                    return Ok(vec![]);
                };
                let mut entries: Vec<_> = index
                    .versions
                    .iter()
                    // A yanked version is only used where the lockfile already pins it.
                    .filter(|e| !e.yanked || Some(e.semver()) == locked)
                    .filter(|e| version_req.matches(&e.semver()))
                    .collect();
                entries.sort_by_key(|e| std::cmp::Reverse(e.semver()));
                if let Some(pos) = locked.and_then(|v| entries.iter().position(|e| e.semver() == v))
                {
                    let preferred = entries.remove(pos);
                    entries.insert(0, preferred);
                }
                entries
                    .into_iter()
                    .map(|e| registry_candidate(&req.name, e))
                    .collect()
            }
            Want::Git {
                spec,
                req: version_req,
            } => {
                let cand = self.git_candidate(req, spec)?;
                Ok(version_req
                    .as_ref()
                    .is_none_or(|r| r.matches(&cand.version))
                    .then_some(cand)
                    .into_iter()
                    .collect())
            }
            Want::Path {
                dir,
                req: version_req,
            } => {
                let cand = self.path_candidate(req, dir)?;
                Ok(version_req
                    .as_ref()
                    .is_none_or(|r| r.matches(&cand.version))
                    .then_some(cand)
                    .into_iter()
                    .collect())
            }
        }
    }

    /// The error for a requirement without candidates.
    pub fn no_match(&mut self, req: &Requirement) -> String {
        let available = match (&req.want, self.index(&req.name)) {
            (Want::Registry(_), Ok(Some(index))) => {
                let mut versions: Vec<(Version, bool)> = index
                    .versions
                    .iter()
                    .map(|e| (e.semver(), e.yanked))
                    .collect();
                versions.sort();
                Some(
                    versions
                        .iter()
                        .map(|(v, yanked)| match yanked {
                            true => format!("{v} (yanked)"),
                            false => v.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            }
            _ => None,
        };
        report::no_match(req, available.as_deref(), &self.loc.registry)
    }

    /// Whether registry version `name` `version` is yanked.
    pub fn is_yanked(&mut self, name: &str, version: &Version) -> Result<bool, String> {
        Ok(self.index(name)?.is_some_and(|index| {
            index
                .versions
                .iter()
                .any(|e| e.yanked && e.semver() == *version)
        }))
    }

    fn locked_version(&self, name: &str) -> Option<Version> {
        let locked = self.prefer?.get(name)?;
        (locked.source == REGISTRY_SOURCE)
            .then(|| Version::parse(&locked.version).ok())
            .flatten()
    }

    fn index(&mut self, name: &str) -> Result<Option<&Index>, String> {
        if !self.indexes.contains_key(name) {
            let index = registry::read_index(self.loc, name)?;
            self.indexes.insert(name.to_string(), index);
        }
        Ok(self.indexes[name].as_ref())
    }

    fn path_candidate(&self, req: &Requirement, dir: &Path) -> Result<Candidate, String> {
        let manifest = match self.manifests.get(dir) {
            Some(m) => m.clone(),
            None => Manifest::from_dir(dir).map_err(|e| {
                let shown = crate::relpath::relative(dir, self.root_dir);
                format!("path dependency `{}` at `{shown}`: {e}", req.name)
            })?,
        };
        if manifest.package.name != req.name {
            return Err(format!(
                "path dependency `{}` at `{}` is the package `{}`",
                req.name,
                dir.display(),
                manifest.package.name
            ));
        }
        let version = manifest.version();
        let chain = [req.chain.clone(), vec![format!("{} {version}", req.name)]].concat();
        let dependencies = dependency_requirements(&manifest, dir, &chain)?
            .into_iter()
            .map(|r| (r.name, r.want))
            .collect();
        Ok(Candidate {
            version,
            source: Source::Path {
                dir: dir.to_path_buf(),
            },
            dependencies,
        })
    }
}

impl Provider<'_> {
    /// The package of a git requirement: at the commit the lockfile records for the same spec
    /// (no network), else at the commit its reference names now; checked out into the cache.
    fn git_candidate(&self, req: &Requirement, spec: &GitSpec) -> Result<Candidate, String> {
        let named = |e: String| format!("git dependency `{}`: {e}", req.name);
        let locked = self
            .prefer
            .and_then(|lock| lock.get(&req.name))
            .and_then(|p| {
                let (locked_spec, commit) = GitSpec::parse_source(&p.source)?;
                (locked_spec == *spec).then(|| (commit, p.checksum.clone()))
            });
        let (commit, checksum) = match locked {
            Some((commit, checksum)) => (commit, checksum),
            None => (crate::git::resolve_ref(spec).map_err(named)?, None),
        };
        let checkout = crate::git::checkout(self.loc, &spec.url, &commit, checksum.as_deref())
            .map_err(named)?;
        let sub = spec.subdir.as_deref().unwrap_or(".");
        let dir = match &spec.subdir {
            Some(sub) => checkout.join(sub),
            None => checkout.clone(),
        };
        let short = &commit[..12];
        if !crate::probe::is_file(&dir.join(crate::manifest::MANIFEST_FILE)) {
            return Err(named(format!(
                "{} at commit {short} has no package at `{sub}` (no package.vlt there)",
                spec.url
            )));
        }
        if !inside(&dir, &checkout) {
            return Err(named(format!(
                "`{sub}` leads out of the repository (through a symbolic link)"
            )));
        }
        let manifest = Manifest::from_dir(&dir).map_err(named)?;
        if manifest.package.name != req.name {
            return Err(named(format!(
                "the package at `{sub}` of {} is `{}`",
                spec.url, manifest.package.name
            )));
        }
        let version = manifest.version();
        let chain = [req.chain.clone(), vec![format!("{} {version}", req.name)]].concat();
        let mut dependencies = vec![];
        for r in dependency_requirements(&manifest, &dir, &chain)? {
            let want = match r.want {
                // A path dependency of a git package is another package of the same commit,
                // locked and verified as part of it, never a directory elsewhere (as in Cargo).
                Want::Path { dir: path, req } => {
                    if !path.starts_with(&checkout) || !inside(&path, &checkout) {
                        return Err(named(format!(
                            "its dependency `{}` has a `path` that leads out of the repository",
                            r.name
                        )));
                    }
                    let rel = crate::relpath::relative(&path, &checkout);
                    let sibling = GitSpec {
                        url: spec.url.clone(),
                        reference: GitRef::Rev(commit.clone()),
                        subdir: (rel != ".").then_some(rel),
                    };
                    Want::Git { spec: sibling, req }
                }
                want => want,
            };
            dependencies.push((r.name, want));
        }
        let checksum = match checksum {
            Some(sum) => sum,
            None => crate::git::tree_checksum(&checkout)?,
        };
        Ok(Candidate {
            version,
            source: Source::Git {
                spec: spec.clone(),
                commit,
                checksum,
                dir,
            },
            dependencies,
        })
    }
}

/// Whether `dir` is inside `checkout` once symbolic links are followed. A path that doesn't exist
/// counts as inside: reading it reports it.
fn inside(dir: &Path, checkout: &Path) -> bool {
    match (dir.canonicalize(), checkout.canonicalize()) {
        (Ok(dir), Ok(checkout)) => dir.starts_with(checkout),
        _ => true,
    }
}

fn registry_candidate(name: &str, entry: &registry::IndexEntry) -> Result<Candidate, String> {
    let mut dependencies = vec![];
    for (dep, req) in &entry.dependencies {
        let req = parse_req(req)
            .map_err(|e| format!("registry index of `{name}` {}: {e}", entry.version))?;
        dependencies.push((dep.clone(), Want::Registry(req)));
    }
    Ok(Candidate {
        version: entry.semver(),
        source: Source::Registry {
            checksum: entry.checksum.clone(),
            native: entry.native.clone(),
            native_abi: entry.native_abi,
        },
        dependencies,
    })
}
