//! `export default …` (#1916), desugared here into ordinary exports, so later stages see
//! named exports and bindings only:
//! - `export default function f() {}` (also `class`, `abstract class`, `struct`, `interface`,
//!   `enum`): the declaration, not exported, followed by the export list `export { f as
//!   default }` whose item span is the `default` keyword (a live binding, as in Node);
//! - `export default function () {}` / `export default class {}`: the declaration named
//!   `default` (a name no declaration can be written with), exported;
//! - `export default x;`: the export list `export { x as default }` when `x` names a declaration
//!   whose value can't change and is there when the item runs, or an import (sema makes the
//!   import of a `let` a snapshot, as only it can tell: `collect::default_snapshots`);
//!   otherwise (`x` is a `let`, a `const` or class declared below, or not a module binding
//!   at all, such as `NaN`) as `export default expr;`,
//! - `export default expr;`: the exported module constant `const default = expr;`, evaluated
//!   once in module order (sema keeps it in module state).
//!
//! Desugared lists are told from written ones by their name standing after their alias
//! ([`crate::written_default`]), so `velt fmt` prints them as written. Default exports that
//! can't stand together are TypeScript's TS2528, and the later one is dropped: a type
//! (`export default interface`) may stand next to a value, but not next to `export default
//! expr;` or `export default x;`, whose `default` is the expression's alone.

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
        if self.at_kw(Kw::Enum) && self.typescript {
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
        self.default_overloads = true;
        let kind = self.parse_item_kind(lo, false);
        self.default_overloads = false;
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

    /// After an overload signature of `export default function`: at `export default` before
    /// the next one, which is skipped (the signature's name is kept when it has none). Whether
    /// it was there.
    pub(super) fn eat_default_overload(&mut self, name: &Ident) -> bool {
        let next =
            self.default_overloads && self.at_kw(Kw::Export) && self.nth(1) == Tok::Kw(Kw::Default);
        if next {
            self.bump();
            self.bump();
            if name.name == DEFAULT {
                self.anonymous_default = Some(name.clone());
            }
        }
        next
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
                c_const: false,
            }),
            exported: true,
            span,
        }
    }

    /// After the whole module is parsed (`stmts`: its top-level statements): `export default
    /// x;` becomes an expression where it must, and default exports that can't stand together
    /// are reported and dropped.
    pub(super) fn finish_default_exports(&mut self, items: &mut Vec<Item>, stmts: &[Stmt]) {
        for i in 0..items.len() {
            let Some((name, kw)) = ident_default(&items[i]) else {
                continue;
            };
            if is_expression(items, stmts, &name.name, items[i].span.lo) {
                let span = items[i].span;
                let expr = self.mk_expr(ExprKind::Ident(name), span);
                items[i] = self.default_const(expr, kw, span);
            }
        }
        let kinds: Vec<Option<DefaultKind>> =
            (0..items.len()).map(|i| default_kind(items, i)).collect();
        if kinds.iter().flatten().count() < 2 {
            return;
        }
        let beside_value = kinds
            .iter()
            .flatten()
            .any(|k| matches!(k, DefaultKind::Declared | DefaultKind::Expression));
        let (mut value, mut expression, mut interface) = (false, false, false);
        let mut out = Vec::with_capacity(items.len());
        for (mut item, kind) in std::mem::take(items).into_iter().zip(kinds) {
            let keep = match kind {
                None => true,
                // Several interfaces merge when they have one name; the first names the type.
                Some(DefaultKind::Interface) => {
                    let first = !interface;
                    interface = true;
                    if first && beside_value {
                        mark_type_only(&mut item);
                    }
                    if expression {
                        self.diags.push(duplicate(default_span(&item)));
                    }
                    first && !expression
                }
                Some(DefaultKind::Declared) => {
                    let keep = !value;
                    value = true;
                    if !keep {
                        self.diags.push(duplicate(default_span(&item)));
                    }
                    keep
                }
                Some(DefaultKind::Expression) => {
                    let keep = !value && !interface;
                    value = true;
                    expression = true;
                    if !keep {
                        self.diags.push(duplicate(default_span(&item)));
                    }
                    keep
                }
            };
            if keep || without_default(&mut item) {
                out.push(item);
            }
        }
        *items = out;
    }
}

/// What a default export is, for which may stand together (TS2528).
#[derive(Clone, Copy, PartialEq)]
enum DefaultKind {
    /// `export default interface I`.
    Interface,
    /// A declaration, `export { x as default }`, `export * as default from`.
    Declared,
    /// `export default x;`, `export default expr;`.
    Expression,
}

/// The default export item `i` of `items` holds, if it does.
fn default_kind(items: &[Item], i: usize) -> Option<DefaultKind> {
    let item = &items[i];
    if !item.exported {
        return None;
    }
    let ItemKind::Import(imp) = &item.kind else {
        return match declared_name(&item.kind) {
            Some(n) if n.name == DEFAULT => Some(match item.kind {
                ItemKind::Var(_) => DefaultKind::Expression,
                _ => DefaultKind::Declared,
            }),
            _ => None,
        };
    };
    if imp.namespace.as_ref().is_some_and(|n| n.name == DEFAULT) {
        return Some(DefaultKind::Declared);
    }
    if ident_default(item).is_some() {
        return Some(DefaultKind::Expression);
    }
    if !imp.names.iter().any(|n| bound(n).name == DEFAULT) {
        return None;
    }
    let declared = i.checked_sub(1).map(|p| &items[p].kind);
    match (crate::written_default(imp), declared) {
        (true, Some(ItemKind::Interface(_))) => Some(DefaultKind::Interface),
        _ => Some(DefaultKind::Declared),
    }
}

/// `export default x;` (desugared to a list): `x` and the `default` keyword's span.
fn ident_default(item: &Item) -> Option<(Ident, Span)> {
    let ItemKind::Import(imp) = &item.kind else {
        return None;
    };
    let [n] = imp.names.as_slice() else {
        return None;
    };
    let alias = n.alias.as_ref()?;
    (item.exported && crate::written_default(imp) && item.span != alias.span)
        .then(|| (n.name.clone(), alias.span))
}

/// Must `export default name;` at offset `at` read `name` as an expression: `name` is a `let`, a
/// `const`, class or enum declared below (the read is then reported, as in a statement), or no
/// binding of the module?
fn is_expression(items: &[Item], stmts: &[Stmt], name: &str, at: u32) -> bool {
    let var = |v: &VarDecl, lo: u32| {
        let mut names = Default::default();
        super::script_names::pattern_names(&v.pattern, &mut names);
        names
            .contains(name)
            .then_some(v.kind != VarKind::Const || lo > at)
    };
    for s in stmts {
        if let StmtKind::Var(v) = &s.kind {
            if let Some(early) = var(v, s.span.lo) {
                return early;
            }
        }
    }
    let mut found = false;
    for item in items {
        match &item.kind {
            ItemKind::Var(v) => {
                if let Some(early) = var(v, item.span.lo) {
                    return early;
                }
            }
            ItemKind::Class(d) if d.name.name == name => return item.span.lo > at,
            ItemKind::Enum(e) if e.name.name == name => return item.span.lo > at,
            ItemKind::Import(imp) if !item.exported => {
                found |= imp.namespace.as_ref().is_some_and(|n| n.name == name)
                    || imp.names.iter().any(|n| bound(n).name == name);
            }
            ItemKind::Namespace(n) => found |= n.name.name == name,
            ItemKind::ExternFn(f) => found |= f.name.name == name,
            kind => found |= declared_name(kind).is_some_and(|n| n.name == name),
        }
    }
    !found
}

fn bound(n: &ImportName) -> &Ident {
    n.alias.as_ref().unwrap_or(&n.name)
}

/// Mark the list of `export default interface I` as giving `default` its type meaning only
/// (a value default stands next to it: `crate::is_type_default`).
fn mark_type_only(item: &mut Item) {
    if let ItemKind::Import(imp) = &mut item.kind {
        imp.names[0].type_only = true;
    }
}

/// Where item `item` says `default`.
fn default_span(item: &Item) -> Span {
    match &item.kind {
        ItemKind::Import(imp) => (imp.names.iter().map(bound))
            .chain(imp.namespace.as_ref())
            .find(|n| n.name == DEFAULT)
            .map_or(item.span, |n| n.span),
        kind => declared_name(kind).map_or(item.span, |n| n.span),
    }
}

/// Remove `default` from export list `item`, for one that exports more (`export { a as default,
/// b }`); false when nothing is left of it.
fn without_default(item: &mut Item) -> bool {
    let ItemKind::Import(imp) = &mut item.kind else {
        return false;
    };
    imp.names.retain(|n| bound(n).name != DEFAULT);
    !imp.names.is_empty()
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
        type_only: false,
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
