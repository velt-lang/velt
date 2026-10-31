//! Element access on a view type (array_view.rs): an index read or an assignment target,
//! with only the element it reaches brought up to date on a view. A view's own header has
//! length 0, so an index below the header's length is a plain array's element, reached as in
//! any array, and only past it is the view bit tested.

use velt_sema::hir::{self, TyId};

use super::array_view::{ViewSync, VIEW_SYNC_AT};
use super::operand::proj;
use super::FnLower;
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The index operand `i` (of VIR type `ity`) as the `u64` the view helpers take.
    pub(super) fn view_index(&mut self, i: Operand, ity: Ty) -> Operand {
        let iu = self.cast_to(i, ity, Ty::U64);
        self.rvalue_temp(Ty::U64, Rvalue::Use(iu))
    }

    /// The target of the assignment being lowered is element `iu` of the view type `t`'s
    /// array, whose box pointer the last entry of `view_writes` since `mark` holds (when the
    /// barrier made one): only that element is written back.
    pub(super) fn view_write_at(&mut self, mark: usize, t: TyId, iu: Operand) {
        if let Some(w) = &mut self.view_writes {
            if w.len() == mark + 1 && w[mark].1 == t {
                w[mark].2 = Some(iu);
            }
        }
    }

    /// The contents of the array at `arr` of type `t` for a read (or, with `mark`, an
    /// assignment target) of element `index`, whose value `given` may hold already: the index
    /// is evaluated first (as JavaScript does, so `v[i++]` too), then a view brings only that
    /// element up to date. Answers the index's value when it is evaluated here.
    pub(super) fn index_content(
        &mut self,
        arr: &Place,
        t: TyId,
        index: &hir::Expr,
        given: Option<Operand>,
        mark: Option<usize>,
    ) -> (Place, Option<Operand>) {
        if !self.cx.array_view(t) {
            return (self.content(arr, t), given);
        }
        let i = match given {
            Some(i) => i,
            None => self.expr(index),
        };
        let ity = self.vty(index.ty);
        let iu = self.view_index(i.clone(), ity);
        if let Some(mark) = mark {
            self.view_write_at(mark, t, iu.clone());
        }
        let c = self.content_with(arr, t, ViewSync::Slots(vec![iu]));
        (c, Some(i))
    }

    /// The element `index` (whose value `given` may hold already) of the array at `arr` of type
    /// `t`, bounds-checked at `span`, as an assignment target (`mark`: `view_writes_mark`).
    pub(super) fn index_target(
        &mut self,
        arr: &Place,
        t: TyId,
        index: &hir::Expr,
        given: Option<Operand>,
        mark: usize,
        span: velt_common::Span,
    ) -> Place {
        let given = match self.cx.array_view(t) {
            true => Some(given.unwrap_or_else(|| self.expr(index))),
            false => given,
        };
        let prev = self.enter_span(span);
        let p = match self.view_index_elem(arr, t, index, given.clone(), Some(mark)) {
            Some(p) => p,
            None => {
                self.restore_loc(prev);
                let (arr, i) = self.index_content(arr, t, index, given, Some(mark));
                let i = i.unwrap_or_else(|| self.expr(index));
                let ity = self.vty(index.ty);
                self.enter_span(span);
                self.elem_place_checked(&arr, t, i, ity)
            }
        };
        self.restore_loc(prev);
        p
    }

    /// The element `index` (whose value `given` may hold already) of the array at `arr` of the
    /// view type `t`, bounds-checked, for a read (or, with `mark`, an assignment target); `None`
    /// for any other array type.
    /// A view's own header has length 0, so an index below the header's length is a plain
    /// array's element, reached as in any array; only past it is the view bit tested.
    pub(super) fn view_index_elem(
        &mut self,
        arr: &Place,
        t: TyId,
        index: &hir::Expr,
        given: Option<Operand>,
        mark: Option<usize>,
    ) -> Option<Place> {
        if !self.cx.array_view(t) {
            return None;
        }
        let i = match given {
            Some(i) => i,
            None => self.expr(index),
        };
        let ity = self.vty(index.ty);
        let iu = self.view_index(i.clone(), ity);
        if let Some(mark) = mark {
            self.view_write_at(mark, t, iu.clone());
        }
        let e = self.elem_ty(t);
        let payload = self.cx.payload_ty(t);
        let base = self.retained_hop(arr, t);
        let hdr = proj(&base, Proj::Deref(payload));
        let len = Operand::Copy(proj(&hdr, Proj::Field(1)));
        let inb = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, iu.clone(), len));
        let ep = Place::local(self.temp(Ty::Ptr));
        let (fast, slow, done) = (self.new_block(), self.new_block(), self.new_block());
        self.note_view_slow(&hdr, &inb);
        self.branch(inb, fast, slow);
        self.switch_to(fast);
        let p = self.elem_place(&hdr, iu.clone(), e);
        let a = self.addr(p);
        self.assign(ep.clone(), Rvalue::Use(a));
        self.goto(done);
        self.switch_to(slow);
        let m = self.view_inline(hdr, t, ViewSync::Slots(vec![iu]));
        let p = self.elem_place_checked(&m, t, i, ity);
        let a = self.addr(p);
        self.assign(ep.clone(), Rvalue::Use(a));
        self.goto(done);
        self.switch_to(done);
        let vt = self.cx.ty(e);
        Some(proj(&ep, Proj::Deref(vt)))
    }

    /// The element place of step `k` of a `for...of` over the array at `arr` of the view type
    /// `t` (`is`: whether it is a view, tested before the loop), entering `body` with it, or
    /// leaving to `exit` past the end. A plain array's step tests nothing more than any loop's;
    /// a view's (past its own header's length 0) syncs element `k` and reads the mirror.
    pub(super) fn view_step(
        &mut self,
        arr: &Place,
        t: TyId,
        k: Operand,
        is: Operand,
        body: vir::BlockId,
        exit: vir::BlockId,
    ) -> Place {
        let e = self.elem_ty(t);
        let payload = self.cx.payload_ty(t);
        let base = self.retained_hop(arr, t);
        let hdr = proj(&base, Proj::Deref(payload));
        let ep = Place::local(self.temp(Ty::Ptr));
        let len = Operand::Copy(proj(&hdr, Proj::Field(1)));
        let more = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, k.clone(), len));
        let (fast, slow, view, at) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.branch(more, fast, slow);
        self.switch_to(fast);
        let p = self.elem_place(&hdr, k.clone(), e);
        let a = self.addr(p);
        self.assign(ep.clone(), Rvalue::Use(a));
        self.goto(body);
        self.switch_to(slow);
        self.branch(is, view, exit);
        self.switch_to(view);
        let (_, v) = self.array_view_of_hdr(&hdr);
        self.view_call(VIEW_SYNC_AT, e, vec![v.clone(), k.clone()], None);
        let seen = self.view_field(&v, e, "seen");
        let mp = Place::local(self.copy_to_temp(Operand::Copy(seen), Ty::Ptr));
        let m = proj(&mp, Proj::Deref(payload));
        let len = Operand::Copy(proj(&m, Proj::Field(1)));
        let more = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, k.clone(), len));
        self.branch(more, at, exit);
        self.switch_to(at);
        let p = self.elem_place(&m, k, e);
        let a = self.addr(p);
        self.assign(ep.clone(), Rvalue::Use(a));
        self.goto(body);
        self.switch_to(body);
        let vt = self.cx.ty(e);
        proj(&ep, Proj::Deref(vt))
    }

    /// The number of entries in `view_writes` (for [`Self::view_write_at`]).
    pub(super) fn view_writes_mark(&self) -> usize {
        self.view_writes.as_ref().map_or(0, Vec::len)
    }
}
