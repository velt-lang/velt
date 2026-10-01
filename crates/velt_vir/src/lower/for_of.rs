//! `for…of` over arrays. Elements are visited in order with a counter `k` that is advanced
//! before the body runs, so `continue` jumps straight back to the condition.
//!
//! - Borrowing loop: the binding borrows (or copies) element `k` of the iterated place.
//! - Consuming loop (`ForOf { consume: true }`, an owned temporary array): element `k` is
//!   moved into the (owned) binding. The array is owned by a scope entry
//!   ([`DropEntry::ConsumedArray`]) that drops the elements `k..len` not reached yet — none after
//!   a normal exit, the rest after `break` / `return` / `throw` — and frees the buffer.

use std::rc::Rc;

use velt_sema::hir::{self, Pat, TyId};

use super::operand::proj;
use super::{cint, ice, DropEntry, FnLower, ScopeKind};
use crate::vir::{BinOp, Local, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `for (const <binding> of iter) body`.
    pub(super) fn for_of(
        &mut self,
        label: &Option<String>,
        binding: &Pat,
        iter: &hir::Expr,
        body: &hir::Block,
        consume: bool,
    ) {
        if self.iterates_shared(iter) {
            return self.for_of_shared(label, binding, iter, body, consume);
        }
        self.push_scope(ScopeKind::Temps);
        let aty = self.sub(iter.ty);
        if !matches!(self.cx.kind(aty), hir::TyKind::Array(_)) {
            ice("for…of over a non-array value");
        }
        let elem = self.elem_ty(aty);
        let arr = if consume {
            let v = self.consume(iter);
            let avt = self.cx.ty(aty);
            Place::local(self.copy_to_temp(v, avt))
        } else {
            let v = self.expr(iter);
            self.place_of(v, aty)
        };
        let len = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(&arr, Proj::Field(1)))),
        );
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let owner = consume && !self.dead();
        if owner {
            self.push_scope(ScopeKind::Block);
            let owner = DropEntry::ConsumedArray {
                arr: arr.clone(),
                next: k,
                elem,
            };
            self.scopes
                .last_mut()
                .unwrap_or_else(|| ice("no scope"))
                .drops
                .push(owner);
        }
        let (cond_bb, body_bb, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(cond_bb);
        self.switch_to(cond_bb);
        let kv = Operand::Copy(Place::local(k));
        let more = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, kv.clone(), len));
        self.branch(more, body_bb, exit);
        self.switch_to(body_bb);
        self.push_scope(ScopeKind::Loop {
            label: label.clone(),
            brk: exit,
            cont: cond_bb,
        });
        self.push_scope(ScopeKind::Block);
        let ep = self.elem_place(&arr, kv.clone(), elem);
        let next = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, kv, cint(1, Ty::U64)));
        self.assign(Place::local(k), Rvalue::Use(next));
        if consume {
            // Element `k - 1` now belongs to the binding (what it does not move is dropped
            // with the iteration's scope).
            self.bind_pat(binding, &ep, elem, true);
            self.own_rest(ep, elem, Rc::new(binding.clone()));
        } else {
            self.bind_pat(binding, &ep, elem, false);
        }
        self.block(body);
        self.pop_scope();
        self.goto(cond_bb);
        self.pop_scope();
        self.switch_to(exit);
        if owner {
            self.pop_scope();
        }
        self.pop_scope();
    }

    /// Cleanup of a consumed array: drop elements `next..len`, then free the buffer.
    pub(super) fn drop_consumed(&mut self, arr: &Place, next: Local, elem: TyId) {
        if self.cx.needs_drop(elem) {
            let j = self.temp(Ty::U64);
            self.assign(
                Place::local(j),
                Rvalue::Use(Operand::Copy(Place::local(next))),
            );
            let len = Operand::Copy(proj(arr, Proj::Field(1)));
            self.count_loop(j, len, |lw, j| {
                let p = lw.elem_place(arr, j, elem);
                lw.drop_glue(p, elem);
            });
        }
        self.free_buffer(arr, elem);
    }
}
