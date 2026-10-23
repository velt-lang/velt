//! Methods of a union whose members are interfaces (`Ready<T> | Errored<T>`, where both extend
//! `Base<T>`): a method that an interface every member extends declares (`Base<T>`'s
//! `refresh()`) is called through that interface, each member converted to it, which keeps the
//! same object (`crate::synth_ifaces`), as TypeScript allows a member every union member has.
//! When each member declares the method itself, the interface is `Pick<I, "name">` of the
//! first member that is an interface, which every member (an interface or a class) converts to
//! when its method has the same type. A union with a class member that has the method too
//! (`Circle | Sq` with `area()` on both) calls each member's own ([`FnCx::union_member_call`]).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// `recv`, a union value, as a value of an interface that declares method `name` and that
    /// every member it can hold extends, is, or (a class) converts to; `Err(recv)` when there
    /// is none.
    pub(super) fn union_iface_receiver(
        &mut self,
        recv: hir::Expr,
        name: &str,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(members) = self.cx.union_members(recv.ty) else {
            return Err(recv);
        };
        let live: Vec<TyId> = match self.narrowed_variants(&recv) {
            Some(vs) => vs.iter().map(|&v| members[v as usize]).collect(),
            None => members,
        };
        if live.is_empty() {
            return Err(recv);
        }
        let mut lines: Vec<Vec<TyId>> = vec![];
        for &m in &live {
            if self.cx.iface_member(m).is_some() {
                lines.push(self.iface_ancestors(m));
            }
        }
        if lines.len() == live.len() {
            let common = lines[0].iter().copied().find(|&a| {
                self.declares_method(a, name) && lines[1..].iter().all(|l| l.contains(&a))
            });
            if let Some(t) = common {
                return self.union_to_common(recv, t);
            }
        }
        // Else each member has it: through `Pick<I, "name">` of the first interface member `I`
        // that declares it, when every member (a class too) has the method with the same type.
        let Some(&first) = live.iter().find(|&&m| self.declares_method(m, name)) else {
            return Err(recv);
        };
        let span = recv.span;
        let pick = self
            .cx
            .pick_iface(first, vec![name.to_string()], false, self.module, span);
        self.union_to_common(recv, pick)
    }

    /// Interface type `t` and the interfaces it extends, directly or not (with their
    /// arguments), nearest first.
    fn iface_ancestors(&mut self, t: TyId) -> Vec<TyId> {
        let mut out = vec![t];
        let mut i = 0;
        while i < out.len() && out.len() < 64 {
            let Some((d, args)) = self.cx.iface_member(out[i]) else {
                i += 1;
                continue;
            };
            let parents = self
                .cx
                .iface(d)
                .map(|f| f.parents.clone())
                .unwrap_or_default();
            for p in parents {
                let pargs: Vec<TyId> = p.args.iter().map(|&a| self.cx.subst(a, &args)).collect();
                let pt = self.cx.ty.intern(TyKind::Dyn(p.iface, pargs));
                if !out.contains(&pt) {
                    out.push(pt);
                }
            }
            i += 1;
        }
        out
    }

    /// Does interface type `t` declare (or inherit) method `name`?
    fn declares_method(&self, t: TyId, name: &str) -> bool {
        self.cx
            .iface_member(t)
            .and_then(|(d, _)| self.cx.iface(d))
            .is_some_and(|i| i.methods.iter().any(|m| m.name == name))
    }

    /// `recv.name(args)` on a union value with interface and class members that each have
    /// method `name` (at least one a class, which converts to no interface it does not
    /// implement): `match (recv) { A(a) => a.name(args), B(b) => b.name(args) }`, the receiver
    /// first, then the arguments, as in JS. Arguments that only read values (names, literals,
    /// fields) are checked in each arm; others are evaluated once, in order after the receiver,
    /// into temporaries (each with the parameter's type when every member's agrees, else the
    /// first member's), which each arm passes. The call's type is the union of the arms'.
    /// `Err(recv)` when a member lacks the method.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn union_member_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
        getter: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(members) = self.cx.union_members(recv.ty) else {
            return Err(recv);
        };
        let narrowed = self.narrowed_variants(&recv);
        let live: Vec<TyId> = match &narrowed {
            Some(vs) => vs.iter().map(|&v| members[v as usize]).collect(),
            None => members,
        };
        let has = |s: &mut Self, m: TyId| match s.cx.class_of(m) {
            Some(_) => s.resolve_method(m, &prop.name).is_some(),
            None => s.declares_method(m, &prop.name),
        };
        let classes = live
            .iter()
            .filter(|&&m| self.cx.class_of(m).is_some())
            .count();
        let spread = args
            .iter()
            .any(|a| matches!(a.kind, ast::ExprKind::Spread(_)));
        if getter || classes == 0 || spread || !live.iter().all(|&m| has(self, m)) {
            return Err(recv);
        }
        let mut stmts = vec![];
        let (recv, args) = match args.iter().all(reads_only) {
            true => (recv, args.to_vec()),
            false => self.union_call_temps(recv, &live, prop, args, &mut stmts),
        };
        let placeholder = self.cx.ty.error;
        let mut f = |s: &mut Self, value: hir::Expr| {
            Some(s.method_call_at(value, prop, type_args, &args, exp, span, false))
        };
        let mut h = self.map_members_in(recv, placeholder, narrowed, &mut f)?;
        h.ty = self.join_arms(&mut h, span);
        Ok(self.with_temps_value(stmts, h))
    }

    /// The receiver, then each argument, evaluated into temporaries (`stmts`): the receiver's
    /// value, and the arguments as names of theirs.
    fn union_call_temps(
        &mut self,
        recv: hir::Expr,
        live: &[TyId],
        prop: &ast::Ident,
        args: &[ast::Expr],
        stmts: &mut Vec<hir::Stmt>,
    ) -> (hir::Expr, Vec<ast::Expr>) {
        let (ty, span) = (recv.ty, recv.span);
        let r = self.owned_operand(recv);
        let l = self.new_local("#recv", ty, false, span, LocalKind::Temp);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(r),
            },
            span,
        });
        let sigs: Vec<Option<Vec<TyId>>> = live
            .iter()
            .map(|&m| self.method_param_tys(m, prop, span).map(|(p, _)| p))
            .collect();
        let mut out = vec![];
        for (i, a) in args.iter().enumerate() {
            let param = |s: &Option<Vec<TyId>>| s.as_ref().and_then(|p| p.get(i).copied());
            let first = sigs.first().and_then(param);
            let exp = match sigs.iter().all(|s| param(s) == first) {
                true => first,
                false => sigs.iter().find_map(param),
            };
            let h = self.expr(a, exp, Want::Borrow);
            let h = self.owned_operand(h);
            out.push(self.temp_ident("#arg", h, stmts));
        }
        (self.mk(H::Local(l, UseMode::Borrow), ty, span), out)
    }

    /// `h` as a value a temporary can own: a share of a place (which keeps its value).
    fn owned_operand(&mut self, h: hir::Expr) -> hir::Expr {
        let mut h = match self.cx.is_copy(h.ty) {
            true => h,
            false => self.share_if_place(h),
        };
        self.force_move(&mut h);
        h
    }

    /// The type of the `match` `h` whose arms' bodies are the calls: theirs when they agree,
    /// else their union (TypeScript's `number | string`), each body converted to it.
    fn join_arms(&mut self, h: &mut hir::Expr, span: Span) -> TyId {
        let H::Match { arms, .. } = &mut h.kind else {
            return h.ty;
        };
        let mut tys: Vec<TyId> = vec![];
        for a in arms.iter() {
            let t = self.cx.widened(a.body.ty);
            if !self.cx.ty.is_bottom(a.body.ty) && !tys.contains(&t) {
                tys.push(t);
            }
        }
        let ty = match tys.as_slice() {
            [] => self.cx.ty.never,
            [t] => *t,
            _ => self.cx.union_of(&tys, false, span),
        };
        for a in arms.iter_mut() {
            if a.body.ty != ty && !self.cx.ty.is_bottom(a.body.ty) {
                let body = std::mem::replace(&mut a.body, self.error_expr(span));
                a.body = self.coerce(body, ty);
            }
        }
        ty
    }
}

/// Does argument `e` only read a value (a name, a literal, a field of one)?
fn reads_only(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) | ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member { object, .. } => reads_only(object),
        _ => false,
    }
}
