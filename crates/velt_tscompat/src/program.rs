//! Linting a loaded program: the modules as the loader produced them, after the type checker
//! ran. `velt check --ts-compat` and the language server both have them, so neither parses or
//! checks anything twice.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, FileId, SourceMap};
use velt_sema::{ide, SourceModule};

use crate::{Finding, LintModule, Program};

/// Lint the modules of a loaded program that are in scope (`in_scope` of their canonical path),
/// that `lint` selects (by file) and that have no error in `diagnostics` (the load's and the
/// checker's), so the rules only see valid Velt. The files in scope are the loaded ones
/// `in_scope` accepts, linted or not: a relative import of any other file leaves the subset.
/// `velt check --ts-compat` lints every module in scope; the language server only the open
/// document, whose imports are still judged against the whole scope. Standard library modules are
/// never in scope (nor canonicalized). `analysis` is the checker's IDE analysis of `modules`
/// (`velt_sema::ide::check_for_ide`), which the rules on types need; without it only the
/// syntax rules run.
pub fn lint_program(
    modules: &[SourceModule],
    sm: &SourceMap,
    diagnostics: &[Diagnostic],
    analysis: Option<&ide::Analysis>,
    in_scope: &dyn Fn(&Path) -> bool,
    lint: &dyn Fn(FileId) -> bool,
) -> Vec<Finding> {
    let paths: Vec<PathBuf> = modules
        .iter()
        .map(|m| {
            let path = &sm.get(m.file).path;
            if m.is_std {
                path.clone()
            } else {
                canonical(path)
            }
        })
        .collect();
    let scoped: Vec<bool> = modules
        .iter()
        .zip(&paths)
        .map(|(m, p)| !m.is_std && in_scope(p))
        .collect();
    let failed: HashSet<FileId> = diagnostics
        .iter()
        .filter(|d| d.is_error())
        .filter_map(|d| d.labels.first().map(|l| l.span.file))
        .collect();
    let index: HashMap<&str, usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();
    let mut lint_modules = vec![];
    for (i, m) in modules.iter().enumerate() {
        if !scoped[i] || !lint(m.file) || failed.contains(&m.file) {
            continue;
        }
        let imports = m
            .imports
            .iter()
            .filter_map(|(spec, target)| index.get(target.as_str()).map(|&j| (spec, j)))
            .map(|(spec, j)| (spec.clone(), paths[j].clone()))
            .collect();
        let runtime = m.jsx_runtime.as_deref().and_then(|rt| index.get(rt));
        lint_modules.push(LintModule {
            path: &paths[i],
            src: &sm.get(m.file).src,
            ast: &m.ast,
            imports,
            default_jsx_provider: runtime.is_some_and(|&j| modules[j].is_std),
        });
    }
    let scope: Vec<&Path> = paths
        .iter()
        .zip(&scoped)
        .filter(|(_, s)| **s)
        .map(|(p, _)| p.as_path())
        .collect();
    let program = analysis.map(|analysis| Program {
        analysis,
        modules,
        sm,
    });
    crate::lint(&lint_modules, &scope, program.as_ref())
}

/// `path` with links and `..` resolved, for comparing files named in different ways. A file
/// that does not exist (an editor buffer not saved yet) is named by its canonical directory, and
/// a path without one stays as given.
pub fn canonical(path: &Path) -> PathBuf {
    if let Ok(found) = std::fs::canonicalize(path) {
        return found;
    }
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => std::fs::canonicalize(dir)
            .map(|dir| dir.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}
