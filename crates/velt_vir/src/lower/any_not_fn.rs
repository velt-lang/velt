//! Calling an `any` that holds no function (`f()` with `f` `undefined`, `o.missing()`): Node
//! throws a `TypeError` (`f is not a function`), which the program can catch (#1422). Typing
//! does not change: the call adds nothing to what its `try` throws. Lowering throws the error,
//! as `throw new TypeError(…)` does, when the innermost `catch` of the same function takes a
//! `TypeError` already (its type is `Error`, `TypeError`, a union with one, or `unknown` /
//! `any`, from what the rest of the `try` body throws). Anywhere else (no such `catch` around
//! the call in its function, a `finally` block, glue, the standard library) it stops the
//! process with the same message: the error never leaves the function.

use velt_common::Span;
use velt_sema::hir::{self, TyId, TyKind};

use super::any_member::ST_CLASS_CALL;
use super::{cint, FnLower, ScopeKind};
use crate::vir::{BinOp, Callee, Operand, Place, Rvalue, Terminator, Ty, STR_AGG};

impl FnLower<'_, '_> {
    /// `TypeError: <text> is not a function`, thrown or stopping (module docs). Ends the block.
    pub(super) fn not_fn_error(&mut self, text: &str) {
        let msg = format!("{text} is not a function");
        if !self.throw_type_error(&msg) {
            self.panic_msg(&format!("TypeError: {msg}"));
            self.terminate(Terminator::Unreachable);
        }
    }

    /// Does a `TypeError` here reach a `catch` of this function (module docs)?
    pub(super) fn routes_type_error(&mut self) -> bool {
        self.type_error_routed().is_some()
    }

    /// `throw new TypeError(msg)` to the `catch` that takes it (module docs), ending the block;
    /// `false`, with nothing emitted, where none does.
    pub(super) fn throw_type_error(&mut self, msg: &str) -> bool {
        let Some((d, te)) = self.type_error_routed() else {
            return false;
        };
        let str_ty = self.cx.intern(TyKind::Str);
        let span = Span::DUMMY;
        let lit = hir::Expr {
            kind: hir::ExprKind::Lit(hir::Lit::Str(msg.to_string())),
            ty: str_ty,
            span,
        };
        let new = hir::Expr {
            kind: hir::ExprKind::New {
                def: d,
                type_args: vec![],
                args: vec![lit],
            },
            ty: te,
            span,
        };
        self.throw(&new);
        true
    }

    /// The prelude's `TypeError` class and type, when the innermost `catch` of this function
    /// can hold one as it is typed. A module state initializer's scope, or none, stops the
    /// search: the error would leave the function, which it never does (module docs). So does
    /// a `catch` that takes nothing: an outer one would skip its body.
    fn type_error_routed(&mut self) -> Option<(hir::DefId, TyId)> {
        if self.dead() || !self.user_body {
            return None;
        }
        // A `finally` block is lowered on each edge out of its `try`, with the scopes of the
        // `try`s it leaves still in place: only those it pushes enclose its code.
        let inside = self.scopes.get(self.finally_floor..).unwrap_or_default();
        let to = inside.iter().rev().find_map(|s| match &s.kind {
            ScopeKind::Try { ty, .. } => Some(Some(*ty)),
            ScopeKind::Uncaught | ScopeKind::CatchesNothing => Some(None),
            _ => None,
        })??;
        let (d, te) = self.type_error_def()?;
        self.takes_error(to, te).then_some((d, te))
    }

    /// The prelude's `TypeError` class and its type (looked up once per program).
    pub(super) fn type_error_def(&mut self) -> Option<(hir::DefId, TyId)> {
        let hir = self.cx.hir;
        let found = *self.cx.prelude_type_error.get_or_insert_with(|| {
            let name = velt_common::std_item!(prelude_error, "TypeError");
            (hir.defs.iter())
                .position(|d| matches!(d, hir::Def::Adt(a) if a.name == name))
                .map(|i| hir::DefId(i as u32))
        });
        let d = found?;
        Some((d, self.cx.intern(TyKind::Adt(d, vec![]))))
    }

    /// Can an error of type `to` hold one of class `from` ([`widen_error`](Self::widen_error)'s
    /// conversions)?
    pub(super) fn takes_error(&mut self, to: TyId, from: TyId) -> bool {
        if to == from || self.cx.is_dynamic(to) {
            return true;
        }
        if let TyKind::Option(payload, _) = self.cx.kind(to) {
            return self.takes_error(payload, from);
        }
        if self.cx.is_union(to) {
            return self.member_variant(to, from).is_some() || self.dynamic_member(to).is_some();
        }
        self.cx.is_class(to) && self.cx.bases_of(from).contains(&to)
    }

    /// After a call through `any` whose status is `st`: `ST_CLASS_CALL` (a class object called
    /// without `new`, `pc(1)`; any_glue.rs) leaves Node's message in the `any` at `out` (of type
    /// `any_ty`). Its `TypeError` (std's `classCallError`) is thrown where a `catch` of this
    /// function takes one, as `not_fn_error`'s; anywhere else the process stops with the message.
    pub(super) fn class_call_check(&mut self, st: &Operand, out: Place, any_ty: TyId) {
        if self.dead() {
            return;
        }
        let is = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st.clone(), cint(ST_CLASS_CALL, Ty::U8)),
        );
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        let hook = self
            .cx
            .std_hook_if_loaded(velt_common::std_hook!(class_call_error));
        match (self.type_error_routed(), hook) {
            (Some((_, te)), Some(h)) => {
                let fid = self.cx.func_for(h, vec![]);
                let unit = self.cx.intern(TyKind::Unit);
                let oa = self.addr(out.clone());
                self.finish_call(Callee::Func(fid), vec![oa], unit, Some(te));
                self.drop_glue(out, any_ty);
                self.terminate(Terminator::Unreachable);
            }
            _ => {
                let str_ty = self.cx.intern(TyKind::Str);
                let msg = self.any_unbox(&out, any_ty, str_ty);
                let m = Place::local(self.copy_to_temp(msg, Ty::Agg(STR_AGG)));
                let ma = self.addr(m);
                self.panic_with_key("", &ma, "");
                self.terminate(Terminator::Unreachable);
            }
        }
        self.switch_to(no);
    }
}
