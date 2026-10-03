//! One analysis run for an open document: load it (with its imports, open buffers overlaid) through
//! the [`ProgramLoader`], then run sema's IDE check (`velt_sema::ide::check_for_ide`) for the
//! editor queries. Sema's diagnostics are shown only when loading and parsing succeeded — the same
//! "stop after the first failing stage" policy as `velt build`, so the editor shows exactly what the
//! compiler would — but its queries also answer on a document the parser had to recover. A
//! document in a package's `tsCompat` folders is then linted on the same modules
//! ([`crate::ts_compat`]).

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap, Span};
use velt_sema::{ide, SourceModule};
use velt_syntax::ast;

use crate::{LoadedProgram, ProgramLoader};

/// Everything the request handlers need about one document's program.
pub struct Analysis {
    /// Every loaded file (the document, its imports, the prelude).
    pub sm: SourceMap,
    /// Parsed modules; `modules[root]` is the document.
    pub modules: Vec<SourceModule>,
    /// Index of the document's module.
    pub root: usize,
    /// Load, parse and sema diagnostics (all files).
    pub diagnostics: Diagnostics,
    /// Sema's query tables (also for programs with errors), unless sema could not run.
    pub ide: Option<ide::Analysis>,
    /// The TypeScript-compatibility findings in the document (none outside `tsCompat` folders).
    pub ts_compat: Vec<velt_tscompat::Finding>,
}

impl Analysis {
    /// The document's module.
    pub fn module(&self) -> &SourceModule {
        &self.modules[self.root]
    }

    /// The document's file.
    pub fn file(&self) -> FileId {
        self.module().file
    }

    /// The document's text as analyzed.
    pub fn text(&self) -> &str {
        &self.sm.get(self.file()).src
    }

    /// Source text of `span` (empty if out of range).
    pub fn snippet(&self, span: Span) -> &str {
        let src = &self.sm.get(span.file).src;
        src.get(span.lo as usize..span.hi as usize).unwrap_or("")
    }

    /// Whether module `module` belongs to the standard library (prelude included).
    pub fn is_std(&self, module: usize) -> bool {
        self.modules.get(module).is_some_and(|m| m.is_std)
    }

    /// Index of the module with canonical path `path`.
    pub fn module_by_path(&self, path: &str) -> Option<usize> {
        self.modules.iter().position(|m| m.path == path)
    }
}

/// Analyze the document at `path` whose current text is `overlay[path]`; `ts_folders` caches the
/// packages' `tsCompat` folders across analyses.
pub fn analyze(
    loader: &dyn ProgramLoader,
    path: &Path,
    overlay: &HashMap<PathBuf, String>,
    ts_folders: &mut crate::ts_compat::FolderCache,
) -> Analysis {
    let mut sm = SourceMap::new();
    let mut diagnostics = vec![];
    let loaded = catch_unwind(AssertUnwindSafe(|| {
        loader.load(path, overlay, &mut sm, &mut diagnostics)
    }));
    let loaded = match loaded {
        Ok(Ok(loaded)) => loaded,
        Ok(Err(msg)) => return standalone(path, overlay, msg),
        Err(_) => return standalone(path, overlay, "internal error while loading".into()),
    };
    let LoadedProgram { modules, root } = loaded;
    let mut analysis = Analysis {
        sm,
        modules,
        root,
        diagnostics,
        ide: None,
        ts_compat: vec![],
    };
    run_sema(&mut analysis);
    let linted = catch_unwind(AssertUnwindSafe(|| {
        crate::ts_compat::findings(&analysis, path, overlay, ts_folders)
    }));
    // A crash in the lint costs its findings, not the document's analysis.
    analysis.ts_compat = linted.unwrap_or_default();
    analysis
}

fn run_sema(analysis: &mut Analysis) {
    let loaded_cleanly = !analysis.diagnostics.iter().any(Diagnostic::is_error);
    let checked = catch_unwind(AssertUnwindSafe(|| {
        ide::check_for_ide(&analysis.modules, analysis.root)
    }));
    match checked {
        Ok(ide) => {
            if loaded_cleanly {
                analysis
                    .diagnostics
                    .extend(ide.diagnostics().iter().cloned());
            }
            analysis.ide = Some(ide);
        }
        Err(_) if !loaded_cleanly => {}
        Err(_) => {
            let span = Span::new(analysis.file(), 0, 0);
            let msg = "internal compiler error in semantic analysis (please report it)";
            analysis.diagnostics.push(Diagnostic::error(msg, span));
        }
    }
}

/// Fallback when the loader cannot even read the document: parse the buffer alone so that
/// symbols and formatting keep working, and report why the program could not be loaded.
fn standalone(path: &Path, overlay: &HashMap<PathBuf, String>, msg: String) -> Analysis {
    let mut sm = SourceMap::new();
    let src = overlay.get(path).cloned().unwrap_or_default();
    let file = sm.add(path, src);
    let (ast, mut diagnostics) = parse(file, &sm.get(file).src);
    diagnostics.push(Diagnostic::error(msg, Span::new(file, 0, 0)));
    let module = SourceModule {
        path: "main".into(),
        is_std: false,
        file,
        ast,
        imports: vec![],
        jsx_runtime: None,
    };
    Analysis {
        sm,
        modules: vec![module],
        root: 0,
        diagnostics,
        ide: None,
        ts_compat: vec![],
    }
}

fn parse(file: FileId, src: &str) -> (ast::Module, Diagnostics) {
    catch_unwind(|| velt_syntax::parse_file(file, src)).unwrap_or_else(|_| {
        let empty = ast::Module {
            items: vec![],
            span: Span::new(file, 0, 0),
            jsx_import_source: None,
        };
        let d = Diagnostic::error("internal error in the parser", Span::new(file, 0, 0));
        (empty, vec![d])
    })
}
