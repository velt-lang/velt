//! `velt check --ts-compat`: a lint for the common subset of TypeScript and Velt (issue #13,
//! docs/internals/design/tsx.md "Sharing components with the client").
//!
//! A file in the subset compiles with both `tsc` and `velt` and behaves the same under both. The
//! lint looks only at code Velt already accepts (the caller runs it after a successful
//! `velt check` of the file) and reports what `tsc` would reject or run differently. Every
//! [`Finding`] says what TypeScript does, why Velt differs and what to write instead, with a
//! [`Fix`] where the replacement is mechanical.
//!
//! This step has the rules that need only the syntax tree ([`rules`]); rules that need types
//! come with a type query on the checked program.

mod program;
mod rules;

use std::path::{Path, PathBuf};

use velt_common::Span;
use velt_syntax::ast;

pub use program::{canonical, lint_program};

/// The code of every rule. The `tsc` oracle (tests/oracle.rs) holds each one to its claim, so a
/// new rule is listed here and given a sample there.
pub const RULES: &[&str] = &[
    "velt-number-type",
    "bool-type",
    "number-suffix",
    "int-cast",
    "struct",
    "extend",
    "throws",
    "promise-error-type",
    "interface-body",
    "velt-import",
    "outside-import",
    "jsx-provider",
    "declare-fn",
];

/// How serious a [`Finding`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// `tsc` rejects the code, or it runs differently under JavaScript.
    Error,
    /// The code may behave differently, depending on values the lint can't see.
    Warning,
}

/// A mechanical replacement that brings the code into the subset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// The source range to replace.
    pub span: Span,
    /// The text to put there.
    pub replacement: String,
    /// What the fix does, for an editor's menu (`replace with `number``).
    pub title: String,
}

/// One construct outside the TypeScript/Velt common subset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The rule's code (`velt-number-type`), shown as `ts-compat(<code>)`.
    pub code: &'static str,
    /// Error or warning.
    pub severity: Severity,
    /// Where the construct is.
    pub span: Span,
    /// What is wrong, in one line.
    pub message: String,
    /// What TypeScript does, why Velt differs, and what to write instead.
    pub notes: Vec<String>,
    /// The replacement, when it is mechanical.
    pub fix: Option<Fix>,
}

/// One module to lint, as the loader produced it.
#[derive(Clone, Debug)]
pub struct LintModule<'a> {
    /// The module's file, as the files in scope name it (the caller canonicalizes both).
    pub path: &'a Path,
    /// The file's source text (the syntax tree's spans index it).
    pub src: &'a str,
    /// The parsed module.
    pub ast: &'a ast::Module,
    /// Each import's specifier with the file it resolved to.
    pub imports: Vec<(String, PathBuf)>,
    /// The module contains JSX and its provider is the standard library's `velt:jsx`, which has
    /// no TypeScript runtime.
    pub default_jsx_provider: bool,
}

/// Lint `modules`. `scope` is every file in scope: the linted ones, plus any the caller skipped
/// (one that failed `velt check`); a relative import of a file outside it leaves the subset.
/// Findings are ordered by file and position.
pub fn lint(modules: &[LintModule], scope: &[&Path]) -> Vec<Finding> {
    let mut findings: Vec<Finding> = modules
        .iter()
        .flat_map(|m| rules::lint_module(m, scope))
        .collect();
    findings.sort_by_key(|f| (f.span.file, f.span.lo, f.span.hi));
    findings
}
