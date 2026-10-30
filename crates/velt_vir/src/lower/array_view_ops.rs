//! Array intrinsics and the stable sort on a view type (array_view.rs): each syncs the mirror
//! and writes it back only as far as it needs, so a loop of reads, swaps or pushes through a
//! view costs a plain loop plus each element's conversion.
//!
//! | intrinsic | mirror synced | written back |
//! |---|---|---|
//! | `length` | nothing (the original's length) | nothing |
//! | `push`, `reserve` | to the original's length (`push`: after its argument) | the pushed elements |
//! | `pop`, `truncate` | to the original's length (and the last element) | the length |
//! | `swap` | the two elements | the two elements |
//! | others (`remove`, `set_len`) | every element | every element |

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::array_view::{ViewFlush, ViewSync, VIEW_FLUSH_FROM, VIEW_LEN, VIEW_SORT, VIEW_SYNC_LEN};
use super::operand::proj;
use super::{cint, ice, unit, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Ty};

/// The stable sort every `sort(cmp)` runs (std/lang/sort/stable.vlt).
const STABLE_SORT: &str = velt_common::std_item!(lang_sort_stable, "stableSort");

impl FnLower<'_, '_> {
    /// The array intrinsic `i` on the array of the view type `aty` whose box pointer `bxp`
    /// holds, `rest` its other arguments.
    pub(super) fn view_array_op(
        &mut self,
        i: hir::Intrinsic,
        rest: &[hir::Expr],
        bxp: &Place,
        aty: TyId,
        ty: TyId,
    ) -> Operand {
        use hir::Intrinsic as I;
        let bx = Operand::Copy(bxp.clone());
        let len = |arr: &Place| Rvalue::Use(Operand::Copy(proj(arr, Proj::Field(1))));
        match (i, rest) {
            (I::ArrayLen, []) => self.view_len(bxp, aty),
            // Only a hint: a view's elements are not where a plain array's are.
            (I::ArrayPrefetch, [n]) => {
                self.expr(n);
                unit()
            }
            (I::ArrayPush, [x]) => self.view_push(bxp, aty, x),
            (I::ArrayPush | I::ArrayReserve, _) => {
                let arr = self.content_with(bxp, aty, ViewSync::Len);
                let old = self.rvalue_temp(Ty::U64, len(&arr));
                let r = self.array_op(i, rest, &arr, aty, ty);
                if i == I::ArrayPush {
                    self.view_flush(bx, aty, ViewFlush::From(old));
                }
                r
            }
            (I::ArrayPop | I::ArrayTruncate, _) => {
                let sync = match i {
                    I::ArrayPop => ViewSync::Last,
                    _ => ViewSync::Len,
                };
                let arr = self.content_with(bxp, aty, sync);
                let r = self.array_op(i, rest, &arr, aty, ty);
                let now = self.rvalue_temp(Ty::U64, len(&arr));
                self.view_flush(bx, aty, ViewFlush::From(now));
                r
            }
            (I::ArrayRotateRight | I::ArrayRotateLeft, _) => {
                let arr = self.content_with(bxp, aty, ViewSync::All);
                let r = self.array_op(i, rest, &arr, aty, ty);
                self.view_flush(bx, aty, ViewFlush::From(cint(0, Ty::U64)));
                r
            }
            (I::ArraySwap, [a, b]) => {
                let (ia, ta) = (self.expr(a), self.vty(a.ty));
                let ia = self.freeze(ia, a.ty);
                let (ib, tb) = (self.expr(b), self.vty(b.ty));
                let ib = self.freeze(ib, b.ty);
                let ua = self.view_index(ia.clone(), ta);
                let ub = self.view_index(ib.clone(), tb);
                let slots = vec![ua.clone(), ub.clone()];
                let arr = self.content_with(bxp, aty, ViewSync::Slots(slots));
                let r = self.swap_elems(&arr, aty, (ia, ta), (ib, tb));
                self.view_flush(bx, aty, ViewFlush::Slots(vec![ua, ub]));
                r
            }
            _ => {
                let arr = self.content(bxp, aty);
                let r = self.array_op(i, rest, &arr, aty, ty);
                self.view_flush(bx, aty, ViewFlush::All);
                r
            }
        }
    }

    /// `length` of the array of the view type `t` whose box pointer `bxp` holds: a view's is
    /// the original's (`__arrayViewLen`), with nothing converted.
    fn view_len(&mut self, bxp: &Place, t: TyId) -> Operand {
        let e = self.elem_ty(t);
        let payload = self.cx.payload_ty(t);
        let base = self.retained_hop(bxp, t);
        let hdr = proj(&base, Proj::Deref(payload));
        let out = Place::local(self.temp(Ty::U64));
        // The header's length in a temporary of its own, which a loop's copy knows as the
        // header's (view_unswitch_plain.rs); `out` is assigned again for a view.
        let n = Operand::Copy(proj(&hdr, Proj::Field(1)));
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(n));
        self.assign(out.clone(), Rvalue::Use(n.clone()));
        // A view's own header has length 0: a longer array is plain.
        let empty = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, n, cint(0, Ty::U64)));
        let (test, yes, join) = (self.new_block(), self.new_block(), self.new_block());
        self.note_view_zero(&hdr, &empty);
        self.branch(empty, test, join);
        self.switch_to(test);
        let (is, v) = self.array_view_of_hdr(&hdr);
        self.branch(is, yes, join);
        self.switch_to(yes);
        // Through an array header's length field, so the optimizer knows it for an array
        // length (below 2^53) as it knows the plain header's (velt_opt numrep).
        let a = self.cx.array_agg();
        let held = Place::local(self.temp(Ty::Agg(a)));
        let n = proj(&held, Proj::Field(1));
        self.view_call_with(VIEW_LEN, vec![e], vec![v], Some(n.clone()));
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(n)));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// Before the direct call of `def` with `targs` and the lowered arguments `argv`: when it
    /// is the stable sort of an array of a view type, a view is sorted by `__arrayViewSort`
    /// instead (a sorted copy of its mirror becomes the mirror), since the stable sort moves
    /// elements' bits between the array and its buffer. The call then lowers in the block
    /// for other arrays, and goes on to the block answered.
    pub(super) fn view_sort_branch(
        &mut self,
        def: DefId,
        targs: &[TyId],
        argv: &[Operand],
    ) -> Option<vir::BlockId> {
        if self.cx.boxing.array_views.is_empty() {
            return None;
        }
        if !matches!(self.cx.hir.def(def), hir::Def::Fn(f) if f.name == STABLE_SORT) {
            return None;
        }
        // A caller-tracking instance's location may follow (track_caller.rs).
        let ([e, c], [xs, cmp, neg, ..]) = (targs, argv) else {
            ice("the stable sort's signature changed")
        };
        let t = self.cx.intern(TyKind::Array(*e));
        if !self.cx.array_view(t) {
            return None;
        }
        let payload = self.cx.payload_ty(t);
        let pp = self.operand_place(xs.clone(), Ty::Ptr);
        let (is, v) = self.array_view_of_hdr(&proj(&pp, Proj::Deref(payload)));
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        let args = vec![v, cmp.clone(), neg.clone()];
        self.view_call_with(VIEW_SORT, vec![*e, *c], args, None);
        self.goto(join);
        self.switch_to(no);
        Some(join)
    }
}

impl FnLower<'_, '_> {
    /// `push(x)` onto the array of the view type `t` whose box pointer `bxp` holds (after `x`
    /// is evaluated). A view's own header is full (length and capacity 0), so a push onto a
    /// plain array with room tests nothing more than any push; a full one tests the view bit:
    /// a view syncs its mirror's length, pushes onto the mirror and writes the new element back.
    pub(super) fn view_push(&mut self, bxp: &Place, t: TyId, x: &hir::Expr) -> Operand {
        let e = self.elem_ty(t);
        let v = self.consume(x);
        let vt = self.cx.ty(e);
        let v = match vt {
            Ty::Unit => v,
            _ => Operand::Copy(Place::local(self.copy_to_temp(v, vt))),
        };
        let payload = self.cx.payload_ty(t);
        let base = self.retained_hop(bxp, t);
        let hdr = proj(&base, Proj::Deref(payload));
        let done = self.new_block();
        self.push_value_full(&hdr, e, v, |lw, v| {
            let is = lw.view_tagged(&hdr);
            let (yes, no) = (lw.new_block(), lw.new_block());
            lw.branch(is, yes, no);
            lw.switch_to(yes);
            let (_, view) = lw.array_view_of_hdr(&hdr);
            let last = Operand::Const(Const::Bool(false), Ty::Bool);
            lw.view_call(VIEW_SYNC_LEN, e, vec![view.clone(), last], None);
            let seen = lw.view_field(&view, e, "__seen");
            let at = Place::local(lw.copy_to_temp(Operand::Copy(seen), Ty::Ptr));
            let mirror = proj(&at, Proj::Deref(payload));
            let len = Operand::Copy(proj(&mirror, Proj::Field(1)));
            let old = lw.rvalue_temp(Ty::U64, Rvalue::Use(len));
            lw.push_value(&mirror, e, v);
            lw.view_call(VIEW_FLUSH_FROM, e, vec![view, old], None);
            lw.goto(done);
            lw.switch_to(no);
        });
        self.goto(done);
        self.switch_to(done);
        super::unit()
    }
}
