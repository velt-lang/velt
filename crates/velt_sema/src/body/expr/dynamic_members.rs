//! A dynamic value converted to a union (`FnCx::dynamic_to_members`), and which members it is
//! told apart by: the one interface or object type member with no test of its own, and the one
//! function type member, told by `typeof`. Of several function type members (`((e: Event) =>
//! void) | (() => void)`, std's listeners), a function converts to the one with the most
//! parameters (two with as many are ambiguous, an error): it is called through the `any` it came from, which passes the arguments a
//! function without them leaves alone, as in JavaScript.

use velt_common::{Diagnostic, Span};

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// A dynamic value as the union or literal type `exp` (`members`: its members, or `exp`
    /// itself): the first member whose test the value passes (`typeof` for a primitive,
    /// `instanceof` for a class, `===` for a literal, `Array.isArray` for the one array
    /// member), converted; else a `TypeError`, the member that is `any` / `unknown` itself
    /// (dynamic_whole.rs), or the one interface or object type member, converted with its own
    /// run-time check.
    pub(super) fn dynamic_to_members(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        members: Vec<TyId>,
    ) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let name = self.cx.display(exp);
        let msg = self.str_lit(&format!("TypeError: the value is not a {name}"), span);
        let never = self.cx.ty.never;
        let mut result = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let catch_all = self.dynamic_member(&members, dty);
        if let Some(c) = catch_all {
            result = self.whole_as_member(tmp, dty, c, exp, span);
        }
        // One member with no test of its own (an interface or object type: `VNode` in
        // `VNode | string | number`): a value that passes no other member's test converts to
        // it, checked at run time as `x as VNode` is.
        let rest = match catch_all {
            None => self.untested_member(&members),
            Some(_) => None,
        };
        if let Some(r) = rest {
            let x = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
            let share = self.intrinsic(Intrinsic::Share, vec![x], dty, span);
            self.note_dynamic_iface(r, span);
            let value = self.intrinsic(Intrinsic::AnyTo, vec![share], r, span);
            result = self.coerce(value, exp);
        }
        let skip = |m: &TyId| Some(*m) != catch_all && Some(*m) != rest;
        let arrays = self.array_members(&members);
        for &m in members.iter().rev().filter(|m| skip(m)) {
            // `Array.isArray` tells one array member from the others, not two arrays apart.
            let tested = match self.cx.ty.kind(m) {
                TyKind::Array(_) if arrays > 1 => None,
                TyKind::FnPtr { .. } if self.fn_members(&members) > 1 => {
                    match self.widest_fn_member(&members) == Some(m) {
                        true => self.member_test(tmp, dty, m, span),
                        false => continue,
                    }
                }
                // A promise is never held as `any` (it has one owner): no value is one.
                TyKind::Promise(..) => continue,
                // `typeof K` (a `unique symbol`): a symbol that is `K`.
                _ if self.unique_symbol_of(m).is_some() => {
                    self.unique_member_from_any(tmp, dty, m, span)
                }
                _ => self.member_test(tmp, dty, m, span),
            };
            let Some((test, value)) = tested else {
                if catch_all.is_some() {
                    continue;
                }
                let d = Diagnostic::error(
                    format!(
                        "converting an `any` to `{name}` is not supported yet (#924): no run-time test for `{}`",
                        self.cx.display(m)
                    ),
                    span,
                )
                .with_note("test the value first (`typeof`, `instanceof`) and convert it to that member");
                self.cx.error(d);
                return self.error_expr(span);
            };
            let value = self.coerce(value, exp);
            let kind = H::If {
                cond: Box::new(test),
                then: Box::new(value),
                els: Box::new(result),
            };
            result = self.mk(kind, exp, span);
        }
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            }],
            value: Some(Box::new(result)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }

    /// The function type member of `members` with the most parameters, when no other has as
    /// many (two would be ambiguous: no test tells them apart) and none takes a rest parameter.
    fn widest_fn_member(&self, members: &[TyId]) -> Option<TyId> {
        let mut best: Option<(TyId, usize)> = None;
        let mut tied = false;
        for &m in members {
            match self.cx.ty.kind(m) {
                TyKind::FnPtr { rest: true, .. } => return None,
                TyKind::FnPtr { params, .. } => match best {
                    Some((_, n)) if params.len() < n => {}
                    Some((_, n)) if params.len() == n => tied = true,
                    _ => {
                        best = Some((m, params.len()));
                        tied = false;
                    }
                },
                _ => {}
            }
        }
        best.filter(|_| !tied).map(|b| b.0)
    }

    /// The one member of `members` that [`Self::member_test`] has no test for, when every other
    /// member has one and it is an interface or object type (which `AnyTo` checks). A promise
    /// is never held as `any`, so it needs none.
    pub(super) fn untested_member(&self, members: &[TyId]) -> Option<TyId> {
        let arrays = self.array_members(members);
        let fns = self.fn_members(members);
        let mut untested = members.iter().copied().filter(|&m| {
            let tested = match self.cx.ty.kind(m) {
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Bool | TyKind::Symbol => {
                    true
                }
                TyKind::Array(_) => arrays == 1,
                TyKind::FnPtr { rest: false, .. } => {
                    fns == 1 || self.widest_fn_member(members).is_some()
                }
                TyKind::Promise(..) => true,
                // `typeof K` (a `unique symbol`): `=== K`.
                _ if self.cx.unique_symbols.values().any(|&u| u == m) => true,
                _ => self.cx.lit_value(m).is_some() || self.cx.class_of(m).is_some(),
            };
            !tested
        });
        let m = untested.next()?;
        if untested.next().is_some() {
            return None;
        }
        let checked = matches!(self.cx.ty.kind(m), TyKind::Dyn(..)) || self.cx.is_object_type(m);
        (checked && self.cx.dynamic(m).is_none()).then_some(m)
    }

    /// How many of `members` are arrays.
    pub(super) fn array_members(&self, members: &[TyId]) -> usize {
        let is_array = |m: &&TyId| matches!(self.cx.ty.kind(**m), TyKind::Array(_));
        members.iter().filter(is_array).count()
    }

    /// How many of `members` are function types: `typeof` tells one of them from the others
    /// (`ViewFn | Promise<ViewFn>`), not two apart.
    pub(super) fn fn_members(&self, members: &[TyId]) -> usize {
        let is_fn = |m: &&TyId| matches!(self.cx.ty.kind(**m), TyKind::FnPtr { .. });
        members.iter().filter(is_fn).count()
    }

    /// The test that the dynamic value `x` holds a function (`typeof x === "function"`), and
    /// `owned` (the same value, owned) as the function type `m`, called through `any` as a
    /// conversion to `m` alone makes it (`dynamic_as_fn`).
    pub(super) fn fn_member_test(
        &mut self,
        x: hir::Expr,
        owned: hir::Expr,
        m: TyId,
        span: Span,
    ) -> Option<(hir::Expr, hir::Expr)> {
        let (b, str_) = (self.cx.ty.bool_, self.cx.ty.str_);
        let t = self.intrinsic(Intrinsic::AnyTypeof, vec![x], str_, span);
        let want = self.str_lit("function", span);
        let kind = H::Binary {
            op: hir::BinOp::Eq,
            lhs: Box::new(t),
            rhs: Box::new(want),
        };
        let test = self.mk(kind, b, span);
        Some((test, self.unbox_dynamic(owned, m)))
    }
}
