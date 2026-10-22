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

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

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
    /// first, then the arguments, as in JS. Only for arguments that read values (names,
    /// literals, fields), which check alike in every arm. `Err(recv)` otherwise.
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
        let live: Vec<TyId> = match self.narrowed_variants(&recv) {
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
        if getter
            || classes == 0
            || !args.iter().all(reads_only)
            || !live.iter().all(|&m| has(self, m))
        {
            return Err(recv);
        }
        let mut first: Option<TyId> = None;
        let placeholder = self.cx.ty.error;
        let mut f = |s: &mut Self, value: hir::Expr| {
            let call = s.method_call_at(value, prop, type_args, args, exp, span, false);
            match first {
                None => {
                    first = Some(call.ty);
                    Some(call)
                }
                Some(t) if t == call.ty => Some(call),
                Some(t) => s.try_coerce(call, t).ok(),
            }
        };
        let mut h = self.map_members(recv, placeholder, &mut f)?;
        h.ty = first.unwrap_or(placeholder);
        Ok(h)
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
