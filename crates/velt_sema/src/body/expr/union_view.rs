//! Union views in expressions (`crate::union_views`, #1410): `w as U` of a wide interface
//! value, and the `<view>` arm each use of the union gives the view: it does the same through
//! the view interface, reading the object's getters on every access.
//!
//! ```text
//! const cell = impl as State<T>;        →  State.<view>(<Upcast impl to the view interface>)
//! if (cell.state === "ready") …         →  match cell { Ready(_) => true, <view>(v) => v.state == "ready", _ => false }
//! cell.value  (narrowed to Ready)       →  match cell { Ready(r) => r.value, <view>(v) => v.value as T, _ => unreachable }
//! ```

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, LitValue, PatKind as P, TyId, UseMode};
use crate::literals::lit_matches;

impl FnCx<'_, '_> {
    /// `w as U` for a union `U` with a view, of an interface value `w` that is no member of `U`
    /// and converts to `U`'s view interface (it has every field all members have, with the
    /// union of their types, and the members' shared methods): the view, holding the same
    /// object. `Err(w)` otherwise.
    pub(super) fn union_view_cast(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((def, variant, view)) = self.cx.union_view(target) else {
            return Err(h);
        };
        if !self.cx.is_method_iface(h.ty) {
            return Err(h);
        }
        let members = self.cx.union_members(target).unwrap_or_default();
        if members.contains(&h.ty) {
            return Err(h);
        }
        let h = self.try_coerce(h, view)?;
        self.cx.union_view_made(def);
        let (_, type_args) = self.cx.union_def(target).expect("ICE: union");
        let kind = H::Variant {
            def,
            type_args,
            variant,
            args: vec![h],
        };
        Ok(self.mk(kind, target, span))
    }

    /// The `<view>` arm of a match on a value of union type `u`: `<view>(v) => body(v)`, `v`
    /// the view interface value (moved out when `consume`). `None` when `u` has no view, or
    /// when `body` gives nothing: then the use is recorded as unsupported (`what`), an error
    /// once the program makes a view of `u`.
    pub(super) fn view_arm(
        &mut self,
        u: TyId,
        consume: bool,
        span: Span,
        what: &str,
        body: &mut dyn FnMut(&mut Self, hir::Expr) -> Option<hir::Expr>,
    ) -> Option<hir::Arm> {
        let (def, variant, view) = self.cx.union_view(u)?;
        let b = self.new_local("<view>", view, false, span, LocalKind::Bind);
        let v = self.mk(H::Local(b, UseMode::Borrow), view, span);
        let mark = self.cx.diags.len();
        let out = body(self, v);
        let failed = self.cx.diags[mark..].iter().any(|d| d.is_error());
        let Some(out) = out.filter(|_| !failed) else {
            self.cx.diags.truncate(mark);
            self.cx.union_view_unsupported(def, span, what);
            return None;
        };
        let mode = match consume {
            true => UseMode::Move,
            false => UseMode::Borrow,
        };
        let bind = self.pat(P::Binding(b, mode), view, span);
        let pat = self.pat(
            P::Variant {
                def,
                variant,
                args: vec![bind],
            },
            u,
            span,
        );
        Some(hir::Arm {
            pat,
            guard: None,
            body: out,
        })
    }

    /// View value `v` as a value of union `exp` (re-tagging into a union of the same family):
    /// `exp`'s `<view>` holding it.
    pub(super) fn view_retag(&mut self, v: hir::Expr, exp: TyId, span: Span) -> Option<hir::Expr> {
        let (def, variant, view) = self.cx.union_view(exp)?;
        let v = self.try_coerce(v, view).ok()?;
        self.cx.union_view_made(def);
        let (_, type_args) = self.cx.union_def(exp)?;
        let kind = H::Variant {
            def,
            type_args,
            variant,
            args: vec![v],
        };
        Some(self.mk(kind, exp, span))
    }

    /// Inserts `arm` before the trailing wildcard of `arms` (or last).
    pub(super) fn push_view_arm(arms: &mut Vec<hir::Arm>, arm: Option<hir::Arm>) {
        let Some(arm) = arm else {
            return;
        };
        let at = match arms.last() {
            Some(a) if matches!(a.pat.kind, P::Wildcard) => arms.len() - 1,
            _ => arms.len(),
        };
        arms.insert(at, arm);
    }

    /// `h`, a value read through a view, as `ty` (the type the members flow narrowing left
    /// give it): converted, else checked as by `h as ty` (a `TypeError` when the object's state
    /// no longer fits them, never another member's value).
    pub(super) fn view_value_as(
        &mut self,
        h: hir::Expr,
        ty: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        if h.ty == ty {
            return Some(h);
        }
        let h = match self.try_coerce(h, ty) {
            Ok(h) => return Some(h),
            Err(h) => h,
        };
        if let Some(h) = self.view_checked_lits(h.clone(), ty, span) {
            return Some(h);
        }
        // `T | null` read where the members left have a `T`.
        if let Some(p) = self.cx.ty.opt_payload(h.ty) {
            if self.cx.ty.opt_payload(ty).is_none() && p != self.cx.ty.never {
                let msg = "TypeError: a value read through a union view does not fit the narrowed member: it is null";
                let h = self.checked_present(h, msg, Some(p), false, span);
                return self.view_value_as(h, ty, span);
            }
        }
        self.narrowing_assertion(h, ty, span).ok()
    }

    /// `h` as `ty` when `ty` is `null`, a literal type or a union of literal types and `h` a
    /// value of their base type (or a nullable one): a match on the value giving the literal
    /// it is, a `TypeError` for any other value.
    fn view_checked_lits(&mut self, h: hir::Expr, ty: TyId, span: Span) -> Option<hir::Expr> {
        let lits: Vec<TyId> = match self.cx.union_members(ty) {
            Some(ms) => ms,
            None => vec![ty],
        };
        let base = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        let null = |s: &Self, m: TyId| s.cx.ty.opt_payload(m) == Some(s.cx.ty.never);
        let all_null = lits.iter().all(|&m| null(self, m));
        if !all_null && base != self.cx.ty.bool_ && base != self.cx.ty.str_ {
            return None;
        }
        let mut arms = vec![];
        for m in lits {
            if self.cx.ty.opt_payload(m) == Some(self.cx.ty.never) {
                let pat = self.pat(P::None, h.ty, span);
                let null = self.mk(H::Lit(hir::Lit::Null), m, span);
                arms.push((pat, null));
                continue;
            }
            let lit = match self.cx.lit_value(m)? {
                LitValue::Bool(b) => hir::Lit::Bool(b),
                LitValue::Str(s) => hir::Lit::Str(s),
                _ => return None,
            };
            let pat = self.pat(P::Lit(lit), h.ty, span);
            let value = self.lit_const(m, span);
            arms.push((pat, value));
        }
        let mut out = vec![];
        for (pat, value) in arms {
            let body = self.try_coerce(value, ty).ok()?;
            out.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let msg = format!(
            "TypeError: a value read through a union view is not a `{}`",
            self.cx.display(ty)
        );
        let msg = self.str_lit(&msg, span);
        let never = self.cx.ty.never;
        out.push(hir::Arm {
            pat: self.pat(P::Wildcard, h.ty, span),
            guard: None,
            body: self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span),
        });
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms: out,
        };
        Some(self.mk(kind, ty, span))
    }

    /// `prop` read through view value `v`, as `ty`.
    pub(super) fn view_field(
        &mut self,
        v: hir::Expr,
        prop: &ast::Ident,
        ty: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let h = self
            .iface_field(v, prop, crate::body::Want::Borrow, span)
            .ok()?;
        self.view_value_as(h, ty, span)
    }

    /// `f === lit` of a discriminant `f` read through a view: a union of literal types matches
    /// the members with that value, a `boolean` its value; `None` for other types.
    pub(super) fn view_lit_test(
        &mut self,
        f: hir::Expr,
        lit: &ast::SignedLit,
        span: Span,
    ) -> Option<hir::Expr> {
        let ty = f.ty;
        if let Some(v) = self.cx.lit_value(ty) {
            let hit = lit_matches(&v, lit);
            let wild = self.pat(P::Wildcard, ty, span);
            let mut m = self.bool_match(f, Some(wild), span);
            if let H::Match { arms, .. } = &mut m.kind {
                arms[0].body.kind = H::Lit(hir::Lit::Bool(hit));
            }
            return Some(m);
        }
        if ty == self.cx.ty.bool_ {
            let b = match lit_value_of(lit)? {
                LitValue::Bool(b) => b,
                _ => return None,
            };
            let pat = self.pat(P::Lit(hir::Lit::Bool(b)), ty, span);
            return Some(self.bool_match(f, Some(pat), span));
        }
        let (def, _) = self.cx.union_def(ty)?;
        let members = self.cx.union_members(ty)?;
        let mut pats = vec![];
        for (i, &m) in members.iter().enumerate() {
            if lit_matches(&self.cx.lit_value(m)?, lit) {
                pats.push(self.variant_pat(def, i as u32, m, ty, span));
            }
        }
        let pat = match pats.len() {
            0 => None,
            1 => pats.pop(),
            _ => Some(self.pat(P::Or(pats), ty, span)),
        };
        Some(self.bool_match(f, pat, span))
    }

    /// The pattern on a discriminant value of type `ty` (read through a view, `switch`) that
    /// matches `value`.
    pub(crate) fn view_key_pat(&mut self, ty: TyId, value: &LitValue, span: Span) -> hir::Pat {
        if self.cx.lit_value(ty).is_some() {
            return self.pat(P::Wildcard, ty, span);
        }
        if let (true, LitValue::Bool(b)) = (ty == self.cx.ty.bool_, value) {
            return self.pat(P::Lit(hir::Lit::Bool(*b)), ty, span);
        }
        let members = self.cx.union_members(ty).unwrap_or_default();
        let (def, _) = self.cx.union_def(ty).expect("ICE: discriminant union");
        let at = members
            .iter()
            .position(|&m| self.cx.lit_value(m).as_ref() == Some(value))
            .expect("ICE: discriminant value");
        self.variant_pat(def, at as u32, members[at], ty, span)
    }
}

/// The value of a literal as a `LitValue`, for `bool`s.
fn lit_value_of(lit: &ast::SignedLit) -> Option<LitValue> {
    match lit {
        ast::SignedLit {
            lit: ast::Lit::Bool(b),
            ..
        } => Some(LitValue::Bool(*b)),
        _ => None,
    }
}
