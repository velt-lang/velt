//! Ambient declarations, read when the caller asks for them ([`crate::parse_with`]):
//! `declare const NAME: T;` (also `let` and `var`, several names per declaration, with or
//! without `export`) and `declare global { var NAME: T; … }`. They declare values the program
//! gets from elsewhere, such as the build-time constants (`__DEV__`) of a package's `env.d.ts`;
//! they are returned beside the module ([`crate::Ambient`]), not as items. Other declarations
//! inside `declare global` are reported. Without the request, `declare const` is the error it
//! always was (the formatter prints the items it parsed, so it must not drop them).

use super::{Fail, PResult, Parser};
use crate::lexer::{Kw, Tok};
use crate::Ambient;

impl Parser<'_> {
    /// At `declare const|let|var` (after an optional `export`) or `declare global {`.
    pub(super) fn at_ambient(&mut self) -> bool {
        if self.ambient.is_none() {
            return false;
        }
        let k = usize::from(self.at_kw(Kw::Export));
        if self.nth(k) != Tok::Kw(Kw::Declare) {
            return false;
        }
        self.is_var_word(k + 1)
            || (k == 0 && self.is_word(1, "global") && self.nth(2) == Tok::LBrace)
    }

    /// At a top-level `import` or `export` (not `import(…)` / `import.meta`): the file is a
    /// module ([`crate::Parsed::is_module`]).
    pub(super) fn at_module_syntax(&mut self) -> bool {
        self.at_kw(Kw::Export)
            || (self.at_kw(Kw::Import) && !matches!(self.nth(1), Tok::LParen | Tok::Dot))
    }

    fn is_word(&mut self, n: usize, word: &str) -> bool {
        let t = self.tok(self.pos + n);
        t.kind == Tok::Ident && self.text(t.lo, t.hi) == word
    }

    fn is_var_word(&mut self, n: usize) -> bool {
        matches!(self.nth(n), Tok::Kw(Kw::Const | Kw::Let)) || self.is_word(n, "var")
    }

    /// One ambient declaration (see [`Parser::at_ambient`]).
    pub(super) fn parse_ambient(&mut self) -> PResult<()> {
        self.eat_kw(Kw::Export);
        self.bump(); // declare
        if !self.at_word("global") {
            return self.parse_ambient_vars(false);
        }
        self.bump(); // global
        self.expect(Tok::LBrace)?;
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.bump();
                    return Ok(());
                }
                Tok::Semi => self.bump(),
                Tok::Eof => return self.expect(Tok::RBrace),
                _ => {
                    let declare = usize::from(self.at_kw(Kw::Declare));
                    if self.is_var_word(declare) {
                        if declare == 1 {
                            self.bump();
                        }
                        self.parse_ambient_vars(true)?;
                    } else {
                        let span = self.cur_span();
                        self.error(
                            "only variables (`var NAME: T;`) can be declared in `declare global` yet",
                            span,
                        );
                        self.skip_to_close();
                        return Err(Fail);
                    }
                }
            }
        }
    }

    /// `const|let|var A: T, B: U;`, at the keyword.
    fn parse_ambient_vars(&mut self, global: bool) -> PResult<()> {
        self.bump(); // const / let / var
        loop {
            let lo = self.cur_lo();
            let name = self.parse_ident()?;
            let ty = if self.eat(Tok::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            if self.at(Tok::Eq) {
                let span = self.cur_span();
                self.error("initializers are not allowed in ambient contexts", span);
                return Err(Fail);
            }
            let span = self.span_from(lo);
            if let Some(list) = &mut self.ambient {
                list.push(Ambient {
                    name,
                    ty,
                    span,
                    global,
                });
            }
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect_semi()
    }

    /// Skips to just past the `}` that closes the block the cursor is in.
    fn skip_to_close(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace if depth == 0 => {
                    self.bump();
                    return;
                }
                Tok::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::ast::{ExprKind, ItemKind, Lit, StmtKind};
    use crate::{parse_file, parse_with, ParseOptions};
    use velt_common::FileId;

    const SRC: &str = "declare const __DEV__: boolean;\nexport declare let a: string, b: number;\ndeclare global {\n  var HOOK: number | null;\n  declare const C: string;\n}\nfunction f(): void {\n  if (__DEV__) {}\n  const g = (__DEV__: boolean) => __DEV__;\n  o.__DEV__;\n}\n";

    #[test]
    fn ambient_declarations_are_returned_not_items() {
        let parsed = parse_with(FileId(0), SRC, &ParseOptions::default());
        assert!(parsed.diags.is_empty(), "{:?}", parsed.diags);
        let names: Vec<&str> = parsed
            .ambient
            .iter()
            .map(|a| a.name.name.as_str())
            .collect();
        assert_eq!(names, ["__DEV__", "a", "b", "HOOK", "C"]);
        assert!(parsed.ambient.iter().all(|a| a.ty.is_some()));
        assert_eq!(parsed.module.items.len(), 1);
        // The formatter's parse keeps reporting them.
        let (_, diags) = parse_file(FileId(0), SRC);
        assert!(!diags.is_empty());
    }

    #[test]
    fn defines_replace_value_uses_only() {
        let defines = HashMap::from([("__DEV__".to_string(), Lit::Bool(false))]);
        let opts = ParseOptions {
            plain_ts: true,
            defines: Some(&defines),
        };
        let parsed = parse_with(FileId(0), SRC, &opts);
        assert!(parsed.diags.is_empty(), "{:?}", parsed.diags);
        let ItemKind::Function(f) = &parsed.module.items[0].kind else {
            panic!("expected f");
        };
        let body = &f.body.stmts;
        let StmtKind::If { cond, .. } = &body[0].kind else {
            panic!("expected if");
        };
        assert!(matches!(cond.kind, ExprKind::Lit(Lit::Bool(false))));
        let dump = crate::dump(&parsed.module);
        // The arrow's parameter and the property keep the name.
        assert!(dump.matches("\"__DEV__\"").count() >= 2, "{dump}");
    }

    #[test]
    fn other_declarations_in_declare_global_are_reported() {
        let src = "declare global {\n  namespace JSX { interface X {} }\n}\nexport function f(): void {}\n";
        let parsed = parse_with(FileId(0), src, &ParseOptions::default());
        let messages: Vec<&str> = parsed.diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            ["only variables (`var NAME: T;`) can be declared in `declare global` yet"]
        );
        assert_eq!(parsed.module.items.len(), 1);
    }
}
