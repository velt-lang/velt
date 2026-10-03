//! `velt check --ts-compat <file|dir>...`: check exactly the given files (directories: their
//! source files) in one front-end run, then lint the ones that passed for the TypeScript/Velt
//! common subset ([`velt_tscompat`]). A file with an error of its own is reported and not
//! linted, so the rules only ever see valid Velt.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, FileId, Severity};
use velt_tscompat::{Finding, LintModule};

use super::test::discover;
use crate::cli::CheckArgs;
use crate::driver::{self, BuildError, CheckScope, Session};
use crate::loader::Loaded;

/// Check the files in `paths` together, as library modules (no `main` required), then lint
/// those without errors. Returns the check's outcome and the findings.
pub(super) fn run(
    sess: &mut Session,
    args: &CheckArgs,
    paths: &[PathBuf],
) -> (Result<(), BuildError>, Vec<Finding>) {
    let files = match expand(paths) {
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
            let findings = lint_loaded(sess, &loaded, &files);
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

/// `path` with links and `..` resolved, for comparing files named in different ways.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Lint the loaded modules that are among `files` and have no error of their own.
fn lint_loaded(sess: &Session, loaded: &Loaded, files: &[PathBuf]) -> Vec<Finding> {
    let scope: Vec<PathBuf> = files.iter().map(|f| canonical(f)).collect();
    let paths: Vec<PathBuf> = loaded
        .modules
        .iter()
        .map(|m| canonical(&sess.sm.get(m.file).path))
        .collect();
    let failed: HashSet<FileId> = sess
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .filter_map(|d| d.labels.first().map(|l| l.span.file))
        .collect();
    let index: HashMap<&str, usize> = loaded
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();
    let mut modules = vec![];
    for (i, m) in loaded.modules.iter().enumerate() {
        if m.is_std || failed.contains(&m.file) || !scope.contains(&paths[i]) {
            continue;
        }
        let imports = m
            .imports
            .iter()
            .filter_map(|(spec, target)| index.get(target.as_str()).map(|&j| (spec, j)))
            .map(|(spec, j)| (spec.clone(), paths[j].clone()))
            .collect();
        let runtime = m.jsx_runtime.as_deref().and_then(|rt| index.get(rt));
        modules.push(LintModule {
            path: &paths[i],
            src: &sess.sm.get(m.file).src,
            ast: &m.ast,
            imports,
            default_jsx_provider: runtime.is_some_and(|&j| loaded.modules[j].is_std),
        });
    }
    let scope: Vec<&Path> = scope.iter().map(PathBuf::as_path).collect();
    velt_tscompat::lint(&modules, &scope)
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
