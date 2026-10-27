//! `Atomic<T>` (docs/internals/design/atomics.md): the `Atomic*` intrinsics as `velt_atomic_*`
//! externs (crate::atomic), which backends emit inline, on the address of the word where it
//! is: in a local, an object, an array, or the box of a `shared` value read in place. A float
//! `Atomic` is the word of its bits; its `add`, `sub`, `max` and `min` are compare-and-swap loops
//! (no CPU has a float read-modify-write).

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, ExprKind as K, Intrinsic, Lit, TyId, TyKind};

use super::async_fn::BoxHold;
use super::operand::proj;
use super::{ice, unit, Cx, FnLower};
use crate::atomic::{word, AtomicCall, Order, Rmw};
use crate::vir::{BinOp, Callee, Const, Operand, Place, Proj, Rvalue, Ty, UnOp};

impl FnLower<'_, '_> {
    pub(super) fn atomic_intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr]) -> Operand {
        use Intrinsic as I;
        match (i, args) {
            (I::AtomicFence, [o]) => self.atomic_call(AtomicCall::Fence(order(o)), vec![]),
            (I::AtomicSpinHint, []) => self.atomic_call(AtomicCall::SpinHint, vec![]),
            (I::AtomicLoad, [a, o]) => {
                let (p, vt, bits) = self.atomic_word(a);
                let order = order(o);
                let w = self.atomic_call(AtomicCall::Load { bits, order }, vec![p]);
                from_word(self, w, vt, bits)
            }
            (I::AtomicStore, [a, v, o]) => {
                let (p, vt, bits) = self.atomic_word(a);
                let v = self.word_arg(v, vt, bits);
                let order = order(o);
                self.atomic_call(AtomicCall::Store { bits, order }, vec![p, v])
            }
            (I::AtomicRmw, [a, v, op, o]) if lit(op) != 0 && self.float_atomic(a) => {
                let (p, vt, bits) = self.atomic_word(a);
                let v = self.expr(v);
                self.float_rmw(p, v, vt, bits, lit(op), order(o))
            }
            (I::AtomicRmw, [a, v, op, o]) => {
                let (p, vt, bits) = self.atomic_word(a);
                let v = self.word_arg(v, vt, bits);
                let op = match (lit(op), vt.is_signed()) {
                    (0, _) => Rmw::Xchg,
                    (1, _) => Rmw::Add,
                    (2, _) => Rmw::Sub,
                    (3, _) => Rmw::And,
                    (4, _) => Rmw::Or,
                    (5, _) => Rmw::Xor,
                    (6, true) => Rmw::Max,
                    (7, true) => Rmw::Min,
                    (6, false) => Rmw::UMax,
                    (7, false) => Rmw::UMin,
                    (n, _) => ice(format_args!("AtomicRmw operation {n}")),
                };
                let order = order(o);
                let w = self.atomic_call(AtomicCall::Rmw { op, bits, order }, vec![p, v]);
                from_word(self, w, vt, bits)
            }
            (I::AtomicCas | I::AtomicCasWeak, [a, e, v, o, f]) => {
                let (p, vt, bits) = self.atomic_word(a);
                let e = self.word_arg(e, vt, bits);
                let v = self.word_arg(v, vt, bits);
                let weak = i == I::AtomicCasWeak;
                let call = AtomicCall::Cas {
                    bits,
                    order: order(o),
                    failure: order(f),
                    weak,
                };
                let w = self.atomic_call(call, vec![p, e, v]);
                match weak {
                    true => w,
                    false => from_word(self, w, vt, bits),
                }
            }
            _ => ice(format_args!(
                "intrinsic {i:?} called with {} arguments",
                args.len()
            )),
        }
    }

    /// The call of the extern `call`, inline in every backend.
    pub(in crate::lower) fn atomic_call(
        &mut self,
        call: AtomicCall,
        args: Vec<Operand>,
    ) -> Operand {
        let (params, ret) = call.signature();
        let id = self.cx.extern_sym(&call.symbol(), params, ret, false);
        if ret == Ty::Unit {
            self.call(Callee::Extern(id), args, None, false);
            return unit();
        }
        let out = self.temp(ret);
        self.call(Callee::Extern(id), args, Some(Place::local(out)), false);
        Operand::Copy(Place::local(out))
    }

    /// The address of the word of the `Atomic<T>` receiver `a`, `T`'s VIR type, and its width.
    fn atomic_word(&mut self, a: &hir::Expr) -> (Operand, Ty, u32) {
        let aty = self.sub(a.ty);
        let p = self.atomic_place(a);
        let cell = self.field_place(&p, aty, 0);
        let vt = match self.cx.kind(aty) {
            TyKind::Adt(_, args) if args.len() == 1 => self.cx.ty(args[0]),
            k => ice(format_args!("Atomic intrinsic on {k:?}")),
        };
        let bits = match vt {
            Ty::Bool => 8,
            Ty::F32 => 32,
            Ty::F64 => 64,
            t if t.is_int() => t.scalar_size().unwrap_or(8) * 8,
            t => ice(format_args!("Atomic<T> of VIR type {t:?}")),
        };
        (self.addr(cell), vt, bits)
    }

    /// The place `a` (an `Atomic`, or an object or array holding one) is, without copying it:
    /// a value read out of a `shared` box is the place in the box.
    fn atomic_place(&mut self, a: &hir::Expr) -> Place {
        match &a.kind {
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::SharedRead),
                args,
            } => self.shared_value(&args[0], BoxHold::IfReplaceable).0,
            K::Global(d) if self.slot_of(*d).is_some() => {
                let slot = self.slot_of(*d).unwrap_or_default();
                self.module_slot(slot)
            }
            K::Field { base, index, .. } => {
                let bty = self.sub(base.ty);
                let bp = self.atomic_place(base);
                self.field_place(&bp, bty, *index)
            }
            K::Index { base, index, .. } => {
                let bty = self.sub(base.ty);
                if self.cx.array_view(bty) {
                    ice("an array of atomics converted from `any`");
                }
                let arr = self.atomic_place(base);
                let arr = self.content(&arr, bty);
                let i = self.expr(index);
                let ity = self.vty(index.ty);
                self.elem_place_checked(&arr, bty, i, ity)
            }
            _ => {
                let v = self.expr(a);
                self.place_of(v, a.ty)
            }
        }
    }

    /// The value argument `v` of a `T` atomic, as the unsigned word of `bits`.
    fn word_arg(&mut self, v: &hir::Expr, vt: Ty, bits: u32) -> Operand {
        let v = self.expr(v);
        match vt.is_float() {
            true => self.reinterpret(v, vt, word(bits)),
            false => self.cast_to(v, vt, word(bits)),
        }
    }

    /// Whether the receiver `a` is an `Atomic<f32>` or `Atomic<f64>`.
    fn float_atomic(&mut self, a: &hir::Expr) -> bool {
        let aty = self.sub(a.ty);
        match self.cx.kind(aty) {
            TyKind::Adt(_, args) if args.len() == 1 => self.cx.ty(args[0]).is_float(),
            _ => false,
        }
    }

    /// `v`'s bits as a value of type `to`, of the same width (one a float).
    pub(in crate::lower) fn reinterpret(&mut self, v: Operand, from: Ty, to: Ty) -> Operand {
        let fa = self.cx.new_agg(format!("{from:?} bits"), &[from]);
        let ta = self.cx.new_agg(format!("{to:?} bits"), &[to]);
        let t = self.temp(Ty::Agg(fa));
        self.assign(Place::local(t), Rvalue::Aggregate(fa, vec![v]));
        let p = Place::local(t);
        Operand::Copy(proj(&proj(&p, Proj::Cast(ta)), Proj::Field(0)))
    }

    /// The float read-modify-write `op` (`AtomicRmw`'s 1 add, 2 sub, 6 max, 7 min) of `v` at
    /// `p`, a word of `bits` holding a `vt`: a compare-and-swap loop from a relaxed load, whose
    /// successful swap has the ordering `order` (a failed one is relaxed: it only reads the
    /// value to try again with). The value before.
    fn float_rmw(
        &mut self,
        p: Operand,
        v: Operand,
        vt: Ty,
        bits: u32,
        op: u128,
        order: Order,
    ) -> Operand {
        let w = word(bits);
        let first = self.atomic_call(
            AtomicCall::Load {
                bits,
                order: Order::Relaxed,
            },
            vec![p.clone()],
        );
        let cur = self.temp(w);
        self.assign(Place::local(cur), Rvalue::Use(first));
        let head = self.new_block();
        let done = self.new_block();
        self.goto(head);
        self.switch_to(head);
        let curw = Operand::Copy(Place::local(cur));
        let before = self.reinterpret(curw.clone(), w, vt);
        let before = self.rvalue_temp(vt, Rvalue::Use(before));
        let after = match op {
            1 => self.rvalue_temp(vt, Rvalue::Binary(BinOp::Add, before.clone(), v)),
            2 => self.rvalue_temp(vt, Rvalue::Binary(BinOp::Sub, before.clone(), v)),
            6 => self.float_max_min(before.clone(), v, vt, true),
            7 => self.float_max_min(before.clone(), v, vt, false),
            n => ice(format_args!("float AtomicRmw operation {n}")),
        };
        let afterw = self.reinterpret(after, vt, w);
        let cas = AtomicCall::Cas {
            bits,
            order,
            failure: Order::Relaxed,
            weak: false,
        };
        // Words compared, not floats: a NaN is its own bits, and -0 is not +0.
        let seen = self.atomic_call(cas, vec![p, curw.clone(), afterw]);
        let seen = self.rvalue_temp(w, Rvalue::Use(seen));
        let swapped = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, seen.clone(), curw));
        self.assign(Place::local(cur), Rvalue::Use(seen));
        self.branch(swapped, done, head);
        self.switch_to(done);
        before
    }

    /// `Math.max(a, b)` or `Math.min(a, b)` of the floats `a` and `b` (prelude `max2`, `min2`):
    /// NaN if either is, and +0 the larger of the zeros.
    fn float_max_min(&mut self, a: Operand, b: Operand, vt: Ty, max: bool) -> Operand {
        let out = self.temp(vt);
        let beats = if max { BinOp::Gt } else { BinOp::Lt };
        let take_a = self.new_block();
        let take_b = self.new_block();
        let b_beats = self.new_block();
        let tie = self.new_block();
        let nonzero = self.new_block();
        let zeros = self.new_block();
        let done = self.new_block();
        let c = self.rvalue_temp(Ty::Bool, Rvalue::Binary(beats, a.clone(), b.clone()));
        self.branch(c, take_a, b_beats);
        self.switch_to(b_beats);
        let c = self.rvalue_temp(Ty::Bool, Rvalue::Binary(beats, b.clone(), a.clone()));
        self.branch(c, take_b, tie);
        // Equal or unordered. Equal nonzero values are either; for zeros and NaN, `a + b` is
        // the larger (+0 unless both are -0) and `-(-a + -b)` the smaller.
        self.switch_to(tie);
        let c = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, a.clone(), b.clone()));
        self.branch(c, nonzero, zeros);
        self.switch_to(nonzero);
        let zero = Operand::Const(Const::Float(0.0), vt);
        let c = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, a.clone(), zero));
        self.branch(c, take_a, zeros);
        self.switch_to(zeros);
        let r = match max {
            true => Rvalue::Binary(BinOp::Add, a.clone(), b.clone()),
            false => {
                let na = self.rvalue_temp(vt, Rvalue::Unary(UnOp::Neg, a.clone()));
                let nb = self.rvalue_temp(vt, Rvalue::Unary(UnOp::Neg, b.clone()));
                let s = self.rvalue_temp(vt, Rvalue::Binary(BinOp::Add, na, nb));
                Rvalue::Unary(UnOp::Neg, s)
            }
        };
        self.assign(Place::local(out), r);
        self.goto(done);
        self.switch_to(take_a);
        self.assign(Place::local(out), Rvalue::Use(a));
        self.goto(done);
        self.switch_to(take_b);
        self.assign(Place::local(out), Rvalue::Use(b));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(Place::local(out))
    }
}

impl FnLower<'_, '_> {
    /// `e`, a frozen value or a part of one, is consumed, so it is deep-copied (frozen.rs): an
    /// error when the copy holds an `Atomic`, whose changes would go to the copy and not to the
    /// value other threads read.
    pub(super) fn note_frozen_copy(&mut self, e: &hir::Expr) {
        let t = self.sub(e.ty);
        if self.cx.holds_atomic(t, &mut FxHashSet::default()) {
            let span = self.cx.user_span(e.span);
            self.cx.atomic_copies.push(span);
        }
    }
}

impl Cx<'_> {
    /// Does a deep copy of a value of `t` copy an `Atomic` (in the value, its arrays or the
    /// objects it holds, not behind a `shared`, a function or an interface value)?
    pub(super) fn holds_atomic(&mut self, t: TyId, seen: &mut FxHashSet<TyId>) -> bool {
        if !seen.insert(t) {
            return false;
        }
        match self.kind(t) {
            TyKind::Adt(d, _) if is_atomic(self, d) => true,
            TyKind::Array(e) => self.holds_atomic(e, seen),
            TyKind::Adt(..) | TyKind::Tuple(_) | TyKind::Option(..) | TyKind::Result(..) => {
                let parts = self.part_types(t);
                parts.into_iter().any(|p| self.holds_atomic(p, seen))
            }
            _ => false,
        }
    }

    /// `span`, or the user code being lowered when `span` is in the standard library (a copy
    /// in `slice` is reported at the user's `s.items.slice()`).
    pub(super) fn user_span(&self, span: velt_common::Span) -> velt_common::Span {
        match self.locs.as_ref().is_some_and(|m| m.is_std(span)) {
            true => self.user_site,
            false => span,
        }
    }

    /// Errors for the copies `note_frozen_copy` found.
    pub(super) fn atomic_copy_diagnostics(&self) -> Vec<velt_common::Diagnostic> {
        let mut spans = self.atomic_copies.clone();
        spans.sort_by_key(|s| (s.file, s.lo, s.hi));
        spans.dedup();
        spans
            .into_iter()
            .map(|s| {
                velt_common::Diagnostic::error(
                    "this copies a part of a `shared` value that holds an `Atomic`: changes to the copy's atomics would not reach the shared value",
                    s,
                )
                .with_note("use it in place (`s.items[i].count.add(1)`, `for (const x of s.items)`), or pass it to a function that only reads or calls its methods")
            })
            .collect()
    }
}

/// Is `d` the prelude's `Atomic` (its private field declared by std)?
pub(super) fn is_atomic(cx: &Cx<'_>, d: hir::DefId) -> bool {
    matches!(cx.hir.def(d), hir::Def::Adt(a)
        if a.opaque && a.name.strip_prefix("std/prelude/sync::") == Some("Atomic"))
}

/// A `bits` word as the `T` of VIR type `vt`.
fn from_word(l: &mut FnLower<'_, '_>, w: Operand, vt: Ty, bits: u32) -> Operand {
    match vt {
        Ty::F32 | Ty::F64 => l.reinterpret(w, word(bits), vt),
        Ty::Bool => l.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, w, super::cint(0, word(bits))),
        ),
        _ => l.cast_to(w, word(bits), vt),
    }
}

/// The value of an integer literal argument (sema wrote it).
fn lit(e: &hir::Expr) -> u128 {
    match &e.kind {
        K::Lit(Lit::Int(n)) => *n,
        k => ice(format_args!(
            "Atomic intrinsic argument {k:?} is not a literal"
        )),
    }
}

/// The ordering an `Atomic*` intrinsic's argument numbers.
fn order(e: &hir::Expr) -> Order {
    match lit(e) {
        0 => Order::Relaxed,
        1 => Order::Acquire,
        2 => Order::Release,
        3 => Order::AcqRel,
        4 => Order::SeqCst,
        n => ice(format_args!("Atomic ordering {n}")),
    }
}
