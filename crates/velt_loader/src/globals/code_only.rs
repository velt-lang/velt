//! Globals that load only for a name written in code (#2585): `lib.dom`'s groups and the event
//! classes, whose names (`Node`, `Text`, `Range`, `Window`, `Event`, `Comment`) are everyday
//! words in comments. A global module marked with [`CODE_ONLY_LINE`] loads for one of its names
//! only where the name appears outside the module's comments, so an HTTP server whose comments
//! mention a node or an event doesn't pay for the DOM's types.
//!
//! A name is in code when the source with its comments blanked out still holds it as a whole
//! word (strings and template literals count, as for every global), or when the parsed module
//! names it (an identifier or a written type): the comment scan doesn't read regular expression
//! literals, so a `//` inside one hides the rest of its line from the scan, never from the
//! module.

use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

use super::mentions;

/// The line that marks a global module as loading only for names in code.
pub(super) const CODE_ONLY_LINE: &str = "// load for: a name in code, not in a comment";

/// Whether `name`, a whole word of `src` (module `module`'s source), appears outside its
/// comments. `stripped` caches `src` with its comments blanked out, made on first use.
pub(crate) fn in_code(
    src: &str,
    module: &ast::Module,
    name: &str,
    stripped: &mut Option<String>,
) -> bool {
    let text = stripped.get_or_insert_with(|| without_comments(src));
    mentions(text, name) || names_in_module(module, name)
}

/// `src` with each comment replaced by spaces (newlines kept). Strings and template literals
/// are skipped whole (a template's `${…}` is read as text: a word there still counts).
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
        // A string counts, as it does for every global.
        assert!(in_code_of("const s = \"Text\";\n", "Text"));
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
