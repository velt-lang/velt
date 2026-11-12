//! Where vpm keeps shared state: the registry and the package cache.
//!
//! `$VELT_HOME` (default `~/.velt`) holds `cache/`. The registry packages come from, first match
//! wins:
//!
//! 1. `$VELT_REGISTRY` set to an `http(s)://` URL: that registry server;
//! 2. the root package's `registry: "…"` in `package.velt`, or for a member of a workspace the
//!    workspace's (members leave `registry` to it): [`Locations::for_package`];
//! 3. `$VELT_REGISTRY` set to a directory: that local directory registry;
//! 4. the public registry, [`DEFAULT_REGISTRY`].
//!
//! [`Locations::from_env`] reads the environment (1, 3, 4) and [`Locations::for_dir`] adds the
//! package or workspace around a directory: every command asks it, so a member of a private
//! workspace never reaches the public registry. [`Locations::under`] is a local
//! directory registry under one home, what tests use. Everything else in vpm takes a
//! [`Locations`] explicitly so tests can use isolated temp dirs without touching process-wide
//! environment variables, and never ask the public registry.

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;
use crate::workspace::WorkspaceFile;

/// The public registry: where packages come from and go to when neither `$VELT_REGISTRY` nor the
/// root package's `registry` names another one (and where `velt login` signs in by default).
pub const DEFAULT_REGISTRY: &str = "https://registry.veltpm.com";

/// Whether a registry location is a URL rather than a directory.
pub fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// `$VELT_HOME`, default `~/.velt`: the cache, the default local registry and the credentials.
pub fn velt_home() -> Result<PathBuf, String> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    match var("VELT_HOME") {
        Some(h) => Ok(h),
        None => var("HOME")
            .or_else(|| var("USERPROFILE"))
            .map(|h| h.join(".velt"))
            .ok_or_else(|| "cannot find the home directory; set VELT_HOME".into()),
    }
}

/// Registry and cache directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    /// Local registry root: `<name>/index.json` and `<name>/<version>/`.
    pub registry: PathBuf,
    /// Extracted packages: `<name>-<version>/`.
    pub cache: PathBuf,
    /// A remote registry's base URL; when set it replaces the local `registry` directory.
    pub remote: Option<String>,
    /// `remote` is [`DEFAULT_REGISTRY`] only because nothing named a registry: the root
    /// package's `registry` replaces it ([`Locations::with_manifest`]).
    pub remote_is_default: bool,
}

impl Locations {
    /// Everything under one home directory (`<home>/registry`, `<home>/cache`).
    pub fn under(home: &Path) -> Locations {
        Locations {
            registry: home.join("registry"),
            cache: home.join("cache"),
            remote: None,
            remote_is_default: false,
        }
    }

    /// Locations from `$VELT_HOME` (default `~/.velt`) and `$VELT_REGISTRY`: a URL, a
    /// directory, or unset for [`DEFAULT_REGISTRY`]. `registry` is a directory in every case
    /// (`<home>/registry` unless `$VELT_REGISTRY` names one): `velt registry serve` serves it.
    pub fn from_env() -> Result<Locations, String> {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Ok(Locations::from_vars(velt_home()?, var("VELT_REGISTRY")))
    }

    /// [`Locations::from_env`] with `$VELT_HOME` resolved to `home` and `$VELT_REGISTRY`'s value.
    pub fn from_vars(home: PathBuf, velt_registry: Option<PathBuf>) -> Locations {
        let mut loc = Locations::under(&home);
        match velt_registry {
            Some(reg) if is_url(&reg.to_string_lossy()) => {
                loc.remote = Some(reg.to_string_lossy().into_owned());
            }
            Some(dir) => loc.registry = dir,
            None => {
                loc.remote = Some(DEFAULT_REGISTRY.to_string());
                loc.remote_is_default = true;
            }
        }
        loc
    }

    /// The registry of the package or workspace around `dir` (the current directory of a
    /// command): [`Locations::for_package`] for the nearest package, else the `registry` of the
    /// workspace whose root (or a directory between it and its members) `dir` is in.
    pub fn for_dir(self, dir: &Path) -> Result<Locations, String> {
        let dir = crate::relpath::absolute(dir);
        if let Some(root) = crate::manifest::find_package_root(&dir) {
            let manifest = Manifest::from_dir(&root)?;
            return self.for_package(&root, &manifest);
        }
        match crate::workspace::find_root(&dir) {
            Some(ws) => {
                let file = WorkspaceFile::from_path(&ws.join(crate::workspace::WORKSPACE_FILE))?;
                Ok(self.with_registry(file.registry.as_deref()))
            }
            None => Ok(self),
        }
    }

    /// The registry of the package rooted at `root` with `manifest`: the `registry` of the
    /// workspace that lists it as a member (members may not set their own), else its own.
    pub fn for_package(self, root: &Path, manifest: &Manifest) -> Result<Locations, String> {
        let registry = match crate::workspace::listing(root)? {
            Some(ws) => ws.registry.or_else(|| manifest.registry.clone()),
            None => manifest.registry.clone(),
        };
        Ok(self.with_registry(registry.as_deref()))
    }

    /// Apply the root package's `registry` (unless `$VELT_REGISTRY` already chose a remote one).
    /// Ignores a workspace: commands use [`Locations::for_package`].
    pub fn with_manifest(self, manifest: &Manifest) -> Locations {
        self.with_registry(manifest.registry.as_deref())
    }

    /// Apply a package's or workspace's `registry` URL: it replaces a local directory registry
    /// and the default one, but not a registry `$VELT_REGISTRY` names by URL.
    pub fn with_registry(mut self, registry: Option<&str>) -> Locations {
        if let Some(url) = registry {
            if self.remote.is_none() || self.remote_is_default {
                self.remote = Some(url.to_string());
                self.remote_is_default = false;
            }
        }
        self
    }

    /// The same registry directory and cache, as a local directory registry: what a registry
    /// server does with its own directory.
    pub fn local(&self) -> Locations {
        Locations {
            remote: None,
            remote_is_default: false,
            ..self.clone()
        }
    }

    /// A warning when nothing names a registry, so packages come from [`DEFAULT_REGISTRY`], but
    /// the directory that was the default before (`<home>/registry`) holds packages: they are
    /// no longer used, and a `velt install` would look for them in the public registry.
    pub fn unused_local_registry(&self) -> Option<String> {
        if !self.remote_is_default || crate::registry::package_names(&self.registry).is_empty() {
            return None;
        }
        let dir = self.registry.display();
        Some(format!(
            "`{dir}` holds packages but is no longer the default registry: packages come from {DEFAULT_REGISTRY}; set VELT_REGISTRY={dir} to keep using it"
        ))
    }

    /// The registry for messages: its URL or directory.
    pub fn describe(&self) -> String {
        match &self.remote {
            Some(url) => url.clone(),
            None => self.registry.display().to_string(),
        }
    }

    /// Directory of one published version inside the registry.
    pub fn registry_package(&self, name: &str, version: &semver::Version) -> PathBuf {
        self.registry.join(name).join(version.to_string())
    }

    /// Directory of one extracted package inside the cache.
    pub fn cached_package(&self, name: &str, version: &semver::Version) -> PathBuf {
        self.cache.join(format!("{name}-{version}"))
    }

    /// Directory of a published native bundle inside the registry.
    pub fn registry_native(&self, name: &str, version: &semver::Version, target: &str) -> PathBuf {
        self.registry
            .join(name)
            .join(format!("{version}.native"))
            .join(target)
    }

    /// Native libraries of one package version inside the cache: `<cache>/native/<name>-<version>/`
    /// (a verified bundle per target, and cargo's work for builds from source).
    pub fn cached_native_root(&self, name: &str, version: &semver::Version) -> PathBuf {
        self.cache.join("native").join(format!("{name}-{version}"))
    }

    /// A verified prebuilt bundle inside the cache.
    pub fn cached_native(&self, name: &str, version: &semver::Version, target: &str) -> PathBuf {
        self.cached_native_root(name, version).join(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_registry_applies_unless_a_remote_is_set() {
        let m = Manifest::parse(
            "export const pkg: Package = { name: \"a\", version: \"1.0.0\", registry: \"http://r.example:8091\" };",
        )
        .unwrap();
        let loc = Locations::under(Path::new("/h")).with_manifest(&m);
        assert_eq!(loc.describe(), "http://r.example:8091");
        let mut env = Locations::under(Path::new("/h"));
        env.remote = Some("https://other".into());
        assert_eq!(
            env.with_manifest(&m).remote.as_deref(),
            Some("https://other")
        );
        assert!(Manifest::parse(
            "export const pkg: Package = { name: \"a\", version: \"1.0.0\", registry: \"/tmp/x\" };"
        )
        .unwrap_err()
        .contains("must be an http"));
        assert!(is_url("https://x") && !is_url("C:/x"));
    }

    fn manifest(registry: Option<&str>) -> Manifest {
        let field = registry.map_or(String::new(), |r| format!(", registry: \"{r}\""));
        Manifest::parse(&format!(
            "export const pkg: Package = {{ name: \"a\", version: \"1.0.0\"{field} }};"
        ))
        .unwrap()
    }

    #[test]
    fn with_nothing_set_the_registry_is_veltpm() {
        let home = PathBuf::from("/h");
        let loc = Locations::from_vars(home.clone(), None);
        assert_eq!(DEFAULT_REGISTRY, "https://registry.veltpm.com");
        assert_eq!(loc.remote.as_deref(), Some(DEFAULT_REGISTRY));
        assert_eq!(loc.describe(), DEFAULT_REGISTRY);
        // `velt registry serve` still has a directory to serve.
        assert_eq!(loc.registry, home.join("registry"));
        // A package without `registry` keeps the default.
        let loc = loc.with_manifest(&manifest(None));
        assert_eq!(loc.remote.as_deref(), Some(DEFAULT_REGISTRY));
    }

    #[test]
    fn velt_registry_set_to_a_directory_is_a_local_registry() {
        let dir = PathBuf::from("/shared/registry");
        let loc = Locations::from_vars("/h".into(), Some(dir.clone()));
        assert_eq!((loc.remote.as_deref(), &loc.registry), (None, &dir));
        assert_eq!(loc.describe(), dir.display().to_string());
        assert_eq!(loc.clone().with_manifest(&manifest(None)), loc);
        // A package's `registry` URL replaces it, as it did the local registry before.
        let named = loc.with_manifest(&manifest(Some("https://r.example")));
        assert_eq!(named.remote.as_deref(), Some("https://r.example"));
    }

    #[test]
    fn velt_registry_set_to_a_url_is_that_registry_even_over_the_manifests() {
        let url = "https://registry-staging.veltpm.com";
        let loc = Locations::from_vars("/h".into(), Some(url.into()));
        assert_eq!(loc.remote.as_deref(), Some(url));
        let loc = loc.with_manifest(&manifest(Some("https://r.example")));
        assert_eq!(loc.describe(), url);
    }

    #[test]
    fn a_home_registry_with_packages_is_reported_when_unused() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let default = Locations::from_vars(home.clone(), None);
        assert_eq!(default.unused_local_registry(), None, "no packages there");
        std::fs::create_dir_all(home.join("registry/greet")).unwrap();
        std::fs::write(home.join("registry/greet/index.json"), "{\"versions\": []}").unwrap();
        let warning = default.unused_local_registry().unwrap();
        assert!(
            warning.contains("is no longer the default registry")
                && warning.contains(&format!(
                    "VELT_REGISTRY={}",
                    home.join("registry").display()
                )),
            "{warning}"
        );
        // Used (or replaced by a named registry): nothing to say.
        let local = Locations::from_vars(home.clone(), Some(home.join("registry")));
        assert_eq!(local.unused_local_registry(), None);
        let named = default.with_manifest(&manifest(Some("https://r.example")));
        assert_eq!(named.unused_local_registry(), None);
    }

    #[test]
    fn the_manifests_registry_replaces_the_default() {
        let loc = Locations::from_vars("/h".into(), None)
            .with_manifest(&manifest(Some("https://r.example")));
        assert_eq!(loc.remote.as_deref(), Some("https://r.example"));
        assert!(!loc.remote_is_default);
        assert_eq!(loc.local().remote, None);
    }
}
