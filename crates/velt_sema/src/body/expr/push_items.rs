//! `push` with any number of items, as in JS: `xs.push(a, b)`, `xs.push(...ys)`,
//! `xs.push(a, ...ys, b)`. One item is the builtin `push` itself (no change to that hot path);
//! any other call is desugared without new HIR into
//!
//! ```text
//! { <the receiver's indices and objects that are values, bound once>;
//!   const t0 = a; const src = <ys, when not a variable or field>; const n = src.length;
//!   const t2 = b;
//!   xs.push(t0); for (let i = 0; i < n; i++) xs.push(<share of src[i]>); xs.push(t2); }
//! ```
//!
//! The receiver and the items are evaluated first, in order, as JS evaluates the arguments
//! before the call. A variable or literal item is read where it is pushed (reading it has no
//! effect), unless a later item has effects (`xs.push(a, (a = 2))`): then it is bound too, and a
//! spread array is copied first (`xs.push(...ys, ys.pop())`). A spread array is otherwise
//! appended element by element, without an intermediate array, up to the length it had when
//! evaluated, before the first push: the receiver itself, under its own name (`xs.push(...xs)`,
//! copied first) or another (`const alias = xs; xs.push(...alias)`), is spread as it was.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::spread_args::evaluated_anywhere;
use crate::body::places::{is_path, is_place, place_root, set_place_mode};
use crate::body::LocalKind;
use crate::body::{FnCx, Want};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, StmtKind as S, TyId, UseMode};

/// One argument of `push`, evaluated.
enum Item {
    /// A value to push.
    One(hir::Expr),
    /// A spread array (a place, or a temporary bound first), its element type, and the local
    /// holding its length when it is evaluated.
    All(hir::Expr, TyId, hir::LocalId),
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
        for (k, a) in args.iter().enumerate() {
            // A later item with effects may change what this one reads.
            let bind = !args[k + 1..]
                .iter()
                .all(|b| evaluated_anywhere(unspread(b)));
            let item = match &a.kind {
                ast::ExprKind::Spread(inner) => {
                    self.push_spread(inner, elem, &recv, bind, &mut stmts)
                }
                _ => Some(self.push_item(a, elem, bind, &mut stmts)),
            };
            items.extend(item);
        }
        for item in items {
            let stmt = match item {
                Item::One(v) => self.push_onto(recv.clone(), v),
                Item::All(src, et, n) => self.push_indexed(recv.clone(), src, n, (et, elem), span),
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
    /// literal, or a variable and no later item has effects (`bind`).
    fn push_item(
        &mut self,
        a: &ast::Expr,
        elem: TyId,
        bind: bool,
        stmts: &mut Vec<hir::Stmt>,
    ) -> Item {
        let mut v = self.expr_coerce(a, elem, Want::Move);
        match v.kind {
            H::Lit(_) => return Item::One(v),
            H::Local(..) if !bind => return Item::One(v),
            H::Local(..) if !self.cx.is_copy(v.ty) => {
                // The value it holds now, shared.
                set_place_mode(&mut v, UseMode::Borrow);
                let (ty, span) = (v.ty, v.span);
                v = self.intrinsic(Intrinsic::Share, vec![v], ty, span);
            }
            _ => {}
        }
        let mode = self.use_mode(v.ty, Want::Move);
        let mut t = self.temp("<item>", v, stmts);
        set_place_mode(&mut t, mode);
        Item::One(t)
    }

    /// `...src`: an array whose elements convert to `elem`. The receiver's own elements
    /// (`xs.push(...xs)`) are copied first, as JS spreads them before pushing, and so is an
    /// array a later item may change (`copy`).
    fn push_spread(
        &mut self,
        inner: &ast::Expr,
        elem: TyId,
        recv: &hir::Expr,
        copy: bool,
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
        let mut src = if aliased || copy {
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
        } else if is_path(&h) {
            h
        } else if is_place(&h) {
            // `xss[i]`: the array it holds now, read once.
            let (ty, span) = (h.ty, h.span);
            let shared = self.intrinsic(Intrinsic::Share, vec![h], ty, span);
            self.temp("<spread>", shared, stmts)
        } else {
            self.temp("<spread>", h, stmts)
        };
        set_place_mode(&mut src, UseMode::Borrow);
        if et == self.cx.ty.never {
            // `...[]`: evaluated, nothing to push.
            return None;
        }
        let usize_ = self.cx.ty.usize;
        let len = self.intrinsic(Intrinsic::ArrayLen, vec![src.clone()], usize_, inner.span);
        let n = self.new_local("<len>", usize_, false, inner.span, LocalKind::Temp);
        stmts.push(let_stmt(n, len, inner.span));
        Some(Item::All(src, et, n))
    }

    /// `for (let i = 0; i < n; i++) target.push(<src[i]>);`: the `n` elements `src` had when
    /// it was evaluated, before the first push, also when it is `target` under another name.
    fn push_indexed(
        &mut self,
        target: hir::Expr,
        src: hir::Expr,
        n: hir::LocalId,
        types: (TyId, TyId),
        span: Span,
    ) -> hir::Stmt {
        let (usize_, bool_, unit) = (self.cx.ty.usize, self.cx.ty.bool_, self.cx.ty.unit);
        self.reject_promise_spread(types.0, span);
        let i = self.new_local("<i>", usize_, true, span, LocalKind::Temp);
        let zero = self.mk(H::Lit(hir::Lit::Int(0)), usize_, span);
        let one = self.mk(H::Lit(hir::Lit::Int(1)), usize_, span);
        let lt = H::Binary {
            op: BinOp::Lt,
            lhs: Box::new(self.mk(H::Local(i, UseMode::Copy), usize_, span)),
            rhs: Box::new(self.mk(H::Local(n, UseMode::Copy), usize_, span)),
        };
        let cond = self.mk(lt, bool_, span);
        let index = H::Index {
            base: Box::new(src),
            index: Box::new(self.mk(H::Local(i, UseMode::Copy), usize_, span)),
            mode: self.elem_read_mode(types.0),
        };
        let read = self.mk(index, types.0, span);
        let value = self.spread_elem_value(read, types);
        let push = self.push_onto(target, value);
        let step = H::CompoundAssign {
            op: BinOp::Add,
            place: Box::new(self.mk(H::Local(i, UseMode::BorrowMut), usize_, span)),
            value: Box::new(one),
        };
        let step = self.mk(step, unit, span);
        let body = hir::Block {
            stmts: vec![push],
            value: None,
            span,
        };
        let stmts = vec![
            let_stmt(i, zero, span),
            hir::Stmt {
                kind: S::While {
                    label: None,
                    cond,
                    body,
                    step: Some(step),
                },
                span,
            },
        ];
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        hir::Stmt {
            kind: S::Block(block),
            span,
        }
    }
}

fn let_stmt(local: hir::LocalId, init: hir::Expr, span: Span) -> hir::Stmt {
    hir::Stmt {
        kind: S::Let {
            local,
            init: Some(init),
        },
        span,
    }
}

/// The array of a spread item (`...ys`), or the item.
fn unspread(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Spread(inner) => inner,
        _ => e,
    }
}
