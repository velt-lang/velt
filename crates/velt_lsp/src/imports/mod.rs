//! Help with imports, as TypeScript editors give it:
//! - inside the braces of `import { … } from "<spec>"`, the module's exports minus the names
//!   already listed (`import type { … }`: types only), read from the program when the module is
//!   loaded and by parsing it otherwise ([`exports`]); the cursor is located in the text
//!   ([`context`]), so this works while the statement doesn't parse;
//! - inside a module specifier, the modules it can name ([`specifiers`]);
//! - auto-import: completing a name that a std module, a dependency or another file of the
//!   package exports, with the edit that imports it ([`edit`]), and the same as a quick fix on
//!   the "cannot find `x`" error.
//!
//! The std modules and dependencies come from the loader ([`crate::ProgramLoader::module_index`],
//! listed by [`index`]); the package's own files from the workspace index
//! ([`crate::disk_index`]).

mod auto;
mod context;
mod edit;
pub mod exports;
pub mod index;
mod specifiers;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::CompletionItem;

use crate::analysis::Analysis;
use crate::ProgramLoader;
use context::ImportContext;
use exports::{Export, ExportCache};

pub use auto::has_unknown_names;

/// At most this many auto-import completions are offered.
const MAX_AUTO_IMPORTS: usize = 50;
/// A quick fix offers at most this many modules to import a name from.
const MAX_IMPORT_FIXES: usize = 5;

/// The exports of one file of the package, from the workspace index.
pub type FileExports<'a> = (&'a Path, &'a [Export]);

/// The exports of the std modules, by specifier.
type StdExports = Arc<Vec<(String, Vec<Export>)>>;

/// A module the document could import a name from.
struct Candidate {
    /// The specifier the document imports it by.
    spec: String,
    export: Export,
}

/// The import help's caches (one per server).
#[derive(Default)]
pub struct ImportHelp {
    exports: ExportCache,
    /// The exports of the std modules (by specifier), built once per std root.
    std: Option<(PathBuf, StdExports)>,
}

impl ImportHelp {
    /// Completion inside an import of the document at `doc` (byte `offset`, the word at the
    /// cursor starting at `word_start`); `None` when the cursor is not in one.
    pub fn complete(
        &mut self,
        analysis: &Analysis,
        loader: &dyn ProgramLoader,
        doc: &Path,
        offset: usize,
        word_start: usize,
    ) -> Option<Vec<CompletionItem>> {
        let text = analysis.text();
        match context::at(text, offset, word_start)? {
            ImportContext::Specifier { typed, lo, hi } => {
                let entries = loader.module_index(doc);
                Some(specifiers::items(&entries, doc, text, &typed, (lo, hi)))
            }
            ImportContext::Names {
                spec,
                listed,
                types_only,
            } => {
                let exports = self.module_exports(analysis, loader, doc, &spec);
                let mut seen: HashSet<String> = listed.into_iter().collect();
                Some(
                    exports
                        .into_iter()
                        .filter(|e| (!types_only || e.is_type) && seen.insert(e.name.clone()))
                        .map(|e| crate::sema_query::item(&e.name, e.kind, &e.detail))
                        .collect(),
                )
            }
        }
    }

    /// The exports of the module `spec` names: from the program when it is loaded, else by
    /// parsing its file.
    fn module_exports(
        &mut self,
        analysis: &Analysis,
        loader: &dyn ProgramLoader,
        doc: &Path,
        spec: &str,
    ) -> Vec<Export> {
        if let Some(target) = crate::index::import_target(analysis, analysis.root, spec) {
            return crate::index::module_items(analysis, target, true)
                .iter()
                .map(|d| exports::of_decl(analysis, d))
                .collect();
        }
        let entries = loader.module_index(doc);
        let std_root = auto::std_root(&entries);
        let path = entries
            .iter()
            .find(|e| e.spec == spec)
            .map(|e| e.path.clone())
            .or_else(|| exports::resolve(spec, doc, std_root.as_deref()));
        path.map_or_else(Vec::new, |p| self.exports.exports(&p, std_root.as_deref()))
    }
}
