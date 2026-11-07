//! vpm subcommands: `add`, `install`, `update`, `publish`, `native build`, `manifest`
//! (`new`/`init` are in `create`, `native headers` in `native_headers`). Status lines go to stderr (cargo style), so stdout stays free
//! for program output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vpm::edit::DependencySpec;
use vpm::native::NativeOrigin;
use vpm::{InstallOptions, Locations};

use super::project::Project;
use crate::style;

/// `velt add <name>[@<req>] [--path <dir>]`: edit package.velt, then install; the manifest is
/// restored if the install fails.
pub fn add(name: &str, version: Option<String>, path: Option<String>) -> Result<(), String> {
    let project_root = Project::current_root()?;
    let loc = Locations::from_env()?.with_manifest(&vpm::Manifest::from_dir(&project_root)?);
    let version = match (version, &path) {
        (None, None) => Some(latest_version(&loc, name)?),
        (v, _) => v,
    };
    let spec = DependencySpec {
        version: version.clone(),
        path: path.clone(),
        ..Default::default()
    };
    let installed = edit_and_install(&project_root, &loc, name, &spec)?;
    warn_yanked(&installed);
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

/// `velt add <url> [--path <dir>]`: download the archive once, write `{ url, sha256 }` (with
/// `strip` and `path` as the archive needs) under the package's own name, then install.
pub fn add_url(url: &str, path: Option<String>) -> Result<(), String> {
    let project_root = Project::current_root()?;
    let loc = Locations::from_env()?.with_manifest(&vpm::Manifest::from_dir(&project_root)?);
    let (name, found) = vpm::url_source::entry_for(&loc, url, path.as_deref())?;
    // The name is the archive's own, not the user's: never a silent replace of an entry.
    vpm::url_source::check_replace(&vpm::Manifest::from_dir(&project_root)?, &name, &found)?;
    let spec = DependencySpec {
        path: found.subdir.clone(),
        url: Some(found.url.clone()),
        sha256: Some(found.sha256.clone()),
        strip: (found.strip > 0).then_some(found.strip),
        ..Default::default()
    };
    let installed = edit_and_install(&project_root, &loc, &name, &spec)?;
    // The resolution's warnings: a credential in the URL's query, links on Windows.
    warn_yanked(&installed);
    style::status("Adding", &format!("`{name}` from {}", found.url));
    let strip = match found.strip {
        0 => String::new(),
        n => format!(", strip {n}"),
    };
    let at = found
        .subdir
        .as_ref()
        .map_or(String::new(), |p| format!(", path {p}"));
    style::status("Pinned", &format!("sha256 {}{strip}{at}", found.sha256));
    style::status(
        "Note",
        "the sha256 was computed from this download; nothing checked it against the publisher. \
         Pin a project's real release asset (a forge's generated archive of a tag can change \
         bytes), or use `git` with a commit for source.",
    );
    report_native(&installed.graph);
    Ok(())
}

/// Add `spec` as dependency `name` of the package at `root` and install; the manifest is
/// restored if the install fails.
fn edit_and_install(
    root: &Path,
    loc: &Locations,
    name: &str,
    spec: &DependencySpec,
) -> Result<vpm::Installed, String> {
    let manifest_path = root.join(vpm::manifest::MANIFEST_FILE);
    let original = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read `{}`: {e}", manifest_path.display()))?;
    vpm::edit::add_dependency(root, name, spec)?;
    let opts = InstallOptions {
        target: Some(velt_codegen_cl::host_triple()),
        ..Default::default()
    };
    vpm::install(root, loc, opts).inspect_err(|_| {
        let _ = std::fs::write(&manifest_path, original);
    })
}

/// `velt manifest [--json]`: read and validate the package's manifest; with `json`, print it on
/// stdout with defaults filled in, for tools that cannot read `package.velt` themselves.
pub fn manifest(json: bool) -> Result<(), String> {
    let root = Project::current_root()?;
    let manifest = vpm::Manifest::from_dir(&root)?;
    super::project::warn_toolchain_mismatch(&manifest);
    if json {
        let json = serde_json::to_string_pretty(&manifest.to_json())
            .expect("ICE: a manifest serializes to JSON");
        println!("{json}");
    } else {
        let path = root.join(vpm::manifest::MANIFEST_FILE);
        let package = &manifest.package;
        style::status(
            "Checked",
            &format!("{} ({} {})", path.display(), package.name, package.version),
        );
    }
    Ok(())
}

/// Warn about installed versions that were yanked after `velt.lock.json` pinned them.
pub fn warn_yanked(installed: &vpm::Installed) {
    for warning in installed.yank_warnings() {
        style::warning(&warning);
    }
}

/// List the packages that run native code (and where their libraries came from).
fn report_native(graph: &vpm::PackageGraph) {
    let prebuilt_c = graph.packages().iter().filter_map(|p| match &p.c_bundle {
        Some(vpm::graph::CBundle::Prebuilt(lib)) => Some((p, &**lib)),
        _ => None,
    });
    for (pkg, lib) in graph.natives().chain(prebuilt_c) {
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

/// The version `velt add` picks for `name`, as a requirement string: the newest stable version
/// that is not yanked, else the newest pre-release that is not (the version `velt search` shows).
fn latest_version(loc: &Locations, name: &str) -> Result<String, String> {
    let index = vpm::registry::read_index(loc, name)?.ok_or_else(|| {
        format!(
            "package `{name}` is not in the registry `{}`",
            loc.describe()
        )
    })?;
    vpm::manifest::ide::registry::newest(&index)
        .map(|v| v.to_string())
        .ok_or_else(|| format!("every version of package `{name}` is yanked"))
}

/// `velt install [--locked]` and `velt update`.
pub fn install(opts: InstallOptions) -> Result<(), String> {
    let installed = match workspace_without_package()? {
        Some(ws) => vpm::install::workspace::install(&ws, None, &Locations::from_env()?, opts)?,
        None => vpm::install(&Project::current_root()?, &Locations::from_env()?, opts)?,
    };
    warn_yanked(&installed);
    for note in &installed.notes {
        eprintln!("note: {note}");
    }
    let count = installed.lockfile.packages.len();
    let lock = if installed.lock_changed {
        "updated velt.lock.json"
    } else {
        "velt.lock.json unchanged"
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

/// The workspace around the current directory when no package of it is nearer (its root, or a
/// directory between the root and its members): that is what `velt install` installs there.
fn workspace_without_package() -> Result<Option<vpm::workspace::Workspace>, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let Some(ws_root) = vpm::workspace::find_root(&cwd) else {
        return Ok(None);
    };
    match vpm::manifest::find_package_root(&cwd) {
        Some(pkg) if pkg.starts_with(&ws_root) => Ok(None),
        _ => vpm::workspace::Workspace::load(&ws_root).map(Some),
    }
}

/// `velt publish`: copy the current package into the local registry, or upload it to the
/// remote one, with a prebuilt native library for every `native` target. `--native-only` adds
/// libraries for targets not yet published to the published version. A package with C
/// libraries has its declarations checked against their headers first (`velt native headers`),
/// and publishes the record.
pub fn publish(native_artifacts: Option<&Path>, native_only: bool) -> Result<(), String> {
    let root = Project::current_root()?;
    let manifest = vpm::Manifest::from_dir(&root)?;
    if manifest.c_libraries.is_some() && !native_only {
        let record = super::native_headers::record(&root)?;
        let targets = record.checked.len().max(record.constants.len());
        if targets > 0 {
            style::status(
                "Checked",
                &format!("C declarations against their headers on {targets} targets"),
            );
        }
    }
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
    if !manifest.has_bundles() {
        if artifacts.is_some() || native_only {
            return Err(format!(
                "package `{}` has no `native` in package.velt",
                manifest.package.name
            ));
        }
        return Ok(BTreeMap::new());
    }
    // zig builds C sources for any target from here; cargo only this machine's.
    let buildable =
        |target: &str| manifest.has_c_sources() || target == velt_codegen_cl::host_triple();
    let built = root.join("target").join("velt-native");
    let mut bundles = BTreeMap::new();
    for target in manifest.bundle_targets() {
        let candidates = artifacts
            .map(|a| a.join(target))
            .into_iter()
            .chain([built.join(target)]);
        let found = candidates
            .into_iter()
            .find(|d| d.join(vpm::native::META_FILE).is_file());
        let dir = match found {
            Some(d) => {
                // A bundle built here may be stale: rebuild it (a no-op when current).
                if buildable(target) && d == built.join(target) {
                    build_bundle(root, manifest, target)?
                } else {
                    d
                }
            }
            None if buildable(target) && !native_only => build_bundle(root, manifest, target)?,
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
    if manifest.has_c_sources() {
        vpm::native::c_build::build(vpm::native::c_build::CBuildRequest {
            root,
            manifest,
            target,
            out: &out,
            origin: NativeOrigin::BuiltFromSource,
        })?;
        return Ok(out);
    }
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
    if !manifest.has_bundles() {
        return Err(format!(
            "package `{}` has no `native` in package.velt",
            manifest.package.name
        ));
    }
    let out = build_bundle(&root, &manifest, &target)?;
    let meta = vpm::native::NativeMeta::read(&out)?;
    let (count, what) = match meta.c.len() {
        0 => (meta.exports.len(), "export"),
        n => (n, "C library"),
    };
    let plural = match (count, what) {
        (1, _) => what.to_string(),
        (_, "C library") => "C libraries".to_string(),
        _ => format!("{what}s"),
    };
    style::status("Built", &format!("{} ({count} {plural})", out.display()));
    Ok(())
}
