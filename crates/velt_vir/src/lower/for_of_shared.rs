//! `for…of` over an array other references may reach (semantics stage 2: a boxed array, one
//! reached through a counted object, or one in a variable held in a shared cell, which a closure
//! the body calls may reassign). The body may change the array through another reference,
//! so the loop works like JS's array iterator: it keeps its own reference to the array, re-reads
//! the length every iteration and shares each element into the binding instead of pointing
//! into the buffer (which `push` may move). A consuming loop over such an array cannot move the
//! elements out (others may see them), so it binds shares as well.

use std::rc::Rc;

use velt_sema::hir::{self, Pat};

use super::operand::proj;
use super::{cint, FnLower, ScopeKind};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Does `for…of` over `iter` need the shared-array loop?
    pub(super) fn iterates_shared(&mut self, iter: &hir::Expr) -> bool {
        let aty = self.sub(iter.ty);
        self.cx.boxed(aty) || self.through_counted(iter, aty) || self.in_shared_cell(iter)
    }

    /// The loop of the module docs (`consume`: the binding is owned, as in a consuming loop).
    pub(super) fn for_of_shared(
        &mut self,
        label: &Option<String>,
        binding: &Pat,
        iter: &hir::Expr,
        body: &hir::Block,
        consume: bool,
    ) {
        self.push_scope(ScopeKind::Temps);
        let aty = self.sub(iter.ty);
        let elem = self.elem_ty(aty);
        let v = if consume {
            self.consume(iter)
        } else {
            let v = self.expr(iter);
            self.share_value(v, aty)
        };
        let own = self.own_value(v, aty);
        let avt = self.cx.ty(aty);
        let arr = self.operand_place(own, avt);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        // Whether the array is a view, tested once: the loop holds a reference to it.
        let view = self.cx.array_view(aty).then(|| {
            let payload = self.cx.payload_ty(aty);
            let hdr = proj(&arr, Proj::Deref(payload));
            let is = self.view_tagged(&hdr);
            let is = Operand::Copy(Place::local(self.copy_to_temp(is, Ty::Bool)));
            self.note_view_test(&hdr, &is);
            is
        });
        let first = self.loop_start();
        let (cond_bb, body_bb, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(cond_bb);
        self.switch_to(cond_bb);
        let kv = Operand::Copy(Place::local(k));
        // A view brings only the element this step reads up to date (array_view_index.rs).
        let ep = match view {
            Some(is) => self.view_step(&arr, aty, kv.clone(), is, body_bb, exit),
            None => {
                let hdr = self.content(&arr, aty);
                let len = Operand::Copy(proj(&hdr, Proj::Field(1)));
                let more = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, kv.clone(), len));
                self.branch(more, body_bb, exit);
                self.switch_to(body_bb);
                self.elem_place(&hdr, kv.clone(), elem)
            }
        };
        self.push_scope(ScopeKind::Loop {
            label: label.clone(),
            brk: exit,
            cont: cond_bb,
        });
        self.push_scope(ScopeKind::Block);
        let next = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, kv, cint(1, Ty::U64)));
        self.assign(Place::local(k), Rvalue::Use(next));
        let s = self.share_value(Operand::Copy(ep), elem);
        let evt = self.cx.ty(elem);
        // An element of a zero-sized type (a single literal type, `"a"[]`) has no value to
        // copy: the binding reads a placeholder byte.
        let e = if evt == Ty::Unit {
            Place::local(self.temp(Ty::U8))
        } else {
            Place::local(self.copy_to_temp(s, evt))
        };
        if consume {
            self.bind_pat(binding, &e, elem, true);
            self.own_rest(e, elem, Rc::new(binding.clone()));
        } else {
            self.own_place(e.clone(), elem);
            self.bind_pat(binding, &e, elem, false);
        }
        self.block(body);
        self.pop_scope();
        self.goto(cond_bb);
        self.pop_scope();
        self.note_loop(cond_bb, first);
        self.switch_to(exit);
        self.pop_scope();
    }
}
