//! Array intrinsics and the stable sort on a view type (array_view.rs): each syncs the mirror
//! and writes it back only as far as it needs, so a loop of reads, swaps or pushes through a
//! view costs a plain loop plus each element's conversion.
//!
//! | intrinsic | mirror synced | written back |
//! |---|---|---|
//! | `length` | nothing (the original's length) | nothing |
//! | `push`, `reserve` | to the original's length | the pushed elements |
//! | `pop`, `truncate` | to the original's length (and the last element) | the length |
//! | `swap` | the two elements | the two elements |
//! | others (`remove`, `set_len`) | every element | every element |

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::array_view::{ViewFlush, ViewSync, VIEW_LEN, VIEW_SORT};
use super::operand::proj;
use super::{ice, FnLower};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Ty};

/// The stable sort every `sort(cmp)` runs (std/sort/stable.vlt).
const STABLE_SORT: &str = "std/sort/stable::stableSort";

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
        self.assign(
            out.clone(),
            Rvalue::Use(Operand::Copy(proj(&hdr, Proj::Field(1)))),
        );
        let (is, v) = self.array_view_of_hdr(&hdr);
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is, yes, join);
        self.switch_to(yes);
        self.view_call_with(VIEW_LEN, vec![e], vec![v], Some(out.clone()));
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
        let ([e, c], [xs, cmp, neg]) = (targs, argv) else {
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
