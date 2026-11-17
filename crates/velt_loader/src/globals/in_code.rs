//! Globals load only for a name written in code (#2585, #3097): a name in a comment or in a
//! string's text is a word, not a use. Comments and strings are full of everyday words that
//! name globals (`Node`, `Text`, `Event`, `Storage`, `navigator`), and a global loaded for one
//! costs more than its compile time when its module holds module state: a comment saying
//! "Storage" loaded `sessionStorage`, whose module-level `let` gave the program module state, and
//! a server's connections stayed on one task (spreadsConnections, std/net/listen.velt).
//!
//! A name is in code when it appears as a whole word outside the module's comments and outside
//! the text of its string and template literals, or when the parsed module names it (an
//! identifier or a written type). Each part errs towards code, so a use is never missed:
//! - Comments come from a scan that skips strings and templates whole; a quote or a backtick it
//!   takes for a string's (in a regular expression literal, in JSX text) only leaves more text
//!   counted as code. A `//` or `/*` inside a regular expression literal hides the rest of its
//!   line (or up to a `*/`) from the scan, never from the module's identifiers.
//! - The strings' text is where the parser put string literals and the text parts of templates
//!   (a template's `${…}` is code), from the expressions the module's walk visits: a string the
//!   walk doesn't reach (an import path, a literal type) stays counted as code.

use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

use super::ident;

/// A module's source as [`in_code`] reads it: comments blanked out, and the byte ranges of its
/// strings' text.
pub(crate) struct Code {
    text: String,
    /// The text of string literals and of templates outside their `${…}`: sorted, disjoint.
    strings: Vec<(usize, usize)>,
}

/// Whether `name`, a whole word of `src` (module `module`'s source), appears in its code.
/// `code` caches what that reads of the module, made on first use.
pub(crate) fn in_code(
    src: &str,
    module: &ast::Module,
    name: &str,
    code: &mut Option<Code>,
) -> bool {
    let code = code.get_or_insert_with(|| Code {
        text: without_comments(src),
        strings: string_text(module),
    });
    code.has(name) || names_in_module(module, name)
}

impl Code {
    /// Whether `name` is a whole word of the code outside the strings' text.
    fn has(&self, name: &str) -> bool {
        self.text.match_indices(name).any(|(at, _)| {
            let before = self.text[..at].chars().next_back();
            let after = self.text[at + name.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident) && !self.in_string(at)
        })
    }

    /// Whether byte `at` is in a string's text.
    fn in_string(&self, at: usize) -> bool {
        let i = self.strings.partition_point(|&(lo, _)| lo <= at);
        i > 0 && at < self.strings[i - 1].1
    }
}

/// The byte ranges of the text of `module`'s string literals and templates (a template's text
/// parts, between its `${…}`), sorted.
fn string_text(module: &ast::Module) -> Vec<(usize, usize)> {
    struct Find(Vec<(usize, usize)>);
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            let (lo, hi) = (e.span.lo as usize, e.span.hi as usize);
            match &e.kind {
                ast::ExprKind::Lit(ast::Lit::Str(_)) => self.0.push((lo, hi)),
                ast::ExprKind::Template { exprs, .. } => {
                    let mut from = lo;
                    for x in exprs {
                        self.0.push((from, (x.span.lo as usize).max(from)));
                        from = x.span.hi as usize;
                    }
                    self.0.push((from, hi.max(from)));
                }
                _ => {}
            }
        }
    }
    let mut find = Find(vec![]);
    visit::walk_module(module, &mut find);
    let mut v = find.0;
    v.retain(|&(lo, hi)| lo < hi);
    v.sort_unstable();
    // Disjoint: a template's text parts and the strings inside its `${…}` don't overlap, and
    // a range inside another (a span the parser shares) adds nothing.
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(v.len());
    for (lo, hi) in v {
        match out.last_mut() {
            Some(last) if lo < last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// `src` with each comment replaced by spaces (newlines kept). Strings and template literals
/// are skipped whole (a template's `${…}` is read as text here; its code is told apart by
/// [`string_text`]).
fn without_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'' | b'`') => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != q && (q == b'`' || b[i] != b'\n') {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                out.extend_from_slice(&b[start..i]);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                out.extend(b[i..end].iter().map(|&c| if c == b'\n' { c } else { b' ' }));
                i = end;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Only ASCII bytes were replaced, each by an ASCII byte, so the text stays UTF-8.
    String::from_utf8(out).unwrap_or_default()
}

/// Whether `module` names `name` as an identifier or as the first part of a written type.
fn names_in_module(module: &ast::Module, name: &str) -> bool {
    struct Find<'n> {
        name: &'n str,
        found: bool,
    }
    impl<'a> Visit<'a> for Find<'_> {
        fn expr(&mut self, e: &'a ast::Expr) {
            if let ast::ExprKind::Ident(id) = &e.kind {
                self.found |= id.name == self.name;
            }
        }
        fn ty(&mut self, t: &'a ast::TypeExpr) {
            if let ast::TypeExprKind::Named { path, .. } = &t.kind {
                self.found |= path.first().is_some_and(|p| p.name == self.name);
            }
        }
    }
    let mut find = Find { name, found: false };
    visit::walk_module(module, &mut find);
    find.found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use velt_common::SourceMap;

    fn in_code_of(src: &str, name: &str) -> bool {
        let mut sm = SourceMap::new();
        let file = sm.add(Path::new("m.ts"), src.to_string());
        let (module, _) = velt_syntax::parse_file(file, &sm.get(file).src);
        in_code(src, &module, name, &mut None)
    }

    #[test]
    fn a_name_in_a_comment_only_is_not_in_code() {
        assert!(!in_code_of(
            "// every Node of the tree\nconst a = 1;\n",
            "Node"
        ));
        assert!(!in_code_of(
            "/* a Text, a Range\n and a Window */ let x = 2;\n",
            "Window"
        ));
        assert!(!in_code_of(
            "/** @param e the Event */\nfunction f(e: number) {}\n",
            "Event"
        ));
    }

    #[test]
    fn a_name_in_code_is_in_code() {
        assert!(in_code_of(
            "const t = new EventTarget(); // a target\n",
            "EventTarget"
        ));
        assert!(in_code_of("function f(n: Node) {}\n", "Node"));
        assert!(in_code_of(
            "let s = \"// not a comment\"; const e = new Event(s);\n",
            "Event"
        ));
        assert!(in_code_of(
            "let s = `a ${1} b`; let w: Window | null = null;\n",
            "Window"
        ));
        // A template's `${…}` is code.
        assert!(in_code_of(
            "const s = `n: ${navigator.hardwareConcurrency}`;\n",
            "navigator"
        ));
    }

    #[test]
    fn a_name_in_a_string_s_text_is_not_in_code() {
        assert!(!in_code_of("const s = \"Text\";\n", "Text"));
        assert!(!in_code_of("const s = 'the Storage is full';\n", "Storage"));
        assert!(!in_code_of(
            "const s = `a navigator ${1 + 2} and \\` Event`;\n",
            "navigator"
        ));
        assert!(!in_code_of(
            "const s = `a navigator ${1 + 2} and \\` Event`;\n",
            "Event"
        ));
        // A quote inside a comment doesn't open a string.
        assert!(in_code_of(
            "// it's\nconst w: Window | null = null;\n",
            "Window"
        ));
        // A brace in a string inside `${…}` doesn't make a comment after it code.
        assert!(!in_code_of(
            "log(`${\"{\"}`); // the sessionStorage\n",
            "sessionStorage"
        ));
        // A `}` in a template inside `${…}`: the outer template's text is still text.
        assert!(!in_code_of(
            "const s = `${a ? `}` : \"\"} navigator`;\n",
            "navigator"
        ));
        assert!(in_code_of(
            "const s = `${a ? `}` : navigator}`;\n",
            "navigator"
        ));
    }

    #[test]
    fn a_quote_or_backtick_the_comment_scan_misreads_leaves_code_code() {
        // A backtick in a regex opens no string for the walk: code after it stays code, even a
        // use the module's identifiers don't show (a shorthand property).
        assert!(in_code_of(
            "const t = /`/;\nconst deps = { fetch };\n",
            "fetch"
        ));
        assert!(in_code_of("const q = /'/; const d = { fetch };\n", "fetch"));
        assert!(in_code_of("type F = typeof fetch; // it's\n", "fetch"));
    }

    #[test]
    fn a_regex_with_slashes_does_not_hide_code() {
        // The scan reads `//` of the regex as a comment; the module still names `Event`.
        assert!(in_code_of(
            "const r = /a\\//; const e = new Event(\"x\");\n",
            "Event"
        ));
    }
}
