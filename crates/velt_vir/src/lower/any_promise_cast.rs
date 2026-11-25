//! `v as Promise<T>` of an `any` / `unknown` holding a promise of another type (#924 step 4; the
//! plan on the issue). TypeScript lets the assertion through, and JavaScript hands on whatever
//! the promise settles with. Velt checks the promise at the cast and its result where it enters
//! typed code, when it settles:
//!
//! - **At the cast** (`any_to_promise`, in `AnyTo`'s slow path): a value that holds no promise
//!   is the `TypeError` of any other conversion. A typed `Promise<T>` of the cast's `T` (it
//!   doesn't reject; the cast's type may) is taken out of its box and widened, one wrapper and
//!   no conversion: the commonest cast. A promise of another type is awaited through its
//!   table's `SLOT_AWAIT`, which gives a `Promise<unknown, unknown>` around a share of it
//!   (any_await.rs), and that is put in a converting wrapper of the target type: step 1's
//!   widening wrapper (`async_fn/widen.rs`) with `narrow_slot` as its conversion. Each wrapper
//!   polls the promise it holds itself, so awaiting the cast takes no turn more than awaiting
//!   the held promise, and each is an alias of it (`velt_rt_fut_alias`), so the cast is `===`
//!   to the held promise, as in JavaScript (any_eq.rs compares two promise types by the
//!   promise each stands for).
//! - **When it settles** (`narrow_slot`): the value is converted to `T` as `v as T` converts it.
//!   A value whose `typeof` `T` can never hold (a string for a `number`) rejects the promise
//!   with `TypeError: the value is not a number (it is a string)`, where JavaScript would hand
//!   the string on: the `await` throws it, and a `then` callback doesn't run. A value of the
//!   right kind that still doesn't convert (an object of another shape) stops the program as
//!   `v as T` does. A rejection passes on, its reason converted the same way to the cast's
//!   error type: `unknown` for a written `Promise<T>` (velt_sema `any_promise_cast.rs`), or a
//!   written `Promise<T, E>`'s `E`.

use velt_sema::hir::{self, TyId, TyKind};

use super::any::{FOUND, TAG_BOOLEAN, TAG_FUNCTION, TAG_NULL, TAG_NUMBER, TAG_OBJECT};
use super::any::{TAG_STRING, TAG_SYMBOL, TAG_UNDEFINED};
use super::any_await::{PROMISE_ID, SLOT_AWAIT};
use super::any_errors::article_of;
use super::operand::proj;
use super::{cint, FnLower, ScopeKind};
use crate::vir::{BinOp, BlockId, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// In `AnyTo` the promise type `ty`, once the `any` at `p` (owned, of type `from`) holds
    /// no value of the target's own table: a promise of another type, converted (module docs),
    /// moved into `out`, then on to `done`. Anything else goes on in the current block.
    pub(super) fn any_to_promise(
        &mut self,
        p: &Place,
        from: TyId,
        ty: TyId,
        out: &Place,
        done: BlockId,
    ) {
        let TyKind::Promise(t, _) = self.cx.kind(ty) else {
            return;
        };
        if !self.cx.executor_loaded() {
            return;
        }
        // A `Promise<void>` that can't reject (a field's or parameter's type written so, not a
        // cast's: that one rejects with `unknown`) has no result to convert into, and the
        // wrapper no slot for it: only its own table converts, as before.
        let slot = self.cx.promise_slot(ty);
        if self.cx.ty(slot) == Ty::Unit {
            return;
        }
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        // The commonest held type, a promise of the same value type that doesn't reject (a
        // typed `Promise<T>`): taken out of its box and widened, one wrapper with no conversion.
        let never = self.cx.intern(TyKind::Never);
        let plain = self.cx.intern(TyKind::Promise(t, never));
        if plain != ty {
            let imp = self.cx.hir.any_impl.unwrap_or(0);
            let table = self.vtable_addr(super::VtableKey::Impl(imp, plain));
            let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), table));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(same, yes, no);
            self.switch_to(yes);
            let data = Operand::Copy(proj(p, Proj::Field(0)));
            let fut = self.dyn_box_take(data, plain);
            let w = self.widen_fut(fut, plain, ty, false);
            self.move_into(w, out);
            self.goto(done);
            self.switch_to(no);
        }
        let id = self.vtable_class_id(desc.clone());
        let want = cint(PROMISE_ID as i128, Ty::U64);
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, want));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        let fut = self.temp(Ty::Ptr);
        let fa = self.addr(Place::local(fut));
        let data = Operand::Copy(proj(p, Proj::Field(0)));
        let _ = self.any_slot_call(desc, SLOT_AWAIT, vec![data, fa], Ty::U8);
        // The slot's promise holds a share of the held one: the `any` is released.
        self.drop_glue(p.clone(), from);
        let held = self.cx.awaited_any_promise();
        let w = self.widen_fut(Operand::Copy(Place::local(fut)), held, ty, false);
        self.move_into(w, out);
        self.goto(done);
        self.switch_to(no);
    }

    /// `await (v as Promise<T>)` of a `v` of the dynamic type `from` (`inner` is the cast, its
    /// type `ty`): a typed `Promise<T>` that `v` holds (the commonest) is awaited itself, as
    /// before the cast converted, with no wrapper; anything else is cast (`any_to_promise`) and
    /// that awaited. Both take the same turns: a wrapper polls the promise it holds itself.
    /// `None` when the cast is no such cast, or the cast's type is the held one's.
    pub(in crate::lower) fn await_any_promise_cast(
        &mut self,
        inner: &hir::Expr,
    ) -> Option<Operand> {
        let hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::AnyTo),
            args,
        } = &inner.kind
        else {
            return None;
        };
        let [a] = args.as_slice() else {
            return None;
        };
        let (from, ty) = (self.sub(a.ty), self.sub(inner.ty));
        let TyKind::Promise(t, _) = self.cx.kind(ty) else {
            return None;
        };
        let never = self.cx.intern(TyKind::Never);
        let plain = self.cx.intern(TyKind::Promise(t, never));
        if plain == ty
            || !self.cx.is_dynamic(from)
            || self.cx.mentions_ph(ty)
            || !self.cx.executor_loaded()
            || matches!(a.kind, hir::ExprKind::ToDyn { .. })
        {
            return None;
        }
        let imp = self.cx.hir.any_impl?;
        // An `any` read where it stays (`for (const p of held)`): its promise is shared out of
        // the box it stays in, with no copy of the box (any_borrowed.rs).
        let borrowed = match &a.kind {
            hir::ExprKind::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::Share),
                args,
            } => match args.as_slice() {
                [x] => {
                    let xt = self.sub(x.ty);
                    (self.cx.is_dynamic(xt) && !self.frozen_root(x)).then_some(x)
                }
                _ => None,
            },
            _ => None,
        };
        let v = match borrowed {
            Some(x) => self.expr(x),
            None => self.consume(a),
        };
        let vt = self.cx.ty(t);
        if self.dead() {
            return Some(self.zero_value(vt));
        }
        let fvt = self.cx.ty(from);
        let p = match borrowed {
            Some(_) => self.place_of(v, from),
            None => Place::local(self.copy_to_temp(v, fvt)),
        };
        let out = (vt != Ty::Unit).then(|| Place::local(self.temp(vt)));
        let desc = Operand::Copy(proj(&p, Proj::Field(1)));
        let table = self.vtable_addr(super::VtableKey::Impl(imp, plain));
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, table));
        let (held, cast, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(same, held, cast);
        self.switch_to(held);
        let data = Operand::Copy(proj(&p, Proj::Field(0)));
        let fut = match borrowed {
            Some(_) => {
                let pt = self.cx.ty(plain);
                let dp = self.operand_place(data, Ty::Ptr);
                self.share_value(Operand::Copy(proj(&dp, Proj::Deref(pt))), plain)
            }
            None => self.dyn_box_take(data, plain),
        };
        let r = self.await_heap(fut, plain);
        self.settled_into(r, out.as_ref(), done);
        self.switch_to(cast);
        let p = match borrowed {
            Some(_) => {
                let copy = self.share_value(Operand::Copy(p), from);
                Place::local(self.copy_to_temp(copy, fvt))
            }
            None => p,
        };
        let fut = self.any_unbox(&p, from, ty);
        if !self.dead() {
            let r = self.await_heap(fut, ty);
            self.settled_into(r, out.as_ref(), done);
        }
        self.switch_to(done);
        Some(match out {
            Some(out) => self.own_value(Operand::Copy(out), t),
            None => super::unit(),
        })
    }

    /// The owned awaited result `r` moved into `out` (none for `void`), then on to `done`.
    fn settled_into(&mut self, r: Operand, out: Option<&Place>, done: BlockId) {
        if self.dead() {
            return;
        }
        if let Some(out) = out {
            if let Operand::Copy(q) = &r {
                self.take_temp(q);
            }
            self.assign(out.clone(), Rvalue::Use(r));
        }
        self.goto(done);
    }

    /// Do the `any` descriptors `xa` and `xb` both belong to promise types (Bool operand)?
    pub(super) fn any_both_promises(&mut self, xa: &Operand, xb: &Operand) -> Operand {
        let want = cint(PROMISE_ID as i128, Ty::U64);
        let ia = self.vtable_class_id(xa.clone());
        let ib = self.vtable_class_id(xb.clone());
        let a = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ia, want.clone()));
        let b = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ib, want));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, a, b))
    }

    /// `===` of the promises held by two `any`s (their data pointers `da`, `db`, each the box
    /// of a promise): one promise seen through two types, as in JavaScript (module docs).
    pub(super) fn any_promises_same(&mut self, da: &Operand, db: &Operand) -> Operand {
        let pa = self.operand_place(da.clone(), Ty::Ptr);
        let pb = self.operand_place(db.clone(), Ty::Ptr);
        let (fa, fb) = (
            proj(&pa, Proj::Deref(Ty::Ptr)),
            proj(&pb, Proj::Deref(Ty::Ptr)),
        );
        self.promise_identity(&fa, &fb)
    }

    /// Does the wrapper from the promise type `from` to `to` convert a promise awaited through
    /// `any` (`Promise<unknown, unknown>`) to a cast's type (module docs), not widen?
    pub(in crate::lower) fn narrows(&mut self, from: TyId, to: TyId) -> bool {
        // Every widening asks: a program without `any` has no `Promise<unknown, unknown>`.
        let TyKind::Promise(t1, _) = self.cx.kind(from) else {
            return false;
        };
        if from == to || !self.cx.is_dynamic(t1) || from != self.cx.awaited_any_promise() {
            return false;
        }
        let TyKind::Promise(t, _) = self.cx.kind(to) else {
            return false;
        };
        let e2 = self.cx.promise_error(to);
        !self.cx.is_dynamic(t) || e2.is_none_or(|e| !self.cx.is_dynamic(e))
    }

    /// The settled result at `*src` of a promise awaited through `any` (`from`,
    /// `Promise<unknown, unknown>`) moved into `out`, the result slot of the cast's promise
    /// type `to`, converted (module docs).
    pub(in crate::lower) fn narrow_slot(&mut self, src: &Place, out: &Place, from: TyId, to: TyId) {
        let TyKind::Promise(t, _) = self.cx.kind(to) else {
            super::ice("a promise cast to a type that is no promise")
        };
        let from_slot = self.cx.promise_slot(from);
        let TyKind::Result(unknown, _) = self.cx.kind(from_slot) else {
            super::ice("a promise awaited through `any` that can't reject")
        };
        let fv = self.cx.ty(from_slot);
        let inner = proj(src, Proj::Deref(fv));
        // Converting makes temporaries: the poll function has no scope of its own.
        let scoped = self.scopes.is_empty();
        if scoped {
            self.push_scope(ScopeKind::Temps);
        }
        let tag = Operand::Copy(proj(&inner, Proj::Field(0)));
        let is_err = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        let (err_bb, ok_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_err, err_bb, ok_bb);
        self.switch_to(ok_bb);
        let ok = self.cx.view(from_slot, 0);
        let v = Operand::Copy(proj(&proj(&inner, Proj::Cast(ok)), Proj::Field(1)));
        self.narrow_settled(v, unknown, (t, false), to, out, done);
        self.switch_to(err_bb);
        let err = self.cx.view(from_slot, 1);
        let e = Operand::Copy(proj(&proj(&inner, Proj::Cast(err)), Proj::Field(1)));
        match self.cx.promise_error(to) {
            Some(e2) => self.narrow_settled(e, unknown, (e2, true), to, out, done),
            None => {
                // A type that can't reject (`Promise<T, never>` written so, or a field's or
                // parameter's `Promise<T>` that nothing rejecting is stored into) can't hold it.
                let name = self.any_target_name(to);
                self.panic_msg(&format!(
                    "TypeError: a promise held as `any` and used as a {name} rejected, which that type does not allow"
                ));
                self.terminate(Terminator::Unreachable);
            }
        }
        self.switch_to(done);
        if scoped {
            self.pop_scope();
        }
    }

    /// The settled value or reason `v` (an owned `unknown` of type `unknown`) converted to
    /// `want`, written to the result slot `out` of the promise type `to` as its value (`is_err`
    /// false) or its reason, then on to `done`. A value of a kind `want` can never hold rejects
    /// with a `TypeError` instead, where the slot can hold one (module docs).
    fn narrow_settled(
        &mut self,
        v: Operand,
        unknown: TyId,
        (want, is_err): (TyId, bool),
        to: TyId,
        out: &Place,
        done: BlockId,
    ) {
        let av = self.cx.ty(unknown);
        let vp = Place::local(self.copy_to_temp(v, av));
        let rejects = self.type_error_reason(to);
        let mask = self.held_tags(want);
        let (Some(mask), Some((te, e2))) = (mask, rejects) else {
            self.narrow_write(&vp, unknown, (want, is_err), to, out);
            self.goto(done);
            return;
        };
        let desc = Operand::Copy(proj(&vp, Proj::Field(1)));
        let tag = self.any_tag_of(desc);
        let blocks: Vec<BlockId> = (0..FOUND.len()).map(|_| self.new_block()).collect();
        let cases = blocks
            .iter()
            .enumerate()
            .map(|(k, b)| (k as i128, *b))
            .collect();
        self.terminate(Terminator::Switch {
            value: tag,
            cases,
            default: blocks[TAG_OBJECT as usize],
        });
        let name = self.any_target_name(want);
        let an = article_of(&name);
        for (k, b) in blocks.into_iter().enumerate() {
            self.switch_to(b);
            if mask & (1 << k) != 0 {
                self.narrow_write(&vp, unknown, (want, is_err), to, out);
            } else {
                self.drop_glue(vp.clone(), unknown);
                let msg = format!("the value is not {an} {name} (it is {})", FOUND[k]);
                let err = self.new_type_error(&msg, te);
                let r = self.widen_error(err, te, e2);
                self.write_slot(r, (e2, true), to, out);
            }
            self.goto(done);
        }
    }

    /// The owned `unknown` at `vp` converted to `want` as `v as want` converts it, written to
    /// the slot `out` of the promise type `to`.
    fn narrow_write(
        &mut self,
        vp: &Place,
        unknown: TyId,
        (want, is_err): (TyId, bool),
        to: TyId,
        out: &Place,
    ) {
        let r = self.any_unbox(vp, unknown, want);
        self.write_slot(r, (want, is_err), to, out);
    }

    /// Write the owned value `r` of type `t` to the result slot `out` of the promise type `to`:
    /// its value, or (`is_err`) its reason.
    fn write_slot(&mut self, r: Operand, (t, is_err): (TyId, bool), to: TyId, out: &Place) {
        let to_slot = self.cx.promise_slot(to);
        if !matches!(self.cx.kind(to_slot), TyKind::Result(..)) {
            if self.cx.ty(t) != Ty::Unit {
                self.assign(out.clone(), Rvalue::Use(r));
            }
            return;
        }
        if self.cx.ty(t) != Ty::Unit {
            let view = self.cx.view(to_slot, is_err as u32);
            let dst = proj(&proj(out, Proj::Cast(view)), Proj::Field(1));
            if let Operand::Copy(p) = &r {
                self.take_temp(p);
            }
            self.assign(dst, Rvalue::Use(r));
        }
        let tag = cint(is_err as i128, Ty::U8);
        self.assign(proj(out, Proj::Field(0)), Rvalue::Use(tag));
    }

    /// The `TypeError` class type and the error type of the promise type `to`, when a reason of
    /// that type can hold a `TypeError` (and the prelude's class is in the program).
    fn type_error_reason(&mut self, to: TyId) -> Option<(TyId, TyId)> {
        let e2 = self.cx.promise_error(to)?;
        let (d, te) = self.type_error_def()?;
        if self.cx.hir.unchecked.contains(&d) {
            return None;
        }
        self.takes_error(e2, te).then_some((te, e2))
    }

    /// `new TypeError(msg)`, owned.
    fn new_type_error(&mut self, msg: &str, te: TyId) -> Operand {
        let TyKind::Adt(d, _) = self.cx.kind(te) else {
            super::ice("`TypeError` is not a class")
        };
        let str_ty = self.cx.intern(TyKind::Str);
        let span = velt_common::Span::DUMMY;
        let lit = hir::Expr {
            kind: hir::ExprKind::Lit(hir::Lit::Str(msg.to_string())),
            ty: str_ty,
            span,
        };
        let new = hir::Expr {
            kind: hir::ExprKind::New {
                def: d,
                type_args: vec![],
                args: vec![lit],
            },
            ty: te,
            span,
        };
        let v = self.expr(&new);
        if let Operand::Copy(p) = &v {
            self.take_temp(p);
        }
        v
    }

    /// The `typeof` tags (a bit each) of the values a conversion from `any` to `t` can take;
    /// `None` where that isn't known from the type alone (an interface, a C-like enum, `void`).
    fn held_tags(&mut self, t: TyId) -> Option<u32> {
        let bit = |tag: u64| 1u32 << tag;
        if self.cx.is_dynamic(t) {
            return None;
        }
        Some(match self.cx.kind(t) {
            TyKind::Never => 0,
            TyKind::Unit | TyKind::Dyn(..) => return None,
            TyKind::Option(inner, n) => {
                let mut m = self.held_tags(inner)?;
                if n.has_null() {
                    m |= bit(TAG_NULL);
                }
                if n.has_undefined() {
                    m |= bit(TAG_UNDEFINED);
                }
                m
            }
            TyKind::Bool => bit(TAG_BOOLEAN),
            TyKind::Int(_) | TyKind::Float(_) => bit(TAG_NUMBER),
            TyKind::Str => bit(TAG_STRING),
            TyKind::Symbol => bit(TAG_SYMBOL),
            TyKind::Literal(hir::LitValue::Str(_)) => bit(TAG_STRING),
            TyKind::Literal(hir::LitValue::Bool(_)) => bit(TAG_BOOLEAN),
            TyKind::Literal(_) => bit(TAG_NUMBER),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => bit(TAG_FUNCTION),
            _ if self.cx.is_union(t) => {
                let n = self.union_len(t);
                let mut m = 0;
                for k in 0..n {
                    let parts = self.cx.variant_tys(t, k);
                    match parts.as_slice() {
                        [pt] => m |= self.held_tags(*pt)?,
                        _ => return None,
                    }
                }
                m
            }
            _ if self.is_enum(t) => return None,
            _ if self.cx.is_function_object(t) => bit(TAG_FUNCTION),
            _ => bit(TAG_OBJECT),
        })
    }
}
