//! `import.meta` and `import(...)` in expressions. `import.meta` reads the module constant
//! `ast::IMPORT_META`, which the parser adds to a module that mentions it (the driver fills in its
//! fields). A dynamic `import(...)` is reported: every module is loaded while compiling.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;
use velt_common::Span;

impl<'a> Parser<'a> {
    /// At `import .`: `import.meta`, as the name [`IMPORT_META`].
    pub(super) fn parse_import_meta(&mut self) -> PResult<ExprKind> {
        let lo = self.cur_lo();
        self.bump(); // import
        self.bump(); // .
        if !self.at_word("meta") {
            self.error_expected("`meta` (`import.meta`)");
            return Err(Fail);
        }
        self.bump();
        let span = self.span_from(lo);
        self.import_meta.get_or_insert(span);
        Ok(ExprKind::Ident(Ident {
            name: IMPORT_META.into(),
            span,
        }))
    }

    /// At `import (`: reports the dynamic import.
    pub(super) fn dynamic_import(&mut self) {
        let lo = self.cur_lo();
        self.bump(); // import
        let constant = matches!(self.nth(1), Tok::Str(_)) && self.nth(2) == Tok::RParen;
        // The arguments, skipped: `(` up to its matching `)`.
        let mut depth = 0u32;
        loop {
            match self.peek() {
                Tok::LParen => depth += 1,
                Tok::RParen => depth -= 1,
                Tok::Eof => break,
                _ => {}
            }
            self.bump();
            if depth == 0 {
                break;
            }
        }
        let span = Span::new(self.file, lo, self.prev_hi);
        let note = match constant {
            true => "Velt loads every module while compiling: import it with a static `import` at the top of the module (`import * as m from \"…\"`)",
            false => "Velt loads every module while compiling, so a module named only at run time cannot be loaded",
        };
        if self.speculating == 0 {
            self.diags.push(
                velt_common::Diagnostic::error("dynamic `import()` is not supported", span)
                    .with_note(note),
            );
        }
    }

    /// The module constant `import.meta` stands for (first mentioned at `span`): `const
    /// import.meta: __ImportMeta = new __ImportMeta("", "", false, "");` (see [`IMPORT_META`]).
    pub(super) fn import_meta_item(&mut self, span: Span) -> Item {
        let ident = |name: &str| Ident {
            name: name.into(),
            span,
        };
        let class = TypeExpr {
            kind: TypeExprKind::Named {
                path: vec![ident(IMPORT_META_CLASS)],
                args: vec![],
            },
            span,
        };
        let string = |p: &mut Self| p.mk_expr(ExprKind::Lit(Lit::Str(String::new())), span);
        let args = vec![
            string(self),
            string(self),
            self.mk_expr(ExprKind::Lit(Lit::Bool(false)), span),
            string(self),
        ];
        let init = self.mk_expr(
            ExprKind::New {
                class: class.clone(),
                args,
            },
            span,
        );
        let pattern = self.mk_pat(PatternKind::Ident(ident(IMPORT_META)), span);
        Item {
            kind: ItemKind::Var(VarDecl {
                kind: VarKind::Const,
                pattern,
                ty: Some(class),
                init: Some(init),
                span,
            }),
            exported: false,
            span,
        }
    }

    /// `import { ImportMeta as __ImportMeta } from "velt:import_meta"`, the class of
    /// `import.meta`. Its span is empty, at the start of the file: it is not in the source
    /// (`crate::is_synthesized_import`).
    pub(super) fn import_meta_import(&self) -> Item {
        let span = Span::new(self.file, 0, 0);
        let import = Import {
            names: vec![ImportName {
                name: Ident {
                    name: "ImportMeta".into(),
                    span,
                },
                alias: Some(Ident {
                    name: IMPORT_META_CLASS.into(),
                    span,
                }),
                type_only: false,
            }],
            from: IMPORT_META_MODULE.into(),
            from_span: span,
            namespace: None,
            all: false,
        };
        Item {
            kind: ItemKind::Import(import),
            exported: false,
            span,
        }
    }
}
