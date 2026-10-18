//! A value whose type is a type parameter converted to `any` or `unknown` (issue #1241:
//! `b.dom = n` with `n: H`, `n as unknown as X`). Sema boxes such a value as it is; the
//! instantiation may be a type sema would have unpacked first (`hir_encodings.md` "any and
//! unknown"): an option holds its payload or the dynamic `null` / `undefined`, `void` is
//! `undefined`, a union the member it holds, a literal type its value.

use velt_sema::hir::{self, FloatTy, LitValue, Nothing, TyId, TyKind};

use super::any::TYPEOF;

use crate::lower::FnLower;
use crate::vir::{Const, Operand, Place, Rvalue, Terminator, Ty, UnOp, STR_AGG};

impl FnLower<'_, '_> {
    /// Does a value of the concrete type `t` need unpacking on its way into `any`: one sema
    /// unpacks before boxing (options, `void`, unions, literal and enum types)?
    pub(super) fn any_unpacks(&self, t: TyId) -> bool {
        match self.cx.kind(t) {
            TyKind::Option(..) | TyKind::Unit | TyKind::Literal(_) => true,
            TyKind::Adt(d, _) if self.is_enum(t) => {
                let e = self.cx.enum_def(d);
                e.is_union || e.variants.iter().all(|v| v.payload.is_empty())
            }
            _ => false,
        }
    }

    /// The owned value `v` of the concrete type `t` ([`any_unpacks`](Self::any_unpacks)) as the
    /// `any` / `unknown` `ty`.
    pub(super) fn any_unpack(&mut self, v: Operand, t: TyId, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        let out = Place::local(self.temp(vt));
        // Assigned on every path: a union's switch has a default edge.
        let zero = self.zero_value(vt);
        self.assign(out.clone(), Rvalue::Use(zero));
        self.any_into(v, t, ty, &out);
        self.own_value(Operand::Copy(out), ty)
    }

    fn any_into(&mut self, v: Operand, t: TyId, ty: TyId, out: &Place) {
        if self.dead() {
            return;
        }
        match self.cx.kind(t) {
            TyKind::Literal(l) => self.any_literal_into(&l, out),
            // `undefined` or `null` alone (an option of `never`): always its nothing.
            TyKind::Option(inner, n) if matches!(self.cx.kind(inner), TyKind::Never) => {
                self.any_nothing_into(n, out)
            }
            TyKind::Option(inner, n) => {
                let vt = self.cx.ty(t);
                let p = Place::local(self.copy_to_temp(v, vt));
                let some = self.option_is_some(&p, t);
                let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
                self.branch(some, yes, no);
                self.switch_to(yes);
                let pp = self.some_payload(&p, t);
                self.any_into(Operand::Copy(pp), inner, ty, out);
                self.goto(done);
                self.switch_to(no);
                self.any_nothing_into(n, out);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Dyn(..) if !self.cx.is_dynamic(t) => {
                let o = self.make_dyn_any(v, t, ty);
                self.assign(out.clone(), Rvalue::Use(o));
            }
            _ if self.cx.is_union(t) => {
                let vt = self.cx.ty(t);
                let p = Place::local(self.copy_to_temp(v, vt));
                self.for_each_variant(&p, t, |lw, k, parts| match parts.as_slice() {
                    [(pp, pt)] => lw.any_into(Operand::Copy(pp.clone()), *pt, ty, out),
                    _ => match lw.variant_literal(t, k) {
                        Some(l) => lw.any_literal_into(&l, out),
                        None => lw.any_nothing_into(Nothing::Undefined, out),
                    },
                });
            }
            _ if self.is_enum(t) => {
                // A C-like or string enum (an `I64` member index): not reached here yet.
                self.panic_msg(
                    "TypeError: an enum value of a type parameter converted to `any` is not supported yet (#1241)",
                );
                self.terminate(Terminator::Unreachable);
                let next = self.new_block();
                self.switch_to(next);
            }
            _ => self.any_box_into(v, t, out),
        }
    }

    /// The value of literal type `l` boxed into `out`.
    fn any_literal_into(&mut self, l: &LitValue, out: &Place) {
        let (v, k) = match l {
            LitValue::Str(s) => (self.str_lit(s), TyKind::Str),
            LitValue::Bool(b) => (Operand::Const(Const::Bool(*b), Ty::Bool), TyKind::Bool),
            LitValue::Int(it, n) => {
                let t = self.cx.intern(TyKind::Int(*it));
                let vt = self.cx.ty(t);
                (Operand::Const(Const::Int(*n), vt), TyKind::Int(*it))
            }
            LitValue::Float(_, bits) => {
                let f = Operand::Const(Const::Float(f64::from_bits(*bits)), Ty::F64);
                (f, TyKind::Float(FloatTy::F64))
            }
        };
        let t = self.cx.intern(k);
        self.any_box_into(v, t, out);
    }

    /// `typeof` of a variable whose type is a type parameter, boxed only to ask it (sema's
    /// `param_as_any`): the instantiation's tag without boxing, tested at run time only for an
    /// option (its nothing or its payload). `None` when the box must be asked.
    pub(super) fn typeof_folded(&mut self, a: &hir::Expr) -> Option<Operand> {
        let local = boxed_local(a)?;
        let t = self.sub(local.ty);
        let TyKind::Option(inner, n) = self.cx.kind(t) else {
            let tag = self.static_typeof(t)?;
            return Some(self.str_lit(tag));
        };
        if n == Nothing::Both {
            return None;
        }
        let nothing = if n.has_null() { "object" } else { "undefined" };
        if matches!(self.cx.kind(inner), TyKind::Never) {
            return Some(self.str_lit(nothing));
        }
        let some_tag = self.static_typeof(inner)?;
        let v = self.expr(local);
        let p = self.place_of(v, t);
        let some = self.option_is_some(&p, t);
        let out = Place::local(self.temp(Ty::Agg(STR_AGG)));
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(some, yes, no);
        for (b, tag) in [(yes, some_tag), (no, nothing)] {
            self.switch_to(b);
            let s = self.str_lit(tag);
            self.assign(out.clone(), Rvalue::Use(s));
            self.goto(done);
        }
        self.switch_to(done);
        Some(Operand::Copy(out))
    }

    /// `a === b` / `a == b` (`loose`) of an `any` boxed from a variable (sema's
    /// `param_as_any`) and the dynamic `undefined` or `null`: answered from the instantiation
    /// without boxing, tested at run time only for an option. `None` when the boxes must be
    /// compared.
    pub(super) fn nothing_eq_folded(
        &mut self,
        a: &hir::Expr,
        b: &hir::Expr,
        loose: bool,
    ) -> Option<Operand> {
        let (local, lit) = match (boxed_local(a), boxed_local(b)) {
            (Some(l), None) => (l, b),
            (None, Some(l)) => (l, a),
            _ => return None,
        };
        let lit = match &lit.kind {
            hir::ExprKind::ToDyn { expr, .. } => expr.as_ref(),
            _ => lit,
        };
        let hir::ExprKind::Lit(hir::Lit::Null) = lit.kind else {
            return None;
        };
        let lt = self.sub(lit.ty);
        let want = match self.cx.kind(lt) {
            // The dynamic nothing is a `null` literal of an option type (`dynamic_nothing_ty`).
            TyKind::Option(_, n) if n != Nothing::Both => n,
            _ => return None,
        };
        let t = self.sub(local.ty);
        let constant = |b| Some(Operand::Const(Const::Bool(b), Ty::Bool));
        let TyKind::Option(inner, n) = self.cx.kind(t) else {
            // `void` is `undefined`; any other value is not a nothing.
            return match self.static_typeof(t)? {
                "undefined" => constant(loose || want == Nothing::Undefined),
                _ => constant(false),
            };
        };
        if n == Nothing::Both {
            return None;
        }
        if !loose && n != want {
            return constant(false);
        }
        if matches!(self.cx.kind(inner), TyKind::Never) {
            return constant(true);
        }
        self.static_typeof(inner)?;
        let v = self.expr(local);
        let p = self.place_of(v, t);
        let some = self.option_is_some(&p, t);
        Some(self.rvalue_temp(Ty::Bool, Rvalue::Unary(UnOp::Not, some)))
    }

    /// The `typeof` of every value of the concrete type `t`, if the type fixes it.
    fn static_typeof(&self, t: TyId) -> Option<&'static str> {
        match self.cx.kind(t) {
            TyKind::Option(..) | TyKind::Dyn(..) | TyKind::Op(..) | TyKind::Param(_) => None,
            _ if self.cx.is_union(t) => None,
            _ if self.is_enum(t) => Some(match self.enum_strings(t) {
                Some(_) => "string",
                None => "number",
            }),
            _ => Some(TYPEOF[self.cx.any_tag(t) as usize]),
        }
    }
}

/// The variable `a` boxes, when `a` is an `any` made of a variable or a share of one.
fn boxed_local(a: &hir::Expr) -> Option<&hir::Expr> {
    let hir::ExprKind::ToDyn { expr, .. } = &a.kind else {
        return None;
    };
    let local = match &expr.kind {
        hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::Share),
            args,
        } if args.len() == 1 => &args[0],
        _ => expr.as_ref(),
    };
    matches!(local.kind, hir::ExprKind::Local(..)).then_some(local)
}
