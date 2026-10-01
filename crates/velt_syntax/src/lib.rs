//! Frontend: source text → [`ast::Module`].
//! `ast.rs` and the signature of `parse_file` are contracts (maintainer-owned).
//!
//! `lexer` turns bytes into a compact token stream; `parser` is a recursive-descent parser with
//! precedence climbing for operators, speculative parsing for arrows / generic calls, and
//! statement/item-level error recovery.

pub mod ast;
mod lexer;
mod parser;

use velt_common::{Diagnostics, FileId};

/// Stack for the parser thread. The recursive-descent parser needs a few KB per nesting level
/// (bounded by its depth limit); a dedicated thread keeps deeply nested input from overflowing the
/// caller's stack (1 MB main thread on Windows). The memory is reserved, not committed, until used.
const PARSER_STACK_BYTES: usize = 64 << 20;

/// CONTRACT: parse one source file. Always returns a (possibly partial) module; parse errors go
/// into the diagnostics. The parser must recover and keep going where reasonable.
pub fn parse_file(file: FileId, src: &str) -> (ast::Module, Diagnostics) {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-parse".into())
            .stack_size(PARSER_STACK_BYTES)
            .spawn_scoped(s, || parse_on_current_thread(file, src));
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => parse_on_current_thread(file, src),
        }
    })
}

fn parse_on_current_thread(file: FileId, src: &str) -> (ast::Module, Diagnostics) {
    let mut lexed = lexer::lex(file, src);
    let mut diags = std::mem::take(&mut lexed.diags);
    let mut parser = parser::Parser::new(file, src, lexed);
    let module = parser.parse_module();
    diags.append(&mut parser.diags);
    // Lexer and parser diagnostics interleave by position (stable sort keeps same-offset order).
    diags.sort_by_key(|d| d.labels.first().map_or(0, |l| l.span.lo));
    (module, diags)
}

/// Byte ranges of the comments in `src`, in source order, exactly as the lexer sees them: `//`
/// and `/*` inside strings, templates, regular expressions and JSX text are not comments. A line
/// comment's range stops before its line break. For tools that keep comments (`velt fmt`).
pub fn comment_ranges(src: &str) -> Vec<std::ops::Range<u32>> {
    lexer::lex(FileId(0), src).comments
}

/// Debug dump of a module for snapshot tests.
pub fn dump(module: &ast::Module) -> String {
    format!("{:#?}", module)
}
