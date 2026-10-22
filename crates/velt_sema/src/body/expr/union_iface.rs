//! Methods of a union whose members are interfaces (`Ready<T> | Errored<T>`, where both extend
//! `Base<T>`): a method that an interface every member extends declares (`Base<T>`'s
//! `refresh()`) is called through that interface, each member converted to it, which keeps the
//! same object (`crate::synth_ifaces`), as TypeScript allows a member every union member has.
//! When each member declares the method itself, the interface is `Pick<First, "name">` of the
//! first member, which every member converts to when its method has the same type.

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `recv`, a union value, as a value of an interface that declares method `name` and that
    /// every member it can hold extends (or is); `Err(recv)` when there is none.
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
            match self.cx.iface_member(m) {
                Some(_) => lines.push(self.iface_ancestors(m)),
                None => return Err(recv),
            }
        }
        let common = lines[0].iter().copied().find(|&a| {
            self.declares_method(a, name) && lines[1..].iter().all(|l| l.contains(&a))
        });
        if let Some(t) = common {
            return self.union_to_common(recv, t);
        }
        // Else each member declares it: through `Pick<First, "name">`, when every member has
        // the method with the same type.
        if !self.declares_method(live[0], name) {
            return Err(recv);
        }
        let span = recv.span;
        let pick = self
            .cx
            .pick_iface(live[0], vec![name.to_string()], false, self.module, span);
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
            let parents = self.cx.iface(d).map(|f| f.parents.clone()).unwrap_or_default();
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
}
