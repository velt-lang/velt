//! Module syntax: `import { a, b as c } from "…"`, `import type { T }`, `import * as ns`,
//! `import "…"`, and the export forms that are not declarations — re-exports
//! (`export { a } from "…"`, `export * from "…"`) and local export lists (`export { a, b }`).
//! A default import `import x from "…"` is `import { default as x } from "…"`; `export
//! default` is in `default_exports`.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::{Diagnostic, Span};

impl<'a> Parser<'a> {
    /// `import { a, b as c } from "path";`, `import type { T } from "path";`,
    /// `import * as ns from "path";` or `import "path";`
    pub(super) fn parse_import(&mut self) -> PResult<Import> {
        self.bump(); // import
        let type_only = self.at_type_only_import();
        if type_only {
            self.bump();
        }
        let mut names = vec![];
        let mut namespace = None;
        // `import x from`, `import x, { y } from`, `import x, * as ns from`.
        let default_import = self.at_ident_like() && !self.at_kw(Kw::From)
            || self.at_kw(Kw::From) && matches!(self.nth(1), Tok::Kw(Kw::From) | Tok::Comma);
        if default_import {
            let local = self.parse_binding_ident()?;
            names.push(ImportName {
                name: Ident {
                    name: super::default_exports::DEFAULT.into(),
                    span: local.span,
                },
                alias: Some(local),
                type_only,
            });
        }
        let more = !default_import || self.eat(Tok::Comma);
        if type_only && default_import && more {
            let span = self.cur_span();
            self.error(
                "a type-only import can specify a default import or named bindings, but not both (TypeScript's TS1363)",
                span,
            );
        }
        let mut clause = default_import;
        if more && self.at(Tok::LBrace) {
            names.extend(self.parse_import_names(type_only, true)?);
            clause = true;
        } else if more && self.eat(Tok::Star) {
            self.expect_kw(Kw::As, "as")?;
            namespace = Some(self.parse_binding_ident()?);
            clause = true;
        } else if default_import && more {
            self.error_expected("`{` or `*`");
            return Err(Fail);
        }
        if clause {
            self.expect_kw(Kw::From, "from")?;
        }
        let (from, from_span) = self.parse_module_spec()?;
        self.expect_semi()?;
        let whole_module = type_only && namespace.is_some();
        Ok(Import {
            names,
            from,
            from_span,
            namespace,
            all: false,
            default_import,
            type_only: whole_module,
        })
    }

    /// After `import`: at `type` marking the import type-only (`import type { T }`, `import
    /// type T from`), not at a default import named `type` (`import type from "…"`).
    fn at_type_only_import(&mut self) -> bool {
        if !self.at_kw(Kw::Type) {
            return false;
        }
        match self.nth(1) {
            Tok::LBrace | Tok::Star => true,
            Tok::Kw(Kw::From) => matches!(self.nth(2), Tok::Kw(Kw::From) | Tok::Comma),
            t => Self::is_ident_like(t),
        }
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
            // `export * as ns from "…"`: the module's namespace, `default` included, exported
            // as `ns` (its members as `ns.x`, as TypeScript namespaces are).
            let namespace = match self.eat_kw(Kw::As) {
                true => Some(self.parse_ident()?),
                false => None,
            };
            self.expect_kw(Kw::From, "from")?;
            let (from, from_span) = self.parse_module_spec()?;
            self.expect_semi()?;
            return Ok(Import {
                names: vec![],
                from,
                from_span,
                all: namespace.is_none(),
                namespace,
                default_import: false,
                type_only: false,
            });
        }
        let names = self.parse_import_names(type_only, false)?;
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
            default_import: false,
            type_only: false,
        })
    }

    /// `{ a, type B, c as d }`; `type_only` marks every name (`import type { … }`). With `binds`
    /// (an import, not a re-export) the names are declared in this module.
    fn parse_import_names(&mut self, type_only: bool, binds: bool) -> PResult<Vec<ImportName>> {
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
            if binds {
                self.check_binding_name(alias.as_ref().unwrap_or(&name));
            }
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

    /// Push a diagnostic built by the caller (with notes), unless speculating.
    pub(super) fn report(&mut self, d: Diagnostic) {
        if self.speculating == 0 {
            self.diags.push(d);
        }
    }
}
