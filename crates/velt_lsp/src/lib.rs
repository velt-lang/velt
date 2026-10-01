//! `velt lsp`: a Language Server Protocol server for Velt (synchronous, on `lsp-server`).

//!
//! Features: diagnostics (parse + imports + sema, debounced, per open document), formatting
//! (`velt_fmt`), document symbols, go to definition, hover, completion, find references, rename,
//! quick fixes (code actions), inlay hints, signature help, semantic tokens, document highlight and
//! workspace symbols.
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
mod documents;
mod highlight;
mod hover;
mod index;
mod inlay_hints;
mod line_index;
mod references;
mod sema_query;
mod semantic_tokens;
mod server;
mod signature;
mod signature_help;
mod symbols;
mod syntax_walk;
mod text_scan;
mod workspace_symbols;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostics, SourceMap};
use velt_sema::SourceModule;

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
