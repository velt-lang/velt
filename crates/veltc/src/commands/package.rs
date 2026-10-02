//! vpm subcommands: `add`, `install`, `update`, `publish`, `native build`, `manifest` (`new`/`init`
//! are in `create`). Status lines go to stderr (cargo style), so stdout stays free for program output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vpm::edit::DependencySpec;
use vpm::native::NativeOrigin;
use vpm::{InstallOptions, Locations};

use super::project::Project;
use crate::style;

/// `velt add <name>[@<req>] [--path <dir>]`: edit package.vlt, then install; the manifest is
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
    let opts = InstallOptions {
        target: Some(velt_codegen_cl::host_triple()),
        ..Default::default()
    };
    let installed = match vpm::install(&project_root, &loc, opts) {
        Ok(i) => i,
        Err(e) => {
            let _ = std::fs::write(&manifest_path, original);
            return Err(e);
        }
    };
    let what = match (version, path) {
        (Some(v), Some(p)) => format!("{v} (path {p})"),
        (Some(v), None) => v,
        (None, Some(p)) => format!("path {p}"),
        (None, None) => unreachable!("ICE: version was filled in above"),
    };
    style::status("Adding", &format!("`{name}` {what}"));
    report_native(&installed.graph);
    Ok(())
}

/// `velt manifest --json`: the package's validated manifest as JSON on stdout, with defaults
/// filled in, for tools that cannot read `package.vlt` themselves.
pub fn manifest_json() -> Result<(), String> {
    let manifest = vpm::Manifest::from_dir(&Project::current_root()?)?;
    let json = serde_json::to_string_pretty(&manifest.to_json())
        .expect("ICE: a manifest serializes to JSON");
    println!("{json}");
    Ok(())
}

/// List the packages that run native code (and where their libraries came from).
fn report_native(graph: &vpm::PackageGraph) {
    for (pkg, lib) in graph.natives() {
        let how = match lib.origin {
            NativeOrigin::Prebuilt => "prebuilt, checksum verified",
            NativeOrigin::BuiltFromSource => "built from source",
        };
        style::status(
            "Native",
            &format!(
                "`{}` {} runs native code ({how}, {})",
                pkg.name, lib.meta.version, lib.meta.target
            ),
        );
    }
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
    report_native(&installed.graph);
    Ok(())
}

/// `velt publish`: copy the current package into the local registry, or upload it to the
/// remote one, with a prebuilt native library for every `native` target. `--native-only` adds
/// libraries for targets not yet published to the published version.
pub fn publish(native_artifacts: Option<&Path>, native_only: bool) -> Result<(), String> {
    let root = Project::current_root()?;
    let manifest = vpm::Manifest::from_dir(&root)?;
    let loc = Locations::from_env()?.with_manifest(&manifest);
    let bundles = collect_bundles(&root, &manifest, native_artifacts, native_only)?;
    let entry = if native_only {
        vpm::registry::add_native(&root, &loc, &bundles)?
    } else {
        vpm::registry::publish_with_native(&root, &loc, &bundles)?
    };
    style::status(
        "Published",
        &format!(
            "`{}` {} to {}",
            manifest.package.name,
            entry.version,
            loc.describe()
        ),
    );
    for target in bundles.keys() {
        style::status("Native", &format!("prebuilt library for {target}"));
    }
    Ok(())
}

/// The bundle of each `native` target: from `artifacts/<triple>/`, else
/// `target/velt-native/<triple>/`; the host's is built when it is in neither. With
/// `native_only`, whatever bundles exist (at least one).
fn collect_bundles(
    root: &Path,
    manifest: &vpm::Manifest,
    artifacts: Option<&Path>,
    native_only: bool,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let Some(native) = &manifest.native else {
        if artifacts.is_some() || native_only {
            return Err(format!(
                "package `{}` has no `native` in package.vlt",
                manifest.package.name
            ));
        }
        return Ok(BTreeMap::new());
    };
    let host = velt_codegen_cl::host_triple();
    let built = root.join("target").join("velt-native");
    let mut bundles = BTreeMap::new();
    for target in &native.targets {
        let candidates = artifacts
            .map(|a| a.join(target))
            .into_iter()
            .chain([built.join(target)]);
        let found = candidates
            .into_iter()
            .find(|d| d.join(vpm::native::META_FILE).is_file());
        let dir = match found {
            Some(d) => {
                // A host bundle built here may be stale: rebuild it (a no-op when current).
                if *target == host && d == built.join(target) {
                    build_bundle(root, manifest, target)?
                } else {
                    d
                }
            }
            None if *target == host && !native_only => build_bundle(root, manifest, target)?,
            None => continue,
        };
        bundles.insert(target.clone(), dir);
    }
    if native_only && bundles.is_empty() {
        return Err(
            "no native libraries to add (build them with `velt native build --target <triple>`)"
                .into(),
        );
    }
    Ok(bundles)
}

fn build_bundle(root: &Path, manifest: &vpm::Manifest, target: &str) -> Result<PathBuf, String> {
    let work = root.join("target").join("velt-native");
    let out = work.join(target);
    style::status("Building", &format!("native library for {target}"));
    vpm::native::build::build(vpm::native::build::BuildRequest {
        root,
        manifest,
        target,
        out: &out,
        cargo_target_dir: &work.join("cargo"),
        locked: false,
    })?;
    Ok(out)
}

/// `velt native build [--target <triple>]`.
pub fn native_build(target: Option<String>) -> Result<(), String> {
    let root = Project::current_root()?;
    let manifest = vpm::Manifest::from_dir(&root)?;
    let target = target.unwrap_or_else(velt_codegen_cl::host_triple);
    if !vpm::manifest::NATIVE_TARGETS.contains(&target.as_str()) {
        return Err(format!(
            "native libraries cannot be built for `{target}` (supported: {})",
            vpm::manifest::NATIVE_TARGETS.join(", ")
        ));
    }
    if manifest.native.is_none() {
        return Err(format!(
            "package `{}` has no `native` in package.vlt",
            manifest.package.name
        ));
    }
    let out = build_bundle(&root, &manifest, &target)?;
    let meta = vpm::native::NativeMeta::read(&out)?;
    style::status(
        "Built",
        &format!(
            "{} ({} export{})",
            out.display(),
            meta.exports.len(),
            if meta.exports.len() == 1 { "" } else { "s" }
        ),
    );
    Ok(())
}
