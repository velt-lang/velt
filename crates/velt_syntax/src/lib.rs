//! Frontend: source text → [`ast::Module`].
//! `ast.rs` and the signature of `parse_file` are contracts (maintainer-owned).
//!
//! `lexer` turns bytes into a compact token stream; `parser` is a recursive-descent parser with
//! precedence climbing for operators, speculative parsing for arrows / generic calls, and
//! statement/item-level error recovery.

pub mod ast;
mod ast_names;
mod lexer;
#[cfg(test)]
mod linear_tests;
mod parser;
pub mod scope;
pub mod visit;
mod work;

use std::collections::HashMap;

use velt_common::{Diagnostics, FileId};

/// Stack for the parser thread. The recursive-descent parser needs a few KB per nesting level
/// (bounded by its depth limit); a dedicated thread keeps deeply nested input from overflowing the
/// caller's stack (1 MB main thread on Windows). The memory is reserved, not committed, until used.
const PARSER_STACK_BYTES: usize = 64 << 20;

/// CONTRACT: parse one source file. Always returns a (possibly partial) module; parse errors go
/// into the diagnostics. The parser must recover and keep going where reasonable.
pub fn parse_file(file: FileId, src: &str) -> (ast::Module, Diagnostics) {
    let (module, diags, _) = on_parser_thread(|| parse_with_comments(file, src));
    (module, diags)
}

/// [`parse_file`] for a plain TypeScript file (`.ts`): there `<T>x` is TypeScript's type
/// assertion, which is reported as unsupported (with a fix) instead of being parsed as JSX.
pub fn parse_ts_file(file: FileId, src: &str) -> (ast::Module, Diagnostics) {
    let (module, diags, _) = on_parser_thread(|| parse_source(file, src, true));
    (module, diags)
}

/// What [`parse_with`] reads besides the source.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParseOptions<'a> {
    /// A plain `.ts` file (as [`parse_ts_file`]).
    pub plain_ts: bool,
    /// Build-time constants (`define` in package.vlt, as Vite's `define`): each identifier
    /// expression of one of these names that nothing in its scope declares parses as the
    /// literal ([`scope::free_values`]).
    pub defines: Option<&'a HashMap<String, ast::Lit>>,
}

/// An ambient variable declaration: `declare const NAME: T;`, or `var NAME: T;` in
/// `declare global { … }`.
#[derive(Clone, Debug)]
pub struct Ambient {
    pub name: ast::Ident,
    /// The declared type (`None`: none written).
    pub ty: Option<ast::TypeExpr>,
    pub span: velt_common::Span,
    /// Declared in `declare global { … }`.
    pub global: bool,
}

/// [`parse_with`]'s result.
pub struct Parsed {
    pub module: ast::Module,
    pub diags: Diagnostics,
    /// The ambient variable declarations, in source order; they are not items of `module`.
    pub ambient: Vec<Ambient>,
    /// The source has `import` or `export` at its top level: it is a module, whose ambient
    /// declarations outside `declare global` are its own, not global (as in TypeScript).
    pub is_module: bool,
}

/// Parse a module as the compiler loads it: with `opts`, and with ambient declarations
/// (`declare const`, `declare global`) returned in [`Parsed::ambient`] instead of being
/// errors.
pub fn parse_with(file: FileId, src: &str, opts: &ParseOptions) -> Parsed {
    let parse = |define_at: &HashMap<u32, ast::Lit>| {
        on_parser_thread(|| {
            let mut parser = parser::Parser::new(file, src);
            parser.plain_ts = opts.plain_ts;
            parser.define_at = Some(define_at);
            parser.ambient = Some(vec![]);
            let module = parser.parse_module();
            let ambient = parser.ambient.take().unwrap_or_default();
            let is_module = parser.saw_module_syntax;
            let (mut diags, _) = parser.finish();
            diags.sort_by_key(|d| d.labels.first().map_or(0, |l| l.span.lo));
            Parsed {
                module,
                diags,
                ambient,
                is_module,
            }
        })
    };
    let none = HashMap::new();
    let parsed = parse(&none);
    let Some(defines) = opts.defines.filter(|d| !d.is_empty()) else {
        return parsed;
    };
    // The references to replace are known once the module's scopes are: parse again,
    // replacing them (rare: only modules that name a constant pay for it).
    let define_at: HashMap<u32, ast::Lit> =
        scope::free_values(&parsed.module, &|n| defines.contains_key(n))
            .into_iter()
            .map(|id| (id.span.lo, defines[id.name.as_str()].clone()))
            .collect();
    if define_at.is_empty() {
        return parsed;
    }
    parse(&define_at)
}

thread_local! {
    /// Set on a thread [`on_parser_thread`] spawned: parsing there needs no thread of its own.
    static ON_PARSER_STACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` on a thread with the parser's stack, where the parse functions run directly instead
/// of each spawning a thread of its own: for callers that parse many sources in a row (the
/// parser's robustness sweep parses millions).
pub fn with_parser_stack<T: Send>(f: impl Fn() -> T + Sync) -> T {
    on_parser_thread(f)
}

/// Runs `f` on a thread with the parser's stack (on this thread if it is one, or if none can be
/// spawned).
fn on_parser_thread<T: Send>(f: impl Fn() -> T + Sync) -> T {
    if ON_PARSER_STACK.get() {
        return f();
    }
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-parse".into())
            .stack_size(PARSER_STACK_BYTES)
            .spawn_scoped(s, || {
                ON_PARSER_STACK.set(true);
                f()
            });
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => f(),
        }
    })
}

/// Parses `src`, returning the module, the diagnostics and the comment ranges.
fn parse_with_comments(
    file: FileId,
    src: &str,
) -> (ast::Module, Diagnostics, Vec<std::ops::Range<u32>>) {
    parse_source(file, src, false)
}

/// [`parse_with_comments`], with `<T>x` read as a type assertion when `plain_ts`.
fn parse_source(
    file: FileId,
    src: &str,
    plain_ts: bool,
) -> (ast::Module, Diagnostics, Vec<std::ops::Range<u32>>) {
    let mut parser = parser::Parser::new(file, src);
    parser.plain_ts = plain_ts;
    let module = parser.parse_module();
    let (mut diags, comments) = parser.finish();
    // Lexer and parser diagnostics interleave by position (stable sort keeps same-offset order).
    diags.sort_by_key(|d| d.labels.first().map_or(0, |l| l.span.lo));
    (module, diags, comments)
}

/// Byte ranges of the comments in `src`, in source order, exactly as the lexer sees them: `//`
/// and `/*` inside strings, templates, regular expressions and JSX text are not comments. A line
/// comment's range stops before its line break. For tools that keep comments (`velt fmt`).
/// Where JSX starts is decided by the parser, so this parses the file.
pub fn comment_ranges(src: &str) -> Vec<std::ops::Range<u32>> {
    on_parser_thread(|| parse_with_comments(FileId(0), src).2)
}

/// Byte ranges of the tokens of `src`, in source order, lexing it as code throughout: where JSX
/// starts is decided by the parser, so inside JSX they are approximate. For tests that cut a file
/// where a token starts or ends (the parser's robustness sweep).
#[doc(hidden)]
pub fn token_ranges(src: &str) -> Vec<std::ops::Range<u32>> {
    let mut lexer = lexer::Lexer::new(FileId(0), src);
    lexer.fill(usize::MAX);
    lexer.toks.iter().map(|t| t.lo..t.hi).collect()
}

/// The first `this: T` parameter of `src` (`function f(this: T, x: A)`, `(this: void) => R`),
/// if any. The AST does not keep them, so tools that print the AST back (`velt fmt`) must not
/// rewrite such a file.
pub fn first_this_param(src: &str) -> Option<std::ops::Range<u32>> {
    on_parser_thread(|| {
        let mut parser = parser::Parser::new(FileId(0), src);
        let _ = parser.parse_module();
        parser.first_this_param.clone()
    })
}

/// Debug dump of a module for snapshot tests.
pub fn dump(module: &ast::Module) -> String {
    format!("{:#?}", module)
}
