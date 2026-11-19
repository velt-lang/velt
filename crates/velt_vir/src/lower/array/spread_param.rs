//! `Intrinsic::SpreadParam` (#943; hir_encodings.md "Spreads in generic bodies"): a value of a
//! type known only per instantiation, spread into a call's rest parameter of `unknown` or `any`
//! values, as an array of that type. At the instantiation lowering sees the value's own type: a
//! tuple gives its elements in order, an array its elements, each shared and converted to the
//! rest's element type (boxed). A value of another type, or an element that holds a lock (which
//! a dynamic value would read without it), is an error at the instantiation.

use velt_common::{Diagnostic, Span};
use velt_sema::hir::{self, TyId, TyKind};

use crate::lower::{cint, unit, FnLower};
use crate::vir::{Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `SpreadParam(x)` of result type `ty` (`U[]`, `U` dynamic).
    pub(in crate::lower) fn spread_param(
        &mut self,
        x: &hir::Expr,
        ty: TyId,
    ) -> crate::vir::Operand {
        let ty = self.sub(ty);
        let elem = self.elem_ty(ty);
        let t = self.sub(x.ty);
        let v = self.expr(x);
        if self.dead() {
            return unit();
        }
        let src = self.place_of(v, t);
        let parts: Option<Vec<TyId>> = match self.cx.kind(t) {
            TyKind::Tuple(_) if std::env::var("VELT_MUT").as_deref() == Ok("1") => Some(vec![]),
            TyKind::Tuple(ts) => Some(ts),
            TyKind::Array(_) => None,
            _ => {
                self.spread_param_error(x.span, t, None);
                return self.array_value(ty, cint(0, Ty::Ptr), cint(0, Ty::U64), cint(0, Ty::U64));
            }
        };
        let elems = match &parts {
            Some(ts) => ts.clone(),
            None => vec![self.elem_ty(t)],
        };
        if let Some(&bad) = elems.iter().find(|&&e| self.cx.reaches_lock_set(e)) {
            self.spread_param_error(x.span, t, Some(bad));
            return self.array_value(ty, cint(0, Ty::Ptr), cint(0, Ty::U64), cint(0, Ty::U64));
        }
        let n = match &parts {
            Some(ts) => cint(ts.len() as i128, Ty::U64),
            None => Self::arr_field(&src, 1),
        };
        let out = self.inline_array_with_len(n.clone(), elem);
        match parts {
            Some(ts) => {
                for (i, et) in ts.into_iter().enumerate() {
                    let from = self.field_place(&src, t, i as u32);
                    let to = self.elem_place(&Place::local(out), cint(i as i128, Ty::U64), elem);
                    self.box_into(from, et, &to, elem);
                }
            }
            None => {
                let et = elems[0];
                let k = self.temp(Ty::U64);
                self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
                self.count_loop(k, n, |lw, k| {
                    let from = lw.elem_place(&src, k.clone(), et);
                    let to = lw.elem_place(&Place::local(out), k, elem);
                    lw.box_into(from, et, &to, elem);
                });
            }
        }
        self.own_array(out, ty)
    }

    /// The `et` value at `from`, shared and boxed as a `dyn_ty` into the unset slot `to`.
    fn box_into(&mut self, from: Place, et: TyId, to: &Place, dyn_ty: TyId) {
        let avt = self.cx.ty(dyn_ty);
        let zero = self.zero_value(avt);
        self.assign(to.clone(), Rvalue::Use(zero));
        let s = self.share_value(crate::vir::Operand::Copy(from), et);
        self.any_into(s, et, dyn_ty, to);
    }

    /// An instantiation whose value of type `t` can't be spread (`bad`: the element that holds
    /// a lock), reported once per site.
    fn spread_param_error(&mut self, span: Span, t: TyId, bad: Option<TyId>) {
        let tn = self.any_target_name(t);
        let d = match bad {
            Some(e) => Diagnostic::error(
                format!(
                    "cannot spread a `{tn}` into `unknown` arguments: its `{}` holds a lock",
                    self.any_target_name(e)
                ),
                span,
            )
            .with_note("its value is read only under its lock; pass the elements as they are, or what a callback returns"),
            None => Diagnostic::error(
                format!("cannot spread a value of type `{tn}` into a call's arguments"),
                span,
            )
            .with_note("what a generic rest is spread from must be a tuple or an array at every instantiation"),
        };
        let seen = (self.cx.spread_errors.iter()).any(|o| {
            o.message == d.message
                && o.labels.first().map(|l| l.span) == d.labels.first().map(|l| l.span)
        });
        if !seen {
            self.cx.spread_errors.push(d);
        }
    }
}
