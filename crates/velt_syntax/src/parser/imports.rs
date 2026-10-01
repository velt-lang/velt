//! Module syntax: `import { a, b as c } from "…"`, `import type { T }`, `import * as ns`,
//! `import "…"`, and the export forms that are not declarations — re-exports
//! (`export { a } from "…"`, `export * from "…"`) and local export lists (`export { a, b }`).
//! `export default` and default imports are rejected with a hint (named exports only).

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::{Diagnostic, Span};

impl<'a> Parser<'a> {
    /// `import { a, b as c } from "path";`, `import type { T } from "path";`,
    /// `import * as ns from "path";` or `import "path";`
    pub(super) fn parse_import(&mut self) -> PResult<Import> {
        self.bump(); // import
        let type_only = self.at_kw(Kw::Type) && self.nth(1) == Tok::LBrace;
        if type_only {
            self.bump();
        }
        let mut names = vec![];
        let mut namespace = None;
        if self.at(Tok::LBrace) {
            names = self.parse_import_names(type_only)?;
            self.expect_kw(Kw::From, "from")?;
        } else if self.eat(Tok::Star) {
            self.expect_kw(Kw::As, "as")?;
            namespace = Some(self.parse_ident()?);
            self.expect_kw(Kw::From, "from")?;
        } else if self.at_ident_like() {
            return Err(self.default_import());
        }
        let (from, from_span) = self.parse_module_spec()?;
        self.expect_semi()?;
        Ok(Import {
            names,
            from,
            from_span,
            namespace,
            all: false,
        })
    }

    /// Is the parser (right after `export`) at a re-export or export list: `{`, `*`, `type {`?
    pub(super) fn at_export_list(&mut self) -> bool {
        match self.peek() {
            Tok::LBrace | Tok::Star => true,
            Tok::Kw(Kw::Type) => self.nth(1) == Tok::LBrace,
            _ => false,
        }
    }

    /// After `export`: `{ a, b as c } [from "path"];`, `type { T } [from "path"];` or
    /// `* from "path";`.
    pub(super) fn parse_export_list(&mut self) -> PResult<Import> {
        let type_only = self.eat_kw(Kw::Type);
        if self.eat(Tok::Star) {
            if self.at_kw(Kw::As) {
                let span = self.cur_span();
                self.error(
                    "`export * as ns from` is not supported: import the namespace with `import * as ns` where it is used",
                    span,
                );
                return Err(Fail);
            }
            self.expect_kw(Kw::From, "from")?;
            let (from, from_span) = self.parse_module_spec()?;
            self.expect_semi()?;
            return Ok(Import {
                names: vec![],
                from,
                from_span,
                namespace: None,
                all: true,
            });
        }
        let names = self.parse_import_names(type_only)?;
        let (from, from_span) = if self.eat_kw(Kw::From) {
            self.parse_module_spec()?
        } else {
            (
                String::new(),
                Span::new(self.file, self.prev_hi, self.prev_hi),
            )
        };
        self.expect_semi()?;
        Ok(Import {
            names,
            from,
            from_span,
            namespace: None,
            all: false,
        })
    }

    /// `{ a, type B, c as d }`; `type_only` marks every name (`import type { … }`).
    fn parse_import_names(&mut self, type_only: bool) -> PResult<Vec<ImportName>> {
        self.expect(Tok::LBrace)?;
        let mut names = Vec::new();
        while !self.at(Tok::RBrace) {
            // `type T` (a modifier) vs. a name called `type` (`{ type }`, `{ type as t }`).
            let modifier = self.at_kw(Kw::Type) && Self::is_ident_like(self.nth(1));
            if modifier {
                self.bump();
            }
            let name = self.parse_ident()?;
            let alias = if self.eat_kw(Kw::As) {
                Some(self.parse_ident()?)
            } else {
                None
            };
            names.push(ImportName {
                name,
                alias,
                type_only: type_only || modifier,
            });
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(names)
    }

    /// The module specifier string and its span.
    fn parse_module_spec(&mut self) -> PResult<(String, Span)> {
        let Tok::Str(idx) = self.peek() else {
            self.error_expected("module path string");
            return Err(Fail);
        };
        let from = self.payload_text(idx);
        let span = self.cur_span();
        self.bump();
        Ok((from, span))
    }

    /// `import x from "…"`: reported, then the parser recovers at the next item.
    fn default_import(&mut self) -> Fail {
        let span = self.cur_span();
        let name = self.text(span.lo, span.hi).to_string();
        self.report(
            Diagnostic::error(
                "default imports are not supported: Velt has named exports only",
                span,
            )
            .with_note(format!(
                "import by name: `import {{ {name} }} from \"…\"`, or the whole module: `import * as {name} from \"…\"`"
            )),
        );
        Fail
    }

    /// At `default` after `export`. A declaration (`export default function f`) is reported and
    /// parsed as a named export; anything else (`export default expr;`) is reported and skipped.
    pub(super) fn export_default(&mut self) -> PResult<()> {
        let span = self.cur_span();
        self.bump(); // default
        let declaration = match self.cur_kw() {
            Some(Kw::Function | Kw::Class | Kw::Struct | Kw::Interface | Kw::Enum) => {
                let t = self.tok(self.pos);
                Some(self.text(t.lo, t.hi))
            }
            Some(Kw::Async) if self.nth(1) == Tok::Kw(Kw::Function) => Some("async function"),
            _ => None,
        };
        let name = self.declaration_name_ahead();
        let note = match (declaration, &name) {
            (Some(kw), Some(name)) => format!("use a named export: `export {kw} {name}`"),
            (Some(kw), None) => format!("use a named export: `export {kw} name`"),
            (None, Some(name)) if self.nth(1) == Tok::Semi => {
                format!("use a named export: `export {{ {name} }};`")
            }
            (None, _) => "use a named export: `export const name = …;`".to_string(),
        };
        self.report(
            Diagnostic::error(
                "`export default` is not supported: Velt has named exports only",
                span,
            )
            .with_note(note),
        );
        if declaration.is_some() {
            Ok(())
        } else {
            Err(Fail)
        }
    }

    /// The name a declaration starting at the cursor declares (`function f` → `f`), or the
    /// identifier at the cursor (`export default f;` → `f`).
    fn declaration_name_ahead(&mut self) -> Option<String> {
        let skip = match self.cur_kw() {
            Some(Kw::Async) => 2,
            Some(Kw::Function | Kw::Class | Kw::Struct | Kw::Interface | Kw::Enum) => 1,
            _ => 0,
        };
        let t = self.tok(self.pos + skip);
        Self::is_ident_like(t.kind).then(|| self.text(t.lo, t.hi).to_string())
    }

    /// Push a diagnostic built by the caller (with notes), unless speculating.
    fn report(&mut self, d: Diagnostic) {
        if self.speculating == 0 {
            self.diags.push(d);
        }
    }
}
