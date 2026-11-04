//! `export default …` (#1916), desugared here into ordinary exports, so later stages see
//! named exports and bindings only:
//! - `export default function f() {}` (also `class`, `abstract class`, `struct`, `interface`,
//!   `enum`): the declaration, not exported, followed by the export list `export { f as
//!   default }` whose item span is the `default` keyword (a live binding, as in Node);
//! - `export default function () {}` / `export default class {}`: the declaration named
//!   `default` (a name no declaration can be written with), exported;
//! - `export default x;`: the export list `export { x as default }`, a live binding, unless `x`
//!   is a module-level `let`, which may change: then, as `export default expr;`,
//! - `export default expr;`: the exported module constant `const default = expr;`, evaluated
//!   once in module order (sema keeps it in module state).
//!
//! Desugared lists are told from written ones by their name standing after their alias
//! ([`crate::written_default`]), so `velt fmt` prints them as written. A second default export
//! is TypeScript's TS2528 and is dropped.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

/// The export name of a default export.
pub(crate) const DEFAULT: &str = "default";

impl Parser<'_> {
    /// At `default` after `export` (the item starts at `lo`).
    pub(super) fn parse_export_default(&mut self, lo: u32, top: bool) -> PResult<Item> {
        let kw = self.cur_span();
        if !top {
            self.error(
                "`export default` is only allowed at the top level of a module",
                kw,
            );
            return Err(Fail);
        }
        self.bump(); // default
        if let Some(anonymous) = self.default_declaration_ahead() {
            return self.default_declaration(lo, kw, anonymous);
        }
        let expr = self.parse_assign()?;
        self.expect_semi()?;
        let span = self.span_from(lo);
        if let ExprKind::Ident(name) = &expr.kind {
            let list = default_list(name.clone(), kw, span);
            return Ok(Item {
                kind: ItemKind::Import(list),
                exported: true,
                span,
            });
        }
        Ok(self.default_const(expr, kw, span))
    }

    /// At a declaration after `export default`: whether it has no name (`function () {}`,
    /// `class extends B {}`), or `None` when no declaration follows.
    fn default_declaration_ahead(&mut self) -> Option<bool> {
        let at = match self.cur_kw() {
            Some(Kw::Function | Kw::Class | Kw::Struct | Kw::Interface | Kw::Enum) => 1,
            Some(Kw::Async) if self.nth(1) == Tok::Kw(Kw::Function) => 2,
            _ if self.at_abstract_class() => 2,
            _ => return None,
        };
        let at = at + usize::from(self.nth(at) == Tok::Star);
        let name = self.nth(at);
        let heritage = matches!(name, Tok::Kw(Kw::Extends | Kw::Implements));
        Some(!Self::is_ident_like(name) || heritage)
    }

    /// `export default <declaration>` (see the module docs).
    fn default_declaration(&mut self, lo: u32, kw: Span, anonymous: bool) -> PResult<Item> {
        if self.at_kw(Kw::Enum) && self.plain_ts {
            let span = self.cur_span();
            self.report(
                velt_common::Diagnostic::error(
                    "`export default` cannot be followed by an `enum` declaration in TypeScript",
                    span,
                )
                .with_note(
                    "declare the enum, then export it: `enum E { … }` and `export default E;`",
                ),
            );
        }
        if anonymous {
            self.anonymous_default = Some(Ident {
                name: DEFAULT.into(),
                span: kw,
            });
        }
        let kind = self.parse_item_kind(lo, false);
        self.anonymous_default = None;
        let kind = kind?;
        let span = self.span_from(lo);
        let name = match declared_name(&kind) {
            Some(name) if !anonymous => name.clone(),
            _ => {
                return Ok(Item {
                    kind,
                    exported: true,
                    span,
                })
            }
        };
        self.more_items.push(Item {
            kind: ItemKind::Import(default_list(name, kw, kw)),
            exported: true,
            span: kw,
        });
        Ok(Item {
            kind,
            exported: false,
            span,
        })
    }

    /// The name of a declaration after `export default` when it has none (taken once).
    pub(super) fn parse_decl_name(&mut self) -> PResult<Ident> {
        match self.anonymous_default.take() {
            Some(name) => Ok(name),
            None => self.parse_binding_ident(),
        }
    }

    /// `export default expr;` as `export const default = expr;`.
    fn default_const(&mut self, expr: Expr, kw: Span, span: Span) -> Item {
        let name = Ident {
            name: DEFAULT.into(),
            span: kw,
        };
        let pattern = self.mk_pat(PatternKind::Ident(name), kw);
        Item {
            kind: ItemKind::Var(VarDecl {
                kind: VarKind::Const,
                pattern,
                ty: None,
                init: Some(expr),
                span,
            }),
            exported: true,
            span,
        }
    }

    /// After the whole module is parsed: `export default x;` of a module-level `let` becomes a
    /// snapshot, and default exports after the first are reported and dropped.
    pub(super) fn finish_default_exports(&mut self, items: &mut Vec<Item>) {
        let lets: Vec<String> = (items.iter())
            .filter_map(|i| match &i.kind {
                ItemKind::Var(v) if v.kind == VarKind::Let => match &v.pattern.kind {
                    PatternKind::Ident(n) => Some(n.name.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let mut seen = false;
        let mut i = 0;
        while i < items.len() {
            if let Some((name, kw)) = self.snapshot_of_let(&items[i], &lets) {
                let expr = self.mk_expr(ExprKind::Ident(name), items[i].span);
                let span = items[i].span;
                items[i] = self.default_const(expr, kw, span);
            }
            let item = &mut items[i];
            let mut keep = true;
            if let ItemKind::Import(imp) = &mut item.kind {
                if item.exported && imp.namespace.is_none() {
                    let diags = &mut self.diags;
                    imp.names.retain(|n| {
                        let bound = n.alias.as_ref().unwrap_or(&n.name);
                        if bound.name != DEFAULT {
                            return true;
                        }
                        let first = !seen;
                        seen = true;
                        if !first {
                            diags.push(duplicate(bound.span));
                        }
                        first
                    });
                    keep = !imp.names.is_empty() || imp.all || imp.namespace.is_some();
                }
            } else if item.exported && declared_name(&item.kind).is_some_and(|n| n.name == DEFAULT)
            {
                keep = !seen;
                seen = true;
                if !keep {
                    let span = declared_name(&item.kind).map_or(item.span, |n| n.span);
                    self.diags.push(duplicate(span));
                }
            }
            if keep {
                i += 1;
            } else {
                items.remove(i);
            }
        }
    }

    /// `export default x;` (desugared to a list) where `x` is a module-level `let`: `x` and the
    /// `default` keyword's span.
    fn snapshot_of_let(&self, item: &Item, lets: &[String]) -> Option<(Ident, Span)> {
        let ItemKind::Import(imp) = &item.kind else {
            return None;
        };
        let [n] = imp.names.as_slice() else {
            return None;
        };
        let alias = n.alias.as_ref()?;
        let expression = crate::written_default(imp) && item.span != alias.span;
        (expression && lets.contains(&n.name.name)).then(|| (n.name.clone(), alias.span))
    }
}

/// `export { name as default }` with the alias at the `default` keyword (`kw`).
fn default_list(name: Ident, kw: Span, span: Span) -> Import {
    Import {
        names: vec![ImportName {
            name,
            alias: Some(Ident {
                name: DEFAULT.into(),
                span: kw,
            }),
            type_only: false,
        }],
        from: String::new(),
        from_span: Span::new(span.file, span.hi, span.hi),
        namespace: None,
        all: false,
        default_import: false,
    }
}

/// The name a declaration after `export default` declares.
fn declared_name(kind: &ItemKind) -> Option<&Ident> {
    match kind {
        ItemKind::Function(f) => Some(&f.sig.name),
        ItemKind::Struct(t) | ItemKind::Class(t) => Some(&t.name),
        ItemKind::Interface(i) => Some(&i.name),
        ItemKind::Enum(e) => Some(&e.name),
        ItemKind::TypeAlias(a) => Some(&a.name),
        ItemKind::Var(v) => match &v.pattern.kind {
            PatternKind::Ident(n) => Some(n),
            _ => None,
        },
        _ => None,
    }
}

/// TypeScript's TS2528.
fn duplicate(span: Span) -> velt_common::Diagnostic {
    velt_common::Diagnostic::error("a module cannot have multiple default exports", span)
        .with_note("keep one `export default`; export the others by name")
}
