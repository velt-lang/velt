//! `a === K` / `a !== K` for a constant `K` holding a fresh symbol (`crate::unique_symbols`):
//! on a union with the member `typeof K`, a test for that member.

use velt_common::Span;
use velt_syntax::ast;

use super::callback::synth;
use super::type_tests::LitEq;
use crate::body::narrow::Fact;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, PatKind as P, TyId, TyKind, UseMode};
use crate::unique_symbols::UniqueAt;

impl FnCx<'_, '_> {
    /// `a === K` / `a !== K` (`negate`; either side may be `K`): a test for the member
    /// `typeof K` when `a` holds a union with it, else the two operands as checked (`None`
    /// when neither side names such a constant).
    pub(super) fn unique_symbol_eq(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        negate: bool,
        span: Span,
    ) -> Option<LitEq> {
        if self.cx.unique_symbols.is_empty() {
            return None;
        }
        let (d, k_first) = match (self.unique_symbol_named(lhs), self.unique_symbol_named(rhs)) {
            (Some(d), None) => (d, true),
            (None, Some(d)) => (d, false),
            _ => return None,
        };
        let (k_e, other_e) = if k_first { (lhs, rhs) } else { (rhs, lhs) };
        let other = self.type_test_operand(other_e);
        if let Some(UniqueAt::Member(v)) = self.cx.unique_symbol_in(other.ty, d) {
            let test = self.unique_member_test(other, v, k_e, span);
            return Some(LitEq::Done(self.negated(test, negate, span)));
        }
        let other = self.unbrand(other);
        let k = self.expr(k_e, None, Want::Borrow);
        Some(match k_first {
            true => LitEq::Operands(k, other),
            false => LitEq::Operands(other, k),
        })
    }
}

impl FnCx<'_, '_> {
    /// `match (s) { <member v>(#sym) if #sym === K => true, _ => false }`: the value is
    /// compared too, since two `unique symbol` members are one `symbol` member once lowered.
    fn unique_member_test(
        &mut self,
        s: hir::Expr,
        v: u32,
        k_e: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let inner = self.cx.ty.core(s.ty).unwrap_or(s.ty);
        let (Some((def, _)), Some(members)) =
            (self.cx.union_def(inner), self.cx.union_members(inner))
        else {
            return self.error_expr(span);
        };
        let u = members[v as usize];
        self.push_scope();
        let name = format!("#sym{}", self.f.locals.len());
        let l = self.new_local(&name, u, false, span, LocalKind::Temp);
        if let Some(scope) = self.f.scopes.last_mut() {
            scope.names.insert(name.clone(), l);
        }
        let ident = ast::Ident { name, span };
        let eq = ast::ExprKind::Binary {
            op: ast::BinaryOp::StrictEq,
            lhs: Box::new(synth(ast::ExprKind::Ident(ident), span)),
            rhs: Box::new(k_e.clone()),
        };
        let guard = self.expr(&synth(eq, span), None, Want::Borrow);
        self.pop_scope();
        let bind = self.pat(P::Binding(l, UseMode::Copy), u, span);
        let variant = P::Variant {
            def,
            variant: v,
            args: vec![bind],
        };
        let mut p = self.pat(variant, inner, span);
        if s.ty != inner {
            p = self.present_pat(p, s.ty, span);
        }
        let b = self.cx.ty.bool_;
        let lit = |x: bool| hir::Expr {
            kind: H::Lit(hir::Lit::Bool(x)),
            ty: b,
            span,
        };
        let wild = self.pat(P::Wildcard, s.ty, span);
        let arms = vec![
            hir::Arm {
                pat: p,
                guard: Some(guard),
                body: lit(true),
            },
            hir::Arm {
                pat: wild,
                guard: None,
                body: lit(false),
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, b, span)
    }

    /// Facts of `x === K` on a union local `x` with the member `typeof K`: that member when
    /// true, the others when false (only `K` has that type).
    pub(crate) fn unique_symbol_facts(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        if self.cx.unique_symbols.is_empty() {
            return None;
        }
        let (d, other) = match (self.unique_symbol_named(lhs), self.unique_symbol_named(rhs)) {
            (Some(d), None) => (d, rhs),
            (None, Some(d)) => (d, lhs),
            _ => return None,
        };
        let (l, nullable, u) = self.local_with_members(other)?;
        let Some(UniqueAt::Member(v)) = self.cx.unique_symbol_in(u, d) else {
            return None;
        };
        let n = self.cx.union_members(u)?.len() as u32;
        let mut yes = vec![Fact::Members(l, vec![v])];
        if nullable {
            yes.push(Fact::NonNull(l));
        }
        let no = vec![Fact::Members(l, (0..n).filter(|&i| i != v).collect())];
        Some((yes, no))
    }
}

impl FnCx<'_, '_> {
    /// The constant whose `unique symbol` type is `t`, if `t` is one.
    pub(super) fn unique_symbol_of(&self, t: TyId) -> Option<DefId> {
        if self.cx.unique_symbols.is_empty() {
            return None;
        }
        (self.cx.unique_symbols.iter())
            .find(|(_, &u)| u == t)
            .map(|(&d, _)| d)
    }

    /// `v === K` for symbol `v` and the constant `K` (`d`).
    pub(crate) fn is_unique_symbol(&mut self, v: hir::Expr, d: DefId) -> hir::Expr {
        let span = v.span;
        let v = self.unbrand(v);
        let k = self.global_read(d, Want::Borrow, span);
        self.binary_checked(ast::BinaryOp::StrictEq, v, k, span)
    }

    /// Symbol `h` as the `unique symbol` type `u` of constant `d`, checked: only `K` has that
    /// type, so another symbol is a `TypeError` (`s as typeof K` in TypeScript is not checked,
    /// but a union narrowed by `x === K` relies on it here).
    pub(super) fn checked_unique(&mut self, h: hir::Expr, d: DefId, u: TyId) -> hir::Expr {
        let span = h.span;
        let symbol = self.cx.ty.intern(TyKind::Symbol);
        let l = self.new_local("<unique symbol>", symbol, false, span, LocalKind::Temp);
        let stmt = hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(h),
            },
            span,
        };
        let read = self.mk(H::Local(l, UseMode::Borrow), symbol, span);
        let cond = self.is_unique_symbol(read, d);
        let mut ok = self.read_temp(l, symbol, span);
        ok.ty = u;
        let name = self.cx.display(u);
        let msg = self.str_lit(&format!("TypeError: the value is not `{name}`"), span);
        let never = self.cx.ty.never;
        let fail = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(ok),
            els: Box::new(fail),
        };
        let checked = self.mk(kind, u, span);
        self.with_temps_value(vec![stmt], checked)
    }
}

impl FnCx<'_, '_> {
    /// The test and value of member `m`, a `unique symbol` type, for the `any` held in `tmp`:
    /// a symbol that is `K` (`dynamic_to_members`).
    pub(super) fn unique_member_from_any(
        &mut self,
        tmp: hir::LocalId,
        dty: TyId,
        m: TyId,
        span: Span,
    ) -> Option<(hir::Expr, hir::Expr)> {
        let d = self.unique_symbol_of(m)?;
        let symbol = self.cx.ty.intern(TyKind::Symbol);
        let (test, mut value) = self.member_test(tmp, dty, symbol, span)?;
        let any = self.any_ty()?;
        let k = self.global_read(d, Want::Borrow, span);
        let boxed = self.coerce(k, any);
        let x = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let same = self.intrinsic(
            Intrinsic::AnyStrictEq,
            vec![x, boxed],
            self.cx.ty.bool_,
            span,
        );
        let b = self.cx.ty.bool_;
        let kind = H::Logical {
            op: hir::LogicOp::And,
            lhs: Box::new(test),
            rhs: Box::new(same),
        };
        let test = self.mk(kind, b, span);
        value.ty = m;
        Some((test, value))
    }
}

impl FnCx<'_, '_> {
    /// Symbol `h` asserted to union `target` whose only symbol member is `typeof K`: that
    /// member, checked to be `K` ([`Self::checked_unique`]); `Err(h)` for other shapes.
    pub(super) fn symbol_as_unique_member(
        &mut self,
        h: hir::Expr,
        target: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if self.cx.unique_symbols.is_empty() || !matches!(self.cx.ty.kind(h.ty), TyKind::Symbol) {
            return Err(h);
        }
        let Some(members) = self.cx.union_members(target) else {
            return Err(h);
        };
        let symbolic = |cx: &crate::ctx::Ctx, m: TyId| {
            matches!(cx.ty.kind(m), TyKind::Symbol) || cx.brand_base(m).is_some()
        };
        let mut found = members.iter().filter(|m| symbolic(self.cx, **m));
        let (Some(&u), None) = (found.next(), found.next()) else {
            return Err(h);
        };
        let Some(d) = self.unique_symbol_of(u) else {
            return Err(h);
        };
        let v = self.checked_unique(h, d, u);
        Ok(self.coerce(v, target))
    }
}

impl FnCx<'_, '_> {
    /// `h` as `exp`, which it is once brands are erased (`Ctx::widens_unique`).
    pub(super) fn unique_retype(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        let span = h.span;
        let block = hir::Block {
            stmts: vec![],
            value: Some(Box::new(h)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }

    /// Union `h` asserted to `target`, the same union with `typeof K` (one or more) where `h`
    /// has `symbol` (`x as Suspend` for `x: { p } | symbol`): a symbol it holds is checked to be
    /// one of those constants (a `TypeError` otherwise), then the value is a `target` as it is.
    pub(super) fn union_as_unique_members(
        &mut self,
        h: hir::Expr,
        target: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.cx.widens_unique(target, h.ty) {
            return Err(h);
        }
        let span = h.span;
        let sty = h.ty;
        let inner = self.cx.ty.core(sty).unwrap_or(sty);
        let symbol = self.cx.ty.intern(TyKind::Symbol);
        let (Some((def, _)), Some(from)) = (self.cx.union_def(inner), self.cx.union_members(inner))
        else {
            return Err(h);
        };
        let Some(v) = from.iter().position(|&m| m == symbol) else {
            // No plain `symbol` member: nothing to check.
            return Ok(self.unique_retype(h, target));
        };
        let core = self.cx.ty.core(target).unwrap_or(target);
        let consts: Vec<DefId> = (self.cx.union_members(core).unwrap_or_default().into_iter())
            .filter_map(|m| self.unique_symbol_of(m))
            .collect();
        let tmp = self.new_local("<as unique symbol>", sty, false, span, LocalKind::Temp);
        let mut stmts = vec![hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(h),
            },
            span,
        }];
        let mode = if self.cx.is_copy(symbol) {
            UseMode::Copy
        } else {
            UseMode::Borrow
        };
        let l = self.new_local("<symbol>", symbol, false, span, LocalKind::Bind);
        let b = self.cx.ty.bool_;
        let mut ok: Option<hir::Expr> = None;
        for d in consts {
            let read = self.mk(H::Local(l, mode), symbol, span);
            let same = self.is_unique_symbol(read, d);
            ok = Some(match ok {
                None => same,
                Some(prev) => self.mk(
                    H::Logical {
                        op: hir::LogicOp::Or,
                        lhs: Box::new(prev),
                        rhs: Box::new(same),
                    },
                    b,
                    span,
                ),
            });
        }
        let ok = ok.unwrap_or_else(|| self.mk(H::Lit(hir::Lit::Bool(false)), b, span));
        let not_ok = self.mk(
            H::Unary {
                op: hir::UnOp::Not,
                expr: Box::new(ok),
            },
            b,
            span,
        );
        let bind = self.pat(P::Binding(l, mode), symbol, span);
        let variant = P::Variant {
            def,
            variant: v as u32,
            args: vec![bind],
        };
        let mut pat = self.pat(variant, inner, span);
        if sty != inner {
            pat = self.present_pat(pat, sty, span);
        }
        let name = self.cx.display(target);
        let msg = self.str_lit(&format!("TypeError: the value is not `{name}`"), span);
        let never = self.cx.ty.never;
        let fail = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let unit = self.cx.ty.unit;
        let nothing = self.mk(
            H::Block(hir::Block {
                stmts: vec![],
                value: None,
                span,
            }),
            unit,
            span,
        );
        let arms = vec![
            hir::Arm {
                pat,
                guard: Some(not_ok),
                body: fail,
            },
            hir::Arm {
                pat: self.pat(P::Wildcard, sty, span),
                guard: None,
                body: nothing,
            },
        ];
        let scrutinee = self.mk(H::Local(tmp, UseMode::Borrow), sty, span);
        let check = self.mk(
            H::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            unit,
            span,
        );
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(check),
            span,
        });
        let value = self.read_temp(tmp, sty, span);
        let value = self.unique_retype(value, target);
        Ok(self.with_temps_value(stmts, value))
    }
}
