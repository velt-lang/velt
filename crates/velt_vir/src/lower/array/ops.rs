//! Array intrinsics: `length`, `push` (amortized growth), `pop`, and the `std/` helpers
//! `__intrinsic_array_with_capacity/swap/remove/truncate/move/set_len`.

use crate::lower::async_fn::BoxHold;
use velt_sema::hir::{self, TyId};

use crate::lower::operand::proj;
use crate::lower::{cint, ice, unit, FnLower, LkMode, Work};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    pub(in crate::lower) fn array_intrinsic(
        &mut self,
        i: hir::Intrinsic,
        args: &[hir::Expr],
        ty: TyId,
    ) -> Operand {
        use hir::Intrinsic as I;
        if i == I::ArrayWithCapacity {
            let cap = self.expr(&args[0]);
            let cty = self.vty(args[0].ty);
            let cap = self.cast_to(cap, cty, Ty::U64);
            return self.with_capacity(cap, ty);
        }
        if i == I::ArrayMove {
            return self.array_move(args);
        }
        let aty = self.sub(args[0].ty);
        if i == I::ArrayLen {
            if let Some(arr) = self.table_read(&args[0]) {
                return self.rvalue_temp(Ty::U64, Rvalue::Use(Self::arr_field(&arr, 1)));
            }
        }
        // An argument that may run code (`aa[0].push(g())`, where `g` grows or replaces `aa`)
        // runs after the array's address is taken: borrow the array as a call borrows its
        // receiver, so that address stays valid.
        if i != I::ArrayLen && !self.cx.lock_counted(aty) {
            // Held inline in an object (caller_stores.rs).
            self.owner_barrier(&args[0]);
        }
        let av = match args[1..].iter().any(|a| self.may_write(a)) {
            true => {
                let outer = self.start_borrows();
                let v = self.stable_borrow(&args[0], &args[1..]);
                self.finish_borrows(outer);
                v
            }
            false => self.projection_base(&args[0], BoxHold::No),
        };
        self.owner_write = false;
        let arr = self.place_of(av, aty);
        let caller = self.mode != LkMode::Plain && self.caller_object(&args[0]);
        if i != I::ArrayLen && self.cx.lock_counted(aty) && !caller {
            // A mutating operation: stops a reader of an `RwLock` changing a shared array (not
            // the caller's, caller_stores.rs `caller_object`).
            self.write_barrier(Operand::Copy(arr.clone()), aty);
        }
        let ptr = Operand::Copy(arr.clone());
        let arr = self.content(&arr, aty);
        if caller && i != I::ArrayLen && self.cx.lock_counted(aty) && !self.captured_root(&args[0])
        {
            // Reached through a call (`mk(out).kids`, `rw.read((s2) => s2.items)`): it may be
            // an array the reader made, not the caller's.
            return self.caller_or_own_op(i, &args[1..], ptr, &arr, aty, ty);
        }
        if caller {
            return self.caller_array_op(i, &args[1..], &arr, aty, ty);
        }
        self.array_op(i, &args[1..], &arr, aty, ty)
    }

    /// An array intrinsic on the caller's array in a `read` callback (caller_stores.rs): it
    /// counts by the counts outside read mode, what it takes out handed to the reader.
    fn caller_array_op(
        &mut self,
        i: hir::Intrinsic,
        rest: &[hir::Expr],
        arr: &Place,
        aty: TyId,
        ty: TyId,
    ) -> Operand {
        let outer = self.mode;
        if let (hir::Intrinsic::ArrayPush, [x]) = (i, rest) {
            let v = self.keep_for_caller(x);
            self.mode = LkMode::Deferred;
            let elem = self.elem_ty(aty);
            self.push_value(arr, elem, v);
            self.mode = outer;
            return unit();
        }
        self.mode = LkMode::Deferred;
        let r = self.array_op(i, rest, arr, aty, ty);
        self.mode = outer;
        let rty = self.sub(ty);
        if self.needs_drop(rty) {
            self.hand_to_reader(&r, rty);
        }
        r
    }

    /// An array intrinsic on the array `ptr` (content `arr`) that a call on the caller's
    /// container returned, in a `read` callback: `caller_array_op` when it is the caller's,
    /// `array_op` by read mode's counts when the reader made it (`LOCAL`: a new object the
    /// call built, a nested `read`'s copy). Arguments other than a pushed value are integers,
    /// lowered in each branch.
    fn caller_or_own_op(
        &mut self,
        i: hir::Intrinsic,
        rest: &[hir::Expr],
        ptr: Operand,
        arr: &Place,
        aty: TyId,
        ty: TyId,
    ) -> Operand {
        let c = self.count_place(ptr);
        let mine = self.is_local(&c);
        let (own, theirs, join) = (self.new_block(), self.new_block(), self.new_block());
        if let (hir::Intrinsic::ArrayPush, [x]) = (i, rest) {
            let held = self.held_by_caller(x);
            let xty = self.sub(x.ty);
            let v = self.consume(x);
            let elem = self.elem_ty(aty);
            let vt = self.cx.ty(elem);
            let v = match vt {
                Ty::Unit => v,
                _ => Operand::Copy(Place::local(self.copy_to_temp(v, vt))),
            };
            self.branch(mine, own, theirs);
            self.switch_to(own);
            self.push_value(arr, elem, v.clone());
            self.goto(join);
            self.switch_to(theirs);
            let k = self.keep_value(v, held, xty);
            let outer = std::mem::replace(&mut self.mode, LkMode::Deferred);
            self.push_value(arr, elem, k);
            self.mode = outer;
            self.goto(join);
            self.switch_to(join);
            return unit();
        }
        let rty = self.sub(ty);
        let rt = self.cx.ty(rty);
        let res = (rt != Ty::Unit).then(|| self.temp(rt));
        self.branch(mine, own, theirs);
        for (bb, reader) in [(own, true), (theirs, false)] {
            self.switch_to(bb);
            let r = match reader {
                true => self.array_op(i, rest, arr, aty, ty),
                false => self.caller_array_op(i, rest, arr, aty, ty),
            };
            if let Some(res) = res {
                if let Operand::Copy(q) = &r {
                    self.take_temp(q);
                }
                self.assign(Place::local(res), Rvalue::Use(r));
            }
            self.goto(join);
        }
        self.switch_to(join);
        match res {
            Some(res) => self.owned_result(Some(res), rty),
            None => unit(),
        }
    }

    fn array_op(
        &mut self,
        i: hir::Intrinsic,
        rest: &[hir::Expr],
        arr: &Place,
        aty: TyId,
        ty: TyId,
    ) -> Operand {
        use hir::Intrinsic as I;
        let arr = arr.clone();
        let elem = self.elem_ty(aty);
        match (i, rest) {
            (I::ArrayLen, []) => self.rvalue_temp(Ty::U64, Rvalue::Use(Self::arr_field(&arr, 1))),
            (I::ArrayPush, [x]) => self.push(&arr, elem, x),
            (I::ArrayPop, []) => self.pop(&arr, elem, ty),
            (I::ArraySwap, [a, b]) => {
                let (ia, ta) = (self.expr(a), self.vty(a.ty));
                let ia = self.freeze(ia, a.ty);
                let (ib, tb) = (self.expr(b), self.vty(b.ty));
                let pa = self.elem_place_checked(&arr, aty, ia, ta);
                let pb = self.elem_place_checked(&arr, aty, ib, tb);
                let vt = self.cx.ty(elem);
                if vt == Ty::Unit {
                    // Zero-sized elements (a single literal type): nothing to exchange.
                    return unit();
                }
                let t = self.copy_to_temp(Operand::Copy(pa.clone()), vt);
                self.assign(pa, Rvalue::Use(Operand::Copy(pb.clone())));
                self.assign(pb, Rvalue::Use(Operand::Copy(Place::local(t))));
                unit()
            }
            (I::ArrayRemove, [idx]) => self.remove(&arr, aty, idx),
            (I::ArrayTruncate, [n]) => {
                let (nv, nt) = (self.expr(n), self.vty(n.ty));
                let nv = self.cast_to(nv, nt, Ty::U64);
                self.truncate(&arr, elem, nv);
                unit()
            }
            (I::ArraySetLen, [n]) => {
                let (nv, nt) = (self.expr(n), self.vty(n.ty));
                let nv = self.cast_to(nv, nt, Ty::U64);
                self.set_len(&arr, elem, nv);
                unit()
            }
            _ => ice(format_args!(
                "intrinsic {i:?} called with {} arguments",
                rest.len() + 1
            )),
        }
    }

    /// `__intrinsic_array_move(dst, d, src, s, n)`: `dst[d..d + n)` = the bits of
    /// `src[s..s + n)`, both ranges bounds-checked; no drop of what `dst` held, no share of
    /// what `src` holds. A literal `n` of 1 is one element copy, anything else one memcpy.
    fn array_move(&mut self, args: &[hir::Expr]) -> Operand {
        let [dst, d, src, s, n] = args else {
            ice(format_args!(
                "ArrayMove called with {} arguments",
                args.len()
            ))
        };
        let (dty, sty) = (self.sub(dst.ty), self.sub(src.ty));
        let elem = self.elem_ty(dty);
        let dv = self.expr(dst);
        let darr = self.place_of(dv, dty);
        if self.cx.lock_counted(dty) && !self.captured_root(dst) {
            self.write_barrier(Operand::Copy(darr.clone()), dty);
        }
        let darr = self.content(&darr, dty);
        let (id, td) = (self.expr(d), self.vty(d.ty));
        let id = self.freeze(id, d.ty);
        let sv = self.expr(src);
        let sarr = self.place_of(sv, sty);
        if self.cx.lock_counted(sty) {
            self.write_barrier(Operand::Copy(sarr.clone()), sty);
        }
        let sarr = self.content(&sarr, sty);
        let (is, ts) = (self.expr(s), self.vty(s.ty));
        let is = self.freeze(is, s.ty);
        let (nv, nt) = (self.expr(n), self.vty(n.ty));
        if matches!(nv, Operand::Const(vir::Const::Int(1), _)) {
            let pd = self.elem_place_checked(&darr, dty, id, td);
            let ps = self.elem_place_checked(&sarr, sty, is, ts);
            self.assign(pd, Rvalue::Use(Operand::Copy(ps)));
            return unit();
        }
        let count = self.cast_to(nv, nt, Ty::U64);
        let count = self.rvalue_temp(Ty::U64, Rvalue::Use(count));
        let id = self.cast_to(id, td, Ty::U64);
        let id = self.rvalue_temp(Ty::U64, Rvalue::Use(id));
        let is = self.cast_to(is, ts, Ty::U64);
        let is = self.rvalue_temp(Ty::U64, Rvalue::Use(is));
        let some = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, count.clone(), cint(0, Ty::U64)),
        );
        let (copy_bb, join) = (self.new_block(), self.new_block());
        self.branch(some, copy_bb, join);
        self.switch_to(copy_bb);
        // The last element of each range is in bounds, so all of it is.
        let back = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, count.clone(), cint(1, Ty::U64)),
        );
        let last_d = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, id.clone(), back.clone()),
        );
        let last_s = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, is.clone(), back));
        self.elem_place_checked(&darr, dty, last_d, Ty::U64);
        self.elem_place_checked(&sarr, sty, last_s, Ty::U64);
        let pd = self.elem_place(&darr, id, elem);
        let ps = self.elem_place(&sarr, is, elem);
        let (stride, _) = self.stride(elem);
        let bytes = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, count, cint(stride as i128, Ty::U64)),
        );
        let (pd, ps) = (self.addr(pd), self.addr(ps));
        self.mem_copy_dyn(pd, ps, bytes, false);
        self.goto(join);
        self.switch_to(join);
        unit()
    }

    /// `len = n` without drops or initialization, growing the buffer (`ArrayGrow`, doubling)
    /// until `n` fits.
    fn set_len(&mut self, arr: &Place, elem: TyId, n: Operand) {
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(n));
        let (check, grow, ok) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(check);
        self.switch_to(check);
        let fits = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Le, n.clone(), Self::arr_field(arr, 2)),
        );
        self.branch(fits, ok, grow);
        self.switch_to(grow);
        let (stride, align) = self.stride(elem);
        let a = self.addr(arr.clone());
        let f = self.cx.func(Work::ArrayGrow);
        let args = vec![
            a,
            cint(stride as i128, Ty::U64),
            cint(align as i128, Ty::U64),
        ];
        self.call(vir::Callee::Func(f), args, None, false);
        self.goto(check);
        self.switch_to(ok);
        self.assign(proj(arr, Proj::Field(1)), Rvalue::Use(n));
    }

    pub(in crate::lower) fn with_capacity(&mut self, cap: Operand, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let elem = self.elem_ty(ty);
        let data = self.temp(Ty::Ptr);
        self.assign(Place::local(data), Rvalue::Use(cint(0, Ty::Ptr)));
        let nz = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, cap.clone(), cint(0, Ty::U64)),
        );
        let alloc_bb = self.new_block();
        let join = self.new_block();
        self.branch(nz, alloc_bb, join);
        self.switch_to(alloc_bb);
        let p = self.alloc_elems(cap.clone(), elem);
        self.assign(Place::local(data), Rvalue::Use(p));
        self.goto(join);
        self.switch_to(join);
        self.array_value(ty, Operand::Copy(Place::local(data)), cint(0, Ty::U64), cap)
    }

    fn push(&mut self, arr: &Place, elem: TyId, x: &hir::Expr) -> Operand {
        let v = self.consume(x);
        self.push_value(arr, elem, v);
        unit()
    }

    /// Append the owned value `v` to the array at `arr` (growing it when full).
    pub(in crate::lower) fn push_value(&mut self, arr: &Place, elem: TyId, v: Operand) {
        let vt = self.cx.ty(elem);
        // Snapshot the value: it may live in the buffer that growing reallocates.
        let v = match v {
            Operand::Copy(p) if !p.proj.is_empty() && vt != Ty::Unit => {
                Operand::Copy(Place::local(self.copy_to_temp(Operand::Copy(p), vt)))
            }
            v => v,
        };
        let len = self.rvalue_temp(Ty::U64, Rvalue::Use(Self::arr_field(arr, 1)));
        let full = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, len.clone(), Self::arr_field(arr, 2)),
        );
        let grow_bb = self.new_block();
        let join = self.new_block();
        self.branch(full, grow_bb, join);
        self.switch_to(grow_bb);
        let (stride, align) = self.stride(elem);
        let a = self.addr(arr.clone());
        let f = self.cx.func(Work::ArrayGrow);
        let args = vec![
            a,
            cint(stride as i128, Ty::U64),
            cint(align as i128, Ty::U64),
        ];
        self.call(vir::Callee::Func(f), args, None, false);
        self.goto(join);
        self.switch_to(join);
        let p = self.elem_place(arr, len.clone(), elem);
        self.store(p, v);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Add, len, cint(1, Ty::U64)));
        self.assign(proj(arr, Proj::Field(1)), Rvalue::Use(n));
    }

    fn pop(&mut self, arr: &Place, elem: TyId, ty: TyId) -> Operand {
        let opt = self.sub(ty);
        let ot = self.cx.ty(opt);
        let res = self.temp(ot);
        let len = self.rvalue_temp(Ty::U64, Rvalue::Use(Self::arr_field(arr, 1)));
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, len.clone(), cint(0, Ty::U64)),
        );
        let (none_bb, some_bb, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, none_bb, some_bb);
        self.switch_to(none_bb);
        let none = self.none_value(opt);
        self.assign(Place::local(res), Rvalue::Use(none));
        self.goto(join);
        self.switch_to(some_bb);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, len, cint(1, Ty::U64)));
        self.assign(proj(arr, Proj::Field(1)), Rvalue::Use(n.clone()));
        let p = self.elem_place(arr, n, elem);
        if self.sub(elem) == opt {
            // Nullable elements: `(T | null)[]`'s pop is `T | null` (TyTable::intern), the
            // element itself.
            self.assign(Place::local(res), Rvalue::Use(Operand::Copy(p)));
        } else {
            self.set_some(Place::local(res), ot, Operand::Copy(p));
        }
        self.goto(join);
        self.switch_to(join);
        self.owned_result(Some(res), opt)
    }

    /// Store `Some(v)` (owned) into the option place `res` of VIR type `ot`.
    pub(in crate::lower) fn set_some(&mut self, res: Place, ot: Ty, v: Operand) {
        match ot {
            Ty::Agg(a) => {
                let ops = vec![super::FnLower::ctrue(), v];
                self.assign(res, Rvalue::Aggregate(a, ops));
            }
            Ty::Bool => self.assign(res, Rvalue::Use(super::FnLower::ctrue())),
            _ => self.assign(res, Rvalue::Use(v)),
        }
    }

    fn remove(&mut self, arr: &Place, aty: TyId, idx: &hir::Expr) -> Operand {
        let elem = self.elem_ty(aty);
        let (iv, it) = (self.expr(idx), self.vty(idx.ty));
        let p = self.elem_place_checked(arr, aty, iv.clone(), it);
        let vt = self.cx.ty(elem);
        let res = self.copy_to_temp(Operand::Copy(p), vt);
        let iu = self.cast_to(iv, it, Ty::U64);
        let iu = self.rvalue_temp(Ty::U64, Rvalue::Use(iu));
        let last = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, Self::arr_field(arr, 1), cint(1, Ty::U64)),
        );
        // Shift the tail left by one element (one memmove).
        let (stride, _) = self.stride(elem);
        let dst = self.elem_place(arr, iu.clone(), elem);
        let next = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, iu.clone(), cint(1, Ty::U64)),
        );
        let src = self.elem_place(arr, next, elem);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, last.clone(), iu));
        let bytes = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, n, cint(stride as i128, Ty::U64)),
        );
        let (d, s) = (self.addr(dst), self.addr(src));
        self.mem_copy_dyn(d, s, bytes, true);
        self.assign(proj(arr, Proj::Field(1)), Rvalue::Use(last));
        let ty = elem;
        self.owned_result(Some(res), ty)
    }

    /// Drop elements `n..len` and set `len = n` (when `n < len`).
    pub(in crate::lower) fn truncate(&mut self, arr: &Place, elem: TyId, n: Operand) {
        let len = self.rvalue_temp(Ty::U64, Rvalue::Use(Self::arr_field(arr, 1)));
        let shorter = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, n.clone(), len.clone()));
        let (cut_bb, join) = (self.new_block(), self.new_block());
        self.branch(shorter, cut_bb, join);
        self.switch_to(cut_bb);
        if self.cx.needs_drop(elem) {
            let k = self.temp(Ty::U64);
            self.assign(Place::local(k), Rvalue::Use(n.clone()));
            self.count_loop(k, len, |lw, k| {
                let p = lw.elem_place(arr, k, elem);
                lw.drop_glue(p, elem);
            });
        }
        self.assign(proj(arr, Proj::Field(1)), Rvalue::Use(n));
        self.goto(join);
        self.switch_to(join);
    }
}
