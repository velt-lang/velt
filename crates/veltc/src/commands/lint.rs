//! The lint rules of `velt check` (docs/internals/design/lint-profiles.md): the `perf/…`
//! rules with `--perf` (or when the package's `lint` makes one a warning or an error), the
//! optimizer's evidence with `--perf`, and `--fix` for every finding with a fix, ts-compat
//! findings included.

use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, FileId};
use velt_lint::{evidence, Kind, Levels, Profiles};

use crate::cli::{CheckArgs, FixMode};
use crate::driver::{self, BuildError, BuildOptions, Session};

/// The package around a checked file: its files' profiles and its rules' levels.
pub(super) struct Package {
    pub profiles: Profiles,
    pub levels: Levels,
    /// Its manifest, for the warnings about `lint`.
    manifest: Option<PathBuf>,
}

impl Package {
    /// The package around the file `input` (none: every file native, every rule a hint).
    pub fn around(input: &Path) -> Package {
        Package::in_dir(input.parent().unwrap_or(Path::new(".")))
    }

    /// The package around the directory `dir`.
    pub fn in_dir(dir: &Path) -> Package {
        let dir = vpm::relpath::absolute(dir);
        let root = vpm::manifest::find_package_root(&dir);
        let manifest = root.map(|r| r.join(vpm::manifest::MANIFEST_FILE));
        let lint = (manifest.as_ref())
            .and_then(|m| vpm::Manifest::from_path(m).ok())
            .and_then(|m| m.lint);
        Package {
            profiles: Profiles::around(&dir),
            levels: Levels::new(lint.as_ref()),
            manifest,
        }
    }

    /// Whether `velt check` with `args` runs the lint rules.
    pub fn lints(&self, args: &CheckArgs) -> bool {
        args.perf || velt_lint::runs_without_perf(&self.levels, Kind::Pattern)
    }
}

/// The lint findings for the modules `loaded` the check loaded (whose diagnostics are in
/// `sess`), with suppressions applied. `checked` is whether the check found no errors: the rules
/// on the optimized program need a program that builds.
pub(super) fn findings(
    sess: &Session,
    args: &CheckArgs,
    opts: &BuildOptions,
    loaded: &velt_loader::Loaded,
    checked: bool,
    package: &Package,
) -> Vec<Diagnostic> {
    let analysis = velt_sema::ide::check_for_ide(&loaded.modules, loaded.root);
    // `velt check` of a package loads all of its modules; of a file, its imports only.
    let whole_package = args.input.is_none() && args.ts_compat.is_none();
    let mut diags = velt_lint::perf::lint(
        &loaded.modules,
        &sess.sm,
        &sess.diagnostics,
        &analysis,
        &package.profiles,
        &package.levels,
        &|_| true,
        whole_package,
    );
    let evidence_ran = args.perf && checked;
    if evidence_ran {
        match number_doubles(opts) {
            Ok(items) => diags.extend(evidence::number_double(
                &items,
                &loaded.modules,
                &sess.sm,
                &analysis,
                &package.profiles,
                &package.levels,
                args.deps,
            )),
            Err(why) => diags.push(Diagnostic {
                severity: velt_common::Severity::Note,
                labels: vec![],
                ..Diagnostic::error(why, velt_common::Span::DUMMY)
            }),
        }
    }
    if let Some(manifest) = &package.manifest {
        for msg in velt_lint::unknown_codes(&package.levels, manifest) {
            diags.push(Diagnostic {
                severity: velt_common::Severity::Warning,
                labels: vec![],
                ..Diagnostic::error(msg, velt_common::Span::DUMMY)
            });
        }
    }
    let linted: Vec<FileId> = loaded
        .modules
        .iter()
        .filter(|m| !m.is_std)
        .map(|m| m.file)
        .collect();
    let ran = |code: &str| match velt_lint::rules::rule(code) {
        Some(rule) => rule.kind == Kind::Pattern || evidence_ran,
        None => false,
    };
    velt_lint::finish(diags, &sess.sm, &linted, &ran)
}

/// The optimizer's `number` variables that stay doubles, from a release build of the program
/// up to optimized VIR (no code generation), or why there is none.
fn number_doubles(opts: &BuildOptions) -> Result<Vec<evidence::NumberDouble>, String> {
    let mut sess = Session::new();
    let mut opts = opts.clone();
    opts.release = true;
    // Line information keeps the optimized program's source locations (`-g` changes no
    // optimization), which the findings are placed by.
    opts.debug_info = true;
    opts.check_variants = false;
    let program = driver::compile(&mut sess, &opts).map_err(|e| {
        let first = sess.diagnostics.iter().find(|d| d.is_error());
        let why = match (first, e) {
            (Some(d), _) => d.message.clone(),
            (None, BuildError::Failed(m) | BuildError::Ice(m)) => m,
            (None, _) => "it did not build".into(),
        };
        format!("`--perf` could not optimize the program, so the `perf/number-double` rule did not run: {why}")
    })?;
    let mut out = vec![];
    for (_, vars) in velt_opt::number_report(&program) {
        for u in vars {
            let Some(loc) = u.loc else { continue };
            let Some(file) = program.files.get(loc.file as usize) else {
                continue;
            };
            out.push(evidence::NumberDouble {
                file: PathBuf::from(file),
                line: loc.line,
                col: loc.col,
                name: u.name,
                reason: u.reason,
                unbounded: u.why == velt_opt::UnnarrowedWhy::Unbounded,
            });
        }
    }
    Ok(out)
}

/// What `--fix` did.
pub(super) struct Fixed {
    /// Problems fixed, and in how many files.
    pub fixes: usize,
    pub files: usize,
    /// The titles of fixes undone because the program did not check after them.
    pub undone: Vec<String>,
}

/// Apply the fixes `mode` picks among `diagnostics` (of the files in `sess`), then check again
/// with `recheck` (true: no more errors than before). When the checks fail after all of them, they are undone
/// and applied one at a time, keeping each after which the program still checks.
pub(super) fn apply(
    sess: &Session,
    diagnostics: &[Diagnostic],
    mode: FixMode,
    recheck: &dyn Fn() -> bool,
) -> Result<Fixed, String> {
    let which = match mode {
        FixMode::Safe => velt_lint::fix::Which::Safe,
        FixMode::Review => velt_lint::fix::Which::Review,
    };
    let (edits, picked) = velt_lint::fix::pick(diagnostics, which);
    let originals: Vec<(PathBuf, &str)> = edits
        .keys()
        .map(|f| (sess.sm.get(*f).path.clone(), sess.sm.get(*f).src.as_str()))
        .collect();
    let write = |edits: &velt_lint::fix::Edits| -> Result<(), String> {
        let texts: Vec<(PathBuf, String)> = edits
            .iter()
            .map(|(file, list)| {
                let f = sess.sm.get(*file);
                (f.path.clone(), velt_lint::fix::apply(&f.src, list))
            })
            .collect();
        write_all(&texts, &originals)
    };
    let restore = || -> Result<(), String> {
        match restore_all(&originals) {
            failed if failed.is_empty() => Ok(()),
            failed => Err(format!(
                "cannot write back {}: the fixes are still in them",
                listed(&failed)
            )),
        }
    };
    write(&edits)?;
    if recheck() {
        return Ok(Fixed {
            fixes: picked.len(),
            files: edits.len(),
            undone: vec![],
        });
    }
    // One at a time, on top of those kept.
    restore()?;
    let mut kept: Vec<usize> = vec![];
    let mut undone = vec![];
    for &i in &picked {
        let mut trial: Vec<Diagnostic> = kept.iter().map(|&k| diagnostics[k].clone()).collect();
        trial.push(diagnostics[i].clone());
        let (edits, _) = velt_lint::fix::pick(&trial, which);
        write(&edits)?;
        if recheck() {
            kept.push(i);
        } else {
            restore()?;
            let title = diagnostics[i]
                .fixes
                .first()
                .map_or("", |f| f.title.as_str());
            undone.push(format!("{} ({})", title, diagnostics[i].message));
        }
    }
    let (edits, _) = velt_lint::fix::pick(
        &kept
            .iter()
            .map(|&k| diagnostics[k].clone())
            .collect::<Vec<_>>(),
        which,
    );
    restore()?;
    write(&edits)?;
    Ok(Fixed {
        fixes: kept.len(),
        files: edits.len(),
        undone,
    })
}

/// Write each of `texts` (path, new text). All or nothing: when one write fails, every file of
/// `originals` (path, text before the fixes) is put back ([`restore_all`]), so a fix that edits
/// several files is never left half applied, and the error says whether that worked.
fn write_all(texts: &[(PathBuf, String)], originals: &[(PathBuf, &str)]) -> Result<(), String> {
    for (path, text) in texts {
        if let Err(e) = std::fs::write(path, text) {
            let failed = restore_all(originals);
            let state = match failed.is_empty() {
                true => "no file was changed".to_string(),
                false => format!("{} could not be written back: check them", listed(&failed)),
            };
            return Err(format!("cannot write `{}`: {e}; {state}", path.display()));
        }
    }
    Ok(())
}

/// Put every file of `originals` back to its text, trying each one (a file that already has it,
/// such as one whose write failed before changing it, is left alone). Returns the files that
/// could not be written.
fn restore_all(originals: &[(PathBuf, &str)]) -> Vec<PathBuf> {
    originals
        .iter()
        .filter(|(path, src)| std::fs::read_to_string(path).ok().as_deref() != Some(*src))
        .filter(|(path, src)| std::fs::write(path, src).is_err())
        .map(|(path, _)| path.clone())
        .collect()
}

/// `` `a`, `b` `` for the files `paths`.
fn listed(paths: &[PathBuf]) -> String {
    let names: Vec<String> = paths.iter().map(|p| format!("`{}`", p.display())).collect();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file that already has its original text is not written back, so one that could not
    /// be written (and so never changed) doesn't count as one that could not be restored.
    #[test]
    fn restoring_skips_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.vlt");
        std::fs::write(&a, "changed").unwrap();
        let missing = dir.path().join("no-such-dir").join("b.vlt");
        // `missing` can't be written, so restoring it fails: it is reported.
        assert_eq!(
            restore_all(&[(a.clone(), "before"), (missing.clone(), "x")]),
            [missing]
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "before");
        // `a` has its text now: nothing to write, nothing reported.
        assert!(restore_all(&[(a, "before")]).is_empty());
    }

    /// A write that fails after another succeeded puts the first file back.
    #[test]
    fn a_failed_write_changes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.vlt");
        std::fs::write(&a, "before").unwrap();
        let missing = dir.path().join("no-such-dir").join("b.vlt");
        let texts = [(a.clone(), "after".to_string()), (missing, "x".to_string())];
        let err = write_all(&texts, &[(a.clone(), "before")]).unwrap_err();
        assert!(err.contains("no file was changed"), "{err}");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "before");
    }
}
