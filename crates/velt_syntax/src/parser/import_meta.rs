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
    /// import.meta: { readonly dirname: string; …; readonly resolve: (specifier: string) =>
    /// string; … } = { dirname: "", filename: "", main: false, resolve: …, url: "" };` (see
    /// [`IMPORT_META`]).
    pub(super) fn import_meta_item(&mut self, span: Span) -> Item {
        let ident = |name: &str| Ident {
            name: name.into(),
            span,
        };
        let named = |name: &str| TypeExpr {
            kind: TypeExprKind::Named {
                path: vec![ident(name)],
                args: vec![],
            },
            span,
        };
        let mut props = vec![];
        let mut fields = vec![];
        for f in IMPORT_META_FIELDS {
            let (ty, value) = match f {
                "main" => (named("boolean"), ExprKind::Lit(Lit::Bool(false))),
                "resolve" => (
                    TypeExpr {
                        kind: TypeExprKind::Function {
                            params: vec![named("string")],
                            ret: Box::new(named("string")),
                            throws: Some(Box::new(named("Error"))),
                            rest: false,
                        },
                        span,
                    },
                    self.import_meta_resolve(span),
                ),
                _ => (named("string"), ExprKind::Lit(Lit::Str(String::new()))),
            };
            let value = self.mk_expr(value, span);
            props.push(ObjectProp::KeyValue(ident(f), value));
            // Its fields are read-only, also in a copy (`const m = import.meta; m.url = "x"`
            // would otherwise write the copy, not `import.meta`).
            fields.push(ObjectTypeField {
                name: ident(f),
                ty,
                optional: false,
                readonly: true,
                span,
            });
        }
        let init = self.mk_expr(ExprKind::Object(props), span);
        let ty = TypeExpr {
            kind: TypeExprKind::Object(fields),
            span,
        };
        let pattern = self.mk_pat(PatternKind::Ident(ident(IMPORT_META)), span);
        Item {
            kind: ItemKind::Var(VarDecl {
                kind: VarKind::Const,
                pattern,
                ty: Some(ty),
                init: Some(init),
                span,
            }),
            exported: false,
            span,
        }
    }

    /// `import { __import_meta_resolve } from "velt:import_meta"`, for `import.meta.resolve`.
    pub(super) fn import_meta_import(span: Span) -> Item {
        let import = Import {
            names: vec![ImportName {
                name: Ident {
                    name: IMPORT_META_RESOLVE_FN.into(),
                    span,
                },
                alias: None,
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

    /// `(specifier: string): string => __import_meta_resolve(specifier, "")`, the value of
    /// `import.meta.resolve` (the driver writes the module's URL into the `""`).
    fn import_meta_resolve(&mut self, span: Span) -> ExprKind {
        let ident = |name: &str| Ident {
            name: name.into(),
            span,
        };
        let named = |name: &str| TypeExpr {
            kind: TypeExprKind::Named {
                path: vec![ident(name)],
                args: vec![],
            },
            span,
        };
        let callee = self.mk_expr(ExprKind::Ident(ident(IMPORT_META_RESOLVE_FN)), span);
        let arg = self.mk_expr(ExprKind::Ident(ident("specifier")), span);
        let url = self.mk_expr(ExprKind::Lit(Lit::Str(String::new())), span);
        let call = ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: vec![arg, url],
            optional: false,
        };
        let body = self.mk_expr(call, span);
        ExprKind::Arrow {
            type_params: vec![],
            params: vec![ArrowParam {
                name: ident("specifier"),
                pattern: None,
                ty: Some(named("string")),
                default: None,
                optional: false,
                rest: false,
            }],
            ret: Some(named("string")),
            throws: Some(named("Error")),
            body: ArrowBody::Expr(Box::new(body)),
            is_async: false,
        }
    }
}
