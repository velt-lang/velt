//! A url package's candidate (`crate::url_source`): the archive's directory, its manifest, and
//! its dependencies, where a `path` is another directory of the same archive.

use super::{dependency_requirements, inside, Candidate, Provider, Requirement, Source, Want};
use crate::manifest::Manifest;
use crate::url_source::UrlSpec;

impl Provider<'_> {
    pub(super) fn url_candidate(
        &mut self,
        req: &Requirement,
        spec: &UrlSpec,
    ) -> Result<Candidate, String> {
        let named = |e: String| format!("url dependency `{}`: {e}", req.name);
        // The lock's checksum, when it names this archive and directory.
        let locked = (self.prefer)
            .and_then(|lock| lock.get(&req.name))
            .filter(|p| UrlSpec::parse_source(&p.source).is_some_and(|l| same(&l, spec)))
            .and_then(|p| p.checksum.clone());
        let (dir, checksum) =
            crate::url_source::checkout(self.loc, spec, locked.as_deref()).map_err(named)?;
        if !crate::probe::is_file(&dir.join(crate::manifest::MANIFEST_FILE)) {
            return Err(named(format!(
                "{} has no `package.velt`{}",
                spec.describe(),
                spec.subdir
                    .as_ref()
                    .map_or(String::new(), |d| format!(" in `{d}`"))
            )));
        }
        let manifest = Manifest::from_dir(&dir).map_err(named)?;
        if manifest.package.name != req.name {
            return Err(named(format!(
                "the package in {} is `{}`",
                spec.describe(),
                manifest.package.name
            )));
        }
        let root = crate::url_source::archive_root(&dir, spec);
        let version = manifest.version();
        let chain = [req.chain.clone(), vec![format!("{} {version}", req.name)]].concat();
        let mut dependencies = vec![];
        for r in dependency_requirements(&manifest, &dir, &chain, false)? {
            let want = match r.want {
                // A path dependency of a url package is another directory of the same archive,
                // pinned by the same hash, never a directory elsewhere.
                Want::Path { dir: path, req } => {
                    if !path.starts_with(&root) || !inside(&path, &root) {
                        return Err(named(format!(
                            "its dependency `{}` has a `path` that leads out of the archive",
                            r.name
                        )));
                    }
                    let rel = crate::relpath::relative(&path, &root);
                    let sibling = UrlSpec {
                        subdir: (rel != ".").then_some(rel),
                        ..spec.clone()
                    };
                    Want::Url { spec: sibling, req }
                }
                want => want,
            };
            dependencies.push((r.name, want));
        }
        Ok(Candidate {
            version,
            source: Source::Url {
                spec: spec.clone(),
                checksum,
                dir,
            },
            dependencies,
            root_controlled: false,
        })
    }
}

/// One archive and directory, whichever URL the lock or the manifest names it by.
fn same(a: &UrlSpec, b: &UrlSpec) -> bool {
    (&a.sha256, a.strip, &a.subdir) == (&b.sha256, b.strip, &b.subdir)
}
