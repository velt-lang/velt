//! `velt lsp`: a Language Server Protocol server for Velt (synchronous, on `lsp-server`).

//!
//! Features: diagnostics (parse + imports + sema, debounced, per open document), formatting
//! (`velt_fmt`), document symbols, go to definition, hover, completion (JSX tags and attributes
//! included), find references, rename, quick fixes (code actions), inlay hints, signature help,
//! semantic tokens, document highlight and workspace symbols. Imports get their own help
//! ([`imports`]): the exports of the module inside `import { … }`, module specifiers after
//! `from "`, and auto-import of exported names that are not imported yet. Documents in a package's
//! `tsCompat` folders also get the TypeScript-compatibility lint's findings and fixes
//! ([`ts_compat`]).
//! Program loading is injected through [`ProgramLoader`] (the CLI's loader lives in `veltc`, which
//! depends on this crate). Editor queries come from sema's IDE API ([`sema_query`] over
//! `velt_sema::ide`, which answers even when the program has errors); the AST-based [`index`]
//! is the fallback when sema has no answer (code the parser could not recover).
//!
//! Every request handler runs isolated: a panic becomes an error response, never a dead server.

mod analysis;
mod callable;
mod code_actions;
mod completion;
mod definition;
mod diagnostics;
mod disk_index;
mod documents;
mod highlight;
mod hover;
mod imports;
mod index;
mod inlay_hints;
mod jsx_completion;
mod line_index;
mod manifest;
mod references;
mod registry;
mod sema_query;
mod semantic_tokens;
mod server;
mod signature;
mod signature_help;
mod symbols;
mod text_scan;
mod ts_compat;
mod workspace_symbols;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostics, SourceMap};
use velt_sema::SourceModule;

pub use imports::index::{package_module_entries, std_module_entries};
pub use lsp_server::Connection;

/// Stack for the server's analysis thread: parsing and sema recurse along the AST (the same
/// reservation `velt build` uses; reserved, not committed, until used).
const SERVER_STACK_BYTES: usize = 256 << 20;

/// Result of loading a program for analysis.
pub struct LoadedProgram {
    /// All modules (prelude, the document, its imports).
    pub modules: Vec<SourceModule>,
    /// Index of the document's module in `modules`.
    pub root: usize,
}

/// Loads a document and everything it imports, like `velt build` would.
pub trait ProgramLoader: Send + Sync {
    /// Load `root` and its imports into `sm`. `overlay` holds the unsaved text of open documents
    /// (keyed by the paths the server derived from their URIs) and wins over the disk. Import and
    /// syntax problems go into `diags`; `Err` means the root itself could not be read.
    fn load(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String>;

    /// The modules a file at `from` can import by a non-relative specifier: the standard
    /// library's public modules (`velt:fs`) and the modules of the dependencies of the package
    /// containing `from`. Relative files are listed by the server itself. Used for specifier
    /// completion and auto-import; the default knows none.
    fn module_index(&self, from: &Path) -> Vec<ModuleEntry> {
        let _ = from;
        vec![]
    }

    /// The file that the specifier `spec`, imported by the file at `from`, names, exactly as
    /// [`ProgramLoader::load`] would resolve it; `None` when that import would fail (invalid, not
    /// found, ambiguous) or, for a loader that does not [resolve
    /// modules](ProgramLoader::resolves_modules), when it cannot tell. The import help reads
    /// modules outside the program through this, and checks the specifiers it writes. The
    /// default resolves nothing.
    fn resolve_module(&self, spec: &str, from: &Path) -> Option<PathBuf> {
        let _ = (spec, from);
        None
    }

    /// Whether [`ProgramLoader::resolve_module`] answers for every specifier, so that `None`
    /// means the import would fail (the import help then leaves out what would not load).
    fn resolves_modules(&self) -> bool {
        false
    }
}

/// A module an import specifier can name ([`ProgramLoader::module_index`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleEntry {
    /// The specifier as written in `from "…"`: `velt:fs`, `velt:collections/set`, `json`,
    /// `json/parse`.
    pub spec: String,
    /// The module's file.
    pub path: PathBuf,
    /// Where the module comes from.
    pub kind: ModuleKind,
    /// One line describing the module (a std module's header comment), or empty.
    pub doc: String,
}

/// Where a [`ModuleEntry`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    /// The standard library (`velt:x`).
    Std,
    /// A dependency of the importing file's package (`name`, `name/sub`).
    Dependency,
}

/// Serve LSP over stdin/stdout until the client says `exit`.
pub fn serve_stdio(loader: &dyn ProgramLoader) -> Result<(), String> {
    let (connection, io_threads) = Connection::stdio();
    serve(connection, loader)?;
    io_threads
        .join()
        .map_err(|e| format!("language server I/O failed: {e}"))
}

/// Serve LSP on `connection` (initialize handshake, then the main loop) until `exit` or until the
/// client disconnects. Runs on a dedicated large-stack thread.
pub fn serve(connection: Connection, loader: &dyn ProgramLoader) -> Result<(), String> {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-lsp".into())
            .stack_size(SERVER_STACK_BYTES)
            .spawn_scoped(s, || server::run(&connection, loader));
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|_| Err("the language server crashed".into())),
            Err(e) => Err(format!("cannot start the language server thread: {e}")),
        }
    })
}
