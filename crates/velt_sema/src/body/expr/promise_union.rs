//! `Promise.resolve(x)` where `x` is a union with promise members (`R | Promise<R>`, the result
//! of a sync-or-async callback): as in JS a promise member is the result itself and any other
//! value is resolved into one. Lowered as a `match` on `x` whose arms all give the one
//! `Promise<T, E>`: `T` from the type argument or the expected type, else the union of the
//! promises' payloads and the other members (as for `await`, `await_union.rs`).

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::ctx::Item;
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// The promise `Promise.resolve(h)` gives when `h` is a union with a promise member, with
    /// `value` the `T` asked for; `Err(h)` otherwise.
    pub(super) fn resolve_promise_union(
        &mut self,
        h: hir::Expr,
        value: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(members) = self
            .cx
            .union_def(h.ty)
            .and_then(|_| self.cx.union_members(h.ty))
        else {
            return Err(h);
        };
        if !members
            .iter()
            .any(|m| self.cx.ty.promise_payload(*m).is_some())
        {
            return Err(h);
        }
        let Some(Item::Def(d)) = self.cx.prelude.get("promiseResolve").cloned() else {
            return Err(h);
        };
        let never = self.cx.ty.never;
        let (mut values, mut errors) = (vec![], vec![]);
        for m in &members {
            let v = self.cx.ty.promise_payload(*m).unwrap_or(*m);
            if v != never && !values.contains(&v) {
                values.push(v);
            }
            match self.cx.ty.promise_error(*m) {
                Some(e) if e != never && !errors.contains(&e) => errors.push(e),
                _ => {}
            }
        }
        let t = match value {
            Some(t) => t,
            None if values.len() <= 1 => values.first().copied().unwrap_or(never),
            None => self.cx.union_of(&values, false, span),
        };
        let e = match errors.as_slice() {
            [] => never,
            [e] => *e,
            _ => self.cx.union_of(&errors, false, span),
        };
        let error = self.cx.ty.error;
        if t == error || e == error {
            return Err(h);
        }
        let target = self.cx.ty.promise_rejecting(t, e);
        // A promise of another payload type is not converted (its value would be read as `t`).
        let other = members.iter().find(|m| {
            self.cx
                .ty
                .promise_payload(**m)
                .is_some_and(|p| p != t && p != never)
        });
        if let Some(m) = other.copied() {
            return Ok(self.unsupported_resolve(m, t, span));
        }
        let mut failed = None;
        let r = self.map_members_in(h, target, None, &mut |s, v| {
            failed = Some(v.ty);
            if s.cx.ty.promise_payload(v.ty).is_some() {
                return s.try_coerce(v, target).ok();
            }
            let v = s.try_coerce(v, t).ok()?;
            let mut report = |s: &mut Self, _: &str, expected: TyId, found: &hir::Expr| {
                s.report_mismatch(expected, found);
            };
            let call = s.call_checked(d, vec![v], span, &mut report);
            s.try_coerce(call, target).ok()
        });
        // Once the union has a promise member, the old path would resolve to the promise itself.
        Ok(match r {
            Ok(p) => p,
            Err(_) => {
                let m = failed.unwrap_or(t);
                self.unsupported_resolve(m, t, span)
            }
        })
    }

    fn unsupported_resolve(&mut self, m: TyId, t: TyId, span: Span) -> hir::Expr {
        let (mn, tn) = (self.cx.display(m), self.cx.display(t));
        self.cx.error(Diagnostic::error(
            format!("`Promise.resolve` of a `{mn}` as a `Promise<{tn}>` is not supported yet"),
            span,
        ));
        self.error_expr(span)
    }
}
