//! `velt check --ts-compat [<file|dir>...]`: check exactly the given files (directories: their
//! source files; no paths: the package's `tsCompat` folders) in one front-end run, then lint the
//! ones that passed for the TypeScript/Velt common subset ([`velt_tscompat`]). A file with an
//! error of its own is reported and not linted, so the rules only ever see valid Velt. The
//! rules on types run on the checker's IDE analysis of the same modules, made only here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Severity};
use velt_tscompat::{canonical, Finding};

use super::test::discover;
use crate::cli::CheckArgs;
use crate::driver::{self, BuildError, CheckScope, Session};

/// Check the files in `paths` (none: the package's `tsCompat` folders) together, as library
/// modules (no `main` required), then lint those without errors. Returns the check's outcome
/// and the findings.
pub(super) fn run(
    sess: &mut Session,
    args: &CheckArgs,
    paths: &[PathBuf],
) -> (Result<(), BuildError>, Vec<Finding>) {
    let files = if paths.is_empty() {
        package_folders().and_then(|dirs| folder_files(&dirs))
    } else {
        expand(paths)
    };
    let files = match files {
        Ok(files) => files,
        Err(msg) => return (Err(BuildError::Failed(msg)), vec![]),
    };
    let Some((first, rest)) = files.split_first() else {
        return (Err(BuildError::Failed("no files to lint".into())), vec![]);
    };
    if let Err(msg) = one_package(&files) {
        return (Err(BuildError::Failed(msg)), vec![]);
    }
    let opts = match super::check::options_for(first, args.locked) {
        Ok(opts) => opts,
        Err(e) => return (Err(e), vec![]),
    };
    let scope = CheckScope {
        require_main: false,
        extra_roots: rest.to_vec(),
    };
    let same_path = super::check::same_path_groups(&files);
    match driver::check_for_lint(sess, &opts, &scope) {
        Ok((loaded, checked)) => {
            let scope: HashSet<PathBuf> = files.iter().map(|f| canonical(f)).collect();
            // The typed rules ask the checker's IDE analysis (types, what names refer to),
            // which the plain check doesn't keep.
            let analysis = velt_sema::ide::check_for_ide(&loaded.modules, loaded.root);
            let findings = velt_tscompat::lint_program(
                &loaded.modules,
                &sess.sm,
                &sess.diagnostics,
                Some(&analysis),
                &|path| scope.contains(path),
                &|_| true,
            );
            super::check::report_same_path(sess, &same_path, first);
            let checked = match checked {
                Ok(()) if !same_path.is_empty() => Err(BuildError::Diagnostics),
                other => other,
            };
            (checked, findings)
        }
        Err(e) => (Err(e), vec![]),
    }
}

/// The package root of `file` (the nearest directory above it with a manifest), if any.
fn package_of(file: &Path) -> Option<PathBuf> {
    let root = vpm::manifest::find_package_root(file.parent().unwrap_or(Path::new("")))?;
    Some(canonical(&root))
}

/// Every file of `files` is in the same package, or none is in one: the files are checked
/// together, inside one package's settings and dependencies.
fn one_package(files: &[PathBuf]) -> Result<(), String> {
    let Some((first, rest)) = files.split_first() else {
        return Ok(());
    };
    let package = package_of(first);
    let Some(other) = rest.iter().find(|f| package_of(f) != package) else {
        return Ok(());
    };
    let describe = |root: Option<PathBuf>| match root {
        Some(root) => {
            let cwd = std::env::current_dir()
                .map(|d| canonical(&d))
                .unwrap_or_default();
            let shown = vpm::relpath::relative(&root, &cwd);
            match vpm::Manifest::from_dir(&root) {
                Ok(m) => format!("package `{}` (`{shown}`)", m.package.name),
                Err(_) => format!("the package at `{shown}`"),
            }
        }
        None => "no package".to_string(),
    };
    Err(format!(
        "lint one package per run: `{}` is in {}, `{}` in {}",
        first.display(),
        describe(package),
        other.display(),
        describe(package_of(other))
    ))
}

/// The `tsCompat` folders of the package around the current directory, as the current
/// directory names them. Outside a package, or without `tsCompat`, the message says to list
/// folders or name paths; a folder that is not there is an error.
fn package_folders() -> Result<Vec<PathBuf>, String> {
    const NAME_PATHS: &str = "Or name the files or directories to lint: `velt check --ts-compat \
                              src/models`";
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let Some(root) = vpm::manifest::find_package_root(&cwd) else {
        return Err(format!(
            "`velt check --ts-compat` without paths lints the folders a package's `tsCompat` \
             lists, but there is no `package.vlt` in `{}` or any parent directory. {NAME_PATHS}",
            cwd.display()
        ));
    };
    let manifest = vpm::Manifest::from_dir(&root)?;
    let name = &manifest.package.name;
    if manifest.ts_compat.is_empty() {
        return Err(format!(
            "package `{name}` has no `tsCompat` folders to lint: list them in package.vlt \
             (`tsCompat: [\"src/models\"]`). {NAME_PATHS}"
        ));
    }
    let mut dirs = vec![];
    for (listed, dir) in manifest
        .ts_compat
        .iter()
        .zip(manifest.ts_compat_dirs(&root))
    {
        if !dir.is_dir() {
            let what = if dir.exists() {
                "is not a folder"
            } else {
                "does not exist"
            };
            return Err(format!(
                "package `{name}`: `tsCompat` folder `{listed}` {what}"
            ));
        }
        dirs.push(PathBuf::from(vpm::relpath::relative(&dir, &cwd)));
    }
    Ok(dirs)
}

/// The source files of the `tsCompat` folders `dirs`; an empty folder adds none, but there must
/// be at least one file.
fn folder_files(dirs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![];
    for dir in dirs {
        // The current directory's files are named without `./`, like the folders' are.
        let found = discover::source_files(dir)?.into_iter();
        files.extend(found.map(|f| f.strip_prefix(".").map(Path::to_path_buf).unwrap_or(f)));
    }
    if files.is_empty() {
        return Err("the `tsCompat` folders have no `.vlt`, `.ts` or `.tsx` files to lint".into());
    }
    Ok(files)
}

/// The source files `paths` name: files as given, directories' `.vlt`, `.ts` and `.tsx` files
/// (recursively, as `velt check` finds a package's modules), each once.
fn expand(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![];
    for path in paths {
        if path.is_dir() {
            let found = discover::source_files(path)?;
            if found.is_empty() {
                return Err(format!(
                    "`{}` has no `.vlt`, `.ts` or `.tsx` files to lint",
                    path.display()
                ));
            }
            files.extend(found);
        } else if path.to_string_lossy().ends_with(".d.ts") {
            return Err(format!(
                "`{}`: declaration files (`.d.ts`) are not modules",
                path.display()
            ));
        } else {
            super::project::check_input_file(path)?;
            files.push(path.clone());
        }
    }
    let mut seen = HashSet::new();
    files.retain(|f| seen.insert(canonical(f)));
    Ok(files)
}

/// `f` as a diagnostic, its code in the last note: `ts-compat(<code>)`.
pub(super) fn diagnostic(f: &Finding) -> Diagnostic {
    let mut d = Diagnostic::error(f.message.clone(), f.span);
    if f.severity == velt_tscompat::Severity::Warning {
        d.severity = Severity::Warning;
    }
    for note in &f.notes {
        d = d.with_note(note.clone());
    }
    d.with_note(format!("ts-compat({})", f.code))
}
