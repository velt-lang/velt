//! Candidate versions for a requirement: registry index entries (cached per name) or the single
//! package at a path, each with its own dependency requirements.
//!
//! A registry requirement that `velt.lock.json`'s version still meets, with that version in the
//! cache under its locked checksum, gets that version alone, read from the cache: no index is
//! asked, so a locked project whose packages are all cached installs and builds offline, or with
//! the registry down (as cargo does). Everything else asks the registry's index.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};

use super::{dependency_requirements, parse_req, report, Requirement, Source, Want};
use crate::git::{GitRef, GitSpec};
use crate::locations::Locations;
use crate::lockfile::{Lockfile, LOCK_FILE, REGISTRY_SOURCE};
use crate::manifest::Manifest;
use crate::registry::{self, Index};

mod url;

/// One version that could be selected.
pub(super) struct Candidate {
    pub version: Version,
    pub source: Source,
    pub dependencies: Vec<(String, Want)>,
    /// Whether the root controls the manifest that wrote `dependencies`: a path package's (only
    /// the root's own manifests can name a path; a git package's paths become git sources).
    pub root_controlled: bool,
}

pub(super) struct Provider<'a> {
    loc: &'a Locations,
    root_dir: &'a Path,
    prefer: Option<&'a Lockfile>,
    /// Manifests to use instead of reading `package.velt`, by package directory: a workspace's
    /// members, with `"workspace"` replaced.
    manifests: &'a BTreeMap<PathBuf, Manifest>,
    indexes: HashMap<String, Option<Index>>,
    /// Warnings for the user, as candidates were made (`git::link_warnings`).
    pub warnings: BTreeSet<String>,
    /// How much of a fetched tree is looked at against its stamp (`crate::tree_stamp`).
    pub check: crate::tree_stamp::TreeCheck,
    /// The build's target triple, if any (`crate::install::InstallOptions::target`).
    pub target: Option<String>,
    /// Whether a locked version in the cache is offered without asking the index (module docs).
    use_cache: bool,
    /// Whether one was.
    from_cache: bool,
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
            warnings: BTreeSet::new(),
            check: crate::tree_stamp::TreeCheck::Files,
            target: None,
            use_cache: true,
            from_cache: false,
        }
    }

    /// Whether a candidate came from the cache instead of the index (module docs).
    pub fn used_cache(&self) -> bool {
        self.from_cache
    }

    /// From now on, ask the index for every registry requirement (each locked version still goes
    /// first).
    pub fn ask_registry(&mut self) {
        self.use_cache = false;
    }

    /// Candidates in preference order (may be empty: see [`Provider::no_match`]).
    pub fn candidates(&mut self, req: &Requirement) -> Result<Vec<Candidate>, String> {
        match &req.want {
            Want::Registry(version_req) => {
                if self.use_cache {
                    if let Some(cand) = self.locked_in_cache(&req.name, version_req) {
                        self.from_cache = true;
                        return Ok(vec![cand]);
                    }
                }
                let locked = self.locked_version(&req.name);
                let in_registry = self.index(&req.name)?.is_some_and(|index| {
                    locked
                        .as_ref()
                        .is_none_or(|v| index.versions.iter().any(|e| e.semver() == *v))
                });
                // A locked version the requirement still accepts must come from this registry:
                // when it isn't there the lock was written against another one (the default
                // registry changed, say), and picking another version of the same name here
                // would silently swap the package's origin.
                if let Some(v) = locked.as_ref().filter(|v| version_req.matches(v)) {
                    if !in_registry {
                        return Err(report::locked_elsewhere(&req.name, v, &self.loc.describe()));
                    }
                }
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
                    .map(|e| {
                        let source = Source::Registry {
                            checksum: e.checksum.clone(),
                            native: e.native.clone(),
                            native_abi: e.native_abi,
                            listed: e.dependencies.clone(),
                        };
                        registry_candidate(&req.name, e.semver(), source)
                    })
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
            Want::Url {
                spec,
                req: version_req,
            } => {
                let cand = self.url_candidate(req, spec)?;
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
        report::no_match(req, available.as_deref(), &self.loc.describe())
    }

    /// Whether registry version `name` `version` is yanked, as far as an index asked during this
    /// resolution says: a locked version taken from the cache asked none, and isn't reported.
    pub fn is_yanked(&self, name: &str, version: &Version) -> bool {
        self.indexes
            .get(name)
            .and_then(Option::as_ref)
            .is_some_and(|index| {
                index
                    .versions
                    .iter()
                    .any(|e| e.yanked && e.semver() == *version)
            })
    }

    /// The candidate for registry package `name` from the cache: the version `velt.lock.json`
    /// pins, when `version_req` accepts it and the cache holds it with the locked checksum. Its
    /// dependencies are its own `package.velt`'s, which that checksum covers; its native
    /// libraries the lock's. `None` sends the requirement to the index.
    fn locked_in_cache(&self, name: &str, version_req: &VersionReq) -> Option<Candidate> {
        let locked = self.prefer?.get(name)?;
        if locked.source != REGISTRY_SOURCE {
            return None;
        }
        let version = Version::parse(&locked.version).ok()?;
        let checksum = locked.checksum.as_ref()?;
        if !version_req.matches(&version) {
            return None;
        }
        // Prebuilt libraries, but none locked for this build's target: the index may have one
        // added since, which an install records.
        let target = self.target.as_deref();
        if !locked.native.is_empty() && target.is_some_and(|t| !locked.native.contains_key(t)) {
            return None;
        }
        let dir = self.loc.cached_package(name, &version);
        if !crate::cache::holds(&dir, checksum) {
            return None;
        }
        let manifest = Manifest::from_dir(&dir).ok()?;
        if manifest.package.name != name || manifest.version() != version {
            return None;
        }
        let source = Source::Registry {
            checksum: checksum.clone(),
            native: locked.native.clone(),
            native_abi: None,
            listed: registry::index_dependencies(&manifest).ok()?,
        };
        registry_candidate(name, version, source).ok()
    }

    fn locked_version(&self, name: &str) -> Option<Version> {
        let locked = self.prefer?.get(name)?;
        (locked.source == REGISTRY_SOURCE)
            .then(|| Version::parse(&locked.version).ok())
            .flatten()
    }

    fn index(&mut self, name: &str) -> Result<Option<&Index>, String> {
        if !self.indexes.contains_key(name) {
            let index =
                registry::read_index(self.loc, name).map_err(|e| match is_unreachable(&e) {
                    true => format!("{e}\n{}", self.why_registry(name)),
                    false => e,
                })?;
            self.indexes.insert(name.to_string(), index);
        }
        Ok(self.indexes[name].as_ref())
    }

    /// Why resolving `name` needed the registry, and what to do when it can't be reached.
    fn why_registry(&self, name: &str) -> String {
        let locked = self
            .prefer
            .and_then(|lock| lock.get(name))
            .filter(|p| p.source == REGISTRY_SOURCE);
        let why = match (self.prefer, locked) {
            (None, _) => format!("there is no {LOCK_FILE} to use (or `velt update` ignores it)"),
            (Some(_), None) => format!("{LOCK_FILE} doesn't pin it yet"),
            (Some(_), Some(p)) if self.use_cache => format!(
                "{LOCK_FILE} pins {}, which package.velt no longer accepts or which isn't in the cache (`{}`)",
                p.version,
                self.loc.cache.display()
            ),
            (Some(_), Some(p)) => format!(
                "{LOCK_FILE} pins {}, but the locked versions don't satisfy package.velt together",
                p.version
            ),
        };
        format!(
            "`{name}` needs the registry: {why}. A package whose {LOCK_FILE} covers package.velt, \
             with every locked package in the cache, installs and builds without the registry; \
             run `velt install` once the registry can be reached (check the network connection, \
             $VELT_REGISTRY and `registry` in package.velt)"
        )
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
        let dependencies = dependency_requirements(&manifest, dir, &chain, true)?
            .into_iter()
            .map(|r| (r.name, r.want))
            .collect();
        Ok(Candidate {
            version,
            source: Source::Path {
                dir: dir.to_path_buf(),
            },
            dependencies,
            root_controlled: true,
        })
    }
}

impl Provider<'_> {
    /// The package of a git requirement: at the commit the lockfile records for the same spec
    /// (no network), else at the commit its reference names now; checked out into the cache.
    fn git_candidate(&mut self, req: &Requirement, spec: &GitSpec) -> Result<Candidate, String> {
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
        let checkout = crate::git::checkout(
            self.loc,
            &spec.url,
            &commit,
            (checksum.as_deref(), self.check),
        )
        .map_err(named)?;
        let sub = spec.subdir.as_deref().unwrap_or(".");
        let dir = match &spec.subdir {
            Some(sub) => checkout.join(sub),
            None => checkout.clone(),
        };
        let short = &commit[..12];
        if !inside(&dir, &checkout) {
            return Err(named(format!(
                "`{sub}` leads out of the repository (through a symbolic link)"
            )));
        }
        let checksum = match checksum {
            Some(sum) => sum,
            None => crate::git::tree_checksum(&checkout)?,
        };
        if !crate::probe::is_file(&dir.join(crate::manifest::MANIFEST_FILE)) {
            // No Velt package: npm packages, compiled from their TypeScript sources.
            let provided = crate::npm::provided::packages_at(&dir)
                .map_err(|e| named(format!("`{sub}` of {} at commit {short} {e}", spec.url)))?;
            let Some(own) = provided.by_name.get(&req.name) else {
                let names: Vec<&str> = provided.by_name.keys().map(String::as_str).collect();
                let cut = provided.cut.as_ref().map_or(String::new(), |pattern| {
                    crate::npm::provided::cut_note(pattern, crate::npm::workspace::MAX_DIRS)
                });
                return Err(named(format!(
                    "`{sub}` of {} provides no npm package named `{}` (it has {}){cut}",
                    spec.url,
                    req.name,
                    names.join(", ")
                )));
            };
            let version = own.version.clone().unwrap_or(Version::new(0, 0, 0));
            let npm = provided
                .by_name
                .into_iter()
                .map(|(name, p)| (name, p.dir))
                .collect();
            return Ok(Candidate {
                version,
                source: Source::Git {
                    spec: spec.clone(),
                    commit,
                    checksum,
                    dir,
                    npm,
                },
                dependencies: vec![],
                root_controlled: false,
            });
        }
        let manifest = Manifest::from_dir(&dir).map_err(named)?;
        let windows = cfg!(windows);
        let warnings =
            crate::git::link_warnings(&checkout, &spec.url, spec.subdir.as_deref(), windows);
        self.warnings.extend(warnings);
        if manifest.package.name != req.name {
            return Err(named(format!(
                "the package at `{sub}` of {} is `{}`",
                spec.url, manifest.package.name
            )));
        }
        let version = manifest.version();
        let chain = [req.chain.clone(), vec![format!("{} {version}", req.name)]].concat();
        let mut dependencies = vec![];
        for r in dependency_requirements(&manifest, &dir, &chain, false)? {
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
        Ok(Candidate {
            version,
            source: Source::Git {
                spec: spec.clone(),
                commit,
                checksum,
                dir,
                npm: BTreeMap::new(),
            },
            dependencies,
            root_controlled: false,
        })
    }
}

/// Whether `dir` is inside `checkout` once symbolic links are followed. A path that doesn't
/// resolve (missing, or a dangling link) counts as inside: reading the package there reports it,
/// and the checkout's links were already checked to point inside it (`git::check_links`).
fn inside(dir: &Path, checkout: &Path) -> bool {
    match (dir.canonicalize(), checkout.canonicalize()) {
        (Ok(dir), Ok(checkout)) => dir.starts_with(checkout),
        _ => true,
    }
}

/// Whether `e` is a registry that could not be reached at all ([`crate::remote::UNREACHABLE`]).
pub(super) fn is_unreachable(e: &str) -> bool {
    e.starts_with(crate::remote::UNREACHABLE)
}

/// Registry package `name` `version` from `source` (a [`Source::Registry`]), depending on what its
/// `listed` names.
fn registry_candidate(name: &str, version: Version, source: Source) -> Result<Candidate, String> {
    let Source::Registry { listed, .. } = &source else {
        unreachable!("ICE: a registry candidate from another source");
    };
    let mut dependencies = vec![];
    for (dep, req) in listed {
        let req =
            parse_req(req).map_err(|e| format!("registry index of `{name}` {version}: {e}"))?;
        dependencies.push((dep.clone(), Want::Registry(req)));
    }
    Ok(Candidate {
        version,
        source,
        dependencies,
        root_controlled: false,
    })
}
