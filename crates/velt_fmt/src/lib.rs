//! `velt fmt`: an opinionated formatter for Velt source (prettier/rustfmt style).

//!
//! The source is parsed with `velt_syntax`; files with parse errors are refused. The AST is
//! turned into a Wadler/prettier-style document ([`doc`]) by [`print`], which is laid out within
//! [`MAX_WIDTH`] columns. Comments are not in the AST: [`comments`] re-scans them from the source
//! and the printer interleaves them by byte position. Spellings the AST drops (number formats,
//! `===`, function-type parameter names) are recovered from the source by [`source`].
//!
//! Style: 2-space indent, width 100, double quotes (single quotes kept when the text contains
//! `"`), semicolons, trailing commas in broken lists, spaces around binary operators and inside
//! object braces, at most one blank line between statements, a blank line between top-level
//! items. Template literals are printed verbatim. Formatting is idempotent and preserves the AST.

mod comments;
mod doc;
mod print;
mod source;

use velt_common::{Diagnostics, FileId};

pub use comments::count as count_comments;

/// Target line width.
pub const MAX_WIDTH: usize = 100;

/// Formats one source file. Returns the parse diagnostics instead if the file does not parse
/// (diagnostic spans refer to `FileId(0)`).
pub fn format_source(src: &str) -> Result<String, Diagnostics> {
    let (module, diags) = velt_syntax::parse_file(FileId(0), src);
    if diags.iter().any(|d| d.is_error()) {
        return Err(diags);
    }
    // The module moves to the printing thread so that its (recursive) drop happens there too.
    let out = on_big_stack(move || {
        let mut printer = print::Printer::new(src);
        let doc = printer.module(&module);
        doc::render(&doc, MAX_WIDTH)
    });
    Ok(if uses_crlf(src) { to_crlf(&out) } else { out })
}

/// Stack for printing. Printing recurses along the AST, which can be deep (the parser bounds
/// most nesting, but not left-nested operator or postfix chains); like the parser, run on a
/// thread whose stack is reserved, not committed, until used.
const PRINTER_STACK_BYTES: usize = 256 << 20;

fn on_big_stack(f: impl FnOnce() -> String + Send) -> String {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-fmt".into())
            .stack_size(PRINTER_STACK_BYTES)
            .spawn_scoped(s, f);
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(e) => panic!("ICE: cannot start the formatter thread: {e}"),
        }
    })
}

/// Does the file use Windows line endings (judged by its first line)?
fn uses_crlf(src: &str) -> bool {
    src.find('\n').is_some_and(|nl| src[..nl].ends_with('\r'))
}

/// `\n` → `\r\n`, leaving existing `\r\n` (verbatim template text) alone.
fn to_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 16);
    let mut prev = '\0';
    for c in s.chars() {
        if c == '\n' && prev != '\r' {
            out.push('\r');
        }
        out.push(c);
        prev = c;
    }
    out
}
