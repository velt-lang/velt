//! Arrays `T[]` = `{ data: ptr, len: u64, cap: u64 }` with elements at `data + i * size(T)`
//! (aggregate sizes are multiples of their alignment, so size is the stride). An empty array
//! has a null buffer and `cap == 0`; buffers come from `velt_rt_alloc` and grow by amortized
//! doubling (the shared `ArrayGrow` helper, starting at 4). Indexing is bounds-checked
//! (the `Oob` helper panics with `index out of bounds: the len is L but the index is I`).

mod builtins;
mod ops;

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, unit, FnLower, Work};
use crate::vir::{self, BinOp, Local, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Element type of the concrete array type `arr`.
    pub(super) fn elem_ty(&mut self, arr: TyId) -> TyId {
        match self.cx.kind(arr) {
            TyKind::Array(e) => e,
            k => ice(format_args!("array operation on {k:?}")),
        }
    }

    /// (size = stride, align) of an element type.
    pub(super) fn stride(&mut self, elem: TyId) -> (u32, u32) {
        let vt = self.cx.ty(elem);
        self.cx.size_align(vt)
    }

    fn arr_field(arr: &Place, n: u32) -> Operand {
        Operand::Copy(proj(arr, Proj::Field(n)))
    }

    /// Place of element `i` (a `u64` operand) of the array at `arr`; no bounds check.
    pub(super) fn elem_place(&mut self, arr: &Place, i: Operand, elem: TyId) -> Place {
        let (stride, _) = self.stride(elem);
        let off = match i {
            Operand::Const(vir::Const::Int(v), _) => cint(v * stride as i128, Ty::U64),
            i => self.rvalue_temp(
                Ty::U64,
                Rvalue::Binary(BinOp::Mul, i, cint(stride as i128, Ty::U64)),
            ),
        };
        let data = Self::arr_field(arr, 0);
        let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, data, off));
        let vt = self.cx.ty(elem);
        let Operand::Copy(pp) = ptr else {
            ice("pointer temp")
        };
        proj(&pp, Proj::Deref(vt))
    }

    /// Bounds-checked element place; `i` has VIR int type `ity`.
    pub(super) fn elem_place_checked(
        &mut self,
        arr: &Place,
        arr_ty: TyId,
        i: Operand,
        ity: Ty,
    ) -> Place {
        let elem = self.elem_ty(arr_ty);
        let iu = self.cast_to(i.clone(), ity, Ty::U64);
        let iu = self.rvalue_temp(Ty::U64, Rvalue::Use(iu));
        let len = self.rvalue_temp(Ty::U64, Rvalue::Use(Self::arr_field(arr, 1)));
        let ok = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, iu.clone(), len.clone()));
        let ok_bb = self.new_block();
        let oob_bb = self.new_block();
        self.branch(ok, ok_bb, oob_bb);
        self.switch_to(oob_bb);
        let signed = ity.is_signed();
        let shown = if signed {
            self.cast_to(i, ity, Ty::I64)
        } else {
            iu.clone()
        };
        let f = self.cx.func(Work::Oob(signed));
        let suffix = self.panic_suffix();
        let at = self.cx.static_str_object(&suffix);
        let at = vir::Operand::Const(vir::Const::Static(at), Ty::Ptr);
        self.call(vir::Callee::Func(f), vec![len, shown, at], None, false);
        self.terminate(vir::Terminator::Unreachable);
        self.switch_to(ok_bb);
        self.elem_place(arr, iu, elem)
    }

    /// A fresh heap buffer for `n` (operand, u64) elements.
    fn alloc_elems(&mut self, n: Operand, elem: TyId) -> Operand {
        let (stride, align) = self.stride(elem);
        let size = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, n, cint(stride as i128, Ty::U64)),
        );
        let p = self.temp(Ty::Ptr);
        self.call_rt(
            Rt::Alloc,
            vec![size, cint(align as i128, Ty::U64)],
            Some(Place::local(p)),
        );
        Operand::Copy(Place::local(p))
    }

    /// Array value `{ data, len, cap }` registered as an owned temp of concrete type `ty`.
    fn array_value(&mut self, ty: TyId, data: Operand, len: Operand, cap: Operand) -> Operand {
        let a = self.cx.array_agg();
        let t = self.temp(Ty::Agg(a));
        self.assign(Place::local(t), Rvalue::Aggregate(a, vec![data, len, cap]));
        self.own_array(t, ty)
    }

    /// The inline array header in temp `t` as an owned value of type `ty` (boxed when `ty` is),
    /// registered for dropping.
    pub(super) fn own_array(&mut self, t: Local, ty: TyId) -> Operand {
        if self.cx.boxed(ty) {
            let v = self.box_value(Operand::Copy(Place::local(t)), ty);
            return self.own_value(v, ty);
        }
        self.own_temp(t, ty);
        Operand::Copy(Place::local(t))
    }

    /// An inline array header (unregistered temp) with room for and length `n` (elements
    /// unset) of element type `elem`.
    pub(super) fn inline_array_with_len(&mut self, n: Operand, elem: TyId) -> Local {
        let data = self.alloc_elems(n.clone(), elem);
        let a = self.cx.array_agg();
        let t = self.temp(Ty::Agg(a));
        self.assign(
            Place::local(t),
            Rvalue::Aggregate(a, vec![data, n.clone(), n]),
        );
        t
    }

    pub(super) fn array_lit(&mut self, es: &[hir::Expr], ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let elem = self.elem_ty(ty);
        let vals = self.consume_each(es);
        if self.dead() {
            return unit();
        }
        let n = cint(es.len() as i128, Ty::U64);
        if es.is_empty() {
            return self.array_value(ty, cint(0, Ty::Ptr), n.clone(), n);
        }
        let data = self.alloc_elems(n.clone(), elem);
        let a = self.cx.array_agg();
        let tmp = self.temp(Ty::Agg(a));
        self.assign(
            Place::local(tmp),
            Rvalue::Aggregate(a, vec![data.clone(), n.clone(), n.clone()]),
        );
        for (i, v) in vals.into_iter().enumerate() {
            let p = self.elem_place(&Place::local(tmp), cint(i as i128, Ty::U64), elem);
            self.store(p, v);
        }
        self.own_array(tmp, ty)
    }

    /// Free the buffer of the array at `arr` (if it has one); elements are not dropped.
    pub(super) fn free_buffer(&mut self, arr: &Place, elem: TyId) {
        let (stride, align) = self.stride(elem);
        let cap = Self::arr_field(arr, 2);
        let has = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, cap.clone(), cint(0, Ty::U64)),
        );
        let (free_bb, join) = (self.new_block(), self.new_block());
        self.branch(has, free_bb, join);
        self.switch_to(free_bb);
        let size = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, cap, cint(stride as i128, Ty::U64)),
        );
        let args = vec![Self::arr_field(arr, 0), size, cint(align as i128, Ty::U64)];
        self.call_rt(Rt::Free, args, None);
        self.goto(join);
        self.switch_to(join);
    }

    /// Copy the `n` elements starting at index `from` of array `src` into the elements `0..n`
    /// of array `dst` (uninitialized): shares when `share`, else deep copies; one `MemCopyDyn`
    /// when elements own nothing.
    pub(super) fn copy_elems(
        &mut self,
        src: &Place,
        from: Operand,
        dst: &Place,
        n: Operand,
        elem: TyId,
        share: bool,
    ) {
        if !self.cx.needs_drop(elem) {
            let (stride, _) = self.stride(elem);
            let sp = self.elem_place(src, from, elem);
            let dp = self.elem_place(dst, cint(0, Ty::U64), elem);
            let bytes = self.rvalue_temp(
                Ty::U64,
                Rvalue::Binary(BinOp::Mul, n, cint(stride as i128, Ty::U64)),
            );
            let (d, s) = (self.addr(dp), self.addr(sp));
            self.mem_copy_dyn(d, s, bytes, false);
            return;
        }
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, n, |lw, k| {
            let i = lw.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, k.clone(), from));
            let sp = lw.elem_place(src, i, elem);
            let dp = lw.elem_place(dst, k, elem);
            match share {
                true => lw.share_into(sp, dp, elem),
                false => lw.clone_into(sp, dp, elem),
            }
        });
    }

    /// `while (k < end) { body(k); k += 1 }` over a `u64` counter local.
    pub(super) fn count_loop(
        &mut self,
        k: Local,
        end: Operand,
        body: impl FnOnce(&mut Self, Operand),
    ) {
        let (cond_bb, body_bb, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(cond_bb);
        self.switch_to(cond_bb);
        let kv = Operand::Copy(Place::local(k));
        let more = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, kv.clone(), end));
        self.branch(more, body_bb, exit);
        self.switch_to(body_bb);
        body(self, kv.clone());
        let next = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, kv, cint(1, Ty::U64)));
        self.assign(Place::local(k), Rvalue::Use(next));
        self.goto(cond_bb);
        self.switch_to(exit);
    }
}
