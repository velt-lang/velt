//! Where vpm keeps shared state: the local registry and the package cache.
//!
//! `$VELT_HOME` (default `~/.velt`) holds `cache/`; the registry is `$VELT_REGISTRY` or
//! `<home>/registry`. A remote registry is an `http(s)://` URL: `$VELT_REGISTRY` set to one, or
//! the root package's `registry = "…"` in `velt.toml` ([`Locations::with_manifest`]). Everything else in vpm takes a [`Locations`] explicitly so tests can use
//! isolated temp dirs without touching process-wide environment variables.

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;

/// Whether a registry location is a URL rather than a directory.
pub fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Registry and cache directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    /// Local registry root: `<name>/index.toml` and `<name>/<version>/`.
    pub registry: PathBuf,
    /// Extracted packages: `<name>-<version>/`.
    pub cache: PathBuf,
    /// A remote registry's base URL; when set it replaces the local `registry` directory.
    pub remote: Option<String>,
}

impl Locations {
    /// Everything under one home directory (`<home>/registry`, `<home>/cache`).
    pub fn under(home: &Path) -> Locations {
        Locations {
            registry: home.join("registry"),
            cache: home.join("cache"),
            remote: None,
        }
    }

    /// Locations from `$VELT_HOME` / `$VELT_REGISTRY`, falling back to `~/.velt`.
    pub fn from_env() -> Result<Locations, String> {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home = match var("VELT_HOME") {
            Some(h) => h,
            None => var("HOME")
                .or_else(|| var("USERPROFILE"))
                .map(|h| h.join(".velt"))
                .ok_or("cannot find the home directory; set VELT_HOME")?,
        };
        let mut loc = Locations::under(&home);
        if let Some(reg) = var("VELT_REGISTRY") {
            let text = reg.to_string_lossy();
            if is_url(&text) {
                loc.remote = Some(text.into_owned());
            } else {
                loc.registry = reg;
            }
        }
        Ok(loc)
    }

    /// Apply the root package's `registry` (unless `$VELT_REGISTRY` already chose a remote one).
    pub fn with_manifest(mut self, manifest: &Manifest) -> Locations {
        if self.remote.is_none() {
            self.remote = manifest.registry.clone();
        }
        self
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_registry_applies_unless_a_remote_is_set() {
        let m = Manifest::parse(
            "registry = \"http://r.example:8091\"\n[package]\nname = \"a\"\nversion = \"1.0.0\"\n",
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
            "registry = \"/tmp/x\"\n[package]\nname = \"a\"\nversion = \"1.0.0\"\n"
        )
        .unwrap_err()
        .contains("must be an http"));
        assert!(is_url("https://x") && !is_url("C:/x"));
    }
}
