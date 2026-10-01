//! vpm subcommands: `add`, `install`, `update`, `publish` (`new`/`init` are in `create`). Status
//! lines go to stderr (cargo style), so stdout stays free for program output.

use vpm::edit::DependencySpec;
use vpm::{InstallOptions, Locations};

use super::project::Project;
use crate::style;

/// `velt add <name>[@<req>] [--path <dir>]`: edit velt.toml, then install; the manifest is
/// restored if the install fails.
pub fn add(name: &str, version: Option<String>, path: Option<String>) -> Result<(), String> {
    let project_root = Project::current_root()?;
    let loc = Locations::from_env()?.with_manifest(&vpm::Manifest::from_dir(&project_root)?);
    let version = match (version, &path) {
        (None, None) => Some(latest_version(&loc, name)?),
        (v, _) => v,
    };
    let manifest_path = project_root.join(vpm::manifest::MANIFEST_FILE);
    let original = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read `{}`: {e}", manifest_path.display()))?;
    let spec = DependencySpec {
        version: version.clone(),
        path: path.clone(),
    };
    vpm::edit::add_dependency(&project_root, name, &spec)?;
    if let Err(e) = vpm::install(&project_root, &loc, InstallOptions::default()) {
        let _ = std::fs::write(&manifest_path, original);
        return Err(e);
    }
    let what = match (version, path) {
        (Some(v), Some(p)) => format!("{v} (path {p})"),
        (Some(v), None) => v,
        (None, Some(p)) => format!("path {p}"),
        (None, None) => unreachable!("ICE: version was filled in above"),
    };
    style::status("Adding", &format!("`{name}` {what}"));
    Ok(())
}

/// The newest published version of `name`, as a requirement string.
fn latest_version(loc: &Locations, name: &str) -> Result<String, String> {
    let index = vpm::registry::read_index(loc, name)?.ok_or_else(|| {
        format!(
            "package `{name}` is not in the registry `{}`",
            loc.describe()
        )
    })?;
    let latest = index
        .versions
        .iter()
        .map(|e| e.semver())
        .filter(|v| v.pre.is_empty())
        .max();
    latest
        .map(|v| v.to_string())
        .ok_or_else(|| format!("package `{name}` has no stable version"))
}

/// `velt install [--locked]` and `velt update`.
pub fn install(opts: InstallOptions) -> Result<(), String> {
    let root = Project::current_root()?;
    let installed = vpm::install(&root, &Locations::from_env()?, opts)?;
    let count = installed.lockfile.packages.len();
    let lock = if installed.lock_changed {
        "updated velt.lock"
    } else {
        "velt.lock unchanged"
    };
    style::status(
        "Installed",
        &format!(
            "{count} package{} ({lock})",
            if count == 1 { "" } else { "s" }
        ),
    );
    Ok(())
}

/// `velt publish`: copy the current package into the local registry, or upload it to the
/// remote one.
pub fn publish() -> Result<(), String> {
    let root = Project::current_root()?;
    let manifest = vpm::Manifest::from_dir(&root)?;
    let loc = Locations::from_env()?.with_manifest(&manifest);
    let entry = vpm::registry::publish(&root, &loc)?;
    style::status(
        "Published",
        &format!(
            "`{}` {} to {}",
            manifest.package.name,
            entry.version,
            loc.describe()
        ),
    );
    Ok(())
}
