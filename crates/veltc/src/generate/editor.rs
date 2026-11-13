//! The source generators' diagnostics for the editor (`velt lsp`): its own check
//! (`velt_sema::ide::check_for_ide`) runs no generator, so what they report (`JSON.parse`'s
//! "cannot decode", a generator that fails or writes code that does not compile) comes from a
//! check of the same program with them, as `velt check` runs it, in the background
//! ([`velt_lsp::ProgramLoader::slow_diagnostics`]). The generator caches make that cheap once
//! warm: the output of a run is cached by its input, and the code a check used is offered to
//! the next one.

use std::path::PathBuf;
use std::sync::Arc;

use velt_common::{Diagnostics, SourceMap};
use velt_sema::{CheckOptions, SourceModule};

use crate::driver::Session;

/// A copy of a program the editor loaded, to check on another thread.
pub(crate) struct Program {
    /// The source map's files, in order (so the program's file ids are the same).
    files: Vec<(PathBuf, String)>,
    modules: Vec<SourceModule>,
    root: usize,
    packages: Option<Arc<vpm::PackageGraph>>,
}

impl Program {
    /// A copy of `modules` (loaded into `sm`, `modules[root]` the document), or `None` when no
    /// module of it declares a generated function (nothing can need a generator).
    pub(crate) fn copy(
        sm: &SourceMap,
        modules: &[SourceModule],
        root: usize,
        packages: Option<Arc<vpm::PackageGraph>>,
    ) -> Option<Program> {
        if !modules.iter().any(super::declares_generated) {
            return None;
        }
        let files = (sm.files())
            .map(|(_, f)| (f.path.clone(), f.src.clone()))
            .collect();
        Some(Program {
            files,
            modules: modules.iter().map(super::copy_module).collect(),
            root,
            packages,
        })
    }

    /// Check the program with its generators: the errors they gave. Their spans index the
    /// files of the source map the program was copied from, or files added after them (the
    /// generated modules).
    pub(crate) fn generator_diagnostics(self) -> Diagnostics {
        let mut sess = Session::new();
        for (path, src) in self.files {
            sess.sm.add(path, src);
        }
        // As `velt check <file>` checks it: a library module needs no `main`.
        let opts = CheckOptions {
            require_main: false,
            ..CheckOptions::default()
        };
        let packages = self.packages.as_deref();
        let _ = super::check(&mut sess, &self.modules, self.root, opts, packages, None);
        sess.generator_reports
    }
}
