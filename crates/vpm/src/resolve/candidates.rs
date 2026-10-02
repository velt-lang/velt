//! Candidate versions for a requirement: registry index entries (cached per name) or the single
//! package at a path, each with its own dependency requirements.

use std::collections::HashMap;
use std::path::Path;

use semver::Version;

use super::{dependency_requirements, parse_req, report, Requirement, Source, Want};
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
    indexes: HashMap<String, Option<Index>>,
}

impl<'a> Provider<'a> {
    pub fn new(loc: &'a Locations, root_dir: &'a Path, prefer: Option<&'a Lockfile>) -> Self {
        Provider {
            loc,
            root_dir,
            prefer,
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
        let manifest = Manifest::from_dir(dir).map_err(|e| {
            let shown = crate::relpath::relative(dir, self.root_dir);
            format!("path dependency `{}` at `{shown}`: {e}", req.name)
        })?;
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
