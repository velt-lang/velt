//! A dependency's helpers and its checks, beside its model (`Dependency`, `DetailedDependency`
//! in this module's parent).

use super::*;

impl Dependency {
    /// How the dependency reads in messages: `version ^1.2`, `path ../util`, `git https://…`,
    /// `url https://… (sha256 0123abcd…)`.
    pub fn describe(&self) -> String {
        if let Some(spec) = self.url_spec() {
            return format!("url {}", spec.describe());
        }
        if let Some(spec) = self.git() {
            return format!("git {}", spec.describe());
        }
        match (self.version(), self.path()) {
            (_, Some(path)) => format!("path {path}"),
            (Some(v), None) => format!("version {v}"),
            (None, None) => "no source".to_string(),
        }
    }

    /// The semver requirement, if any.
    pub fn version(&self) -> Option<&str> {
        match self {
            Dependency::Version(v) => Some(v),
            Dependency::Detailed(d) => d.version.as_deref(),
        }
    }

    /// The local path, if this is a path dependency (with `git`, the directory in the
    /// repository).
    pub fn path(&self) -> Option<&str> {
        match self {
            Dependency::Version(_) => None,
            Dependency::Detailed(d) => d.path.as_deref(),
        }
    }
    /// Whether this is `"workspace"`: provided by the enclosing workspace.
    pub fn is_workspace(&self) -> bool {
        matches!(self, Dependency::Version(v) if v == WORKSPACE_REQUIREMENT)
    }
}

pub(crate) fn check_dependency_name(name: &str) -> Result<(), String> {
    if name.starts_with('@') {
        return check_scoped_name(name, "dependency name");
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(reserved(name));
    }
    if is_windows_device_name(name) {
        return Err(device_name(name));
    }
    if is_native_prefix_name(name) {
        return Err(native_prefix(name));
    }
    if is_valid_package_name(name) {
        Ok(())
    } else {
        Err(format!(
            "invalid dependency name `{name}` (lowercase letters and digits, starting with a letter, with single `-` or `_` between words)"
        ))
    }
}

pub(crate) fn check_dependency(name: &str, dep: &Dependency) -> Result<(), String> {
    if dep.is_workspace() {
        return Ok(());
    }
    if dep.version() == Some(WORKSPACE_REQUIREMENT) {
        return Err(format!(
            "dependency `{name}`: `\"{WORKSPACE_REQUIREMENT}\"` is written as the whole value (`{name}: \"{WORKSPACE_REQUIREMENT}\"`), not as a `version`"
        ));
    }
    if let Dependency::Detailed(d) = dep {
        crate::git::check_fields(name, d)?;
        crate::url_source::check_fields(name, d)?;
    }
    let remote = dep.git().is_some() || dep.url_spec().is_some();
    if dep.version().is_none() && dep.path().is_none() && !remote {
        return Err(format!(
            "dependency `{name}` needs a `version`, a `path`, a `git` repository or a `url`"
        ));
    }
    if split_scoped(name).is_some() && dep.path().is_none() && !remote {
        return Err(format!(
            "dependency `{name}` needs a `path`: registries hold unscoped packages only"
        ));
    }
    if let Some(req) = dep.version() {
        semver::VersionReq::parse(req).map_err(|e| {
            format!("dependency `{name}`: invalid version requirement `{req}`: {e}")
        })?;
    }
    Ok(())
}
