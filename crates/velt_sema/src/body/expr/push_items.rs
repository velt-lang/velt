//! `push` with any number of items, as in JS: `xs.push(a, b)`, `xs.push(...ys)`,
//! `xs.push(a, ...ys, b)`. One item is the builtin `push` itself (no change to that hot path);
//! any other call is desugared without new HIR into
//!
//! ```text
//! { <the receiver's indices and objects that are values, bound once>;
//!   const t0 = a; const src = <ys, when not a place>; const t2 = b;
//!   xs.push(t0); for (const e of src) xs.push(<share of e>); xs.push(t2); }
//! ```
//!
//! The receiver and the items are evaluated first, in order, as JS evaluates the arguments
//! before the call (a variable or literal item is read where it is pushed: reading it has no
//! effect). A spread array is appended element by element, without an intermediate array,
//! except the receiver itself (`xs.push(...xs)`), whose elements are copied first.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::{is_path, is_place, place_root, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, UseMode};

/// One argument of `push`, evaluated.
enum Item {
    /// A value to push.
    One(hir::Expr),
    /// A spread array (a place, or a temporary bound first) and its element type.
    All(hir::Expr, TyId),
}

impl FnCx<'_, '_> {
    /// `recv.push(args)` with other than one plain item (see the module docs).
    pub(super) fn push_items(
        &mut self,
        mut recv: hir::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let unit = self.cx.ty.unit;
        let Some(elem) = self.cx.ty.array_elem(recv.ty) else {
            return self.error_expr(span);
        };
        self.use_mutably(&mut recv, "call a mutating method on");
        let mut stmts = vec![];
        self.hoist_object(&mut recv, &mut stmts);
        let mut items = vec![];
        for a in args {
            let item = match &a.kind {
                ast::ExprKind::Spread(inner) => self.push_spread(inner, elem, &recv, &mut stmts),
                _ => Some(self.push_item(a, elem, &mut stmts)),
            };
            items.extend(item);
        }
        for item in items {
            let stmt = match item {
                Item::One(v) => self.push_onto(recv.clone(), v),
                Item::All(src, et) => self.push_all_to(recv.clone(), src, (et, elem), span),
            };
            stmts.push(stmt);
        }
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        self.mk(H::Block(block), unit, span)
    }

    /// A plain item, converted to the element type; bound to a temporary unless it is a
    /// variable or a literal.
    fn push_item(&mut self, a: &ast::Expr, elem: TyId, stmts: &mut Vec<hir::Stmt>) -> Item {
        let v = self.expr_coerce(a, elem, Want::Move);
        if matches!(v.kind, H::Lit(_) | H::Local(..)) {
            return Item::One(v);
        }
        let mode = self.use_mode(v.ty, Want::Move);
        let mut t = self.temp("<item>", v, stmts);
        set_place_mode(&mut t, mode);
        Item::One(t)
    }

    /// `...src`: an array whose elements convert to `elem`. The receiver's own elements
    /// (`xs.push(...xs)`) are copied first, as JS spreads them before pushing.
    fn push_spread(
        &mut self,
        inner: &ast::Expr,
        elem: TyId,
        recv: &hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) -> Option<Item> {
        let h = self.expr(inner, None, Want::Borrow);
        let Some(et) = self.cx.ty.array_elem(h.ty) else {
            if !self.cx.ty.is_bottom(h.ty) {
                let t = self.cx.display(h.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("cannot spread a value of type `{t}` into `push`"),
                        inner.span,
                    )
                    .with_note("`push(...xs)` takes an array; `for...of` pushes the values of other iterables"),
                );
            }
            return None;
        };
        if !self.spread_fits(et, elem) {
            if !self.cx.ty.has_error(et) {
                let (from, to) = (self.cx.display(et), self.cx.display(elem));
                let msg = format!("cannot push `{from}` elements into an array of `{to}`");
                self.cx.err(msg, inner.span);
            }
            return None;
        }
        // A path into the receiver (`xs.push(...xs)`) is read again as the copy `[...xs]`.
        let aliased = is_path(&h) && place_root(&h).is_some() && place_root(&h) == place_root(recv);
        let mut src = if aliased {
            let copy = ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Array(vec![ast::Expr {
                    id: ast::NodeId(u32::MAX),
                    kind: ast::ExprKind::Spread(Box::new(inner.clone())),
                    span: inner.span,
                }]),
                span: inner.span,
            };
            let ty = h.ty;
            let c = self.expr(&copy, Some(ty), Want::Move);
            self.temp("<spread>", c, stmts)
        } else if is_place(&h) {
            h
        } else {
            self.temp("<spread>", h, stmts)
        };
        set_place_mode(&mut src, UseMode::Borrow);
        Some(Item::All(src, et))
    }
}
