//! Errors of functions called through `any` (#1938; hir_encodings.md "Member reads and calls").
//! In a program whose calls through `any` throw (`Program::any_calls_throw`: it converts a
//! throwing function to `any`, velt_sema `throws/any_calls.rs`), the `AnyApply` slot and the
//! field and method branches of the `AnyCall` slot call a throwing function with its error
//! caught: boxed as an `any` into `*out`, the slot returns [`ST_THREW`]. At a call through `any`
//! in a user function (or one of the standard library modules that call through `any` on their
//! caller's behalf, `ANY_CALL_MODULES`), sema counted an `any` thrown, and the status sends the
//! error to the innermost `catch` or out of the function, as `throw` does; at any other caller
//! (glue, the rest of the standard library, a typed proxy's call) it ends the process with
//! Node's `Uncaught …` report,
//! as before. In another program the slots run such a function in an `Uncaught` scope, so its
//! error ends the process from there (`finish_ready_call`).

use velt_sema::hir::TyId;

use super::any_member::ST_THREW;
use super::{FnLower, ScopeKind};
use crate::vir::{BinOp, Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// In a slot: the error type of a function it calls that throws `throws`, when the slot
    /// carries that error back (module docs).
    pub(super) fn slot_catches(&self, throws: Option<TyId>) -> Option<TyId> {
        throws.filter(|_| self.cx.hir.any_calls_throw)
    }

    /// In a slot writing its result to the `any` at `out`: `call` (a call that throws `throws`)
    /// with its error caught, boxed into `*out`, and the slot returning [`ST_THREW`]. The owned
    /// values in `borrowed` (what the call borrows) are dropped after it on both ways out. The
    /// call's result, owned.
    pub(super) fn call_catching(
        &mut self,
        throws: TyId,
        out: &Place,
        borrowed: Vec<(Place, TyId)>,
        call: impl FnOnce(&mut Self) -> Operand,
    ) -> Operand {
        self.call_catching_then(throws, out, borrowed, |_| {}, call)
    }

    /// [`call_catching`](Self::call_catching), running `on_error` (dropping what the slot still
    /// owns, its arguments) before the slot returns [`ST_THREW`].
    pub(super) fn call_catching_then(
        &mut self,
        throws: TyId,
        out: &Place,
        borrowed: Vec<(Place, TyId)>,
        on_error: impl FnOnce(&mut Self),
        call: impl FnOnce(&mut Self) -> Operand,
    ) -> Operand {
        let vt = self.cx.ty(throws);
        let caught = self.new_local(vt, Some("threw".into()));
        let handler = self.new_block();
        self.push_scope(ScopeKind::Try {
            handler,
            slot: caught,
            ty: throws,
        });
        self.push_scope(ScopeKind::Block);
        for (p, t) in borrowed {
            self.own_place(p, t);
        }
        let outer = std::mem::replace(&mut self.slot_catch, true);
        let v = call(self);
        self.slot_catch = outer;
        if let Operand::Copy(p) = &v {
            self.take_temp(&p.clone());
        }
        self.pop_scope();
        self.scopes.pop();
        let next = self.new_block();
        self.goto(next);
        self.switch_to(handler);
        let any = self.cx.any_ty();
        self.any_into(Operand::Copy(Place::local(caught)), throws, any, out);
        on_error(self);
        self.ret_status(ST_THREW);
        self.switch_to(next);
        v
    }

    /// After the call slot's status `st` was found not `ST_OK`: for [`ST_THREW`] (only in a
    /// program whose calls through `any` throw), the error in the `any` at `out` thrown where
    /// sema counted it (`routed`: a call through `any`, in a body that routes them), or reported as
    /// uncaught. Continues in the block for another status.
    pub(super) fn any_threw_check(&mut self, st: &Operand, out: &Place, routed: bool) {
        if !self.cx.hir.any_calls_throw || self.dead() {
            return;
        }
        let threw = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st.clone(), super::cint(ST_THREW, Ty::U8)),
        );
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(threw, yes, no);
        self.switch_to(yes);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let err = Operand::Copy(Place::local(
            self.copy_to_temp(Operand::Copy(out.clone()), avt),
        ));
        match routed && self.routes_any_error() {
            true => self.route_error(err, any),
            false => self.uncaught_error(err, any),
        }
        if !self.dead() {
            self.terminate(Terminator::Unreachable);
        }
        self.switch_to(no);
    }

    /// Does an `any` thrown here reach a `catch` that takes it, or leave a function whose
    /// error type holds it (what sema counted for a call through `any`)? `route_error`'s search.
    fn routes_any_error(&mut self) -> bool {
        if !self.any_call_routes {
            return false;
        }
        let any = self.cx.any_ty();
        let scopes = self.scopes.iter().enumerate().rev();
        let mut open = scopes.filter(|(i, _)| !self.left_by_edge(*i));
        let to = open.find_map(|(_, s)| match s.kind {
            ScopeKind::Try { ty, .. } => Some(Some(ty)),
            ScopeKind::Uncaught => Some(None),
            _ => None,
        });
        match to {
            Some(Some(ty)) => self.takes_error(ty, any),
            Some(None) => false,
            None => {
                let throws = self.throws;
                throws.is_some_and(|t| self.takes_error(t, any))
            }
        }
    }
}
