//! `Atomic<string>` (docs/internals/design/atomics.md "Strings"): a string is three words, so
//! the `Atomic`'s word points to a counted box holding one, the box a `shared<string>` has
//! (`Cx::adt_field_tys` lays the field out as `shared<string>`, so drop glue releases it). Each
//! store boxes the new string; the box taken out of the word is retired (atomic_ref.rs).
//! - `load` gives the string in the box, borrowed: the function runs in an epoch section, and a
//!   consumer that keeps it copies it (`consume`). In an async function or a generator it is
//!   copied at once, inside a section of its own.
//! - `exchange(v)` gives a copy of the string before.
//! - `compareExchange(expected, v)` compares contents (strings are values): it reads the box,
//!   compares its string with `expected`, and swaps the box pointer only if no writer replaced
//!   the box meanwhile; another writer's box makes it read again. It returns whether it made
//!   `v` the value, and drops `v` when it did not.

use velt_sema::hir::{self, ExprKind as K, Intrinsic, TyId, TyKind};

use super::operand::proj;
use super::FnLower;
use crate::atomic::{AtomicCall, Order, Rmw};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty, STR_AGG};

impl FnLower<'_, '_> {
    /// An `Atomic*` intrinsic on an `Atomic<string>`.
    pub(super) fn atomic_str_intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr]) -> Operand {
        use Intrinsic as I;
        let str_ty = self.cx.intern(TyKind::Str);
        let box_ty = self.cx.intern(TyKind::Shared(str_ty));
        let async_body = self.asyncx.is_some() && !self.epoch_exempt;
        let str_vt = self.cx.ty(str_ty);
        match (i, args) {
            (I::AtomicLoad, [a, o]) => {
                let (p, _, bits) = self.atomic_word(a);
                let order = super::atomic::order(o);
                if async_body {
                    self.epoch_call(super::epoch_sections::PIN);
                }
                let w = self.atomic_call(AtomicCall::Load { bits, order }, vec![p]);
                let b = self.cast_to(w, Ty::U64, Ty::Ptr);
                let s = Operand::Copy(self.boxed_str(b));
                if async_body {
                    let copied = self.clone_value(s, str_ty);
                    let tmp = self.copy_to_temp(copied, str_vt);
                    self.own_temp(tmp, str_ty);
                    self.epoch_call(super::epoch_sections::UNPIN);
                    return Operand::Copy(Place::local(tmp));
                }
                self.ref_loads = true;
                // Borrowed, in a temporary that only reads the box's string (never dropped).
                let tmp = self.copy_to_temp(s, str_vt);
                Operand::Copy(Place::local(tmp))
            }
            (I::AtomicStore, [a, v, o]) => {
                let old = self.str_exchange(a, v, super::atomic::order(o));
                self.retire(old, box_ty);
                super::unit()
            }
            (I::AtomicRmw, [a, v, _, o]) => {
                let old = self.str_exchange(a, v, super::atomic::order(o));
                let old = self.rvalue_temp(Ty::Ptr, Rvalue::Use(old));
                let s = Operand::Copy(self.boxed_str(old.clone()));
                let copied = self.clone_value(s, str_ty);
                let tmp = self.copy_to_temp(copied, str_vt);
                self.own_temp(tmp, str_ty);
                self.retire(old, box_ty);
                Operand::Copy(Place::local(tmp))
            }
            (I::AtomicCas | I::AtomicCasWeak, [a, e, v, o, f]) => {
                let weak = i == I::AtomicCasWeak;
                self.str_cas(a, e, v, (o, f), weak, box_ty, async_body)
            }
            _ => super::ice(format_args!(
                "intrinsic {i:?} on an Atomic<string> with {} arguments",
                args.len()
            )),
        }
    }

    /// The place of the string in the box `b` points to.
    fn boxed_str(&mut self, b: Operand) -> Place {
        let str_ty = self.cx.intern(TyKind::Str);
        let bx = self.cx.shared_box(str_ty);
        let bp = self.operand_place(b, Ty::Ptr);
        proj(&proj(&bp, Proj::Deref(Ty::Agg(bx))), Proj::Field(1))
    }

    /// A new box holding the string `v` (moved in); its pointer.
    pub(super) fn box_str(&mut self, v: Operand) -> Operand {
        let str_ty = self.cx.intern(TyKind::Str);
        let bx = self.cx.shared_box(str_ty);
        let p = self.alloc(Ty::Agg(bx));
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(p));
        let bp = proj(
            &self.operand_place(p.clone(), Ty::Ptr),
            Proj::Deref(Ty::Agg(bx)),
        );
        self.assign(
            proj(&bp, Proj::Field(0)),
            Rvalue::Use(super::cint(1, Ty::U64)),
        );
        self.store(proj(&bp, Proj::Field(1)), v);
        p
    }

    /// Box the string `v` and swap it into the word of `a`; the old box, now the caller's.
    fn str_exchange(&mut self, a: &hir::Expr, v: &hir::Expr, order: Order) -> Operand {
        let (p, _, bits) = self.atomic_word(a);
        let v = self.consume(v);
        let b = self.box_str(v);
        let w = self.cast_to(b, Ty::Ptr, Ty::U64);
        let call = AtomicCall::Rmw {
            op: Rmw::Xchg,
            bits,
            order,
        };
        let old = self.atomic_call(call, vec![p, w]);
        self.cast_to(old, Ty::U64, Ty::Ptr)
    }

    /// `compareExchange(expected, v)` on an `Atomic<string>` (module docs): whether it made `v`
    /// the value.
    #[allow(clippy::too_many_arguments)] // the call's parts, the box type and the section
    fn str_cas(
        &mut self,
        a: &hir::Expr,
        e: &hir::Expr,
        v: &hir::Expr,
        (o, f): (&hir::Expr, &hir::Expr),
        weak: bool,
        box_ty: TyId,
        async_body: bool,
    ) -> Operand {
        let (p, _, bits) = self.atomic_word(a);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(p));
        // Only read: sema's share of a place argument is not needed.
        let e = match &e.kind {
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 => &args[0],
            _ => e,
        };
        let expected = self.expr(e);
        let expected_at = self.operand_addr(expected, Ty::Agg(STR_AGG));
        let v = self.consume(v);
        let new_box = self.box_str(v);
        let new_word = self.cast_to(new_box.clone(), Ty::Ptr, Ty::U64);
        let new_word = self.rvalue_temp(Ty::U64, Rvalue::Use(new_word));
        let out = self.temp(Ty::Bool);
        if async_body {
            self.epoch_call(super::epoch_sections::PIN);
        } else {
            self.ref_loads = true;
        }
        let (head, try_swap, swapped, failed, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.goto(head);
        self.switch_to(head);
        // Acquire whatever `failure` says: the box's string is read next.
        let load = AtomicCall::Load {
            bits,
            order: Order::Acquire,
        };
        let cur = self.atomic_call(load, vec![p.clone()]);
        let cur = self.rvalue_temp(Ty::U64, Rvalue::Use(cur));
        let cur_box = self.cast_to(cur.clone(), Ty::U64, Ty::Ptr);
        let held = self.boxed_str(cur_box);
        let held = self.addr(held);
        let same = self.str_eq(held, expected_at.clone());
        self.branch(same, try_swap, failed);
        self.switch_to(try_swap);
        let cas = AtomicCall::Cas {
            bits,
            order: super::atomic::order(o),
            failure: super::atomic::order(f),
            weak,
        };
        let seen = self.atomic_call(cas, vec![p, cur.clone(), new_word]);
        let ok = match weak {
            true => self.rvalue_temp(Ty::Bool, Rvalue::Use(seen)),
            false => self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, seen, cur.clone())),
        };
        // A weak swap may fail spuriously: it fails then. A strong one failed because another
        // writer replaced the box: read it again.
        let retry = if weak { failed } else { head };
        self.branch(ok, swapped, retry);
        self.switch_to(swapped);
        let old = self.cast_to(cur, Ty::U64, Ty::Ptr);
        self.retire(old, box_ty);
        self.assign(
            Place::local(out),
            Rvalue::Use(Operand::Const(crate::vir::Const::Bool(true), Ty::Bool)),
        );
        self.goto(done);
        self.switch_to(failed);
        let tmp = self.copy_to_temp(new_box, Ty::Ptr);
        self.drop_glue(Place::local(tmp), box_ty);
        self.assign(
            Place::local(out),
            Rvalue::Use(Operand::Const(crate::vir::Const::Bool(false), Ty::Bool)),
        );
        self.goto(done);
        self.switch_to(done);
        if async_body {
            self.epoch_call(super::epoch_sections::UNPIN);
        }
        Operand::Copy(Place::local(out))
    }
}
